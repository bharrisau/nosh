# Technology Stack — nosh v1.3 (M5) Additions

**One-line summary:** No new transport crates are needed; M5 adds native QUIC multistream over the existing quinn 0.11.9 connection, extends `nosh-proto` with a channel control sub-protocol and scrollback wire types, replaces the terminal model's no-op alt-screen flag with a real second grid, and adds `unicode-segmentation` for grapheme-cluster correctness alongside the already-present `unicode-width`.

**Researched:** 2026-06-07
**Confidence:** HIGH for quinn API surface (verified against docs.rs 0.11.9); HIGH for unicode crates (verified); MEDIUM for termwiz internal alt-screen (verified API, no dual-buffer shortcut found); HIGH for scrollback wire design (derived from existing proto constraints).

---

## Recommended Additions

| Crate | Version | Purpose | Why |
|-------|---------|---------|-----|
| `unicode-segmentation` | 1.12.0 | Grapheme-cluster iteration and cluster boundary detection | Required to correctly split wide/emoji/combining-mark sequences into logical cells. `unicode-width 0.2` reports column width per char/str but does not split strings into grapheme clusters; you need both: segment with `unicode-segmentation`, then width-per-cluster with `unicode-width` or `termwiz::cell::grapheme_column_width`. |
| *(no new crates)* | — | Channel multiplexing | Quinn 0.11.9 already exposes `open_bi`/`accept_bi`/`open_uni`/`accept_uni` and the full stream flow-control surface. No framing library is needed; the existing `postcard` codec already frames messages on streams. |
| *(no new crates)* | — | Scrollback sync wire format | `postcard` + the existing `nosh-proto` message enum handle this. New `Message` variants appended in discriminant order. |
| *(no new crates)* | — | Alternate-screen buffer | Built from a second `Vec<Vec<Cell>>` (the same `Cell` type already in `nosh-server::terminal`) — no new crate needed. `termwiz::surface::Surface` is not a reusable alternate-screen abstraction (see below). |

**Version changes from current workspace pins:**

| Crate | Current pin | Recommended | Reason |
|-------|------------|-------------|--------|
| `unicode-width` | `"0.2"` (already in nosh-client) | No change needed | 0.2.2 is current; newline-width change in 0.2.0 is irrelevant for our terminal cell use (we never pass raw newlines to width functions) |
| `vte` | `"0.15"` | No change | 0.15.0 is current; alt-screen handling added inside `TerminalState`, not in `vte` itself |
| `termwiz` | Not currently in workspace | **Do not add** — see below | termwiz 0.23.3 is current but its `Surface` does not give us a ready-made two-buffer alt-screen abstraction (verified) |

---

## quinn 0.11 Multistream and Flow Control

### The core question: new QUIC streams per channel vs. app-level framing inside one stream

**Verdict: use native QUIC streams (one stream per logical channel type), not a framing layer inside the existing single stream.**

Rationale:

- Quinn 0.11.9 streams are cheap to open. The docs state "streams are cheap and instantaneous to open unless blocked by flow control." Opening a new bidi stream for the scrollback channel does not require a round-trip — the STREAM frame arrives at the peer and implicitly creates the stream.
- The existing single-stream design has a migration/reattach invariant: on cold reattach the server replays `SequencedOutputBuffer` chunks over a fresh QUIC stream (the shell output stream). Adding a mux framing layer inside that one stream would couple reattach replay with the new channels' state, making replay harder to reason about. Separate streams let the reattach stream remain unchanged.
- The alternative (app-level framing inside one stream, SSH-connection-protocol-style with channel ids) gains nothing on quinn: QUIC already provides independent streams with no head-of-line blocking between them. Multiplexing on top of QUIC streams is strictly redundant — it is what Mosh's custom UDP protocol and SSH over TCP had to do precisely because they lacked native stream isolation.
- **The design penalty:** you do need a control protocol to coordinate channel opens (which stream ID carries which logical channel?). Quinn does not expose its internal QUIC stream ID numbering to the application in a stable way — streams are identified by the `SendStream`/`RecvStream` pair returned by `open_bi`/`accept_bi`, not by a raw numeric ID. This is why the brief's OPEN/ACCEPT/REJECT control-channel protocol exists: it runs on the designated control stream (logical channel 0, the first bidi stream opened after connection) and announces new logical channels by a role-specific tag before the peer binds the corresponding `accept_bi` stream.

### Confirmed quinn 0.11.9 API surface for multistream

**Opening streams (initiator side):**
```rust
// Returns (SendStream, RecvStream); awaiting completes when QUIC permits the stream.
let (send, recv) = conn.open_bi().await?;

// Unidirectional (scrollback push is server→client only: good candidate for open_uni)
let send = conn.open_uni().await?;
```

**Accepting streams (responder side):**
```rust
// In the server/client select! loop:
let (send, recv) = conn.accept_bi().await?;
let recv = conn.accept_uni().await?;
```

**Concurrent stream limits (set before or after connection):**
```rust
// TransportConfig (set at ServerConfig/ClientConfig build time):
transport.max_concurrent_bidi_streams(VarInt::from_u32(16));
transport.max_concurrent_uni_streams(VarInt::from_u32(8));

// Also adjustable live on a Connection (per-RFC 9000 credit updates):
conn.set_max_concurrent_bi_streams(VarInt::from_u32(16));
conn.set_max_concurrent_uni_streams(VarInt::from_u32(8));
```

**Per-stream flow-control window (TransportConfig, applies to all streams on the connection):**
```rust
// Maximum bytes the peer may send on any single stream before blocking.
transport.stream_receive_window(VarInt::from_u32(1_024 * 1_024)); // 1 MiB per stream

// Connection-wide aggregate receive window.
transport.receive_window(VarInt::from_u32(8 * 1_024 * 1_024)); // 8 MiB total

// Send window (local constraint on outgoing data buffering).
transport.send_window(4 * 1_024 * 1_024u64); // u64, not VarInt
```

**Stream priority** (useful for prioritising the shell output stream over scrollback):
```rust
send_stream.set_priority(priority: i32);
// Higher i32 → higher priority. Default is 0.
// Shell output stream: set_priority(10); scrollback stream: set_priority(0).
```

**Important:** there is no per-stream-instance `set_receive_window` at runtime — `stream_receive_window` in `TransportConfig` is a static configuration that governs ALL streams on ALL connections of that endpoint. The connection-level `set_receive_window` updates the connection-wide window. There is no API to assign a different QUIC-level receive window to individual streams after they are opened.

### Implication: app-level per-channel flow control IS needed for scrollback

Because QUIC stream-level flow control is configured uniformly per endpoint (not per stream instance), and the scrollback channel's back-pressure semantics are different from the shell output stream (the client can explicitly pause scrollback delivery, while shell output must never block), an application-level per-channel flow-control window is required for the scrollback channel.

Design: the scrollback channel carries a credit-based app protocol:
- Server sends scrollback lines only up to the client's advertised credit count.
- Client sends `ScrollbackCredit { lines: u32 }` control messages to grant more lines.
- This is independent of QUIC flow control (which remains the lower-level byte-level throttle preventing buffer exhaustion at the transport layer).

The shell output channel (the existing single bidi stream) does NOT need app-level flow control — its current design (PTY bytes pushed as fast as they arrive) is intentional and correct.

### Migration and reattach interaction

When the client cold-reattaches (new QUIC connection):
1. The shell output bidi stream is re-opened and replay begins as before (unchanged).
2. The control channel (channel 0) is re-opened; the client sends OPEN requests for any channels it wants resumed.
3. The scrollback channel is re-opened from the server at the client's last-seen scrollback position (client includes its highest-received scrollback sequence in the Reattach or a new control message).
4. The datagram channel (RFC 9221 StateDiff) requires no reattach — it is connectionless by design and resumes naturally.

QUIC connection migration (IP change without disconnect) requires no changes — all streams migrate transparently with the connection.

---

## Scrollback Storage and Sync

### Server-side storage

`TerminalState` already has a `scrollback: VecDeque<Vec<Cell>>` bounded at `SCROLLBACK_LINE_CAP` (10,000 lines). This is the authoritative storage; no new data structure is needed. What is needed:

- A monotonically increasing `scrollback_seq: u64` counter that increments each time a line is appended to scrollback. This is the sequence number the client uses to resume sync after reattach.
- A method `scrollback_since(seq: u64) -> Vec<(u64, &[Cell])>` that returns lines with sequence numbers ≥ `seq`.

Because `VecDeque` does not intrinsically carry per-line sequence numbers, the simplest correct approach is to track `oldest_scrollback_seq: u64` (the sequence of `scrollback[0]`) and compute `seq_of_index(i) = oldest_scrollback_seq + i`. On `scroll_up`, increment `oldest_scrollback_seq` if the deque was already at cap and the front was dropped.

### Wire format

New variants appended to the `Message` enum (preserving discriminant order — existing variants 0–10 are inviolable):

```rust
// Server → client: one or more scrollback lines (sent on the scrollback reliable stream).
ScrollbackLines {
    // Sequence number of the FIRST line in this batch (0-based, monotonically increasing).
    from_seq: u64,
    // Batch of lines. Each line is a Vec<ScrollbackCell>.
    lines: Vec<Vec<ScrollbackCell>>,
}

// Client → server: grant N more scrollback lines (app-level flow control credit).
ScrollbackCredit { lines: u32 }

// Client → server: on reattach, resume scrollback from this sequence number.
ScrollbackResume { last_seq: u64 }
```

`ScrollbackCell` reuses the existing field layout from `DiffRun` (same `fg: Option<u8>`, `bg: Option<u8>`, `style: CellStyle`) plus a `ch: String` (grapheme cluster, not `char`, to handle wide cells correctly). Postcard serialises this efficiently.

**Wire format approach: postcard on the scrollback reliable bidi stream.** This is the right choice because:
- Scrollback is reliable (ordering matters, no loss acceptable).
- It is already framed by the existing `write_message`/`read_message` codec (length-prefixed postcard).
- The scrollback stream is independent of the shell output stream — a slow client draining scrollback does not block shell output.
- No new codec is needed.

**Do not send scrollback as datagrams.** Scrollback is not loss-tolerant; a lost datagram means a missing line, which corrupts the scrollback history.

---

## Alternate-Screen Buffer and Unicode Width

### Does termwiz Surface give us an alternate-screen buffer we can reuse?

No. `termwiz::surface::Surface` is a compositing surface that tracks a change log for delta rendering. It does not model the dual-buffer (`?1049h`/`?1049l` save/restore/clear) semantics of a VT terminal. Its `draw_from_screen` saves and restores the cursor, but there is no `save_primary_and_switch_to_alternate()` / `restore_primary()` concept in its API.

The `termwiz::terminal` module (which DOES handle alt-screen in a full terminal emulator context) is part of the wezterm monorepo and not published with a stable public API — `termwiz 0.23.3` on crates.io does not expose a two-buffer terminal state you can drive from raw PTY bytes.

**Correct approach: implement a second grid directly in `TerminalState`.**

The `TerminalState` struct already holds:
- `grid: Vec<Vec<Cell>>` — the normal screen
- `scrollback: VecDeque<Vec<Cell>>` — scrollback history
- `cursor: CursorPos`, `sgr: SgrState`, `echo_state: EchoState`

Adding alternate-screen support requires:
```rust
// New fields in TerminalState:
alt_grid: Option<Vec<Vec<Cell>>>,        // None when alt screen inactive
saved_cursor: Option<CursorPos>,         // normal-screen cursor saved at ?1049h
saved_sgr: Option<SgrState>,             // optional: save SGR state too
```

On `?1049h` (enter alt screen):
1. Save current `cursor` → `saved_cursor`, current `sgr` → `saved_sgr`.
2. Move current `grid` → `alt_grid` (or create a fresh one — xterm creates a NEW cleared alt grid; saving the primary is done implicitly).
3. Replace `grid` with a fresh blank grid of the same dimensions.
4. Set `echo_state.alt_screen = true`.

On `?1049l` (leave alt screen):
1. Restore `grid` from `alt_grid` (or create blank if none was saved).
2. Restore `cursor` from `saved_cursor`.
3. Set `alt_grid = None`, `echo_state.alt_screen = false`.

Scrollback is suppressed while the alt screen is active (lines that scroll off the alt-screen viewport should NOT enter scrollback — this is the xterm/VT220 convention; vim's TUI depends on it).

### Confirmed: existing no-op location

In `crates/nosh-server/src/terminal.rs`, the `csi_dispatch` handler for `?1049h`/`?1049l` currently only sets `self.echo_state.alt_screen = enable` (confirmed at line ~503-504). The fix is entirely within this file; no other crates are affected.

### Unicode width and grapheme cluster correctness

**`unicode-width` 0.2.2** (already a dependency in `nosh-client`, version `"0.2"` in `Cargo.toml`) provides:
- `UnicodeWidthChar::width(c: char) -> Option<usize>` — column width of a single Unicode scalar value
- `UnicodeWidthStr::width(s: &str) -> usize` — sum of widths of all chars in a string
- Both traits expose `width_cjk()` variants for East Asian Ambiguous treatment

**`unicode-segmentation` 1.12.0** (NEW dependency) provides:
- `UnicodeSegmentation::graphemes(s: &str, is_extended: bool) -> Graphemes<'_>` — iterator over UAX#29 grapheme clusters
- `GraphemeCursor` for incremental segmentation at arbitrary byte offsets

**The correct two-step pattern for wide-character cell placement:**
```rust
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

for cluster in text.graphemes(true) {
    let width = UnicodeWidthStr::width(cluster); // 0=combining, 1=narrow, 2=wide
    match width {
        0 => { /* combining mark: attach to previous cell */ }
        1 => { print_single_width_cell(cluster); }
        2 => { print_wide_cell(cluster); /* place continuation cell in col+1 */ }
        _ => { /* emoji or other extended cluster: treat as width 1 or 2 */ }
    }
}
```

**`termwiz::cell::grapheme_column_width(s: &str, version: Option<UnicodeVersion>) -> usize`** is available in termwiz 0.23.3 and uses a widecharwidth-derived lookup table with emoji special-casing. It is more accurate than `unicode-width` for terminal rendering (handles emoji variation selectors, ZWJ sequences). **Recommended:** use `grapheme_column_width` from termwiz for the width calculation step (since termwiz is already a transitive dep via `portable-pty`), and `unicode-segmentation` for the cluster-splitting step.

**Cell model change for wide chars:** `Cell.ch` is currently `char`. For grapheme-cluster correctness it needs to become a `String` (to hold multi-codepoint clusters like emoji+variation_selector). This is a breaking change to the `Cell` type — flag it as a phase task. The `DiffRun.chars: String` in `nosh-proto` already uses the correct representation; aligning `Cell.ch` to `String` makes the server→proto diff path zero-copy.

**Add to `nosh-server/Cargo.toml`:**
```toml
unicode-segmentation = "1.12"
```

**Add to workspace `Cargo.toml` (shared dependency):**
```toml
unicode-segmentation = "1.12"
```

---

## What NOT to Add

| Avoid | Why | What to use instead |
|-------|-----|---------------------|
| App-level mux framing inside the existing single QUIC stream (SSH-connection-protocol-style) | Redundant with QUIC native streams; couples reattach replay with new channels; no HOL-blocking benefit | Native QUIC `open_bi`/`accept_bi` per logical channel type |
| `termwiz::surface::Surface` as an alt-screen buffer | It is a compositing/diff abstraction, not a dual-buffer VT state machine. No `?1049h`/`?1049l` semantics exist in its API | Second `Vec<Vec<Cell>>` field in `TerminalState` |
| `termwiz` as a new workspace dep | `portable-pty 0.9` already pulls it in as a transitive dep. Adding it explicitly creates a version-skew risk if `portable-pty` bumps its dep | Use via `termwiz::cell::grapheme_column_width` accessed through the existing transitive path, or duplicate the table inline |
| Sending scrollback as datagrams | Scrollback lines must be delivered reliably and in order; loss would corrupt history | Reliable bidi stream (same postcard codec as existing messages) |
| `grapheme-width-rs` (pascalkuthe/grapheme-width-rs) | Not published on crates.io as a stable crate (only found on GitHub); requires caller-specified Unicode capability level; adds operational complexity | `termwiz::cell::grapheme_column_width` (already in the transitive dep graph, same Unicode data approach) |
| A new QUIC library (`s2n-quic`, `quiche`) | No new capability needed; quinn 0.11.9 already supports everything M5 requires | No change to quinn version |
| `termwiz` 0.24.x | Not yet released as of research date (0.23.3 is current on crates.io); fork crate `tattoy-termwiz 0.24.0-fork` exists but is not suitable for production use | Stay on 0.23.3 |
| Per-stream QUIC window differentiation at runtime | Quinn 0.11.9 has no per-stream-instance `set_receive_window` — only a uniform `stream_receive_window` in `TransportConfig` | App-level credit protocol on the scrollback channel (see above) |

---

## Version Compatibility Notes

| Package | Version in workspace | Compatible with | Notes |
|---------|---------------------|-----------------|-------|
| `quinn` | 0.11.9 | `rustls` 0.23.x, `tokio` 1.x | No version change needed; `open_bi`/`accept_bi`/`open_uni`/`accept_uni` all verified present at 0.11.9 |
| `unicode-width` | `"0.2"` (in nosh-client) | All existing code | 0.2.2 is current; add to `nosh-server` and workspace if needed for server-side grapheme width |
| `unicode-segmentation` | Not yet in workspace | Compatible with all crates | 1.12.0 is current; no transitive conflicts expected |
| `vte` | `"0.15"` | No change | Alt-screen implemented inside `TerminalState::csi_dispatch`, not in vte |
| `termwiz` | 0.23.3 (transitive via `portable-pty 0.9`) | `portable-pty 0.9.0` | Do not pin explicitly unless you need `grapheme_column_width` directly — the transitive path is sufficient for ad-hoc use |
| `postcard` | `"1"` (workspace) | Existing wire format | New `Message` variants appended in discriminant order; existing clients will receive unknown discriminants as decode errors (expected for version mismatch) |

**The `Message` enum discriminant order invariant (CRITICAL):** postcard encodes enum variants by their 0-based index in source order. Adding new variants for scrollback control MUST append them after the current last variant (`TerminalControl` = discriminant 10). Never insert or reorder. Confirmed as an established invariant from the existing codebase (see messages.rs line 154-174 comment).

---

## Sources

| URL | What it verified |
|-----|-----------------|
| https://docs.rs/quinn/0.11.9/quinn/struct.Connection.html | Full `Connection` method list: `open_bi`, `open_uni`, `accept_bi`, `accept_uni`, `set_max_concurrent_bi_streams`, `set_max_concurrent_uni_streams`, `set_send_window`, `set_receive_window`, `send_datagram`, `read_datagram`, `max_datagram_size`, `rtt` — HIGH confidence |
| https://docs.rs/quinn/0.11.9/quinn/struct.TransportConfig.html | `stream_receive_window(VarInt)`, `receive_window(VarInt)`, `max_concurrent_bidi_streams(VarInt)`, `max_concurrent_uni_streams(VarInt)`, `send_window(u64)` — all methods confirmed with exact signatures — HIGH confidence |
| https://docs.rs/quinn/0.11.9/quinn/struct.SendStream.html | `write`, `write_all`, `set_priority(i32)`, `reset` — HIGH confidence |
| https://docs.rs/quinn/0.11.9/quinn/struct.RecvStream.html | `read`, `read_chunk`, `read_chunks`, `read_to_end` — HIGH confidence; no per-stream-instance receive-window setter found |
| https://docs.rs/termwiz/0.23.3/termwiz/surface/struct.Surface.html | Surface API: `new`, `add_change`, `add_changes`, `get_changes`, `draw_from_screen`, `diff_screens`, `screen_lines` — no dual-buffer alt-screen concept — HIGH confidence |
| https://docs.rs/termwiz/0.23.3/termwiz/cell/fn.grapheme_column_width.html | `grapheme_column_width(s: &str, version: Option<UnicodeVersion>) -> usize` — HIGH confidence |
| https://docs.rs/unicode-segmentation/latest/unicode_segmentation/ | Version 1.12.0 current; `graphemes(is_extended: bool)`, `GraphemeCursor` — HIGH confidence |
| https://docs.rs/crate/unicode-width/latest | Version 0.2.2 current; `UnicodeWidthChar::width`, `UnicodeWidthStr::width`, `width_cjk` variants; 0.2.0 change: newlines now width 1 (irrelevant for our use) — HIGH confidence |
| https://quinn-rs.github.io/quinn/quinn/data-transfer.html | Confirmed streams are cheap and instantaneous to open; `open_bi().await` / `accept_bi().await` pattern — MEDIUM confidence (official but not versioned) |
| https://github.com/libp2p/rust-libp2p/discussions/5799 | Real-world `stream_receive_window` tuning discussion; confirms per-stream uniform configuration — MEDIUM confidence |
| WebSearch for grapheme-width-rs | Not published on crates.io as stable; GitHub only — LOW confidence on publishability, MEDIUM on crate existence |
| Codebase reading: `crates/nosh-server/src/terminal.rs` | Confirmed alt-screen is a no-op flag at `csi_dispatch` line ~503; `Cell.ch: char` confirmed; `scrollback: VecDeque<Vec<Cell>>` already exists with SCROLLBACK_LINE_CAP=10_000 — HIGH confidence (direct source) |
| Codebase reading: `crates/nosh-proto/src/messages.rs` | Confirmed discriminant append-only invariant; current last variant is `TerminalControl` (discriminant 10) — HIGH confidence (direct source) |
