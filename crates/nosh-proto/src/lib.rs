//! `nosh-proto` — shared wire types, message codec, ALPN constant, and the
//! shared quinn transport configuration used by both `nosh-server` and
//! `nosh-client`.
//!
//! The serialization format (postcard) is isolated behind the [`codec`] module
//! and the single [`messages::Message`] type so it can be swapped for protobuf
//! (prost) later as a one-file change — see decision D-04.

pub mod codec;
pub mod messages;
pub mod transport;

pub use codec::{decode, encode, read_message, write_message, ProtoError};
pub use messages::Message;
pub use transport::transport_config;

/// The single canonical ALPN identifier for the nosh protocol.
///
/// QUIC mandates ALPN; this exact byte string MUST be set on both the client
/// and server rustls configs. A mismatch aborts the TLS handshake with QUIC
/// error 0x178 (`no_application_protocol`). See research PITFALL 4.
pub const ALPN: &[u8] = b"nosh/0";
