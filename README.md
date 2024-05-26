# Evergreen Universe / Rust

Rust bindings, libs, and binaries for Evergreen and related projects.

## Included Packages

### MPTC

General purpose threaded server, similar to Perl Net::Server.

### Evergreen

Evergreen + OpenSRF bindings with OpenSRF server, nascent services, and 
other binaries.

[README](./evergreen/README.md)

### MARC

Library for reading/writing MARC Binary, MARC XML, and MARC Breaker.

[README](./marc/README.md)

### SIP2

SIP2 client library

[README](./sip2/README.md)

### SIP2-Mediator

SIP2 Mediator

[README](./sip2-mediator/README.md)

## Evergreen Rust Primer

Currently assumes Ubuntu 22.04.

### Setup

Actions that communicate via OpenSRF require the OpenSRF/Evergreen
Redis branches be installed and running.

Other actions, e.g. eg-marc-export, which communicate via database 
connection do not require special OpenSRF/Evergreen code.

#### Install OpenSRF / Evergreen with Redis

#### Ansible Version

Follow [these ansible instructions](
    https://github.com/berick/evergreen-ansible-installer/tree/working/ubuntu-22.04-redis)
to install on a server/VM.

#### Docker Version

Follow [these instructions](https://github.com/mcoia/eg-docker) to create
a Docker container.

#### Setup Rust

```sh
sudo apt install rust-all 
git clone github.com:kcls/evergreen-universe-rs                                
```

### Build Everything and Run Tests

#### Makefile Note

Build and install commands are compiled into a Makefile for convenience
and documentation.  See the Makefile for individual `cargo` commands.

#### Build and Test

```sh
cd evergreen-universe-rs

# This will also download and compile dependencies.
make build

# Run unit tests
make test

# To also run the live tests.
# These require a locally running Evergreen instance with
# Concerto data.
cargo test --package evergreen --test live -- --ignored --nocapture

# OPTIONAL: Install compiled binaries to /usr/local/bin/
sudo make install-bin
```

### Example: Running egsh ("eggshell")

`egsh` is an Evergreen-aware srfsh clone

```sh
cargo run --package evergreen --bin egsh
```

> **_NOTE:_** If binaries are installed, the above command may be shortened to just `egsh`.

#### Some Commands

```sh
egsh# help

egsh# login admin demo123

# This uses the authtoken stored from last successful use of 'login'
# as the implicit first parameter to the pcrud API call.
egsh# reqauth open-ils.pcrud open-ils.pcrud.retrieve.au 1

egsh# req opensrf.settings opensrf.system.echo {"a b c":123} "12" [1,2,3]

egsh# cstore retrieve actor::user 1

egsh# cstore search aou {"shortname":"BR1"}
```


