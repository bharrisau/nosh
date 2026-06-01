//! Datagram wire-format module for nosh (decision D-11-01/D-11-02/D-11-03).
//!
//! This module defines the [`StateDiff`] sparse terminal-diff type and the
//! [`encode_datagram`] / [`decode_datagram`] pair — the shared contract that
//! every subsequent server (Phase 12/13) and client (Phase 14/15) component
//! depends on (SYNC-01).
//!
//! Datagrams bypass the reliable-stream length-prefix framing in `codec.rs`
//! entirely (D-11-03a). Do NOT serialize [`StateDiff`] as a [`Message`] variant;
//! these are independent channels.
//!
//! [`Message`]: crate::messages::Message

use bytes::Bytes;
use serde::{Deserialize, Serialize};

use crate::codec::ProtoError;

// ── Tag byte discriminant (Pattern 3 — extensible) ───────────────────────────
/// Tag byte for a [`StateDiff`] datagram payload (server → client).
const TAG_STATE_DIFF: u8 = 0x01;
/// Tag byte for a [`ClientEpoch`] datagram payload (client → server).
const TAG_CLIENT_EPOCH: u8 = 0x02;

/// Maximum accepted run count in a decoded [`StateDiff`]. Guards against a
/// malformed packet forcing a multi-megabyte allocation (T-11-02 DoS guard).
pub const MAX_RUNS: usize = 4096;

/// Minimum valid `cap` argument for [`encode_datagram`].
///
/// The header-only (zero-run) [`StateDiff`] payload is 7 bytes under postcard
/// (1 tag byte + 6-byte body: epoch=1-byte varint, cols=1, rows=1, cursor.row=1,
/// cursor.col=1, runs.len=1). Any `cap <= 7` cannot satisfy the strict
/// `payload.len() < cap` guarantee; callers must pass `cap >= MIN_CAP`.
///
/// In practice `cap` always derives from `Connection::max_datagram_size()` (QUIC
/// negotiated MTU minus overhead), which is always >= 1200 bytes — well above
/// this floor. The guard exists to make the API contract explicit and to catch
/// any future callsite that constructs a synthetic cap.
pub const MIN_CAP: usize = 8;

// ── Wire types ────────────────────────────────────────────────────────────────

/// A sparse terminal-state diff sent as a QUIC datagram (loss-tolerant,
/// latest-state-wins channel).
///
/// The client applies a diff only if `epoch > last_applied_epoch` (D-11-03).
/// Resize events are represented as a diff with updated `cols`/`rows` — the
/// epoch increments monotonically and never resets (D-11-03).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateDiff {
    /// Server-side state version. Monotonically increasing, **never resets**.
    /// The client applies this diff only if `epoch > last_applied_epoch`.
    ///
    /// **DISTINCT from reliable-stream `seq`** (D-11-03a). The `seq` field in
    /// [`Message::Reattach`] / [`Message::Ack`] belongs to the sequenced
    /// output buffer; `epoch` belongs to the datagram state-sync channel.
    /// These are independent counters with independent semantics.
    ///
    /// [`Message::Reattach`]: crate::messages::Message::Reattach
    /// [`Message::Ack`]: crate::messages::Message::Ack
    pub epoch: u64,
    /// Terminal width in columns at the time this diff was encoded.
    pub cols: u16,
    /// Terminal height in rows at the time this diff was encoded.
    pub rows: u16,
    /// Cursor position at the time this diff was encoded.
    pub cursor: CursorPos,
    /// Changed cells encoded as run-length runs (sparse; only changed cells).
    ///
    /// May be a subset of all changed cells if the full set exceeded the
    /// datagram cap (cursor-priority partial update, D-11-01). Deferred cells
    /// reappear naturally in subsequent ticks because diffs are computed against
    /// the last-acked state.
    pub runs: Vec<DiffRun>,
}

/// A cursor position (0-based row/column).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CursorPos {
    /// Row index, 0-based.
    pub row: u16,
    /// Column index, 0-based.
    pub col: u16,
}

/// A run of contiguous changed cells on one terminal row sharing the same style.
///
/// `chars` is a [`String`] (NOT `Vec<char>`) so ASCII text encodes as
/// 1 byte per character under postcard (varint_len + raw UTF-8). Using
/// `Vec<char>` would double the per-ASCII-char cost to 2 bytes because
/// postcard serializes `char` as a length-prefixed UTF-8 str (RESEARCH Pitfall 3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffRun {
    /// Row index of this run (0-based).
    pub row: u16,
    /// Column index of the first cell in the run (0-based).
    pub start_col: u16,
    /// SGR attributes for all cells in this run (packed bitflags).
    pub style: CellStyle,
    /// ANSI 256-color foreground color.
    ///
    /// `None` = use the terminal's default foreground color (not the same as
    /// palette index 0, which is black). `Some(n)` = palette index `n` (0–255).
    ///
    /// `Option<u8>` distinguishes "default" from "palette index 0 (black)":
    /// with a plain `u8`, `fg=0` would be ambiguous between the two.
    pub fg: Option<u8>,
    /// ANSI 256-color background color.
    ///
    /// `None` = use the terminal's default background color (not the same as
    /// palette index 0, which is black). `Some(n)` = palette index `n` (0–255).
    ///
    /// `Option<u8>` distinguishes "default" from "palette index 0 (black)":
    /// with a plain `u8`, `bg=0` would be ambiguous between the two.
    pub bg: Option<u8>,
    /// UTF-8 text for all cells in the run. The number of Unicode scalar values
    /// (`chars().count()`) equals the column count of the run for single-width
    /// characters. Wide character handling is deferred to Phase 15.
    pub chars: String,
}

/// SGR attribute bitflags packed into a single byte. Encodes as exactly 1 byte
/// under postcard (a `u8` newtype, no varint).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CellStyle(pub u8);

impl CellStyle {
    /// No attributes set (plain text).
    pub const NONE: u8 = 0x00;
    /// Bold / increased intensity.
    pub const BOLD: u8 = 0x01;
    /// Italic.
    pub const ITALIC: u8 = 0x02;
    /// Underline.
    pub const UNDERLINE: u8 = 0x04;
    /// Reverse video (swap fg/bg).
    pub const REVERSE: u8 = 0x08;
    // Bits 0x10, 0x20, 0x40, 0x80: reserved for future SGR attributes.
}

/// A client→server epoch acknowledgement sent as a QUIC datagram (D-13-01/D-13-01a).
///
/// After the client applies a [`StateDiff`] with a given `epoch`, it sends this
/// message to inform the server of its confirmed display state. The server uses this
/// to advance the baseline for subsequent diffs — only cells that changed since the
/// acked epoch are re-transmitted.
///
/// `Copy` is intentional: this is a single `u64` field with no heap allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientEpoch {
    /// The last epoch the client has applied to its display.
    pub epoch: u64,
}

/// Encode a `ClientEpoch` as a tagged datagram payload (infallible).
///
/// The returned [`Bytes`] begins with `TAG_CLIENT_EPOCH` (0x02) followed by a
/// postcard-serialized `u64`. Serialization of a single `u64` under postcard
/// cannot fail (no heap allocation limit, no variant tagging — just a varint).
pub fn encode_epoch_ack(epoch: u64) -> Bytes {
    let body = postcard::to_allocvec(&ClientEpoch { epoch })
        .expect("postcard serialization of ClientEpoch (u64) cannot fail");
    let mut payload = Vec::with_capacity(1 + body.len());
    payload.push(TAG_CLIENT_EPOCH);
    payload.extend_from_slice(&body);
    Bytes::from(payload)
}

/// Decode an epoch-ack datagram payload into the acknowledged epoch value.
///
/// Returns `Err` for any tag other than `TAG_CLIENT_EPOCH` (0x02), including
/// `TAG_STATE_DIFF` (0x01) — a misrouted [`StateDiff`] is never read as an epoch-ack.
///
/// # Errors
///
/// Returns [`ProtoError`] (never panics) on:
/// * Empty input — no tag byte present.
/// * Wrong tag byte — any value other than `TAG_CLIENT_EPOCH` (0x02), including
///   `TAG_STATE_DIFF` (0x01). This is the security-relevant guard (T-13-01): an
///   attacker cannot re-use a StateDiff payload as an epoch-ack.
/// * Truncated or corrupt postcard body — `postcard::from_bytes` returns `Err`.
pub fn decode_epoch_ack(bytes: &[u8]) -> Result<u64, ProtoError> {
    let (tag, body) = bytes
        .split_first()
        .ok_or(ProtoError::Postcard(postcard::Error::DeserializeUnexpectedEnd))?;
    if *tag != TAG_CLIENT_EPOCH {
        return Err(ProtoError::Postcard(postcard::Error::DeserializeBadEncoding));
    }
    let ce: ClientEpoch = postcard::from_bytes(body).map_err(ProtoError::Postcard)?;
    Ok(ce.epoch)
}

// ── Encode / Decode ───────────────────────────────────────────────────────────

/// Encode a [`StateDiff`] into a datagram payload that is **strictly less than
/// `cap`** bytes for ANY input.
///
/// # Large-repaint decision (D-11-01 / D-11-01a)
///
/// When the set of changed cells (`diff.runs`) would exceed the datagram cap,
/// this function encodes cells prioritized by **proximity to the cursor** (sorted
/// by Manhattan distance to `diff.cursor`, ascending) and defers the rest.
/// Deferred cells naturally reappear in subsequent ticks because diffs are
/// computed against the last-acked state — no cell is permanently lost.
///
/// ## Alternatives explicitly rejected (D-11-01a)
///
/// * **Skip-frame:** A persistently-large screen (full-screen vim, `cat big_file`)
///   could fail to converge — the screen never fully repaints if every tick
///   overflows the cap.
/// * **Reliable-stream fallback:** Couples the datagram and stream channels;
///   reintroduces head-of-line blocking for what should be a loss-tolerant path,
///   directly contradicting the core architecture (CLAUDE.md load-bearing decision
///   on QUIC datagram design).
///
/// ## Size guarantee (D-11-01b)
///
/// For any input where `cap >= MIN_CAP` (8), `payload.len() < cap` strictly.
/// This is enforced by the cursor-priority fill loop using
/// `postcard::experimental::serialized_size` with a strict bound: a run is kept
/// only if `serialized_size(candidate_body) + 1 < cap` (the `+1` accounts for
/// the tag byte; strict less-than per the off-by-one pitfall).
///
/// **Precondition:** `cap >= MIN_CAP`. Callers that pass `cap < MIN_CAP` receive
/// `Err(ProtoError::CapTooSmall)`. In practice, `cap` always derives from
/// `Connection::max_datagram_size()` (QUIC MTU, always >= 1200) — this bound is
/// never reached from real callers.
///
/// ## Continued-fill past rejection
///
/// When a run is rejected (too large to fit), the fill loop does **NOT** break.
/// It continues to the next run, which may be shorter and still fit. This is
/// critical for correctness: a single oversize run (e.g., a wide terminal row)
/// must not prevent smaller runs elsewhere on the screen from being included.
///
/// # Returns
///
/// `Ok((payload, deferred))` where:
/// * `payload` is a [`Bytes`] value with `payload.len() < cap` strictly.
/// * `deferred` is the list of [`DiffRun`] values that did not fit; the caller
///   (Phase 13 tick loop) re-presents them on the next tick.
///
/// # Errors
///
/// Returns [`ProtoError::CapTooSmall`] if `cap < MIN_CAP` (8). The minimum
/// valid cap is [`MIN_CAP`]; the header-only payload is 7 bytes so any cap
/// below 8 cannot satisfy the strict `< cap` guarantee.
///
/// Returns [`ProtoError::Postcard`] if postcard serialization fails (should
/// not occur for well-formed types).
pub fn encode_datagram(
    diff: &StateDiff,
    cap: usize,
) -> Result<(Bytes, Vec<DiffRun>), ProtoError> {
    // Enforce the minimum-cap precondition before touching anything else.
    // Callers that derive cap from max_datagram_size() (always >= 1200) will
    // never hit this; the guard exists for future callers and tests.
    if cap < MIN_CAP {
        return Err(ProtoError::CapTooSmall(cap, MIN_CAP));
    }

    // Reserve 1 byte for the TAG_STATE_DIFF prefix (Pitfall 4).
    // All `serialized_size` comparisons use `body_cap`; the final payload is
    // `body + 1 tag byte`, so `payload.len() = body.len() + 1 < cap` iff
    // `body.len() < cap - 1 = body_cap`.
    let body_cap = cap.saturating_sub(1);

    // Sort runs by Manhattan distance to cursor (ascending = cursor-closest first).
    // The run whose start_col/row matches the cursor sorts with priority 0.
    let mut sorted_runs = diff.runs.clone();
    let cols = diff.cols as u32;
    let cursor = &diff.cursor;
    sorted_runs.sort_by_key(|r| {
        (r.row as i32 - cursor.row as i32).unsigned_abs() * cols
            + (r.start_col as i32 - cursor.col as i32).unsigned_abs()
    });

    // Build header (empty runs) for candidate-diff construction.
    let header = StateDiff {
        epoch: diff.epoch,
        cols: diff.cols,
        rows: diff.rows,
        cursor: diff.cursor,
        runs: vec![],
    };

    let mut encoded_runs: Vec<DiffRun> = Vec::new();
    let mut deferred_runs: Vec<DiffRun> = Vec::new();

    for run in sorted_runs {
        // Tentatively add this run and check total encoded size.
        encoded_runs.push(run.clone());
        let candidate = StateDiff {
            runs: encoded_runs.clone(),
            ..header.clone()
        };
        let size = postcard::experimental::serialized_size(&candidate)
            .map_err(ProtoError::Postcard)?;

        // Strict less-than: body must be < body_cap so that body + 1 tag < cap.
        if size < body_cap {
            // Run fits — keep it.
        } else {
            // Run does not fit. Remove it and attempt a char-level split.
            encoded_runs.pop();

            // Compute remaining budget for this run's body contribution.
            // current_body_size = size of the diff WITHOUT this run.
            let current = StateDiff {
                runs: encoded_runs.clone(),
                ..header.clone()
            };
            let current_size = postcard::experimental::serialized_size(&current)
                .map_err(ProtoError::Postcard)?;

            // Budget for a run's chars field content (excluding the header
            // overhead). We estimate run header bytes conservatively (worst-case
            // varint + 1-byte fields: row=3, start_col=3, style=1, fg=1, bg=1,
            // str_varint=3 → 12 bytes max). We subtract both the run overhead
            // and the 1-byte tag, and require strict less-than.
            const RUN_HEADER_OVERHEAD: usize = 12;
            let remaining = body_cap.saturating_sub(current_size + RUN_HEADER_OVERHEAD);

            if remaining > 0 {
                // Try to split: find how many leading chars of run.chars fit.
                let prefix_chars = fit_chars_in_bytes(&run.chars, remaining);
                if prefix_chars > 0 {
                    let split_point = char_byte_offset(&run.chars, prefix_chars);
                    let left_chars = run.chars[..split_point].to_string();
                    let right_chars = run.chars[split_point..].to_string();

                    let left_run = DiffRun {
                        row: run.row,
                        start_col: run.start_col,
                        style: run.style,
                        fg: run.fg,
                        bg: run.bg,
                        chars: left_chars,
                    };
                    // Deferred run's start_col advances by the number of
                    // Unicode scalar values (chars) in the prefix (Pitfall 5).
                    // Use saturating_add to prevent u16 overflow: a run whose
                    // start_col + prefix_chars would exceed u16::MAX (65535) is
                    // a degenerate terminal state; saturating at u16::MAX is the
                    // least-surprising fallback (the right portion is already
                    // deferred, so the column position is at worst clamped).
                    let right_run = DiffRun {
                        row: run.row,
                        start_col: run.start_col.saturating_add(prefix_chars as u16),
                        style: run.style,
                        fg: run.fg,
                        bg: run.bg,
                        chars: right_chars,
                    };

                    // Verify the left run actually fits with a strict check.
                    encoded_runs.push(left_run.clone());
                    let candidate_with_left = StateDiff {
                        runs: encoded_runs.clone(),
                        ..header.clone()
                    };
                    let size_with_left = postcard::experimental::serialized_size(
                        &candidate_with_left,
                    )
                    .map_err(ProtoError::Postcard)?;

                    if size_with_left < body_cap {
                        // Split prefix fits — keep left, defer right.
                        if !right_run.chars.is_empty() {
                            deferred_runs.push(right_run);
                        }
                    } else {
                        // Split didn't fit after all — defer the whole run.
                        // Continue to the next run (do NOT break).
                        encoded_runs.pop();
                        deferred_runs.push(run);
                    }
                } else {
                    // Zero chars fit — defer the whole run.
                    // Continue to the next run (do NOT break).
                    deferred_runs.push(run);
                }
            } else {
                // No budget at all — defer the whole run.
                // Continue to the next run (do NOT break).
                deferred_runs.push(run);
            }
            // NOTE: The loop does NOT break here. A later, shorter run may
            // still fit within the remaining budget. Stopping on the first
            // rejection would fail the heterogeneous-fill test
            // (continue-past-rejection guard — see tests::heterogeneous_continue_past_rejection).
        }
    }

    let final_diff = StateDiff {
        runs: encoded_runs,
        ..header
    };
    let body = postcard::to_allocvec(&final_diff).map_err(ProtoError::Postcard)?;

    // Strict invariant: body must be < body_cap so payload < cap.
    // Hard assert (not debug_assert) — after CR-01 guarantees cap >= MIN_CAP,
    // this can only fire due to an implementation bug in the fill loop, not a
    // caller error. A hard assert makes such bugs visible in release builds too.
    assert!(
        body.len() < body_cap,
        "encode_datagram fill-loop invariant violated: body {} >= body_cap {}",
        body.len(),
        body_cap
    );

    let mut payload = Vec::with_capacity(1 + body.len());
    payload.push(TAG_STATE_DIFF);
    payload.extend_from_slice(&body);

    Ok((Bytes::from(payload), deferred_runs))
}

/// Decode a datagram payload into a [`StateDiff`].
///
/// # Errors
///
/// Returns [`ProtoError`] (never panics) on:
/// * Empty input — no tag byte present.
/// * Unknown tag byte — not `TAG_STATE_DIFF` (0x01).
/// * Truncated or corrupt postcard body — `postcard::from_bytes` returns `Err`.
/// * Run vector exceeding [`MAX_RUNS`] — guards against a malformed packet
///   forcing a large allocation (T-11-02 DoS guard).
pub fn decode_datagram(bytes: &[u8]) -> Result<StateDiff, ProtoError> {
    let (tag, body) = bytes
        .split_first()
        .ok_or(ProtoError::Postcard(postcard::Error::DeserializeUnexpectedEnd))?;
    if *tag != TAG_STATE_DIFF {
        return Err(ProtoError::Postcard(
            postcard::Error::DeserializeBadEncoding,
        ));
    }
    let diff: StateDiff = postcard::from_bytes(body).map_err(ProtoError::Postcard)?;
    // T-11-02: sanity-cap the run count to bound allocation from a malformed packet.
    if diff.runs.len() > MAX_RUNS {
        return Err(ProtoError::Postcard(
            postcard::Error::DeserializeBadEncoding,
        ));
    }
    Ok(diff)
}

// ── Private helpers ───────────────────────────────────────────────────────────

/// Return how many leading Unicode scalar values (chars) of `s` fit within
/// `max_bytes` UTF-8 bytes.
fn fit_chars_in_bytes(s: &str, max_bytes: usize) -> usize {
    let mut byte_count = 0usize;
    let mut char_count = 0usize;
    for ch in s.chars() {
        let ch_len = ch.len_utf8();
        if byte_count + ch_len > max_bytes {
            break;
        }
        byte_count += ch_len;
        char_count += 1;
    }
    char_count
}

/// Return the byte offset at which the `n`-th Unicode scalar value begins in `s`.
///
/// Returns `s.len()` if `n >= s.chars().count()` (the end-of-string sentinel,
/// yielding an empty split tail). Never panics. Callers that pass a count from
/// [`fit_chars_in_bytes`] need not worry about out-of-bounds.
fn char_byte_offset(s: &str, n: usize) -> usize {
    s.char_indices()
        .nth(n)
        .map(|(i, _)| i)
        .unwrap_or(s.len())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Task A ────────────────────────────────────────────────────────────────

    /// Confirm that a single 80-char-"a" DiffRun encodes to ≤ 90 bytes under
    /// postcard (validates String vs Vec<char> encoding cost).
    #[test]
    fn single_run_80_chars_serialized_size_lte_90() {
        let run = DiffRun {
            row: 0,
            start_col: 0,
            style: CellStyle(CellStyle::NONE),
            fg: None,
            bg: None,
            chars: "a".repeat(80),
        };
        // Wrap in a StateDiff to measure via the same serializer path.
        let diff = StateDiff {
            epoch: 1,
            cols: 80,
            rows: 24,
            cursor: CursorPos { row: 0, col: 0 },
            runs: vec![run],
        };
        // Measure size of just the single-run diff; subtract the empty-run-list
        // header to isolate the run's contribution.
        let with_run =
            postcard::experimental::serialized_size(&diff).expect("serialized_size");
        let empty_diff = StateDiff {
            runs: vec![],
            ..diff.clone()
        };
        let without_run =
            postcard::experimental::serialized_size(&empty_diff).expect("serialized_size");
        let run_size = with_run - without_run;
        assert!(
            run_size <= 90,
            "80-char run should be <= 90 bytes under postcard (String encoding), got {run_size}"
        );
    }

    // ── Task B helpers ────────────────────────────────────────────────────────

    /// Test-only helper: produce a tag-prefixed postcard payload from a
    /// [`StateDiff`] without going through the cap-enforcing `encode_datagram`.
    /// Used exclusively in Task B decode-hardening tests so no cap-violating
    /// encode function is exported during Tasks A/B.
    fn tag_encode(diff: &StateDiff) -> Vec<u8> {
        let body = postcard::to_allocvec(diff).expect("postcard::to_allocvec in tag_encode");
        let mut payload = Vec::with_capacity(1 + body.len());
        payload.push(TAG_STATE_DIFF);
        payload.extend_from_slice(&body);
        payload
    }

    fn make_diff(epoch: u64, runs: Vec<DiffRun>) -> StateDiff {
        StateDiff {
            epoch,
            cols: 80,
            rows: 24,
            cursor: CursorPos { row: 12, col: 40 },
            runs,
        }
    }

    // ── Task B: round-trip tests ──────────────────────────────────────────────

    #[test]
    fn round_trip_empty_runs() {
        let diff = make_diff(1, vec![]);
        assert_eq!(decode_datagram(&tag_encode(&diff)).unwrap(), diff);
    }

    #[test]
    fn round_trip_single_ascii_run() {
        let diff = make_diff(
            2,
            vec![DiffRun {
                row: 5,
                start_col: 10,
                style: CellStyle(CellStyle::NONE),
                fg: None,
                bg: None,
                chars: "hello".to_string(),
            }],
        );
        assert_eq!(decode_datagram(&tag_encode(&diff)).unwrap(), diff);
    }

    #[test]
    fn round_trip_full_80_char_row() {
        let diff = make_diff(
            3,
            vec![DiffRun {
                row: 0,
                start_col: 0,
                style: CellStyle(CellStyle::NONE),
                fg: None,
                bg: None,
                chars: "a".repeat(80),
            }],
        );
        assert_eq!(decode_datagram(&tag_encode(&diff)).unwrap(), diff);
    }

    #[test]
    fn round_trip_multibyte_utf8_run() {
        // Mix of accented, CJK, and emoji characters.
        let diff = make_diff(
            4,
            vec![DiffRun {
                row: 3,
                start_col: 0,
                style: CellStyle(CellStyle::NONE),
                fg: None,
                bg: None,
                chars: "héllo wörld 你好".to_string(),
            }],
        );
        assert_eq!(decode_datagram(&tag_encode(&diff)).unwrap(), diff);
    }

    #[test]
    fn round_trip_styled_run() {
        let diff = make_diff(
            5,
            vec![DiffRun {
                row: 12,
                start_col: 40,
                style: CellStyle(CellStyle::BOLD | CellStyle::UNDERLINE),
                fg: Some(2),
                bg: Some(3),
                chars: "styled".to_string(),
            }],
        );
        assert_eq!(decode_datagram(&tag_encode(&diff)).unwrap(), diff);
    }

    // ── Task B: tag byte contract ─────────────────────────────────────────────

    #[test]
    fn tag_byte_is_0x01() {
        let diff = make_diff(42, vec![]);
        let payload = tag_encode(&diff);
        assert_eq!(
            payload[0], 0x01,
            "first byte of tag_encode output must be TAG_STATE_DIFF (0x01)"
        );
    }

    // ── Task B: decode negative tests ────────────────────────────────────────

    #[test]
    fn decode_empty_bytes_is_err() {
        assert!(decode_datagram(&[]).is_err(), "empty bytes must return Err");
    }

    #[test]
    fn decode_unknown_tag_is_err() {
        let payload = [0xFF, 0x01, 0x02];
        assert!(
            decode_datagram(&payload).is_err(),
            "unknown tag byte must return Err"
        );
    }

    #[test]
    fn decode_truncated_body_is_err() {
        let diff = make_diff(1, vec![DiffRun {
            row: 0, start_col: 0,
            style: CellStyle(CellStyle::NONE),
            fg: None, bg: None,
            chars: "test".to_string(),
        }]);
        let full = tag_encode(&diff);
        // Truncate to just tag + 1 corrupt byte.
        let truncated = &full[..2.min(full.len())];
        // The truncated payload may decode the tag ok but fail on the body.
        // For length-1 payload (tag only), body is empty — postcard should Err.
        let tag_only = &full[..1];
        assert!(
            decode_datagram(tag_only).is_err(),
            "tag-only (no body) must return Err"
        );
        // A 2-byte truncation with an invalid postcard body should also Err.
        let bad_body = [TAG_STATE_DIFF, 0xFF];
        assert!(
            decode_datagram(&bad_body).is_err(),
            "truncated/corrupt body must return Err"
        );
        // Ensure we don't just pass through on the truncated full payload either.
        if truncated.len() < full.len() {
            let result = decode_datagram(truncated);
            // Either it errors (truncated body) or it succeeds — but it must not panic.
            let _ = result;
        }
    }

    /// T-11-02 DoS guard: a payload whose decoded StateDiff has > MAX_RUNS runs
    /// must return Err(ProtoError), never panic, never over-allocate.
    #[test]
    fn decode_max_runs_guard() {
        let over_limit_runs: Vec<DiffRun> = (0..=MAX_RUNS as u16)
            .map(|i| DiffRun {
                row: i % 24,
                start_col: 0,
                style: CellStyle(CellStyle::NONE),
                fg: None,
                bg: None,
                chars: String::new(), // empty chars to keep payload small
            })
            .collect();
        let diff = StateDiff {
            epoch: 1,
            cols: 80,
            rows: 24,
            cursor: CursorPos { row: 0, col: 0 },
            runs: over_limit_runs,
        };
        let payload = tag_encode(&diff);
        let result = decode_datagram(&payload);
        assert!(
            result.is_err(),
            "StateDiff with > MAX_RUNS runs must return Err (T-11-02 DoS guard)"
        );
    }

    // ── Task C: encode_datagram tests ─────────────────────────────────────────

    /// Size-cap test (D-11-01b): a full 80x24 repaint (24 full-row runs, cursor
    /// at row 0) must produce a payload strictly less than 1100 bytes, with at
    /// least some runs deferred.
    #[test]
    fn size_cap_full_80x24_repaint() {
        let runs: Vec<DiffRun> = (0u16..24)
            .map(|row| DiffRun {
                row,
                start_col: 0,
                style: CellStyle(CellStyle::NONE),
                fg: None,
                bg: None,
                chars: "a".repeat(80),
            })
            .collect();
        let diff = StateDiff {
            epoch: 1,
            cols: 80,
            rows: 24,
            cursor: CursorPos { row: 0, col: 0 },
            runs,
        };
        let (encoded, deferred) = encode_datagram(&diff, 1100).expect("encode_datagram");
        assert!(
            encoded.len() < 1100,
            "payload {} bytes must be strictly < 1100 (STRICT cap, D-11-01b)",
            encoded.len()
        );
        assert!(
            !deferred.is_empty(),
            "full 80x24 repaint must produce deferred runs (not all fit)"
        );
    }

    /// Cursor-priority test: with all 24 rows changed and the cursor at row 23,
    /// the decoded payload must include the run for row 23.
    #[test]
    fn cursor_priority_includes_cursor_row() {
        let rows: Vec<DiffRun> = (0u16..24)
            .map(|row| DiffRun {
                row,
                start_col: 0,
                style: CellStyle(CellStyle::NONE),
                fg: None,
                bg: None,
                chars: "x".repeat(80),
            })
            .collect();
        let diff = StateDiff {
            epoch: 2,
            cols: 80,
            rows: 24,
            cursor: CursorPos { row: 23, col: 0 },
            runs: rows,
        };
        let (encoded, _deferred) = encode_datagram(&diff, 1100).expect("encode_datagram");
        let decoded = decode_datagram(&encoded).expect("decode_datagram");
        assert!(
            decoded.runs.iter().any(|r| r.row == 23),
            "cursor row 23 must be included in the encoded payload (cursor-priority)"
        );
    }

    /// Heterogeneous continue-past-rejection guard (BLOCKER 1):
    /// One full 80-char row (large) at row 0 (cursor row), plus 23 single-char
    /// runs on rows 1–23. The cap is sized so the large row is **fully deferred**
    /// (even the split path cannot include any prefix), but the small single-char
    /// runs each fit individually after the large run is skipped.
    ///
    /// A break-on-first-rejection bug would stop after the large row is rejected
    /// and return a header-only payload (no "x" runs), causing this test to FAIL.
    /// The correct continue-past-rejection implementation keeps iterating and
    /// picks up the small single-char runs that fit after the large one is skipped.
    ///
    /// Sizing rationale (empirically verified — CR-02 fix):
    ///   Measured sizes at epoch=3, cols=80, rows=24, cursor=(0,0):
    ///     header-only body = 6 bytes (postcard varint encoding)
    ///     header + large run (80-char "a") = 92 bytes → large run contributes 86 bytes
    ///     header + small run (1-char "x") = 13 bytes → small run contributes 7 bytes
    ///     RUN_HEADER_OVERHEAD = 12 bytes (worst-case varint for all run fields)
    ///
    ///   cap=19 → body_cap=18:
    ///     large run whole-run check: 92 >= 18 → REJECTED
    ///     split path: remaining = body_cap - header_size - 12 = 18 - 6 - 12 = 0
    ///                 → remaining == 0 → large run FULLY DEFERRED (no split, no chars added)
    ///     small runs: 13 < 18 → ACCEPTED (fill loop continues past rejection)
    ///     second small run: 13 + 7 = 20 >= 18 → rejected; only 1 "x" run fits per packet
    ///
    ///   Under break-on-first-rejection (injecting `break;` at rejection site):
    ///     large run fails → break → 0 "x" runs → test FAILS (correctly red)
    ///   Under correct continue-past-rejection:
    ///     large run fails → continue → 1 "x" run accepted → test PASSES (correctly green)
    #[test]
    fn heterogeneous_continue_past_rejection() {
        // The large run (80 chars; measured header+run=92 bytes at these field values).
        let large_run = DiffRun {
            row: 0,
            start_col: 0,
            style: CellStyle(CellStyle::NONE),
            fg: None,
            bg: None,
            chars: "a".repeat(80),
        };
        // 23 single-char runs on rows 1–23 (measured header+run=13 bytes each).
        let small_runs: Vec<DiffRun> = (1u16..24)
            .map(|row| DiffRun {
                row,
                start_col: 0,
                style: CellStyle(CellStyle::NONE),
                fg: None,
                bg: None,
                chars: "x".to_string(),
            })
            .collect();

        let mut all_runs = vec![large_run];
        all_runs.extend_from_slice(&small_runs);

        let diff = StateDiff {
            epoch: 3,
            cols: 80,
            rows: 24,
            cursor: CursorPos { row: 0, col: 0 }, // cursor at row 0 = large row sorts first
            runs: all_runs,
        };

        // cap=19 (body_cap=18): large run body 92 >> 18 → whole-run rejected AND
        // split cannot fire (remaining = 18-6-12 = 0 → no budget for split prefix).
        // Large run is fully deferred; continue → small "x" runs (size 13 < 18) are accepted.
        // Break-on-first-rejection returns 0 "x" runs → test FAILS (correctly red under bug).
        // Correct continue implementation returns at least 1 "x" run → test PASSES.
        let cap = 19;
        let (encoded, deferred) = encode_datagram(&diff, cap).expect("encode_datagram");

        // Confirm the payload respects the cap.
        assert!(
            encoded.len() < cap,
            "payload {} must be < cap {}",
            encoded.len(),
            cap
        );
        // Confirm the large run was fully deferred (not even partially split).
        assert!(
            deferred.iter().any(|r| r.chars.len() > 1),
            "large 80-char run must be in deferred list (not encoded) at cap=19"
        );

        // Decode and confirm at least one single-char "x" run made it in.
        // (This is the continue-past-rejection guard — fails under break-on-first-rejection.)
        let decoded = decode_datagram(&encoded).expect("decode_datagram");
        let has_small_run = decoded.runs.iter().any(|r| r.chars == "x");
        assert!(
            has_small_run,
            "decoded payload must contain at least one single-char 'x' run — \
             fill loop must CONTINUE past rejected oversize run, not break. \
             decoded.runs = {:?}, deferred.len() = {}",
            decoded.runs,
            deferred.len()
        );
        // Also assert the large run was not included in the payload (it was deferred entirely).
        let has_large_run = decoded.runs.iter().any(|r| r.chars.len() > 1);
        assert!(
            !has_large_run,
            "large 80-char run must not appear in encoded payload at cap=19. \
             decoded.runs = {:?}",
            decoded.runs
        );
    }

    /// Single-cell change: no runs should be deferred, and the payload must
    /// be well under 1100 bytes.
    #[test]
    fn single_cell_change_no_deferred() {
        let diff = StateDiff {
            epoch: 5,
            cols: 80,
            rows: 24,
            cursor: CursorPos { row: 12, col: 40 },
            runs: vec![DiffRun {
                row: 12,
                start_col: 40,
                style: CellStyle(CellStyle::NONE),
                fg: None,
                bg: None,
                chars: "x".to_string(),
            }],
        };
        let (encoded, deferred) = encode_datagram(&diff, 1100).expect("encode_datagram");
        assert!(
            deferred.is_empty(),
            "single-cell change must have no deferred runs"
        );
        assert!(
            encoded.len() < 1100,
            "single-cell payload must be < 1100 bytes, got {}",
            encoded.len()
        );
    }

    /// Regression: Task B round-trip tests still pass after Task C adds
    /// encode_datagram. Verifies encode_datagram output round-trips through
    /// decode_datagram.
    #[test]
    fn encode_decode_round_trip_via_encode_datagram() {
        let diff = StateDiff {
            epoch: 42,
            cols: 80,
            rows: 24,
            cursor: CursorPos { row: 12, col: 40 },
            runs: vec![DiffRun {
                row: 12,
                start_col: 40,
                style: CellStyle(CellStyle::BOLD | CellStyle::UNDERLINE),
                fg: Some(2),
                bg: None,
                chars: "hello".to_string(),
            }],
        };
        let (encoded, deferred) = encode_datagram(&diff, 1100).expect("encode_datagram");
        assert!(deferred.is_empty());
        let decoded = decode_datagram(&encoded).expect("decode_datagram");
        assert_eq!(diff, decoded);
    }

    // ── Task 1 (Phase 13): epoch-ack wire format tests ────────────────────────

    /// encode_epoch_ack then decode_epoch_ack round-trips for epoch=1.
    #[test]
    fn epoch_ack_roundtrip() {
        let payload = encode_epoch_ack(1);
        let epoch = decode_epoch_ack(&payload).expect("decode_epoch_ack must succeed for epoch=1");
        assert_eq!(epoch, 1, "epoch round-trip failed");
    }

    /// encode_epoch_ack / decode_epoch_ack round-trips for edge-case epochs: 0 and u64::MAX.
    #[test]
    fn epoch_ack_roundtrip_extremes() {
        for &n in &[0u64, u64::MAX] {
            let payload = encode_epoch_ack(n);
            let epoch = decode_epoch_ack(&payload)
                .unwrap_or_else(|_| panic!("decode_epoch_ack must succeed for epoch={n}"));
            assert_eq!(epoch, n, "epoch round-trip failed for n={n}");
        }
    }

    /// decode_epoch_ack must return Err when the first byte is TAG_STATE_DIFF (0x01).
    /// This is the security-relevant guard: a misrouted StateDiff must NOT be read as an epoch-ack.
    #[test]
    fn decode_epoch_ack_rejects_state_diff_tag() {
        // Build a valid-looking payload with the StateDiff tag (0x01).
        let diff = make_diff(42, vec![]);
        let state_diff_payload = tag_encode(&diff);
        // First byte is 0x01 — decode_epoch_ack must reject it.
        let result = decode_epoch_ack(&state_diff_payload);
        assert!(
            result.is_err(),
            "decode_epoch_ack must reject TAG_STATE_DIFF (0x01) tag — got Ok({:?})",
            result.ok()
        );
    }

    /// decode_epoch_ack on an empty slice must return Err (no panic / no index-out-of-bounds).
    #[test]
    fn decode_epoch_ack_rejects_empty() {
        let result = decode_epoch_ack(&[]);
        assert!(result.is_err(), "decode_epoch_ack must return Err on empty slice");
    }

    /// decode_epoch_ack on a payload with the correct tag 0x02 but a truncated/garbage
    /// body must return Err (no panic).
    #[test]
    fn decode_epoch_ack_rejects_bad_body() {
        // Correct tag byte but a 10-byte-all-0x80 body — postcard reads these as
        // continuation bits with no final byte, so it returns DeserializeUnexpectedEnd.
        let bad_payload = [0x02u8, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80];
        let result = decode_epoch_ack(&bad_payload);
        assert!(result.is_err(), "decode_epoch_ack must return Err on truncated/garbage body");

        // Correct tag but empty body (tag-only).
        let tag_only = [0x02u8];
        let result2 = decode_epoch_ack(&tag_only);
        assert!(result2.is_err(), "decode_epoch_ack must return Err on tag-only (no body) payload");
    }

    /// The first byte of encode_epoch_ack(N) must be exactly 0x02 (TAG_CLIENT_EPOCH).
    #[test]
    fn encode_epoch_ack_first_byte_is_tag() {
        let payload = encode_epoch_ack(42);
        assert_eq!(
            payload[0], 0x02,
            "first byte of encode_epoch_ack output must be TAG_CLIENT_EPOCH (0x02)"
        );
    }
}
