//! `nosh-server` library surface — connection setup and the PTY session pump,
//! exposed so integration tests can drive an in-process server.

pub mod channel;
pub mod pty_io;
pub mod quinn_transport;
pub mod registry;
pub mod server;
pub mod session;
pub mod terminal;

pub use server::{build_server_config, make_endpoint, run_accept_loop};
