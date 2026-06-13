//! `nosh-proto` — shared wire types, message codec, ALPN constant, and the
//! shared quinn transport configuration used by both `nosh-server` and
//! `nosh-client`.
//!
//! The serialization format (postcard) is isolated behind the [`codec`] module
//! and the single [`messages::Message`] type so it can be swapped for protobuf
//! (prost) later as a one-file change — see decision D-04.

pub mod codec;
pub mod datagram;
pub mod messages;
pub mod transport;
pub mod transport_trait;

#[cfg(test)]
mod transport_trait_tests;

pub use codec::{decode, encode, read_message, write_message, ProtoError};
pub use datagram::{CellStyle, ClientEpoch, CursorPos, DiffRun, MAX_RUNS, MIN_CAP, StateDiff, decode_datagram, decode_epoch_ack, encode_datagram, encode_epoch_ack};
pub use messages::{Message, TerminalControlPayload};
pub use transport::transport_config;
pub use transport_trait::{
    NoshTransport, NoshSendStream, NoshRecvStream, SendDatagramError,
    write_message_ns, read_message_ns,
};

/// The single canonical ALPN identifier for the nosh protocol.
///
/// QUIC mandates ALPN; this exact byte string MUST be set on both the client
/// and server rustls configs. A mismatch aborts the TLS handshake with QUIC
/// error 0x178 (`no_application_protocol`). See research PITFALL 4.
pub const ALPN: &[u8] = b"nosh/0";

// ── Phase 25: Inner SSH-key handshake transcript layout constants (D-01) ─────
//
// Single source of truth for the inner-auth transcript. Both
// `nosh-server/src/inner_auth.rs` and `nosh-client/src/inner_auth.rs` import
// these constants rather than inlining literals. This is the mandatory guard
// against Pitfall 1 (label divergence between client and server causing each
// endpoint to derive different EKM bytes, silently breaking auth).

/// RFC 9266 EKM label for the inner SSH-key handshake.
///
/// Both sides call `NoshTransport::export_keying_material(output,
/// INNER_AUTH_EKM_LABEL, INNER_AUTH_EKM_CONTEXT)`. Using the same label +
/// context + output length on both ends guarantees identical 32-byte EKM
/// (D-01 channel-binding requirement).
pub const INNER_AUTH_EKM_LABEL: &[u8] = b"nosh-inner-auth-v1";

/// RFC 9266 EKM context for the inner SSH-key handshake.
///
/// Empty context — the label alone is sufficient for domain separation here.
/// Paired with `INNER_AUTH_EKM_LABEL` at every `export_keying_material` call site.
pub const INNER_AUTH_EKM_CONTEXT: &[u8] = b"";

/// Transcript domain-separation label for the message the CLIENT signs.
///
/// The client signs `SHA256(INNER_AUTH_LABEL_CLIENT || ekm || server_nonce ||
/// client_nonce || server_spki)`. The NUL byte suffix (`\0`) domain-separates
/// this transcript from `INNER_AUTH_LABEL_SERVER` so a server-transcript
/// signature cannot be reused as a valid client-transcript signature.
pub const INNER_AUTH_LABEL_CLIENT: &[u8] = b"nosh-inner-auth-v1\0";

/// Transcript domain-separation label for the message the SERVER signs.
///
/// The server signs `SHA256(INNER_AUTH_LABEL_SERVER || ekm || server_nonce ||
/// client_nonce || client_spki)`. The distinct label prevents a cross-role
/// transcript substitution attack where a client-transcript signature is
/// presented as a server-transcript signature (D-01 / Pitfall 3).
pub const INNER_AUTH_LABEL_SERVER: &[u8] = b"nosh-inner-auth-v1-server\0";
