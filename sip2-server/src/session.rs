use super::conf;
use eg::auth;
use evergreen as eg;
use opensrf as osrf;
use sip2;
use std::collections::HashMap;
use std::fmt;
use std::net;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

// Block this many seconds before waking to see if we need
// to perform any maintenance / shutdown.
const SIP_RECV_TIMEOUT: u64 = 5;

/* --------------------------------------------------------- */
// By order of appearance in the INSTITUTION_SUPPORTS string:
// patron status request
// checkout
// checkin
// block patron
// acs status
// request sc/acs resend
// login
// patron information
// end patron session
// fee paid
// item information
// item status update
// patron enable
// hold
// renew
// renew all
const INSTITUTION_SUPPORTS: &str = "YYYNYNYYNYYNNNYN";
/* --------------------------------------------------------- */

/// Manages a single SIP client connection.
pub struct Session {
    /// Unique session identifier; mostly for logging.
    sesid: usize,

    sip_connection: sip2::Connection,

    /// If true, the server is shutting down, so we should exit.
    shutdown: Arc<AtomicBool>,
    sip_config: conf::Config,
    osrf_client: osrf::Client,

    /// Used for pulling trivial data from Evergreen, i.e. no API required.
    editor: eg::editor::Editor,

    // We won't have some values until the SIP client logs in.
    account: Option<conf::SipAccount>,

    /// Cache of org unit shortnames and IDs.
    org_cache: HashMap<i64, json::JsonValue>,
}

impl Session {
    /// Our thread starts here.  If anything fails, we just log and exit
    pub fn run(
        sip_config: conf::Config,
        osrf_config: Arc<osrf::Config>,
        idl: Arc<eg::idl::Parser>,
        stream: net::TcpStream,
        sesid: usize,
        shutdown: Arc<AtomicBool>,
    ) {
        match stream.peer_addr() {
            Ok(a) => log::info!("New SIP connection from {a}"),
            Err(e) => {
                log::error!("SIP connection has no peer addr? {e}");
                return;
            }
        }

        let mut con = sip2::Connection::from_stream(stream);
        con.set_ascii(sip_config.ascii());

        let osrf_client = match osrf::Client::connect(osrf_config.clone()) {
            Ok(c) => c,
            Err(e) => {
                log::error!("Cannot connect to OpenSRF: {e}");
                return;
            }
        };

        osrf_client.set_serializer(eg::idl::Parser::as_serializer(&idl));

        let editor = eg::Editor::new(&osrf_client, &idl);

        let mut ses = Session {
            sesid,
            editor,
            shutdown,
            sip_config,
            osrf_client,
            account: None,
            sip_connection: con,
            org_cache: HashMap::new(),
        };

        if let Err(e) = ses.start() {
            // This is not necessarily an error.  The client may simply
            // have disconnected.  There is no "disconnect" message in
            // SIP -- you just chop off the socket.
            log::info!("{ses} exited with message: {e}");
        }
    }

    pub fn org_cache(&self) -> &HashMap<i64, json::JsonValue> {
        &self.org_cache
    }

    pub fn org_cache_mut(&mut self) -> &mut HashMap<i64, json::JsonValue> {
        &mut self.org_cache
    }

    /// True if our SIP client has successfully logged in.
    pub fn has_account(&self) -> bool {
        self.account.is_some()
    }

    /// Panics if no account has been set
    pub fn account(&self) -> &conf::SipAccount {
        self.account.as_ref().expect("No account set")
    }

    /// Panics if no account has been set
    pub fn account_mut(&mut self) -> &mut conf::SipAccount {
        self.account.as_mut().expect("No account set")
    }

    pub fn sip_config(&self) -> &conf::Config {
        &self.sip_config
    }

    pub fn osrf_client_mut(&mut self) -> &mut osrf::Client {
        &mut self.osrf_client
    }

    pub fn editor_mut(&mut self) -> &mut eg::editor::Editor {
        &mut self.editor
    }

    pub fn editor(&self) -> &eg::editor::Editor {
        &self.editor
    }

    /// Verifies the existing authtoken if present, requesting a new
    /// authtoken when necessary.
    ///
    /// Returns Err if we fail to verify the token or login as needed.
    pub fn set_authtoken(&mut self) -> Result<(), String> {
        if self.editor.authtoken().is_some() {
            if self.editor.checkauth()? {
                return Ok(());
            }
        }

        self.login()
    }

    pub fn authtoken(&self) -> Result<&str, String> {
        match self.editor().authtoken() {
            Some(a) => Ok(a),
            None => Err(format!("Authtoken is unset")),
        }
    }

    /// Find the ID of the ILS user account whose username matches
    /// the ILS username for our SIP account.
    ///
    /// Cache the user id after the first lookup.
    fn get_ils_user_id(&mut self) -> Result<i64, String> {
        if let Some(id) = self.account().ils_user_id() {
            return Ok(id);
        }

        let ils_username = self.account().ils_username().to_string();

        let search = json::object! {
            usrname: ils_username.as_str(),
            deleted: "f",
        };

        let users = self.editor_mut().search("au", search)?;

        let user_id = match users.len() > 0 {
            true => eg::util::json_int(&users[0]["id"])?,
            false => Err(format!("No such user: {ils_username}"))?,
        };

        self.account_mut().set_ils_user_id(user_id);

        Ok(user_id)
    }

    /// Create a internal auth session in the ILS
    fn login(&mut self) -> Result<(), String> {
        let ils_user_id = self.get_ils_user_id()?;
        let mut args = auth::AuthInternalLoginArgs::new(ils_user_id, "staff");

        if self.has_account() {
            if let Some(w) = self.account().workstation() {
                args.workstation = Some(w.to_string());
            }
        }

        let auth_ses = match auth::AuthSession::internal_session(&self.osrf_client, &args)? {
            Some(s) => s,
            None => Err(format!("Internal Login failed"))?,
        };

        self.editor.set_authtoken(auth_ses.token());

        // Set editor.requestor
        self.editor.checkauth()?;

        Ok(())
    }

    /// Wait for SIP requests in a loop and send replies.
    ///
    /// Exits when the shutdown signal is set or on unrecoverable error.
    fn start(&mut self) -> Result<(), String> {
        log::debug!("{self} starting");

        loop {
            if self.shutdown.load(Ordering::Relaxed) {
                log::debug!("{self} Shutdown notice received, exiting listen loop");
                break;
            }

            // Blocks waiting for a SIP request to arrive
            let sip_req_op = self
                .sip_connection
                .recv_with_timeout(SIP_RECV_TIMEOUT)
                .or_else(|e| Err(format!("{self} SIP recv() failed: {e}")))?;

            let sip_req = match sip_req_op {
                Some(r) => r,
                None => continue,
            };

            log::trace!("{self} Read SIP message: {:?}", sip_req);

            let sip_resp = self.handle_sip_request(&sip_req)?;

            log::trace!("{self} server replying with {sip_resp:?}");

            // Send the SIP response back to the SIP client
            self.sip_connection
                .send(&sip_resp)
                .or_else(|e| Err(format!("SIP send failed: {e}")))?;

            log::debug!("{self} Successfully relayed response back to SIP client");
        }

        log::info!("{self} shutting down");

        self.sip_connection.disconnect().ok();

        Ok(())
    }

    /// Process a single SIP request.
    fn handle_sip_request(&mut self, msg: &sip2::Message) -> Result<sip2::Message, String> {
        let code = msg.spec().code;

        if code.eq("99") {
            return self.handle_sc_status(msg);
        } else if code.eq("93") {
            return self.handle_login(msg);
        }

        // All remaining request require authentication
        if self.account.is_none() {
            Err(format!("SIP client is not logged in"))?;
        }

        match code {
            "09" => self.handle_checkin(msg),
            "11" => self.handle_checkout(msg),
            "17" => self.handle_item_info(msg),
            "23" => self.handle_patron_status(msg),
            "37" => self.handle_payment(msg),
            "63" => self.handle_patron_info(msg),
            _ => Err(format!("Unsupported SIP message code={}", msg.spec().code)),
        }
    }

    fn handle_login(&mut self, msg: &sip2::Message) -> Result<sip2::Message, String> {
        self.account = None;
        let mut login_ok = sip2::util::num_bool(false);

        if let Some(username) = msg.get_field_value("CN") {
            if let Some(password) = msg.get_field_value("CO") {
                // Caller sent enough values to attempt login

                if let Some(account) = self.sip_config().get_account(&username) {
                    if account.sip_password().eq(&password) {
                        login_ok = sip2::util::num_bool(true);
                        self.account = Some(account.clone());
                    }
                } else {
                    log::warn!("No such SIP account: {username}");
                }
            } else {
                log::warn!("Login called with no password");
            }
        } else {
            log::warn!("Login called with no username");
        }

        Ok(sip2::Message::from_ff_values(&sip2::spec::M_LOGIN_RESP, &[login_ok]).unwrap())
    }

    fn handle_sc_status(&mut self, _msg: &sip2::Message) -> Result<sip2::Message, String> {
        if self.account.is_none() && !self.sip_config().sc_status_before_login() {
            Err(format!("SC Status before login disabled"))?;
        }

        let mut resp = sip2::Message::from_values(
            &sip2::spec::M_ACS_STATUS,
            &[
                sip2::util::sip_bool(true),  // online status
                sip2::util::sip_bool(true),  // checkin ok
                sip2::util::sip_bool(true),  // checkout ok
                sip2::util::sip_bool(true),  // renewal policy
                sip2::util::sip_bool(false), // status update
                sip2::util::sip_bool(false), // offline ok
                "999",                       // timeout
                "999",                       // max retries
                &sip2::util::sip_date_now(),
                "2.00", // SIP version
            ],
            &[("BX", INSTITUTION_SUPPORTS), ("AF", ""), ("AG", "")],
        )
        .unwrap();

        if let Some(a) = &self.account {
            resp.add_field("AO", a.settings().institution());

            // This sets the requestor value on our editor so we can
            // find its workstation / home org.
            self.set_authtoken()?;

            if let Some(org) = self.org_from_id(self.get_ws_org_id()?)? {
                resp.add_field("AM", org["name"].as_str().unwrap());
                resp.add_field("AN", org["shortname"].as_str().unwrap());
            }
        }

        Ok(resp)
    }
}

impl fmt::Display for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(ref acct) = self.account {
            write!(f, "SIPSession({} {})", self.sesid, acct.sip_username())
        } else {
            write!(f, "SIPSession({})", self.sesid)
        }
    }
}
