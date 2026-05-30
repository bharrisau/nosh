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

    // ── Phase 6: Cold Reattach Protocol ──────────────────────────────────────
    //
    // These five variants are appended AFTER `SessionClose` to preserve the
    // postcard discriminant order of all existing variants. Inserting or
    // reordering is NOT backward-compatible. The token fields carry CSPRNG
    // bytes and MUST NOT be logged; callers log only the identity fingerprint.

    /// Server → client, sent immediately after a successful fresh `SessionOpen`.
    /// Delivers the initial reattach token the client must hold in memory
    /// (D-01 / D-05) for the next cold reattach attempt.
    ///
    /// WARNING: the `token` bytes MUST NOT be logged. Log only the identity
    /// fingerprint (D-07).
    SessionOpened {
        /// CSPRNG reattach token (122-bit, uuid v4 bytes). Single-use: rotated
        /// on every successful reattach.
        token: [u8; 16],
    },

    /// Client → server, the FIRST frame on a reconnected QUIC connection, in
    /// place of `SessionOpen` (D-03 / D-04).
    ///
    /// `last_acked_seq` convention (LOCKED — **next-expected-seq**):
    /// it is the **count of output chunks the client has applied**, which —
    /// because the server numbers chunks 0-based — equals the **sequence number
    /// of the next chunk the client expects** (the lowest seq it has NOT yet
    /// applied). After applying 0-based seqs `0..=K` (i.e. `K+1` chunks) the
    /// client reports `K+1`.
    ///
    /// The server replays every buffered chunk with `seq >= last_acked_seq`
    /// (inclusive — see `SequencedOutputBuffer::replay_from`). A value of `0`
    /// means "applied nothing": replay everything from the first retained chunk
    /// (seq 0), or from `lowest_retained_seq` if the buffer was truncated. No
    /// sentinel is needed because seq is 0-based: "next expected = 0" is the
    /// natural empty state.
    ///
    /// WARNING: the `token` bytes MUST NOT be logged. Log only the identity
    /// fingerprint (D-07).
    Reattach {
        /// The reattach token last received from the server (initial
        /// `SessionOpened.token` or the most recent `ReattachOk.new_token`).
        token: [u8; 16],
        /// Count of output chunks the client has applied == the seq of the
        /// next chunk it expects (next-expected-seq convention). The server
        /// replays all chunks with seq GREATER THAN OR EQUAL TO this value.
        last_acked_seq: u64,
    },

    /// Server → client on a successful reattach (D-03 / D-05 / D-09).
    /// The server sends this as the very first frame on the new stream, then
    /// replays one `PtyData` frame for each chunk with seq `>= last_acked_seq`
    /// (next-expected-seq convention; see `Message::Reattach`). When the buffer
    /// was truncated below the requested resume point, replay instead starts at
    /// `lowest_retained_seq == replaying_from_seq`.
    ///
    /// WARNING: `new_token` MUST NOT be logged. Log only the identity
    /// fingerprint (D-07).
    ReattachOk {
        /// Rotated single-use reattach token. The client MUST replace its
        /// stored token with this value immediately.
        new_token: [u8; 16],
        /// The seq of the FIRST replayed chunk. Normally equals the client's
        /// reported `last_acked_seq` (next-expected-seq); equals
        /// `lowest_retained_seq` when `truncated == true`. The client rebases
        /// its applied-count to this value so the first replayed chunk lands at
        /// the right offset with no off-by-one.
        replaying_from_seq: u64,
        /// `true` when the requested resume point (`last_acked_seq`) predates
        /// the buffer's `lowest_retained_seq` (the 64 KiB cap dropped those
        /// bytes). The client should display a truncation notice (D-09).
        truncated: bool,
    },

    /// Server → client on ANY reattach failure. FIELDLESS and UNIFORM — there
    /// is deliberately no reason code or distinguishing field (D-07). Unknown
    /// token, expired token, wrong SSH identity, active/reconnecting session:
    /// ALL map to this identical variant. This is the no-oracle invariant:
    /// an attacker cannot distinguish "session exists but wrong key" from
    /// "session does not exist".
    ///
    /// INVARIANT: this variant MUST remain fieldless forever. Adding a
    /// reason field would create a session-existence oracle.
    ReattachErr,

    /// Client → server, periodic; carries the **next-expected-seq** == the
    /// count of output chunks the client has applied (D-08 continuous acking),
    /// using the SAME convention as `Message::Reattach::last_acked_seq`.
    ///
    /// The server calls `SequencedOutputBuffer::trim_acked(seq)`, which drops
    /// every chunk with seq STRICTLY LESS THAN `seq` (the chunks the client has
    /// already applied: seqs `0..seq`). It MUST NOT drop seq `>= seq` (chunks
    /// the client has not yet applied). Cadence is coarse (time-interval or
    /// byte-threshold), not per-chunk.
    Ack {
        /// Next-expected-seq == count of output chunks the client has applied.
        seq: u64,
    },
}

impl Message {
    /// The variant's static name, with NO payload. Use this for logging instead
    /// of `Debug` (`{:?}`): several variants (`SessionOpened`, `Reattach`,
    /// `ReattachOk`) carry CSPRNG token bytes, and the D-07 invariant forbids
    /// ever logging a token. Logging the variant name is always safe.
    pub fn variant_name(&self) -> &'static str {
        match self {
            Message::SessionOpen { .. } => "SessionOpen",
            Message::PtyData { .. } => "PtyData",
            Message::Resize { .. } => "Resize",
            Message::SessionClose { .. } => "SessionClose",
            Message::SessionOpened { .. } => "SessionOpened",
            Message::Reattach { .. } => "Reattach",
            Message::ReattachOk { .. } => "ReattachOk",
            Message::ReattachErr => "ReattachErr",
            Message::Ack { .. } => "Ack",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// W3 / D-07: `variant_name` must render ONLY the variant name for
    /// token-bearing variants — never the token bytes. This is the safe
    /// logging path that replaces `Debug` at every dispatch/error site.
    #[test]
    fn variant_name_never_leaks_token_bytes() {
        let secret = [0xABu8; 16];
        for (msg, expected) in [
            (Message::SessionOpened { token: secret }, "SessionOpened"),
            (
                Message::Reattach { token: secret, last_acked_seq: 7 },
                "Reattach",
            ),
            (
                Message::ReattachOk { new_token: secret, replaying_from_seq: 3, truncated: false },
                "ReattachOk",
            ),
            (Message::ReattachErr, "ReattachErr"),
            (Message::Ack { seq: 1 }, "Ack"),
        ] {
            let name = msg.variant_name();
            assert_eq!(name, expected);
            // The hex of the secret token must NOT appear anywhere in the name.
            assert!(
                !name.to_lowercase().contains("ab"),
                "variant_name must not contain token bytes: {name}"
            );
        }
    }
}
