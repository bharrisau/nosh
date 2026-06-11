//! `nosh-client` binary — connects to a `nosh-server` with SSH-key mutual auth
//! (Phase 2), then runs an interactive PTY session (Phase 3): the local
//! terminal is put in raw mode (RAII-restored), keystrokes are forwarded to the
//! remote PTY, shell output is rendered locally, window resizes propagate
//! (coalesced), and the client process exits with the remote shell's exit code.
//!
//! Phase 6: adds a reconnect supervisor that auto-reconnects with exponential
//! backoff on transport drop (D-10). Holds the reattach token in memory; sends
//! `Ack` on a coarse cadence (D-08); handles `ReattachErr` as a terminal
//! condition (D-11). The `RawModeGuard` is entered ONCE and dropped once on
//! final exit regardless of how many reconnects occurred (RAII).
//!
//! Phase 8: adds `--identity-file` for on-disk Ed25519 key auth (WIN-02).
//! Auth-path selection:
//! - If `--identity-file` is given → `ClientIdentity::from_identity_file` (all platforms).
//! - Else on Unix → ssh-agent via `SSH_AUTH_SOCK` (existing default).
//! - Else on Windows → default to `%USERPROFILE%\.ssh\id_ed25519`; error if absent.
//!
//! Resize handling is `#[cfg]`-split via [`platform::ResizeWatcher`]:
//! - Unix: SIGWINCH → debounce → `Message::Resize`
//! - Windows: poll `crossterm::terminal::size()` (~300ms) → debounce → `Message::Resize`
//!   Both paths preserve the ~40 ms coalescing and the authoritative `terminal::size()` re-read.
//!
//! The reconnect-window quit uses the cross-platform `platform::quit_signal()`
//! (backed by `tokio::signal::ctrl_c`).

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Context;
use clap::Parser;
use nosh_client::channel::{EvenIdAllocator, run_scrollback_drain_task};
use nosh_client::client::{self, ClientIdentity, ReattachOutcome};
use nosh_client::platform;
use nosh_client::predictor::{PredictDisplayMode, PredictionOverlay};
use nosh_client::screen::ConnectionLossOverlay;
use nosh_proto::{Message, TerminalControlPayload};
use nosh_proto::messages::ChannelType;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

/// SIGWINCH / console-resize debounce window (~40 ms) — coalesces a window-drag
/// burst into one `Resize` (SESS-05, avoids resize storms). Preserved on both
/// Unix (SIGWINCH) and Windows (terminal::size() polling).
const RESIZE_DEBOUNCE: Duration = Duration::from_millis(40);

/// Ack cadence: send an Ack frame roughly every 750ms when output has been
/// applied (D-08 continuous acking, Claude's discretion).
const ACK_INTERVAL: Duration = Duration::from_millis(750);

/// Reconnect backoff: start 250ms, double on each retry up to 10s cap (D-10).
const BACKOFF_INITIAL: Duration = Duration::from_millis(250);
const BACKOFF_MAX: Duration = Duration::from_secs(10);

/// Classify a `client::connect()` failure as a PERMANENT (fatal) error that must
/// abort immediately, vs. a transient one that should be retried with backoff
/// (BUG-A — host-key mismatch infinite-retry fix).
///
/// Fatal = security-critical or configuration errors where retrying can never
/// succeed and silently looping would hide a security violation:
///   - host-key mismatch (TOFU `known_hosts` pin violation — MITM indicator),
///   - client key rejected / unauthorized (not in `authorized_keys`),
///   - bad / encrypted / unparseable identity key, missing key material.
///
/// Transient = network-level conditions that may recover (timeout, unreachable,
/// connection reset, ALPN renegotiation hiccups) — these retry.
///
/// rustls surfaces the custom verifier `Error::General`/`InvalidCertificate`
/// strings inside quinn's `ConnectionError::TransportError` → anyhow chain, so
/// we match on the rendered error text of the whole chain (the verifier message
/// is `host key mismatch for ...`). TLS alerts for a rejected client cert render
/// as `certificate required`/`access denied`/`bad certificate`/`unknown ca`.
fn is_fatal_connect_error(e: &anyhow::Error) -> bool {
    // Render the full error chain (anyhow `{:#}` includes `.context()` causes and
    // the rustls/quinn source error text).
    let msg = format!("{e:#}").to_ascii_lowercase();
    const FATAL_MARKERS: &[&str] = &[
        // Our HostKeyVerifier mismatch message (verifier.rs).
        "host key mismatch",
        // rustls TLS alerts for client-cert rejection (server-side AuthorizedKeysVerifier
        // rejects → handshake fails with one of these alert descriptions).
        "certificate required",
        "access denied",
        "bad certificate",
        "certificate unknown",
        "unknown ca",
        "decrypt error",
        "handshake failure",
        // Local identity-key problems (resolve_identity / FileSigner) that can never
        // be fixed by retrying the connection.
        "encrypted",
        "not ed25519",
        "passphrase",
    ];
    FATAL_MARKERS.iter().any(|m| msg.contains(m))
}

/// Resolve when the user requests quit during a pre-session connect/reconnect
/// backoff wait (BUG-B).
///
/// Two independent quit paths, whichever fires first:
///   1. `platform::quit_signal()` — tokio `ctrl_c` (SIGINT on Unix; on Windows
///      this only fires when ENABLE_PROCESSED_INPUT is set, which raw mode clears,
///      so it is effectively a Unix-only / non-raw safety net here).
///   2. Reading STDIN for the byte `0x03` (Ctrl-C → ETX, the form raw-mode Windows
///      actually delivers) or the SSH-style `~.` escape. This is the path that
///      makes Ctrl-C work in the connect window on native Windows.
///
/// `~.` is matched as a simple two-byte tail anywhere in a read batch — sufficient
/// for the pre-session window where the user is just trying to bail out (the full
/// line-start escape state machine only runs once a session is active).
async fn quit_during_backoff(stdin: &mut tokio::io::Stdin) {
    let mut buf = [0u8; 256];
    tokio::select! {
        _ = platform::quit_signal() => {}
        // Read stdin; treat EOF, Ctrl-C (0x03), or a `~.` sequence as a quit request.
        res = stdin.read(&mut buf) => {
            match res {
                Ok(0) => {} // EOF on stdin → quit
                Ok(n) => {
                    let bytes = &buf[..n];
                    let wants_quit = bytes.contains(&0x03) // Ctrl-C / ETX
                        || bytes.windows(2).any(|w| w == b"~."); // SSH-style ~. escape
                    if !wants_quit {
                        // Not a quit request (e.g. stray keystroke while connecting):
                        // swallow it and stay pending so the backoff sleep can elapse.
                        std::future::pending::<()>().await;
                    }
                    // else: fall through → resolve (quit).
                }
                Err(_) => {} // stdin read error → treat as quit (cannot read input)
            }
        }
    }
}

// ── Scrollback view state machine ─────────────────────────────────────────────
//
// The client-side scrollback view has two modes:
//
//   Live    — normal operation; datagram diffs apply and display normally.
//   Active  — scrollback mode; live datagram diffs are display-suspended (but
//              their epochs are still acked so the server pump never stalls).
//              Historical lines from the server are rendered using a local
//              line buffer + offset.
//
// Transitions:
//   Live → Active:    Shift-PageUp CSI sequence detected in stdin bytes.
//   Active → Live:    (a) Any non-paging keystroke: snap back AND forward
//                         the triggering byte to the shell (T-22-15 LOCKED).
//                     (b) Paging down past line 0 (live boundary) auto-exits.
//                     (c) Live datagram with epoch >= epoch_at_snapshot arrives
//                         (SCROLL-05 epoch gate implemented in Task 2b).

/// Client-side scrollback view state (SCROLL-04 / SCROLL-05).
///
/// `Live` is the default and is freshly initialised at the start of every
/// `run_pump` call (including the reattach path), so no residual `Active`
/// state from a prior connection can leak across reattach (T-22-13).
#[derive(Debug)]
pub enum ScrollbackView {
    /// Normal live mode: datagram diffs apply and display normally.
    Live,
    /// Scrollback mode: historical lines are being displayed.
    Active {
        /// Fetched historical lines in display order (oldest first).
        /// Lazily prefetched: grows as the user pages up.
        lines: Vec<nosh_proto::messages::ScrollbackLine>,
        /// How many lines from the bottom of `lines` are scrolled off the
        /// bottom of the visible viewport. 0 = showing the most-recent lines.
        offset: usize,
        /// True while a `ScrollbackRequest` is in-flight to the server.
        /// Prevents duplicate requests (lazy-prefetch gate).
        pending_request: bool,
        /// Datagram epoch at the time the scrollback snapshot was taken.
        /// The client exits `Active` when a live datagram with
        /// `epoch >= epoch_at_snapshot` arrives (SCROLL-05).
        epoch_at_snapshot: u64,
        /// Total scrollback lines available on the server at snapshot time.
        /// Used to detect when the client has reached the top of history
        /// (all lines fetched; further page-up is a no-op).
        total_available: u64,
    },
}

// ── CSI sequence accumulator ──────────────────────────────────────────────────
//
// The Shift-PageUp/Down sequences are 6 bytes:
//   Shift-PageUp:   ESC [ 5 ; 2 ~  = [0x1b, 0x5b, 0x35, 0x3b, 0x32, 0x7e]
//   Shift-PageDown: ESC [ 6 ; 2 ~  = [0x1b, 0x5b, 0x36, 0x3b, 0x32, 0x7e]
//
// Because the OS pipe buffer may deliver these as split reads (Pitfall 6 /
// T-22-11), we maintain a rolling 8-byte accumulator. On each stdin read we
// append to the accumulator and scan for the 6-byte sequences. Once matched,
// the matched bytes are consumed from the accumulator. Bytes that are neither
// part of a paging sequence prefix nor already matched are returned as
// `non_paging_remainder` so run_pump can forward them via EscapeState.
//
// This is a stateful prefix-match: partial sequences are held in the
// accumulator across reads until either the sequence completes or a byte
// that cannot possibly be part of either paging sequence is received.

/// Bytes of the Shift-PageUp CSI sequence.
const CSI_SHIFT_PAGEUP: [u8; 6] = [0x1b, 0x5b, 0x35, 0x3b, 0x32, 0x7e];
/// Bytes of the Shift-PageDown CSI sequence.
const CSI_SHIFT_PAGEDOWN: [u8; 6] = [0x1b, 0x5b, 0x36, 0x3b, 0x32, 0x7e];

/// Result from one `CsiAccumulator::process` call.
#[derive(Debug, Default)]
pub struct CsiResult {
    /// True if a Shift-PageUp sequence was completed this call.
    pub shift_pageup: bool,
    /// True if a Shift-PageDown sequence was completed this call.
    pub shift_pagedown: bool,
    /// Bytes from `input` that were NOT consumed by paging sequences.
    /// `run_pump` forwards these through `EscapeState` and then to the shell.
    pub non_paging_remainder: Vec<u8>,
}

/// Rolling 8-byte accumulator for Shift-PageUp/Down CSI sequence detection.
///
/// Handles split reads (Pitfall 6 / T-22-11): the 6-byte sequence may arrive
/// as multiple smaller stdin reads. The accumulator holds up to 8 pending
/// bytes so a maximum-split sequence (6 × 1-byte reads) never loses bytes.
pub struct CsiAccumulator {
    /// Pending bytes that form the current partial CSI prefix.
    pending: Vec<u8>,
}

impl CsiAccumulator {
    /// Create a new accumulator with no pending bytes.
    pub fn new() -> Self {
        CsiAccumulator { pending: Vec::with_capacity(8) }
    }

    /// Process `input` bytes against the CSI paging sequences.
    ///
    /// Returns a [`CsiResult`] describing any paging sequences detected and
    /// any non-paging bytes to forward. May be called with an empty slice.
    ///
    /// # Split-read safety (Pitfall 6 / T-22-11)
    ///
    /// Bytes that form a partial prefix of either sequence are held in
    /// `self.pending` and combined with bytes from the next `process` call.
    /// At most 8 bytes are ever held (the sequences are 6 bytes each).
    pub fn process(&mut self, input: &[u8]) -> CsiResult {
        let mut result = CsiResult::default();

        // Append the new bytes to any partial match pending from previous reads.
        self.pending.extend_from_slice(input);

        let mut i = 0;
        while i < self.pending.len() {
            let remaining = &self.pending[i..];

            if remaining.len() >= 6 && remaining[..6] == CSI_SHIFT_PAGEUP {
                // Full Shift-PageUp match — consume 6 bytes.
                result.shift_pageup = true;
                i += 6;
            } else if remaining.len() >= 6 && remaining[..6] == CSI_SHIFT_PAGEDOWN {
                // Full Shift-PageDown match — consume 6 bytes.
                result.shift_pagedown = true;
                i += 6;
            } else if is_paging_prefix(remaining) {
                // `remaining` is a proper prefix of a paging sequence (< 6 bytes
                // that match the start of CSI_SHIFT_PAGEUP or CSI_SHIFT_PAGEDOWN).
                // Hold these bytes for the next call (split-read path).
                break;
            } else {
                // First byte cannot start or continue a paging sequence.
                // Return it as a non-paging byte to forward.
                result.non_paging_remainder.push(self.pending[i]);
                i += 1;
            }
        }

        // Retain only unprocessed bytes (partial prefix held for next call).
        self.pending.drain(..i);

        result
    }
}

/// Returns true if `bytes` is a non-empty PROPER prefix of either paging CSI
/// sequence (length 1–5, matching the beginning of the respective 6-byte sequence).
///
/// "Proper prefix" means: `bytes.len() < 6` AND `bytes == target[..bytes.len()]`.
/// A full 6-byte match is NOT a prefix (handled by the full-match arms above).
fn is_paging_prefix(bytes: &[u8]) -> bool {
    if bytes.is_empty() || bytes.len() >= 6 {
        return false;
    }
    let len = bytes.len();
    bytes == &CSI_SHIFT_PAGEUP[..len] || bytes == &CSI_SHIFT_PAGEDOWN[..len]
}

// ── SSH-style escape state machine ────────────────────────────────────────────
//
// This implements the OpenSSH client escape mechanism. It sits BETWEEN the
// local stdin read and `client::send_input`, so it is fed ONLY local keystrokes.
// Server-sourced `PtyData` bytes NO LONGER go directly to stdout (D-14-02);
// display flows exclusively through ClientScreen::render_to_stdout via the
// datagram arm. PtyData is received on the reliable stream only to advance
// highest_applied (cold-reattach ack, D-14-03). The escape machine is fed
// ONLY local keystrokes (T-09-01: a malicious server cannot inject `~.`).
//
// State machine:
//   - LineStart: at the beginning of the stream, or just after forwarding a '\n'
//     or '\r'. In raw mode (ICRNL disabled), the Enter key delivers '\r' (CR,
//     0x0D), not '\n' (LF, 0x0A). Both are treated as line-start triggers,
//     matching OpenSSH's `last_was_cr` logic (clientloop.c).
//     A '~' at LineStart does NOT get forwarded immediately; transitions to SeenTilde.
//   - SeenTilde: a preceding '~' at line-start is pending.
//     '.' → quit (no bytes forwarded)
//     '~' → forward one literal '~', return to MidLine (not LineStart — a literal
//            tilde is not a newline)
//     other → forward the pending '~' + this byte; update state based on the byte
//   - MidLine: any byte forwarded literally; transitions to LineStart after '\n' or '\r'.
//
// Escape sequences (recognized only at line start):
//   ~.   Disconnect the local client (PumpOutcome::UserQuit).
//   ~~   Send a literal tilde (~) to the remote.

/// The result of processing a chunk of stdin bytes through the escape machine.
#[derive(Debug)]
struct EscapeResult {
    /// Bytes to forward to the server (may be empty if the input was consumed
    /// by the escape logic or produced no output).
    bytes_to_forward: Vec<u8>,
    /// Whether a `~.` escape was detected, signalling local quit.
    quit: bool,
}

/// State of the `~`-escape state machine.
#[derive(Debug, Clone, Copy, PartialEq)]
enum EscapeState {
    /// At line-start: a '~' byte will begin an escape sequence.
    LineStart,
    /// A '~' at line-start was just seen; pending the next byte.
    SeenTilde,
    /// Mid-line: '~' has no escape meaning; transitions to LineStart on '\n'.
    MidLine,
}

impl EscapeState {
    fn new() -> Self {
        // Session start counts as line-start (like OpenSSH).
        EscapeState::LineStart
    }

    /// Process `input` bytes through the escape machine. Returns bytes to forward
    /// and whether `~.` was encountered (local quit). Must be fed ONLY local stdin
    /// bytes — NEVER server output.
    fn process(&mut self, input: &[u8]) -> EscapeResult {
        let mut out = Vec::with_capacity(input.len());
        let mut quit = false;

        for &byte in input {
            match *self {
                EscapeState::LineStart => {
                    if byte == b'~' {
                        // Pending tilde: do NOT forward yet, wait for next byte.
                        *self = EscapeState::SeenTilde;
                    } else {
                        out.push(byte);
                        // '\r' (CR) and '\n' (LF) both count as line-start.
                        // In raw mode, Enter delivers '\r'; ICRNL is disabled.
                        *self = if matches!(byte, b'\n' | b'\r') {
                            EscapeState::LineStart
                        } else {
                            EscapeState::MidLine
                        };
                    }
                }
                EscapeState::SeenTilde => {
                    if byte == b'.' {
                        // ~. → local quit; forward nothing.
                        quit = true;
                        // Consume remaining input on quit; the caller will
                        // discard out and return UserQuit.
                        break;
                    } else if byte == b'~' {
                        // ~~ → forward one literal tilde, return to MidLine.
                        // (A literal tilde is not a newline; mid-line is correct.)
                        out.push(b'~');
                        *self = EscapeState::MidLine;
                    } else {
                        // ~<other>: forward both the pending '~' and this byte.
                        out.push(b'~');
                        out.push(byte);
                        // '\r' or '\n' after an unrecognised escape also resets.
                        *self = if matches!(byte, b'\n' | b'\r') {
                            EscapeState::LineStart
                        } else {
                            EscapeState::MidLine
                        };
                    }
                }
                EscapeState::MidLine => {
                    out.push(byte);
                    // '\r' (CR) and '\n' (LF) both transition to LineStart.
                    if matches!(byte, b'\n' | b'\r') {
                        *self = EscapeState::LineStart;
                    }
                }
            }
        }

        EscapeResult { bytes_to_forward: out, quit }
    }
}

#[cfg(test)]
mod escape_tests {
    use super::{EscapeState};

    /// Helper: run the escape machine over the given bytes and return
    /// (bytes_forwarded, quit_flag).
    fn run(state: &mut EscapeState, input: &[u8]) -> (Vec<u8>, bool) {
        let r = state.process(input);
        (r.bytes_to_forward, r.quit)
    }

    #[test]
    fn line_start_tilde_dot_quits_no_bytes() {
        let mut s = EscapeState::new(); // starts at LineStart
        let (fwd, quit) = run(&mut s, b"~.");
        assert!(quit, "~. at line-start must signal quit");
        assert!(fwd.is_empty(), "~. must not forward any bytes");
    }

    #[test]
    fn tilde_tilde_forwards_one_literal_tilde() {
        let mut s = EscapeState::new(); // LineStart
        let (fwd, quit) = run(&mut s, b"~~");
        assert!(!quit);
        assert_eq!(fwd, b"~", "~~ must forward exactly one ~");
    }

    #[test]
    fn mid_line_tilde_is_literal() {
        // 'a' takes us to MidLine; subsequent '~' has no escape semantics.
        let mut s = EscapeState::new();
        let (fwd, quit) = run(&mut s, b"a~b");
        assert!(!quit);
        assert_eq!(fwd, b"a~b", "mid-line ~ must be forwarded literally");
    }

    #[test]
    fn newline_resets_to_line_start_enabling_escape() {
        // '\n' at mid-line → LineStart; then '~.' should quit.
        let mut s = EscapeState::new();
        // First put us at mid-line with 'x'.
        let (_, _) = run(&mut s, b"x");
        // '\n' moves to LineStart; '~.' quits.
        let (fwd_n, quit_n) = run(&mut s, b"\n~.");
        // '\n' is forwarded, then '~.' quits.
        assert!(quit_n, "~. after newline must quit");
        assert_eq!(fwd_n, b"\n", "newline before ~. must be forwarded");
    }

    #[test]
    fn mid_line_tilde_dot_is_literal() {
        // 'x' puts us at MidLine; '~.' must NOT quit.
        let mut s = EscapeState::new();
        let (fwd, quit) = run(&mut s, b"x~.");
        assert!(!quit, "~. not at line-start must NOT quit");
        assert_eq!(fwd, b"x~.", "all three bytes forwarded literally");
    }

    #[test]
    fn session_start_counts_as_line_start() {
        // EscapeState::new() starts at LineStart — confirm '~.' quits immediately.
        let mut s = EscapeState::new();
        let r = s.process(b"~.");
        assert!(r.quit);
        assert!(r.bytes_to_forward.is_empty());
    }

    #[test]
    fn tilde_other_byte_forwarded_both() {
        // '~' at line start + 'q' → forward "~q" (no quit).
        let mut s = EscapeState::new();
        let (fwd, quit) = run(&mut s, b"~q");
        assert!(!quit);
        assert_eq!(fwd, b"~q");
    }

    #[test]
    fn carriage_return_resets_to_line_start_enabling_escape() {
        // In raw mode, Enter delivers '\r' (CR, 0x0D), NOT '\n' (LF, 0x0A) —
        // ICRNL is disabled. This test guards the CR-01 fix: the escape machine
        // must accept '~.' after a CR exactly as it does after a LF.
        let mut s = EscapeState::new();
        let (_, _) = run(&mut s, b"x"); // 'x' → MidLine
        let (fwd, quit) = run(&mut s, b"\r~.");
        assert!(quit, "~. after \\r must quit (CR-01: raw-mode Enter is \\r not \\n)");
        assert_eq!(fwd, b"\r", "\\r before ~. must be forwarded");
    }

    #[test]
    fn carriage_return_mid_line_tilde_dot_is_literal() {
        // A '~.' that is NOT preceded by a line-start must never quit,
        // even when the line-start was established by '\r'. After the \r~
        // sequence, the machine is at SeenTilde; then a subsequent non-dot
        // byte (here '\r' itself to form a new mid-line) ensures we test
        // the mid-line path.
        let mut s = EscapeState::new();
        // Put us mid-line by consuming 'x'.
        run(&mut s, b"x");
        // '\r' at MidLine → LineStart; 'y' → MidLine; '~.' must NOT quit.
        let (fwd, quit) = run(&mut s, b"\ry~.");
        assert!(!quit, "~. after mid-line 'y' (reached via \\r) must NOT quit");
        assert_eq!(fwd, b"\ry~.", "all four bytes must be forwarded literally");
    }
}

// ── ScrollbackView state machine tests (TDD RED gate) ────────────────────────
//
// These tests establish the RED gate for Task 2a. They reference `ScrollbackView`
// and `CsiAccumulator` which don't exist yet, causing compile errors in RED state.
// Once implemented (GREEN), all tests pass.

#[cfg(test)]
mod scrollback_view_tests {
    use super::{ScrollbackView, CsiAccumulator};

    // CSI sequence byte values
    const ESC: u8 = 0x1b;
    const LBRACKET: u8 = 0x5b;
    const SHIFT_PGUP: &[u8] = &[0x1b, 0x5b, 0x35, 0x3b, 0x32, 0x7e]; // ESC [ 5 ; 2 ~
    const SHIFT_PGDN: &[u8] = &[0x1b, 0x5b, 0x36, 0x3b, 0x32, 0x7e]; // ESC [ 6 ; 2 ~
    #[allow(unused)] const _SUPPRESS: (u8, u8) = (ESC, LBRACKET);

    /// ScrollbackView starts in Live mode.
    #[test]
    fn scrollback_view_starts_live() {
        let view = ScrollbackView::Live;
        assert!(matches!(view, ScrollbackView::Live),
            "ScrollbackView must start as Live");
    }

    /// ScrollbackView Active variant carries epoch_at_snapshot.
    #[test]
    fn scrollback_view_active_has_epoch_at_snapshot() {
        let view = ScrollbackView::Active {
            lines: vec![],
            offset: 0,
            pending_request: false,
            epoch_at_snapshot: 42,
            total_available: 0,
        };
        match view {
            ScrollbackView::Active { epoch_at_snapshot, .. } => {
                assert_eq!(epoch_at_snapshot, 42);
            }
            ScrollbackView::Live => panic!("expected Active"),
        }
    }

    /// CsiAccumulator detects Shift-PageUp across a single read.
    #[test]
    fn csi_accumulator_detects_shift_pageup_single_read() {
        let mut acc = CsiAccumulator::new();
        let result = acc.process(SHIFT_PGUP);
        assert!(result.shift_pageup, "Shift-PageUp sequence must be detected");
        assert!(!result.shift_pagedown, "only Shift-PageUp must be signalled");
    }

    /// CsiAccumulator detects Shift-PageDown across a single read.
    #[test]
    fn csi_accumulator_detects_shift_pagedown_single_read() {
        let mut acc = CsiAccumulator::new();
        let result = acc.process(SHIFT_PGDN);
        assert!(result.shift_pagedown, "Shift-PageDown sequence must be detected");
        assert!(!result.shift_pageup, "only Shift-PageDown must be signalled");
    }

    /// CsiAccumulator handles split reads (Pitfall 6 / T-22-11).
    /// The 6-byte Shift-PageUp sequence is split 3+3 across two reads.
    #[test]
    fn csi_accumulator_detects_shift_pageup_split_read() {
        let mut acc = CsiAccumulator::new();
        // First 3 bytes: ESC [ 5
        let r1 = acc.process(&SHIFT_PGUP[..3]);
        assert!(!r1.shift_pageup, "partial sequence must not trigger yet");
        // Remaining 3 bytes: ; 2 ~
        let r2 = acc.process(&SHIFT_PGUP[3..]);
        assert!(r2.shift_pageup, "split Shift-PageUp must be detected across reads");
    }

    /// CsiAccumulator handles split reads for Shift-PageDown.
    #[test]
    fn csi_accumulator_detects_shift_pagedown_split_read() {
        let mut acc = CsiAccumulator::new();
        let r1 = acc.process(&SHIFT_PGDN[..4]); // ESC [ 6 ;
        assert!(!r1.shift_pagedown, "partial sequence must not trigger yet");
        let r2 = acc.process(&SHIFT_PGDN[4..]); // 2 ~
        assert!(r2.shift_pagedown, "split Shift-PageDown must be detected");
    }

    /// Non-paging bytes while accumulating a partial CSI sequence are returned
    /// as remainder bytes to forward to the shell.
    #[test]
    fn csi_accumulator_non_paging_byte_resets_and_returns() {
        let mut acc = CsiAccumulator::new();
        // Start a potential Shift-PageUp then send a different byte.
        let r = acc.process(&[0x1b, 0x5b, 0x41]); // ESC [ A (cursor up)
        assert!(!r.shift_pageup, "ESC [ A must not trigger Shift-PageUp");
        assert!(!r.shift_pagedown, "ESC [ A must not trigger Shift-PageDown");
        // The bytes ESC [ A that don't match a paging sequence should be returned
        // as non_paging_remainder so run_pump can forward them.
        assert!(
            !r.non_paging_remainder.is_empty(),
            "non-paging bytes must be returned for forwarding"
        );
    }

    /// CsiAccumulator does not swallow regular keystrokes.
    #[test]
    fn csi_accumulator_passes_regular_keystrokes_through() {
        let mut acc = CsiAccumulator::new();
        let r = acc.process(b"hello");
        assert!(!r.shift_pageup);
        assert!(!r.shift_pagedown);
        assert_eq!(r.non_paging_remainder, b"hello");
    }
}

/// nosh client (Phase 3 — interactive PTY session over authenticated QUIC).
#[derive(Parser, Debug)]
#[command(
    name = "nosh-client",
    about = "nosh — roaming-tolerant remote shell over QUIC",
    long_about = "nosh — roaming-tolerant remote shell over QUIC\n\n\
        Escape sequences (recognized at line start, i.e. after CR/LF (Enter) or at session start):\n  \
          ~.   Disconnect and quit the local client.\n  \
          ~~   Send a literal tilde (~) to the remote."
)]
struct Args {
    /// Server address.
    #[arg(long, default_value = "127.0.0.1")]
    addr: IpAddr,

    /// Server port (matches the server's --port; default 4433).
    #[arg(long, default_value_t = 4433)]
    port: u16,

    /// Host name used both as the QUIC SNI and the `known_hosts` lookup key.
    #[arg(long, default_value = "localhost")]
    host: String,

    /// Path to the identity public key selecting which ssh-agent key to use.
    /// If omitted, the agent's single key is used (Unix only, D-04).
    #[arg(long)]
    identity: Option<PathBuf>,

    /// On-disk OpenSSH Ed25519 private key for authentication.
    /// Opt-in on Linux (ssh-agent is the default); the ONLY auth path on
    /// Windows (default: %USERPROFILE%\.ssh\id_ed25519 when flag is omitted).
    #[arg(long)]
    identity_file: Option<PathBuf>,

    /// OpenSSH `known_hosts` file for host-key pinning/TOFU (D-05/D-08).
    /// Default `~/.ssh/known_hosts`.
    #[arg(long)]
    known_hosts: Option<PathBuf>,

    /// Timeout in seconds for the initial QUIC connection handshake.
    /// If the server does not respond within this window, the client
    /// reports a clear error and enters the reconnect backoff loop.
    /// Default 10 seconds.
    #[arg(long, default_value_t = 10)]
    connect_timeout: u64,

    /// Speculative-echo prediction mode (PREDICT-05, D-15-02).
    ///
    /// adaptive (default): show predictions only on high-latency links (>~30ms RTT);
    /// invisible on loopback. always: show predictions regardless of RTT (useful for
    /// testing). never: disable predictions entirely.
    #[arg(long, default_value = "adaptive")]
    predict: PredictDisplayMode,

    /// Surface measured RTT (SRTT) in the terminal title via OSC 0/2 (QOL-04).
    ///
    /// When active, the title is set to `nosh: <N>ms` on every datagram received.
    /// Forwarded OSC 0/2 title frames from the server are suppressed while --status
    /// is active (the RTT title takes precedence — Pitfall 5).
    #[arg(long)]
    status: bool,
}

fn default_known_hosts() -> anyhow::Result<PathBuf> {
    let home = dirs::home_dir().context("locate home dir for default known_hosts")?;
    Ok(home.join(".ssh").join("known_hosts"))
}

/// Resolve the client identity based on CLI args and platform.
///
/// Priority:
/// 1. `--identity-file <path>` (all platforms, opt-in)
/// 2. Unix: ssh-agent via `SSH_AUTH_SOCK` + optional `--identity` key selector
/// 3. Windows (no --identity-file): default to `%USERPROFILE%\.ssh\id_ed25519`
fn resolve_identity(args: &Args) -> anyhow::Result<ClientIdentity> {
    // Warn if --identity is supplied on a platform where it has no effect.
    // On Unix, --identity selects which ssh-agent key to use. On Windows,
    // ssh-agent is not available and --identity is silently discarded; the
    // user almost certainly wants --identity-file instead.
    #[cfg(not(unix))]
    if args.identity.is_some() {
        tracing::warn!(
            "--identity is only used on Unix (ssh-agent key selector); \
             on Windows use --identity-file instead"
        );
    }

    // Branch 1: explicit --identity-file (all platforms, no SSH_AUTH_SOCK needed).
    if let Some(ref path) = args.identity_file {
        return ClientIdentity::from_identity_file(path);
    }

    // Branch 2: Unix — use ssh-agent (SSH_AUTH_SOCK required).
    #[cfg(unix)]
    {
        let socket = std::env::var_os("SSH_AUTH_SOCK")
            .map(PathBuf::from)
            .context("SSH_AUTH_SOCK not set — start an ssh-agent and add your key, or use --identity-file")?;
        ClientIdentity::from_agent(socket, args.identity.as_deref())
    }

    // Branch 3: Windows — no ssh-agent available; default to %USERPROFILE%\.ssh\id_ed25519.
    #[cfg(windows)]
    {
        let default_key = dirs::home_dir()
            .context("locate home directory for default identity file")?
            .join(".ssh")
            .join("id_ed25519");
        if default_key.exists() {
            return ClientIdentity::from_identity_file(&default_key);
        }
        anyhow::bail!(
            "no --identity-file given and no key found at {}; \
             Windows requires --identity-file (ssh-agent is not available in this version)",
            default_key.display()
        );
    }

    // Fallback for platforms that are neither unix nor windows (should not occur).
    #[cfg(not(any(unix, windows)))]
    {
        anyhow::bail!(
            "unsupported platform: use --identity-file to specify an Ed25519 private key"
        );
    }
}

/// How the pump loop ended.
#[derive(Debug)]
enum PumpOutcome {
    /// Server sent SessionClose with exit code.
    CleanExit(i32),
    /// Transport dropped without a SessionClose — reconnect.
    TransportDrop,
    /// User explicitly quit (stdin EOF, or Ctrl-C while active — we treat both
    /// as clean exits in the interactive case).
    UserQuit,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // HARDEN-03 / D-16-04a: On Windows, quinn_udp emits a WARN for every datagram
    // where the GRO receive path appends UDP_COALESCED_INFO metadata (WSAEMSGSIZE).
    // The datagram is NOT lost — only the GRO metadata is affected. This is a
    // known Windows-specific behaviour tracked upstream at quinn-rs/quinn#2041 (open).
    // We suppress quinn_udp WARN (setting quinn_udp=error) on Windows only, leaving
    // all other quinn connection/auth WARNs visible. The filter is quinn_udp=error,
    // NOT quinn=error — the latter would silence genuine connection-level warnings.
    #[cfg(target_os = "windows")]
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "info".into())
        .add_directive("quinn_udp=error".parse().unwrap());
    #[cfg(not(target_os = "windows"))]
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "info".into());

    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_writer(std::io::stderr)
        .init();

    let args = Args::parse();
    let server_addr = SocketAddr::new(args.addr, args.port);

    let identity = resolve_identity(&args)?;
    let known_hosts = match args.known_hosts {
        Some(p) => p,
        None => default_known_hosts()?,
    };

    // Raw mode entered ONCE for the lifetime of the reconnect supervisor.
    // Restored explicitly before std::process::exit below (which skips Drop),
    // and via Drop on any `?` early-return path — NEVER re-entered per reconnect (D-11).
    let raw_guard = client::RawModeGuard::enable().context("enter raw mode")?;

    let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
    let term = std::env::var("TERM").unwrap_or_else(|_| "xterm-256color".to_string());

    // Build the connect timeout once from the CLI argument.
    let connect_timeout = Duration::from_secs(args.connect_timeout);

    // Per-session reconnect state held across reconnects (D-01 in-memory only).
    let mut token: Option<[u8; 16]> = None; // None = no session yet
    let mut highest_applied: u64 = 0;       // highest seq applied to terminal

    let mut backoff = BACKOFF_INITIAL;
    let mut exit_code: i32 = 0;

    // Platform-abstracted resize watcher. Unix: SIGWINCH; Windows: terminal::size() polling.
    let mut resize = platform::ResizeWatcher::new().context("install resize handler")?;

    // BUG-B: pre-session abort path. While we are in the connect/reconnect backoff
    // window (no session yet), the only previous escape was `platform::quit_signal()`
    // (tokio ctrl_c). On Windows, raw mode CLEARS ENABLE_PROCESSED_INPUT so Ctrl-C
    // is delivered as the byte 0x03 on STDIN — it does NOT raise the console
    // CTRL_C_EVENT that tokio's ctrl_c() listens for, so quit_signal() never fires
    // and the user is stuck in a looping/failing connect. To fix this we ALSO read
    // stdin for 0x03 (ETX) and the SSH-style `~.` escape during every backoff wait.
    // The stdin handle is created once here (the pump loop creates its own; only one
    // reader is active at a time because the supervisor and run_pump never overlap).
    let mut stdin_quit = tokio::io::stdin();

    // Outer reconnect supervisor loop (D-10).
    loop {
        // Build a fresh endpoint and connection for this attempt.
        let endpoint = match client::make_endpoint(&identity, known_hosts.clone(), args.host.clone()) {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!("make_endpoint failed: {e}");
                // Wait with backoff, honouring quit signal (BUG-B: stdin Ctrl-C/~. too).
                tokio::select! {
                    _ = tokio::time::sleep(backoff) => {}
                    _ = quit_during_backoff(&mut stdin_quit) => {
                        eprintln!("\r\nnosh: quit\r");
                        break;
                    }
                }
                backoff = (backoff * 2).min(BACKOFF_MAX);
                continue;
            }
        };

        let conn = match client::connect(&endpoint, server_addr, &args.host, connect_timeout).await {
            Ok(c) => c,
            Err(e) => {
                // BUG-A: a host-key mismatch (TOFU known_hosts pin violation) or a
                // client-key/identity rejection is a PERMANENT, security-critical
                // failure. Retrying can never succeed and silently looping would
                // hide a possible MITM. Abort immediately with a terminal-visible
                // error and a non-zero exit code — do NOT enter the backoff loop.
                if is_fatal_connect_error(&e) {
                    tracing::error!("fatal connect error (not retrying): {e:#}");
                    // Surface the cause on the terminal in raw mode (\r\n line ends).
                    eprintln!("\r\nnosh: connection aborted — {e:#}\r");
                    eprintln!(
                        "\r\nnosh: this is a permanent failure (host-key mismatch or key \
                         rejected); not reconnecting. If you intentionally rotated the server \
                         host key, remove the stale line for '{}' from your known_hosts file.\r",
                        args.host
                    );
                    endpoint.close(0u32.into(), b"fatal connect error");
                    exit_code = 1;
                    break;
                }
                tracing::warn!("connect failed: {e}");
                eprintln!("\r\nnosh: reconnecting…\r");
                tokio::select! {
                    _ = tokio::time::sleep(backoff) => {}
                    _ = quit_during_backoff(&mut stdin_quit) => {
                        eprintln!("\r\nnosh: quit\r");
                        break;
                    }
                }
                backoff = (backoff * 2).min(BACKOFF_MAX);
                endpoint.close(0u32.into(), b"connect failed");
                continue;
            }
        };

        // Either fresh open (no token) or reattach (have token).
        let pump_outcome = if let Some(tok) = token {
            // Reattach path (D-03 / ROAM-02).
            let reattach_result = reattach_session(
                &conn,
                tok,
                highest_applied,
                &mut highest_applied,
                &mut resize,
                &mut token,
                args.predict,
                args.status,
            )
            .await;
            reattach_result.unwrap_or(PumpOutcome::TransportDrop)
        } else {
            // Fresh session path.
            let fresh_result = fresh_session(
                &conn,
                term.clone(),
                cols,
                rows,
                &mut highest_applied,
                &mut resize,
                &mut token,
                args.predict,
                args.status,
            )
            .await;
            fresh_result.unwrap_or(PumpOutcome::TransportDrop)
        };

        conn.close(0u32.into(), b"pump ended");
        endpoint.wait_idle().await;

        match pump_outcome {
            PumpOutcome::CleanExit(code) => {
                exit_code = code;
                break;
            }
            PumpOutcome::UserQuit => {
                break;
            }
            PumpOutcome::TransportDrop => {
                eprintln!("\r\nnosh: reconnecting…\r");
                // Backoff before retry (BUG-B: stdin Ctrl-C/~. also aborts here).
                tokio::select! {
                    _ = tokio::time::sleep(backoff) => {}
                    _ = quit_during_backoff(&mut stdin_quit) => {
                        // Explicit quit during reconnect window (D-11 escape path).
                        eprintln!("\r\nnosh: quit\r");
                        break;
                    }
                }
                backoff = (backoff * 2).min(BACKOFF_MAX);
            }
        }
    }

    // std::process::exit() does NOT run destructors, so the RawModeGuard's Drop
    // would never fire on this path — restore the terminal explicitly first.
    drop(raw_guard);
    std::process::exit(exit_code);
}

/// Run a fresh session (first connect or reconnect without a token).
/// Updates `highest_applied` and `token` in-place.
#[allow(clippy::too_many_arguments)]
async fn fresh_session(
    conn: &quinn::Connection,
    term: String,
    cols: u16,
    rows: u16,
    highest_applied: &mut u64,
    resize: &mut platform::ResizeWatcher,
    token_out: &mut Option<[u8; 16]>,
    predict_mode: PredictDisplayMode,
    status: bool,
) -> anyhow::Result<PumpOutcome> {
    let (mut send, mut recv, tok) =
        client::open_session_with_token(conn, term, cols, rows, client::collect_client_env())
            .await?;
    *token_out = Some(tok);
    // Fresh session starts at seq 0.
    *highest_applied = 0;

    run_pump(conn, cols, rows, &mut send, &mut recv, highest_applied, resize, 0, predict_mode, status).await
}

/// Run a reattach session. Updates `highest_applied` and `token_out` in-place.
/// Returns `PumpOutcome::CleanExit(code)` if the session ended cleanly,
/// `PumpOutcome::TransportDrop` if the link dropped again, or
/// `PumpOutcome::UserQuit` if the user quit.
#[allow(clippy::too_many_arguments)]
async fn reattach_session(
    conn: &quinn::Connection,
    token: [u8; 16],
    last_acked_seq: u64,
    highest_applied: &mut u64,
    resize: &mut platform::ResizeWatcher,
    token_out: &mut Option<[u8; 16]>,
    predict_mode: PredictDisplayMode,
    status: bool,
) -> anyhow::Result<PumpOutcome> {
    let (mut send, mut recv) = conn.open_bi().await.context("open bi for reattach")?;
    client::send_reattach(&mut send, token, last_acked_seq).await?;

    match client::await_reattach_reply(&mut recv).await? {
        ReattachOutcome::Err => {
            // Terminal: the session is gone (D-11). Clear the token so we do
            // not try to reattach again — a new session would be started.
            *token_out = None;
            eprintln!("\r\nnosh: session ended\r");
            // Return CleanExit so the outer loop does not retry with TransportDrop.
            Ok(PumpOutcome::CleanExit(1))
        }
        ReattachOutcome::Ok {
            new_token,
            replaying_from_seq,
            truncated,
        } => {
            *token_out = Some(new_token);
            if truncated {
                eprintln!("\r\nnosh: output truncated\r");
            }
            // Rebase the applied-count to the server's first replayed seq
            // (next-expected-seq convention). `replaying_from_seq` is the seq
            // of the FIRST chunk the server is about to replay; after run_pump
            // applies that chunk it will increment highest_applied to
            // `replaying_from_seq + 1`, which is the correct next-expected.
            //
            // This MUST be `= replaying_from_seq`, NOT `replaying_from_seq - 1`:
            // the prior `- 1` was the compounding off-by-one that dropped one
            // chunk per reconnect cycle (ROAM-02 BLOCKER). On truncation,
            // `replaying_from_seq == lowest_retained_seq` so this resyncs the
            // baseline to exactly what the server is sending.
            *highest_applied = replaying_from_seq;
            let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
            run_pump(conn, cols, rows, &mut send, &mut recv, highest_applied, resize, *highest_applied, predict_mode, status).await
        }
    }
}

/// Core pump loop: render output, forward input, debounce resize, send periodic
/// Ack. Returns the pump outcome.
#[allow(clippy::too_many_arguments)] // 10 args are load-bearing: conn + streams + state + watcher + baseline + predict_mode + status
async fn run_pump(
    conn: &quinn::Connection,
    cols: u16,
    rows: u16,
    send: &mut quinn::SendStream,
    recv: &mut quinn::RecvStream,
    highest_applied: &mut u64,
    resize: &mut platform::ResizeWatcher,
    _seq_baseline: u64,
    predict_mode: PredictDisplayMode,
    status: bool,
) -> anyhow::Result<PumpOutcome> {
    let mut stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let mut stdin_buf = [0u8; 8 * 1024];
    let mut resize_deadline: Option<tokio::time::Instant> = None;
    let mut ack_interval = tokio::time::interval(ACK_INTERVAL);
    ack_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Track the last seq for which we sent an Ack to avoid redundant sends.
    let mut last_acked = *highest_applied;
    // Escape state machine: persisted across reads so line-start state is
    // maintained correctly. Fed ONLY local stdin bytes — server PtyData output
    // NEVER enters this machine (T-09-01).
    let mut escape = EscapeState::new();
    // ClientScreen compositor: the SOLE display path (D-14-02 / CLAUDE.md).
    // Constructing a fresh screen per run_pump invocation means physical grid
    // always starts blank — this IS the reattach full-repaint reset (D-13-01b
    // symmetric). Calling reset_physical() explicitly is only needed if the
    // screen is ever hoisted above run_pump scope; do NOT hoist it.
    let mut screen = nosh_client::screen::ClientScreen::new(cols, rows);
    // D-03 / BUG-H: clear the physical terminal on connect so any prior content
    // does not bleed through the first render. emit_connect_clear writes \x1b[2J\x1b[H
    // (ED2 + cursor-home) and calls reset_physical(), so the subsequent
    // render_with_predictor forces a full repaint from a known-clean terminal state.
    // This is the single sanctioned pre-render clear (CONTEXT.md D-03b exception).
    {
        let mut init_buf: Vec<u8> = Vec::new();
        if let Err(e) = screen.emit_connect_clear(&mut init_buf) {
            tracing::warn!("emit_connect_clear error: {e}");
            // IN-01 fix: call reset_physical() so the physical grid is marked unclean
            // and the first render will be a full repaint, consistent with the write-
            // failure paths below. In practice Vec<u8>::write_all is infallible, but
            // the symmetry prevents a misleading code pattern for future refactors.
            screen.reset_physical();
        }
        if !init_buf.is_empty() {
            if let Err(e) = stdout.write_all(&init_buf).await {
                tracing::warn!("emit_connect_clear stdout write_all failed: {e}");
                screen.reset_physical();
            } else if let Err(e) = stdout.flush().await {
                tracing::warn!("emit_connect_clear stdout flush failed: {e}");
                screen.reset_physical();
            }
        }
    }
    // Speculative-echo overlay (Phase 15, PREDICT-02/04/05).
    // Mutably owned here so both stdin arm (on_input) and datagram arm (cull)
    // can drive it, and render_with_predictor receives it by shared ref.
    let mut predictor = PredictionOverlay::new(predict_mode, cols, rows);
    // TUI-05: track last known alt-screen state so the datagram arm can detect
    // the false→true transition and flush all pending predictions via
    // predictor.reset() on entry.  Mirrors the was_cols/was_rows resize pattern.
    let mut was_alt_screen = false;
    // Connection-loss overlay (Phase 16, QOL-01).
    // Mutably owned here (mirroring predictor) so the silence timer and datagram
    // arm can activate/clear it; render_with_predictor receives it by shared ref.
    let mut loss_overlay = ConnectionLossOverlay::new(cols);
    // Tracks the tokio::time::Instant of the last received datagram.
    // Used by the silence timer to set the activation threshold at last_datagram_time + 5s,
    // and stored in loss_overlay.last_contact (as std::time::Instant) for elapsed display.
    let mut last_datagram_time = tokio::time::Instant::now();
    // Live elapsed-counter tick (QOL-01): when the loss overlay is active, this 1s
    // interval drives a re-render so the "last contact Ns ago" counter increments live
    // on screen (not frozen at activation). The arm is guarded by `if loss_overlay.active`
    // so it is a no-op when the overlay is inactive (no spurious re-renders).
    //
    // WR-02: use MissedTickBehavior::Skip (not the default Burst) to prevent back-to-back
    // re-renders if the event loop was delayed (e.g. heavy keystroke traffic) while the
    // overlay was active. Matches the ack_interval policy.
    //
    // WR-03: use interval_at(now + 1s, 1s) so the first tick fires 1s after creation,
    // not immediately. Without interval_at, interval() fires its first tick on the first
    // .tick() call; combined with the silence arm activating loss_overlay.active on the
    // same iteration, this causes a spurious double repaint at activation time (the silence
    // arm renders, then loss_tick immediately fires and renders again).
    let mut loss_tick = tokio::time::interval_at(
        tokio::time::Instant::now() + Duration::from_secs(1),
        Duration::from_secs(1),
    );
    loss_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // D-04 latency instrumentation (rework of D-17-02a): map from per-keystroke monotonic
    // counter (keystroke_id) → enqueue Instant. Records one timestamp per forwarded keystroke
    // batch so the datagram arm can compute per-keystroke RTT (predicted→confirmed), not the
    // coarser epoch-confirmation time that included user think-time.
    // Key: u64 keystroke_id (incremented per forwarded keystroke in the stdin arm).
    // Value: Instant at which the keystroke batch was handed to the predictor + forwarded.
    // The retain() prune on the confirm side prevents unbounded growth (D-04 invariant).
    // Logged at debug level under target "nosh::predict" — no character content (T-15-08).
    let mut predict_enqueue_times: std::collections::HashMap<u64, Instant> =
        std::collections::HashMap::new();
    // Monotonic counter incremented once per forwarded keystroke batch.
    // Used as the map key so per-keystroke RTT can be measured independently of epoch grouping.
    let mut keystroke_id: u64 = 0;

    // BUG-G (Windows-specific): ConPTY startup size-sync lag.
    //
    // On Windows, the very first `crossterm::terminal::size()` (which reads
    // `GetConsoleScreenBufferInfo`) can report a stale default (≈80×24) at process
    // startup, BEFORE the conhost/Windows-Terminal ConPTY host has synchronised the
    // real window dimensions into the pseudoconsole. The size sent in the initial
    // `SessionOpen` (measured once at startup in `main`) is therefore wrong, so the
    // remote PTY opens tiny — vim renders as a small square at the top-left — until
    // the user physically resizes the window (which then sends a correct `Resize`).
    //
    // On Unix, `terminal::size()` reads `TIOCGWINSZ` which is reliable at startup,
    // so this never happens — hence the fix is `#[cfg(windows)]`-gated and the Unix
    // path is left completely unchanged.
    //
    // Fix: a single one-shot timer (~400 ms after the pump starts) re-reads the
    // authoritative `terminal::size()` and, if it differs from the dimensions this
    // session was opened with, sends one corrective `Resize`. If the size was already
    // correct, the dims match and no Resize is sent (no-op). This reuses the exact
    // `send_resize` → server `Resize` handler path that the working manual-resize
    // already exercises. It fires once: after firing, `recheck_size` is set to a
    // pending() future so the arm never wakes again.
    // `session_open_dims` is only read by the Windows arm; suppress the unused
    // warning on non-Windows (where the arm body is compiled out).
    #[cfg_attr(not(windows), allow(unused_variables))]
    let session_open_dims = (cols, rows);
    // On Windows: armed ~400 ms ahead. On other platforms: never armed (None) so the
    // one-shot future is always pending() and the arm is an inert no-op.
    #[cfg(windows)]
    let mut size_recheck_deadline: Option<tokio::time::Instant> =
        Some(tokio::time::Instant::now() + Duration::from_millis(400));
    #[cfg(not(windows))]
    let size_recheck_deadline: Option<tokio::time::Instant> = None;

    // ── Scrollback view state machine (SCROLL-04 / SCROLL-05) ──────────────────
    //
    // Initialised as Live at the start of every run_pump invocation (including
    // reattach path), so no residual Active state from a prior connection leaks
    // across reattach (T-22-13 / SCROLL-05 re-open invariant).
    let mut scrollback_view = ScrollbackView::Live;

    // CSI accumulator for split-read-safe Shift-PageUp/Down detection (Pitfall 6 / T-22-11).
    let mut csi_acc = CsiAccumulator::new();

    // Open the Scrollback channel on pump entry (SCROLL-01).
    // Both fresh_session and reattach_session converge here so the re-open logic is shared.
    // channel_id=2: first even client-initiated channel; EvenIdAllocator is local to
    // run_pump since it is the sole channel opener at this scope.
    let mut id_alloc = EvenIdAllocator::new();
    let scrollback_channel_id = id_alloc.next_id(); // = 2

    // mpsc channel pair: scrollback drain task → run_pump page delivery.
    // Capacity 32 prevents unbounded buffer growth if run_pump is in Live mode
    // but pages are still arriving (T-22-14 drop-on-full semantics in drain task).
    let (page_tx, mut page_rx) = mpsc::channel::<Message>(32);

    // mpsc for control messages from the drain task to the pump (credits, close).
    // The pump is the sole writer of the control stream (A4); the drain task
    // sends ChannelCredit / ChannelClose via this mpsc, which run_pump flushes
    // to the wire in a dedicated select arm.
    let (scrollback_ctrl_tx, mut scrollback_ctrl_rx) = mpsc::channel::<Message>(16);

    // Open the Scrollback channel. A Rejected response is non-fatal — the view
    // simply stays in Live mode with no history fetching (server may not support
    // it yet during a phase transition).
    let scrollback_streams = match client::open_channel(
        conn,
        send,
        recv,
        scrollback_channel_id,
        ChannelType::Scrollback,
    )
    .await
    {
        Ok(Some(streams)) => Some(streams),
        Ok(None) => {
            tracing::debug!("Scrollback channel rejected by server — no history available");
            None
        }
        Err(e) => {
            tracing::warn!("Scrollback channel open failed: {e} — continuing without scrollback");
            None
        }
    };

    // Spawn the drain task if the channel was accepted.
    // The task holds ch_send/ch_recv exclusively (M-6 isolation: M-2 pitfall avoidance).
    if let Some((ch_send, ch_recv)) = scrollback_streams {
        tokio::spawn(run_scrollback_drain_task(
            scrollback_channel_id,
            ch_recv,
            ch_send,
            scrollback_ctrl_tx.clone(),
            page_tx,
        ));
    }

    let exit_code;

    loop {
        // BUG-G one-shot (Windows): resolve at the recheck deadline, else pending.
        // On non-Windows the deadline is always None so this is permanently pending.
        let size_recheck = async {
            match size_recheck_deadline {
                Some(d) => tokio::time::sleep_until(d).await,
                None => std::future::pending::<()>().await,
            }
        };

        let resize_sleep = async {
            match resize_deadline {
                Some(d) => tokio::time::sleep_until(d).await,
                None => std::future::pending::<()>().await,
            }
        };

        // Silence-detection future: sleeps until last_datagram_time + 5s, then fires.
        // Uses the resize-deadline sleep_until-or-pending idiom: always resolves at
        // last_datagram_time + 5s (even if already past), which re-arms cleanly each
        // iteration after a datagram arrives (last_datagram_time is reset in datagram arm).
        let silence_sleep = tokio::time::sleep_until(last_datagram_time + Duration::from_secs(5));

        tokio::select! {
            // Server → client reliable stream frames (QOL-02/03: TerminalControl re-emit).
            msg = nosh_proto::read_message(recv) => {
                match msg {
                    Ok(Message::PtyData { data }) => {
                        // D-14-02: display comes exclusively from datagrams via
                        // ClientScreen.render_to_stdout — do NOT write PtyData to stdout.
                        // D-14-03: advance the reattach counter so the cold-reattach
                        // Ack{seq} mechanism on the reliable stream is unaffected.
                        // The reliable-stream Ack{seq} is DISTINCT from the datagram
                        // epoch-ack (D-14-03a / Pitfall 3).
                        //
                        // Phase 22 (SCROLL-01): PtyData on the control stream is the
                        // reattach-replay byte sequence — it still increments
                        // highest_applied for ack tracking. Scrollback content arrives
                        // via the separate Scrollback channel (page_rx arm below).
                        let _ = data;
                        *highest_applied = highest_applied.saturating_add(1);
                    }
                    Ok(Message::SessionClose { exit_code: code, .. }) => {
                        exit_code = code;
                        break;
                    }
                    Ok(Message::SessionOpened { .. }) => {
                        // SessionOpened was already consumed by open_session_with_token;
                        // if it arrives here it's unexpected — ignore.
                    }
                    // Phase 16 / D-16-01: TerminalControl out-of-band re-emit.
                    // OSC 52 / OSC 0/2 are control sequences that carry no cursor motion
                    // and do not write cells — safe to interleave; tokio::select! arms
                    // serialize so no byte-level interleaving with compositor renders.
                    Ok(Message::TerminalControl(payload)) => {
                        match payload {
                            TerminalControlPayload::Clipboard { selection, data } => {
                                // Re-emit OSC 52 clipboard WRITE to the local terminal.
                                // Write-only by construction (T-16-05): the read/query form
                                // was dropped server-side in Plan 16-01, D-16-01a.
                                //
                                // WR-01: Defensively strip ESC (\x1b) and BEL (\x07) from
                                // both sel and b64 before interpolation. The server-side
                                // osc_dispatch rejects malformed selection bytes, but this
                                // client-side strip is a defense-in-depth measure: a premature
                                // BEL terminator in selection would close the OSC sequence early
                                // and allow the remainder to be interpreted as raw terminal bytes.
                                let sel: String = String::from_utf8_lossy(&selection)
                                    .chars()
                                    .filter(|&c| c != '\x07' && c != '\x1b')
                                    .collect();
                                let b64: String = String::from_utf8_lossy(&data)
                                    .chars()
                                    .filter(|&c| c != '\x07' && c != '\x1b')
                                    .collect();
                                let osc52 = format!("\x1b]52;{sel};{b64}\x07");
                                let _ = stdout.write_all(osc52.as_bytes()).await;
                                let _ = stdout.flush().await;
                            }
                            TerminalControlPayload::Title { title } => {
                                // Re-emit OSC 0/2 title only when --status is not active.
                                // When --status is active, the RTT title in the datagram arm
                                // takes precedence (Pitfall 5 — suppress forwarded title).
                                if !status {
                                    // WR-03 fix: strip ESC (\x1b) and BEL (\x07) from the
                                    // server-controlled title before interpolation — defense-
                                    // in-depth, consistent with the OSC 52 clipboard path.
                                    // A premature BEL in title would terminate the OSC sequence
                                    // early and allow remaining bytes to be interpreted as raw
                                    // terminal escape sequences by the local terminal.
                                    let clean_title: String = title
                                        .chars()
                                        .filter(|&c| c != '\x07' && c != '\x1b')
                                        .collect();
                                    let osc02 = format!("\x1b]0;{clean_title}\x07");
                                    let _ = stdout.write_all(osc02.as_bytes()).await;
                                    let _ = stdout.flush().await;
                                }
                            }
                        }
                    }
                    Ok(_) => {} // ignore other control frames
                    Err(e) => {
                        tracing::warn!("reliable stream error, triggering reconnect: {e}");
                        return Ok(PumpOutcome::TransportDrop);
                    }
                }
            }
            // Datagram arm: receive StateDiff, apply to ClientScreen, cull predictions,
            // render display, emit datagram epoch-ack (D-14-02, D-14-03a).
            // Also: update last_datagram_time; clear loss_overlay on resume; emit RTT title.
            // This is the SOLE display path (CLAUDE.md single screen-composition invariant).
            datagram = conn.read_datagram() => {
                match datagram {
                    Ok(bytes) => {
                        if let Ok(diff) = nosh_proto::datagram::decode_datagram(&bytes) {
                            // T-14-06: monotonic epoch gate — stale/replayed diffs discarded.
                            if diff.epoch > screen.last_applied_epoch() {
                                // QOL-01: reset silence timer on every fresh datagram.
                                last_datagram_time = tokio::time::Instant::now();
                                // Clear the loss overlay if it was active (connection resumed).
                                if loss_overlay.active {
                                    loss_overlay.active = false;
                                }

                                // WR-01: capture dims before apply so we can detect a resize.
                                let (cols_before, rows_before) = screen.size();
                                screen.apply(&diff);
                                // TUI-05: alt-screen entry suppression.
                                // When the server signals a false→true alt_screen transition
                                // (i.e. the user launched vim/htop), flush all speculative
                                // predictions so no overlay appears inside cursor-addressing apps.
                                // predictor.reset() clears pending + calls become_tentative() —
                                // the same path as the bracketed-paste and resize resets.
                                if diff.alt_screen && !was_alt_screen {
                                    predictor.reset();
                                }
                                was_alt_screen = diff.alt_screen;
                                // Cull predictions against the new confirmed state and quinn RTT
                                // (D-17-02a: latency instrumentation hook — see below).
                                let rtt_ms = conn.rtt().as_millis() as u64;
                                let epoch_before_cull = predictor.confirmed_epoch();
                                predictor.cull(&screen, diff.epoch, rtt_ms);
                                // WR-01: update predictor dimensions when terminal was resized.
                                let (cols_after, rows_after) = screen.size();
                                if cols_after != cols_before || rows_after != rows_before {
                                    predictor.set_size(cols_after, rows_after);
                                    predictor.reset();
                                }
                                // CR-01: sync predicted cursor from confirmed cursor so that new
                                // predictions land on the correct row (not the hard-zeroed row 0).
                                predictor.sync_cursor_from_confirmed(screen.confirmed_cursor());
                                // D-04 latency instrumentation: when cull advances confirmed_epoch,
                                // one or more keystroke batches were confirmed. Drain all entries
                                // from predict_enqueue_times whose keystroke_id is at or below
                                // the current watermark (keystroke_id at confirm time), log per-
                                // keystroke RTT (predicted→confirmed latency_ms) for each.
                                // No character content is logged — only timing + ids (T-15-08).
                                let epoch_after_cull = predictor.confirmed_epoch();
                                if epoch_after_cull > epoch_before_cull {
                                    // All keystrokes enqueued up to the current keystroke_id
                                    // counter are confirmed by this epoch advance. Drain them in
                                    // order and log per-keystroke latency.
                                    let confirmed_watermark = keystroke_id;
                                    let mut ids_to_drain: Vec<u64> = predict_enqueue_times
                                        .keys()
                                        .copied()
                                        .filter(|&k| k <= confirmed_watermark)
                                        .collect();
                                    ids_to_drain.sort_unstable();
                                    for kid in ids_to_drain {
                                        if let Some(enqueued_at) = predict_enqueue_times.remove(&kid) {
                                            let latency_ms = enqueued_at.elapsed().as_millis() as u64;
                                            tracing::debug!(
                                                target: "nosh::predict",
                                                event = "confirm",
                                                keystroke_id = kid,
                                                epoch = epoch_after_cull,
                                                latency_ms,
                                                "prediction confirmed"
                                            );
                                        }
                                    }
                                    // Prune any remaining stale entries (keystroke_ids from epochs
                                    // that were reset/culled without confirming). Keep only entries
                                    // above the watermark that may still be in-flight.
                                    predict_enqueue_times.retain(|&k, _| k > confirmed_watermark);
                                }
                                // Pitfall 1: render_with_predictor requires std::io::Write (NOT
                                // tokio::io::AsyncWrite). Buffer to Vec<u8>, then async flush.
                                let mut buf: Vec<u8> = Vec::new();
                                screen.render_with_predictor(&mut buf, &predictor, &loss_overlay).unwrap_or_else(|e| {
                                    tracing::warn!("render_with_predictor error: {e}");
                                });
                                if !buf.is_empty() {
                                    if let Err(e) = stdout.write_all(&buf).await {
                                        tracing::warn!("stdout write_all failed: {e} — forcing full repaint");
                                        screen.reset_physical();
                                    } else if let Err(e) = stdout.flush().await {
                                        tracing::warn!("stdout flush failed: {e} — forcing full repaint");
                                        screen.reset_physical();
                                    }
                                }
                                // QOL-04: --status RTT title (out-of-band, no cursor motion).
                                // Best-effort: ignore error (control sequence, not display state;
                                // no reset_physical — the compositor render above already completed).
                                if status {
                                    let title = format!("\x1b]0;nosh: {rtt_ms}ms\x07");
                                    let _ = stdout.write_all(title.as_bytes()).await;
                                    let _ = stdout.flush().await;
                                }
                                // D-14-03a: emit epoch-ack as DATAGRAM on the datagram channel
                                // (TAG_CLIENT_EPOCH 0x02), DISTINCT from reliable-stream Ack{seq}
                                // (Pitfall 3). Best-effort; ignore error (RESEARCH A6 / T-14-DoS).
                                let ack_payload = nosh_proto::datagram::encode_epoch_ack(diff.epoch);
                                let _ = conn.send_datagram(ack_payload);
                            }
                            // Stale epoch silently discarded (Pitfall 6).
                        }
                        // Non-StateDiff datagrams (unknown tag): decode_datagram returns Err
                        // → silently discarded (T-14-05 resilience; T-14-08 injection block).
                    }
                    Err(e) => {
                        // Transport drop on datagram channel — mirror reliable-stream behavior.
                        tracing::warn!("datagram channel error, triggering reconnect: {e}");
                        return Ok(PumpOutcome::TransportDrop);
                    }
                }
            }
            // Silence detection (QOL-01): fires when no datagram arrives for >5 s.
            // Activates the loss overlay and forces an immediate render so the banner
            // appears exactly at the 5 s threshold (not frozen until next datagram).
            // Guard: `if !loss_overlay.active` prevents a hot-spin after activation —
            // without the guard, silence_sleep is recreated each iteration at a deadline
            // already in the past (last_datagram_time is not updated after activation),
            // causing it to resolve immediately every iteration and flood the render path.
            // With the guard, once the overlay is active this arm becomes pending() and
            // only loss_tick.tick() drives the 1s re-renders (CR-01).
            _ = silence_sleep, if !loss_overlay.active => {
                // BUG-C: datagram silence is NOT connection loss. An idle but healthy
                // interactive shell produces no state-sync datagrams, yet the QUIC
                // connection stays alive via keep-alive PINGs (transport_config:
                // KEEP_ALIVE=15s, MAX_IDLE_TIMEOUT=300s). Previously this arm activated
                // the "reconnecting" overlay after a flat 5s of datagram silence, so the
                // overlay falsely appeared whenever the user stopped typing.
                //
                // Gate the overlay on ACTUAL connection health instead of mere silence:
                //   - conn.close_reason().is_some()  → the QUIC connection has closed /
                //     the path failed / idle-timed-out (genuine loss).
                // If the connection is still live (close_reason() == None), this is just
                // an idle shell — do NOT show the overlay. Re-arm the silence timer for
                // another interval so a LATER genuine loss is still detected promptly.
                //
                // Genuine loss is ALSO (and primarily) surfaced by the datagram/reliable
                // -stream read arms returning Err → PumpOutcome::TransportDrop, which tears
                // down the pump and shows "reconnecting…" in the supervisor. This overlay
                // is the in-session banner for a path that has gone quiet AND is confirmed
                // closed. C6 migration (which keeps close_reason() == None throughout) does
                // NOT trip this — preserving the working migration path.
                if conn.close_reason().is_some() {
                    loss_overlay.active = true;
                    loss_overlay.last_contact = last_datagram_time.into_std();
                    let mut buf: Vec<u8> = Vec::new();
                    screen.render_with_predictor(&mut buf, &predictor, &loss_overlay).unwrap_or_else(|e| {
                        tracing::warn!("render_with_predictor error (silence): {e}");
                    });
                    if !buf.is_empty() {
                        if let Err(e) = stdout.write_all(&buf).await {
                            tracing::warn!("stdout write_all failed (silence): {e} — forcing full repaint");
                            screen.reset_physical();
                        } else if let Err(e) = stdout.flush().await {
                            tracing::warn!("stdout flush failed (silence): {e} — forcing full repaint");
                            screen.reset_physical();
                        }
                    }
                } else {
                    // Healthy idle connection: bump the silence baseline so the timer
                    // re-arms for another interval instead of spinning on a past deadline.
                    // No render — the overlay did not change, so emitting a cursor move
                    // every 5s while the user is idle would be a spurious jiggle.
                    last_datagram_time = tokio::time::Instant::now();
                }
            }
            // Live elapsed-counter tick (QOL-01): drives a 1s re-render while active
            // so the "last contact Ns ago" seconds counter increments live on screen.
            // Guard: only fires when loss_overlay.active to avoid spurious re-renders.
            _ = loss_tick.tick(), if loss_overlay.active => {
                let mut buf: Vec<u8> = Vec::new();
                screen.render_with_predictor(&mut buf, &predictor, &loss_overlay).unwrap_or_else(|e| {
                    tracing::warn!("render_with_predictor error (loss_tick): {e}");
                });
                if !buf.is_empty() {
                    if let Err(e) = stdout.write_all(&buf).await {
                        tracing::warn!("stdout write_all failed (loss_tick): {e} — forcing full repaint");
                        screen.reset_physical();
                    } else if let Err(e) = stdout.flush().await {
                        tracing::warn!("stdout flush failed (loss_tick): {e} — forcing full repaint");
                        screen.reset_physical();
                    }
                }
            }
            // Scrollback page delivery from drain task to view buffer (Task 2b).
            // Minimal stub here: page forwarding and epoch-gated exit are implemented
            // in Task 2b. Here we just drain the channel to avoid back-pressure.
            msg = page_rx.recv() => {
                if let Some(Message::ScrollbackPage { .. }) = msg {
                    // Task 2b will implement the full page_rx arm behavior.
                    // For now: if Active, append lines; if Live, drop.
                    // Stub: drop unconditionally (no scrollback display yet).
                    let _ = &scrollback_view; // suppress unused-variable warning
                }
            }
            // Scrollback control frames (ChannelCredit, ChannelClose) from drain task.
            // The pump writes them to the wire (A4 single-writer invariant).
            ctrl_msg = scrollback_ctrl_rx.recv() => {
                if let Some(msg) = ctrl_msg {
                    match &msg {
                        Message::ScrollbackCredit { .. } | Message::ChannelClose { .. } => {
                            // Forward the credit/close to the server via the control stream.
                            if nosh_proto::write_message(send, &msg).await.is_err() {
                                return Ok(PumpOutcome::TransportDrop);
                            }
                        }
                        _ => {}
                    }
                }
            }
            // Keystrokes: CSI-intercepted BEFORE the escape machine (Pitfall 6 / T-22-11).
            //
            // Processing order (SCROLL-04):
            // 1. CsiAccumulator intercepts Shift-PageUp/Down 6-byte sequences (split-read safe).
            // 2. Non-paging bytes are forwarded to EscapeState + shell.
            // 3. In Active mode, any non-paging byte: snap to Live + forward (T-22-15 LOCKED).
            // 4. The escape machine is fed ONLY local stdin bytes (T-09-01).
            n = stdin.read(&mut stdin_buf) => {
                match n {
                    Ok(0) => return Ok(PumpOutcome::UserQuit),
                    Ok(n) => {
                        // Phase 22 (SCROLL-04): scan for paging CSI sequences BEFORE the
                        // escape machine. CsiAccumulator handles split reads (Pitfall 6).
                        let csi = csi_acc.process(&stdin_buf[..n]);

                        // Handle Shift-PageUp.
                        if csi.shift_pageup {
                            match &scrollback_view {
                                ScrollbackView::Live => {
                                    // Enter scrollback mode and issue the first request.
                                    scrollback_view = ScrollbackView::Active {
                                        lines: vec![],
                                        offset: 0,
                                        pending_request: true,
                                        epoch_at_snapshot: 0, // updated on first page
                                        total_available: 0,
                                    };
                                    // Send the first ScrollbackRequest on the scrollback channel.
                                    // Note: ch_send was consumed by the drain task — the request
                                    // is sent by the drain task reading from ch_recv (server side).
                                    // The client signals via the control stream (ScrollbackRequest
                                    // travels on the channel's own data stream per Open Question 1).
                                    // For now, note that the scrollback ch_send is in the drain task.
                                    // TODO in Task 2b: wire the request send path.
                                    tracing::debug!("Shift-PageUp: entering scrollback Active mode");
                                }
                                ScrollbackView::Active { offset, .. } => {
                                    // Already active: page up.
                                    let o = *offset;
                                    if let ScrollbackView::Active { offset, .. } = &mut scrollback_view {
                                        *offset = o.saturating_add(screen.size().1 as usize);
                                    }
                                    tracing::debug!("Shift-PageUp: paging up in scrollback");
                                }
                            }
                        }

                        // Handle Shift-PageDown.
                        if csi.shift_pagedown {
                            match &scrollback_view {
                                ScrollbackView::Active { offset, lines, .. } => {
                                    let rows = screen.size().1 as usize;
                                    let current_offset = *offset;
                                    let lines_len = lines.len();
                                    if current_offset <= rows {
                                        // Paging down past the live boundary: auto-exit (LOCKED).
                                        scrollback_view = ScrollbackView::Live;
                                        tracing::debug!("Shift-PageDown: reached live boundary, exiting scrollback");
                                    } else {
                                        // Page down.
                                        if let ScrollbackView::Active { offset, .. } = &mut scrollback_view {
                                            *offset = current_offset.saturating_sub(rows);
                                        }
                                        let _ = lines_len;
                                        tracing::debug!("Shift-PageDown: paging down in scrollback");
                                    }
                                }
                                ScrollbackView::Live => {
                                    // Shift-PageDown while Live: no-op.
                                }
                            }
                        }

                        // Non-paging bytes from the CSI accumulator.
                        // In Active mode: any non-paging byte MUST snap back to Live
                        // AND be forwarded to the shell (T-22-15 LOCKED / T-22-11).
                        if !csi.non_paging_remainder.is_empty() {
                            let bytes_to_process = csi.non_paging_remainder.as_slice();

                            // Snap back to Live mode if we were in Active (SCROLL-04 LOCKED).
                            if matches!(scrollback_view, ScrollbackView::Active { .. }) {
                                scrollback_view = ScrollbackView::Live;
                                tracing::debug!("non-paging keystroke while in scrollback: snapping to Live");
                            }

                            // Now process through the existing escape machine + forward path.
                            let result = escape.process(bytes_to_process);
                            if result.quit {
                                return Ok(PumpOutcome::UserQuit);
                            }
                            if !result.bytes_to_forward.is_empty() {
                                predictor.on_input(&result.bytes_to_forward, &screen);

                                if predictor.pending_len() == 0 {
                                    let watermark = keystroke_id;
                                    predict_enqueue_times.retain(|&k, _| k > watermark);
                                }

                                keystroke_id += 1;
                                predict_enqueue_times.insert(keystroke_id, Instant::now());
                                let epoch_required = predictor.prediction_epoch();
                                tracing::debug!(
                                    target: "nosh::predict",
                                    event = "predict",
                                    keystroke_id,
                                    epoch_required,
                                    "keystroke predicted"
                                );

                                let mut buf: Vec<u8> = Vec::new();
                                screen.render_with_predictor(&mut buf, &predictor, &loss_overlay).unwrap_or_else(|e| {
                                    tracing::warn!("render_with_predictor error: {e}");
                                });
                                if !buf.is_empty() {
                                    if let Err(e) = stdout.write_all(&buf).await {
                                        tracing::warn!("stdout write_all failed: {e} — forcing full repaint");
                                        screen.reset_physical();
                                    } else if let Err(e) = stdout.flush().await {
                                        tracing::warn!("stdout flush failed: {e} — forcing full repaint");
                                        screen.reset_physical();
                                    }
                                }

                                if client::send_input(send, &result.bytes_to_forward).await.is_err() {
                                    return Ok(PumpOutcome::TransportDrop);
                                }
                            }
                        }
                    }
                    Err(_) => return Ok(PumpOutcome::UserQuit),
                }
            }
            // Terminal resize: SIGWINCH (Unix) or terminal::size() poll (Windows).
            // Platform abstraction via ResizeWatcher::next_resize().
            // The AUTHORITATIVE size is re-read via crossterm::terminal::size() in
            // the resize_sleep arm (Pitfall 14 — do not trust event fields directly).
            _ = resize.next_resize() => {
                resize_deadline = Some(tokio::time::Instant::now() + RESIZE_DEBOUNCE);
            }
            // Debounce elapsed: send one coalesced Resize.
            // Re-reads terminal::size() for authoritative dims (Pitfall 14).
            _ = resize_sleep => {
                resize_deadline = None;
                if let Ok((c, r)) = crossterm::terminal::size() {
                    let _ = client::send_resize(send, c, r).await;
                }
            }
            // BUG-G (Windows-only): one-shot post-open size re-measure to correct
            // the ConPTY startup size-sync lag. Fires once ~400 ms after the pump
            // starts; sends a corrective Resize only if the authoritative size now
            // differs from the dimensions the session was opened with. Disarms itself.
            // On non-Windows this arm is permanently pending (deadline is None) and
            // the body compiles out, so it is a true no-op.
            _ = size_recheck => {
                #[cfg(windows)]
                {
                    // Disarm: never fire again for this session.
                    size_recheck_deadline = None;
                    if let Ok((c, r)) = crossterm::terminal::size() {
                        if (c, r) != session_open_dims {
                            tracing::debug!(
                                opened = ?session_open_dims,
                                now = ?(c, r),
                                "BUG-G: correcting ConPTY startup size via post-open Resize"
                            );
                            let _ = client::send_resize(send, c, r).await;
                        }
                    }
                }
            }
            // Periodic Ack (D-08): send only when highest_applied advanced.
            _ = ack_interval.tick() => {
                if *highest_applied != last_acked {
                    if client::send_ack(send, *highest_applied).await.is_err() {
                        return Ok(PumpOutcome::TransportDrop);
                    }
                    last_acked = *highest_applied;
                }
            }
        }
    }

    let _ = send.finish();
    Ok(PumpOutcome::CleanExit(exit_code))
}
