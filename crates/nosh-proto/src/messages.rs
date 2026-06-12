//! Wire message types for the nosh control protocol.
//!
//! Phase 1 defined the `Message` enum with `SessionClose`. Phase 3 (PTY session
//! core, decision D-01) carries the ENTIRE interactive session — `SessionOpen`,
//! PTY data both directions, `Resize`, and `SessionClose` — as `Message`
//! variants framed over a single bidirectional QUIC stream by the existing
//! length-delimited postcard [`codec`](crate::codec). No raw-byte side channel
//! and no datagrams for shell I/O this milestone (D-02).

use serde::{Deserialize, Serialize};

// ── Phase 22: Scrollback wire payload types ───────────────────────────────────

/// A single cell in a scrollback line, carrying the full cell content including
/// per-character SGR attributes (S-3: original per-line width metadata retained;
/// no server-side reflow).
///
/// Field types match `nosh_proto::datagram::DiffRun` to enable zero-copy
/// assembly from `terminal.rs` `Cell` values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScrollbackCell {
    /// Unicode scalar value. `' '` means blank/empty.
    pub ch: char,
    /// SGR attributes packed as bitflags. Same type as `DiffRun.style`.
    pub style: crate::datagram::CellStyle,
    /// ANSI 256-colour foreground. `None` = terminal default; `Some(n)` = palette index.
    pub fg: Option<u8>,
    /// ANSI 256-colour background. `None` = terminal default; `Some(n)` = palette index.
    pub bg: Option<u8>,
}

/// One scrollback line: original column width metadata (S-3) plus the cell content.
///
/// The `width` field carries the column count at the time the line scrolled into
/// history (i.e. the terminal width when the line was last visible). The client
/// may use this to render variable-width lines without server-side reflow.
///
/// An empty `cells` vec is valid — it represents a blank line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScrollbackLine {
    /// Terminal column width when this line was live (S-3 original width metadata).
    pub width: u16,
    /// Per-cell content. All cells in the line are always sent; length == `width`
    /// (no trailing-blank omission — every cell is included for correct rendering).
    pub cells: Vec<ScrollbackCell>,
}

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

    // ── Phase 16: Out-of-band terminal control passthrough ───────────────────
    //
    // Appended AFTER `Ack` (discriminant 9) to preserve all existing discriminant
    // orderings. NEVER insert or reorder variants — postcard encoding is
    // NOT backward-compatible if discriminants shift (see append-only invariant
    // at lines 56-62 above).

    /// Server → client out-of-band terminal control passthrough (D-16-01, D-16-02).
    ///
    /// Carries OSC sequences that the server intercepted and wants to forward
    /// to the client for out-of-band re-emission. The client re-emits the
    /// payload to stdout, BYPASSING the compositor (not through the cell grid).
    ///
    /// **Reliable stream only (no MTU limit)**: this variant is always sent over
    /// the reliable bidirectional QUIC stream using `write_message` / `read_message`,
    /// NEVER as a datagram (`conn.send_datagram`).
    ///
    /// **Security**: only WRITE-form payloads are ever forwarded. The OSC 52
    /// read/query form (`?`) is silently dropped in `osc_dispatch` before it can
    /// reach the forwarding path (D-16-01a / T-16-01).
    TerminalControl(TerminalControlPayload),

    // ── Phase 21: Channel Multiplexing Foundation ────────────────────────────
    //
    // These variants are appended AFTER `TerminalControl` (discriminant 9) to
    // preserve the postcard discriminant order of all existing variants.
    // Inserting or reordering is NOT backward-compatible. The
    // discriminant-stability test in codec.rs (message_discriminant_order_is_stable)
    // enforces this invariant.
    // APPEND-ONLY from here.

    /// Client → server: request to open a new logical channel (MUX-01).
    ///
    /// Client-initiated channels MUST use even `channel_id` values; server-initiated
    /// channels use odd values (parity prevents simultaneous-open collisions, MUX-04).
    /// Channel id 0 is reserved for the session control stream.
    ///
    /// The server responds with [`Message::ChannelAccept`] or [`Message::ChannelReject`]
    /// on the control stream before any data stream is opened.
    ChannelOpen {
        /// The application-level channel identifier. Client-initiated: MUST be even.
        channel_id: u32,
        /// The requested logical channel type.
        channel_type: ChannelType,
    },

    /// Server → client: the requested channel has been accepted (MUX-01).
    ///
    /// After receiving this, the opener writes the `channel_id` as a varint prefix
    /// at the start of a freshly opened QUIC bidi stream (MUX-02).
    ChannelAccept {
        /// The application-level channel identifier that was accepted.
        channel_id: u32,
    },

    /// Server → client: the requested channel has been rejected (MUX-01).
    ///
    /// OPAQUE by design (MUX-01 / T-21-02): carries no reason code so a rejected
    /// channel reveals nothing about server capabilities.
    ChannelReject {
        /// The application-level channel identifier that was rejected.
        channel_id: u32,
    },

    /// Either direction: grants additional byte-credit to the send side of a
    /// channel (MUX-03). Sent on the control stream, not the channel's data stream.
    ///
    /// Initial window is 256 KiB per channel. The consumer sends this after
    /// draining its buffer to allow more data.
    ChannelCredit {
        /// The channel identifier whose credit is being replenished.
        channel_id: u32,
        /// Number of additional bytes the send side may transmit.
        bytes: u64,
    },

    /// Either direction: signals that the sender is closing its side of the
    /// channel (MUX-04). The receiver should finish draining and close its side.
    ChannelClose {
        /// The channel identifier being closed.
        channel_id: u32,
    },

    // ── Phase 22: Scrollback Sync — append-only after ChannelClose (discriminant 14). ─
    //
    // These variants are appended AFTER `ChannelClose` (discriminant 14) to
    // preserve the postcard discriminant order of all existing variants.
    // Inserting or reordering is NOT backward-compatible. The
    // discriminant-stability test in codec.rs (message_discriminant_order_is_stable)
    // enforces this invariant.
    // APPEND-ONLY from here.

    /// Client → server: request a page of scrollback lines (SCROLL-01).
    ///
    /// Travels on the scrollback channel's own data stream (`RecvStream`), NOT the
    /// control stream — avoids the M-2 control/data flow-control deadlock (Pitfall 7).
    ///
    /// `from_line` is indexed from the newest scrollback line backwards:
    /// 0 = line just above the live viewport.
    ScrollbackRequest {
        /// The scrollback channel identifier.
        channel_id: u32,
        /// Index of the first line to fetch, from newest backwards.
        /// 0 = the most recent line (just above the live viewport).
        from_line: u64,
        /// Number of lines requested (default page size: 256).
        count: u32,
    },

    /// Server → client: a page of scrollback lines (SCROLL-01 / S-5).
    ///
    /// Travels on the scrollback channel's `SendStream` (reliable, NEVER via
    /// `send_datagram` — S-1 type-level enforcement). May have an empty `lines`
    /// vec if the requested range is past the top of history.
    ScrollbackPage {
        /// The scrollback channel identifier.
        channel_id: u32,
        /// The index of the first line in this page (same coordinate as
        /// `ScrollbackRequest.from_line`).
        from_line: u64,
        /// Total number of scrollback lines available at snapshot time.
        /// The client uses this to detect when it has reached the top of history.
        total_available: u64,
        /// The datagram epoch at the moment this page was snapshotted (LOCKED — S-5).
        ///
        /// The client applies scrollback history up to (not including) this epoch,
        /// then waits for a live datagram with `epoch >= epoch_at_snapshot` before
        /// transitioning back to live-grid rendering — ensuring no gap or duplicate
        /// at the scrollback/live boundary.
        ///
        /// Captured atomically with the scrollback lines under the same
        /// `terminal_state` mutex acquisition (no torn read).
        epoch_at_snapshot: u64,
        /// Per-line content in display order (oldest first within the page).
        /// May be empty (past top of history).
        lines: Vec<ScrollbackLine>,
    },

    /// Either direction: grants additional byte-credit on the scrollback channel
    /// (SCROLL-02 / MUX-03). Byte-granular, consistent with `ChannelCredit`.
    ///
    /// Sent on the control stream (not the channel's data stream).
    ScrollbackCredit {
        /// The scrollback channel identifier.
        channel_id: u32,
        /// Number of additional bytes the send side may transmit.
        bytes: u64,
    },
}

/// Payload for a [`Message::TerminalControl`] frame.
///
/// Each variant represents one category of out-of-band terminal control
/// sequence forwarded from server to client (D-16-01, D-16-02).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TerminalControlPayload {
    /// OSC 52 clipboard-write passthrough (write-only, D-16-01a).
    ///
    /// The server detected an OSC 52 clipboard-write sequence in the PTY output
    /// stream and forwards it to the client for out-of-band re-emission to the
    /// local clipboard. The client re-emits `\x1b]52;<selection>;<data>\x07`
    /// directly to stdout, bypassing the compositor.
    ///
    /// **NEVER contains the read/query form**: the `?` data value is silently
    /// dropped in `osc_dispatch` before reaching this type (D-16-01a / T-16-01).
    /// Only write payloads (non-`?` base64 data) appear here.
    Clipboard {
        /// The clipboard selection designator bytes (e.g. `b"c"` for the system
        /// clipboard). Corresponds to OSC 52's first parameter after the code.
        selection: Vec<u8>,
        /// The base64-encoded clipboard content bytes. Corresponds to OSC 52's
        /// second parameter. Never `b"?"` (the read/query form is always dropped).
        data: Vec<u8>,
    },
    /// OSC 0/2 terminal title passthrough (D-16-02).
    ///
    /// The server detected an OSC 0 (icon + window title) or OSC 2 (window title)
    /// sequence and forwards the title string to the client. The client re-emits
    /// `\x1b]2;<title>\x07` directly to stdout so the local terminal window title
    /// is updated. Bounded to `MAX_TITLE_BYTES` (1024 bytes) by `osc_dispatch`.
    Title {
        /// The terminal window title string. Bounded to `MAX_TITLE_BYTES` (1024).
        title: String,
    },
}

/// The logical channel type carried in a [`Message::ChannelOpen`] frame.
///
/// APPEND-ONLY: adding a variant is backward-compatible; removing or reordering
/// corrupts deployed connections (postcard encodes by source-order position).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChannelType {
    /// Test-only echo channel. NOT a production channel type.
    ///
    /// Accepted by the server only in `#[cfg(test)]` builds; unconditionally
    /// rejected in production. Exists solely to prove the mux layer end-to-end.
    Echo,
    /// Scrollback sync channel (Phase 22 consumer).
    Scrollback,
    /// Port-forward channel — declared but REJECTed by v1.3 peers (FWD-01 deferred).
    PortForward,
    /// Agent-forward channel — declared but REJECTed by v1.3 peers (FWD-02 deferred).
    AgentForward,
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
            Message::TerminalControl(_) => "TerminalControl",
            // Phase 21 mux variants:
            Message::ChannelOpen { .. } => "ChannelOpen",
            Message::ChannelAccept { .. } => "ChannelAccept",
            Message::ChannelReject { .. } => "ChannelReject",
            Message::ChannelCredit { .. } => "ChannelCredit",
            Message::ChannelClose { .. } => "ChannelClose",
            // Phase 22 scrollback variants:
            Message::ScrollbackRequest { .. } => "ScrollbackRequest",
            Message::ScrollbackPage { .. } => "ScrollbackPage",
            Message::ScrollbackCredit { .. } => "ScrollbackCredit",
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
            // Phase 16: TerminalControl must not leak payload bytes.
            (
                Message::TerminalControl(TerminalControlPayload::Clipboard {
                    selection: b"c".to_vec(),
                    data: b"SGVsbG8=".to_vec(),
                }),
                "TerminalControl",
            ),
            (
                Message::TerminalControl(TerminalControlPayload::Title {
                    title: "My Terminal".into(),
                }),
                "TerminalControl",
            ),
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

    /// Phase 16 / D-16-01: `Message::TerminalControl(Clipboard{..})` round-trips
    /// through postcard encode/decode identically.
    #[test]
    fn terminal_control_clipboard_round_trips() {
        use crate::codec;
        let msg = Message::TerminalControl(TerminalControlPayload::Clipboard {
            selection: b"c".to_vec(),
            data: b"SGVsbG8=".to_vec(),
        });
        // encode() returns a length-prefixed frame; decode() takes the body only (strip 4-byte prefix).
        let frame = codec::encode(&msg).expect("encode must succeed");
        let decoded = codec::decode(&frame[4..]).expect("decode must succeed");
        assert_eq!(msg, decoded, "Clipboard TerminalControl must round-trip through postcard");
    }

    /// Phase 16 / D-16-02: `Message::TerminalControl(Title{..})` round-trips
    /// through postcard encode/decode identically.
    #[test]
    fn terminal_control_title_round_trips() {
        use crate::codec;
        let msg = Message::TerminalControl(TerminalControlPayload::Title {
            title: "x".into(),
        });
        // encode() returns a length-prefixed frame; decode() takes the body only (strip 4-byte prefix).
        let frame = codec::encode(&msg).expect("encode must succeed");
        let decoded = codec::decode(&frame[4..]).expect("decode must succeed");
        assert_eq!(msg, decoded, "Title TerminalControl must round-trip through postcard");
    }
}
