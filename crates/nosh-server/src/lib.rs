//! `nosh-server` library surface — connection setup and echo handlers exposed
//! so integration tests can drive an in-process server.

pub mod server;

pub use server::{build_server_config, make_endpoint, run_accept_loop};
