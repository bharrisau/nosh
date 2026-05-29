//! Wire message types for the nosh control protocol.
//!
//! Phase 1 defined the `Message` enum with `SessionClose`. Phase 3 (PTY session
//! core, decision D-01) carries the ENTIRE interactive session — `SessionOpen`,
//! PTY data both directions, `Resize`, and `SessionClose` — as `Message`
//! variants framed over a single bidirectional QUIC stream by the existing
//! length-delimited postcard [`codec`](crate::codec). No raw-byte side channel
//! and no datagrams for shell I/O this milestone (D-02).

use serde::{Deserialize, Serialize};

/// A control/session-protocol message exchanged over a reliable QUIC stream.
///
/// The session lifecycle on the single bidi stream is:
/// 1. client → server: [`Message::SessionOpen`] (always the first frame),
/// 2. both directions: [`Message::PtyData`] (keystrokes up, shell output down)
///    and client → server [`Message::Resize`] on window changes,
/// 3. server → client: [`Message::SessionClose`] carrying the shell exit code,
///    immediately before the QUIC connection is closed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Message {
    /// First frame the client sends: requests a PTY session with the local
    /// terminal type, initial window size, and the client's *whitelisted*
    /// environment (SendEnv-style, D-05). The server re-filters this env
    /// deny-by-default before spawning the shell (D-06) — it is never trusted
    /// verbatim. `env` is an ordered list (not a map) for deterministic
    /// postcard encoding and stable test assertions.
    SessionOpen {
        /// `TERM` value for the remote PTY.
        term: String,
        /// Initial window width in columns.
        cols: u16,
        /// Initial window height in rows.
        rows: u16,
        /// Client-forwarded environment as ordered (key, value) pairs.
        env: Vec<(String, String)>,
    },
    /// Raw PTY bytes. Sent client → server (keystrokes, incl. Ctrl-C as `0x03`)
    /// and server → client (shell output). Carries no framing beyond the codec.
    PtyData {
        /// The raw PTY byte payload.
        data: Vec<u8>,
    },
    /// Window resize (SESS-05): client → server when the local terminal size
    /// changes (debounced/coalesced). The server calls `MasterPty::resize`.
    Resize {
        /// New width in columns.
        cols: u16,
        /// New height in rows.
        rows: u16,
    },
    /// Session terminated; carries the shell exit code and a reason string.
    /// The client then exits its own process with `exit_code` (SESS-08).
    SessionClose { exit_code: i32, reason: String },
}
