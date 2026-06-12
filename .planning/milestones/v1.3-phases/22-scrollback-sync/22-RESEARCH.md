# Phase 22: Scrollback Sync - Research

**Researched:** 2026-06-12
**Domain:** Rust async — QUIC channel multiplexing, reliable-stream scrollback delivery, CSI escape parsing, epoch-gated live-grid handoff
**Confidence:** HIGH

---

<user_constraints>
## User Constraints (from CONTEXT.md)

### Locked Decisions

- Client-driven pull protocol: `ScrollbackRequest { channel_id, from_line, count }` on the Scrollback channel; server replies with `ScrollbackPage` frames. No server-push bulk dump.
- `ScrollbackPage` carries `epoch_at_snapshot` (LOCKED), the page's line range, the total available count, and per-line content. The client gates its transition back to the live datagram grid on receiving a datagram with `epoch >= epoch_at_snapshot`.
- `epoch_at_snapshot` is read from `current_epoch` under the same `terminal_state` mutex acquisition that reads the scrollback lines — no torn read.
- New `Message` variants appended after `ChannelClose` (discriminant 14), preserving the postcard discriminant order (MUX-06 invariant). Discriminant-stability assertion covers the new variants.
- The 256 KiB per-channel byte-credit window from Phase 21 is reused for the Scrollback channel; `ScrollbackCredit` is the channel-typed credit grant (byte-granular).
- Default page size: 256 lines per `ScrollbackRequest`.
- Server-side scrollback sender runs as a dedicated `tokio::spawn` task (the channel task model from `nosh-server/src/channel.rs`), fed by a bounded `mpsc::channel` from the pump. If the channel is full, the pump drops the oldest queued lines rather than blocking (LOCKED).
- Pending-send buffer cap: 512 lines.
- Reliable-only enforced at the type level (LOCKED): the scrollback sender's function signature accepts a `quinn::SendStream` only.
- The `!alt_screen` gate on `scroll_up()` already exists at `terminal.rs` ~line 631 and the `resize()` path ~line 489; this phase verifies it, not re-implements it.
- Server reads scrollback through a `TerminalState` accessor (e.g. `scrollback_lines(from, count)`), never by exposing the `VecDeque` directly.
- Keybindings: Shift-PageUp enters scrollback view and pages up; Shift-PageDown pages down (LOCKED). Detected as raw CSI sequences `ESC [ 5 ; 2 ~` / `ESC [ 6 ; 2 ~` in the stdin escape machine.
- Any non-paging keystroke while in scrollback view immediately snaps back to the live viewport AND is delivered to the shell (LOCKED, keystroke is not swallowed). Reaching the live view (paging down past line 0) auto-exits scrollback mode (LOCKED).
- Client scrollback rendering model: a client-side scroll offset over a locally retained line buffer; live datagram application is suspended for display only while in scrollback; the connection keeps acking epochs so the server pump never stalls.
- Client prefetch is lazy/on-demand; paging up near the top of what is held triggers a new `ScrollbackRequest`.
- Migration is transparent (channel survives via connection IDs, no application action).
- Cold reattach: client re-opens the Scrollback channel after `ResumeComplete` (LOCKED); channel state is never byte-replayed; `TerminalState.scrollback` survives in the `SessionSlot`.
- `SCROLLBACK_LINE_CAP = 10_000` must not be raised.

### Claude's Discretion

- Exact default page size (256 lines) and pending-send buffer cap (512 lines) — tune during planning/implementation against the credit window.
- Exact new `Message`/wire variant names and field layouts for `ScrollbackRequest` / `ScrollbackPage` / `ScrollbackCredit` — append-only discriminants, `epoch_at_snapshot` present, byte-granular credit.
- Line-width / reflow handling (S-3): recommended default is to send each scrollback line with its original column width as per-line metadata; no server-side reflow.
- Whether `ScrollbackRequest` travels on the channel's own data stream or the control stream — provided it does not reintroduce the M-2 control/data flow-control deadlock.

### Deferred Ideas (OUT OF SCOPE)

- Scrollback search and copy-mode / text selection — v1.3 scrollback is view + page only.
- Server-side scrollback reflow to current width — later enhancement.
- Raising `SCROLLBACK_LINE_CAP` above 10,000.
- `tracing::warn!` when `scrollback.len()` nears the cap — can fold in here or defer.
</user_constraints>

<phase_requirements>
## Phase Requirements

| ID | Description | Research Support |
|----|-------------|------------------|
| SCROLL-01 | Client can view terminal history that has scrolled off the visible grid — server retains scrollback and serves requested lines to the client | `TerminalState.scrollback: VecDeque<Vec<Cell>>` already exists and is populated; `ChannelType::Scrollback` already declared in proto; `open_channel()` helper already exists in `client.rs` |
| SCROLL-02 | Scrollback is delivered over a reliable channel (never datagrams), paged with credit-based flow control — no bulk dump, no PTY stall while history is fetched | Server channel task model in `channel.rs` is the template; bounded mpsc + drop-oldest pattern is the M-6 fix; `ChannelCredit` / `INITIAL_CREDIT` = 256 KiB already exist |
| SCROLL-03 | Alt-screen content never enters primary scrollback — `scroll_up()` gated on `!alt_screen` | Gate already implemented at `terminal.rs:631` and `terminal.rs:489`; this phase adds the verification unit test |
| SCROLL-04 | Client enters scrollback view with Shift-PageUp / Shift-PageDown; any keystroke snaps back to live and is sent to shell | Raw CSI sequences `ESC[5;2~` / `ESC[6;2~` must be intercepted in `run_pump`'s stdin arm (currently the `EscapeState` machine passes all bytes through); client needs a scrollback-view state machine |
| SCROLL-05 | Scrollback↔live-grid handoff contains no gap or duplicate lines; `epoch_at_snapshot` framing; scrollback viewable after cold reattach | `epoch_snapshots` VecDeque in `server.rs` is the precedent; `SessionSlot.terminal_state` (Mutex<TerminalState>) survives orphan window; reattach re-open path mirrors Phase 21 echo-channel reattach test |
</phase_requirements>

---

## Summary

Phase 22 adds the first real consumer of the Phase 21 mux layer. All foundational machinery is in place and verified: `ChannelType::Scrollback` is declared in `nosh-proto/src/messages.rs`; `TerminalState.scrollback` is a `VecDeque<Vec<Cell>>` capped at `SCROLLBACK_LINE_CAP = 10_000` in `nosh-server/src/terminal.rs`; the `!alt_screen` gate is implemented in `scroll_up()` at line 631 and in `resize()` at line 489; `current_epoch` and `epoch_snapshots` are the precedent for `epoch_at_snapshot`; and `open_channel()` / `await_channel_accept()` in `client.rs` give the client side its channel-open primitive. The server currently rejects `ChannelType::Scrollback` with a logged no-op at `server.rs:1060–1067` (fresh sessions) and `server.rs:1819` (reattach sessions).

The implementation has five concrete integration surfaces: (1) three new `Message` variants appended after `ChannelClose` (discriminant 14); (2) a `scrollback_lines(from, count)` read accessor on `TerminalState` that never exposes the `VecDeque` directly; (3) a server scrollback sender task (mirroring `run_channel_task` in `channel.rs`) wired to the pump via a bounded mpsc, with `ChannelType::Scrollback` accepted instead of rejected; (4) a client scrollback view state machine in `run_pump` intercepting raw CSI `ESC[5;2~` / `ESC[6;2~` before the existing escape machine; and (5) a post-`ResumeComplete` channel re-open call in `reattach_session`.

The highest-risk area is the `epoch_at_snapshot` handoff (S-5 / SCROLL-05): the server must snapshot the epoch and scrollback lines atomically under the same `terminal_state` mutex acquisition so no tearing occurs between what the scrollback reports and what the datagram stream is doing.

**Primary recommendation:** Follow the seven-step implementation order from CONTEXT.md specifics — wire variants first, then the `TerminalState` accessor + alt-screen gate test, then the server sender task, then the client view and escape handling, then the epoch handoff test, then the reattach test, then the lossy in-order test.

---

## Architectural Responsibility Map

| Capability | Primary Tier | Secondary Tier | Rationale |
|------------|-------------|----------------|-----------|
| Scrollback storage and read accessor | Server (`terminal.rs`) | — | `TerminalState.scrollback` is the authoritative source; read-only accessor enforces cap and alt-screen invariant inside the module |
| `epoch_at_snapshot` sourcing | Server (`server.rs` pump) | — | `current_epoch` lives in the pump; must be read under the same `terminal_state` lock acquisition as the scrollback snapshot |
| Scrollback sender task (bounded mpsc, drop-oldest) | Server (`channel.rs`) | Server pump (`server.rs`) | Channel task model is established; the pump feeds the mpsc and dispatches `ChannelEvent::Stream` on `accept_bi`; task writes `SendStream` exclusively |
| Wire protocol variants (`ScrollbackRequest`, `ScrollbackPage`, `ScrollbackCredit`) | Protocol (`nosh-proto`) | — | Append-only to `messages.rs` after discriminant 14; covered by the discriminant-stability test |
| Client scrollback view state machine | Client (`main.rs` `run_pump`) | — | stdin arm in `run_pump` is where raw CSI bytes are first seen; the view offset + local line buffer lives here |
| CSI escape detection (`ESC[5;2~` / `ESC[6;2~`) | Client (`main.rs` `run_pump`) | — | Raw bytes are read in the `stdin` arm before the `EscapeState` machine; Shift-PageUp/Down must be intercepted at this layer |
| Scrollback channel task (client drain) | Client (`channel.rs`) | — | `run_channel_task` pattern already established; the scrollback client task delivers received page bytes to the view buffer |
| Cold reattach re-open | Client (`main.rs` `reattach_session`) | — | After `ResumeComplete` (i.e. after `await_reattach_reply` returns `ReattachOutcome::Ok`), client calls `open_channel` for `ChannelType::Scrollback` |
| Flow control (credit grant) | Client (`channel.rs`) | Server pump | Client drain task sends `ChannelCredit` via `control_tx`; server pump forwards to channel task via `ChannelEvent::Credit` |

---

## Standard Stack

### Core (all already in workspace — no new dependencies)

| Library | Version | Purpose | Why Standard |
|---------|---------|---------|--------------|
| `postcard` | (workspace) | Append-only `Message` enum encoding for new wire variants | Required for existing `Message` round-trips; discriminant stability already tested |
| `tokio::sync::mpsc` | (tokio workspace) | Bounded mpsc between pump and scrollback sender task | Established channel task pattern; `bounded(N)` + `try_send` for drop-oldest semantics |
| `quinn::SendStream` / `quinn::RecvStream` | (workspace) | Reliable stream for scrollback delivery | Type-level enforcement that scrollback is never sent over datagrams |
| `serde` / `Serialize` / `Deserialize` | (workspace) | New wire type encoding | All existing `Message` variants already use serde + postcard |

No new crate dependencies. All required types are already in the workspace.

### Package Legitimacy Audit

> No new packages are installed in this phase. All code uses existing workspace crates.

N/A — this phase adds no external dependencies.

---

## Architecture Patterns

### System Architecture Diagram

```
PTY output bytes
      |
      v
TerminalState::advance()
      |
   scroll_up()  [!alt_screen gate at terminal.rs:631]
      |
  scrollback: VecDeque<Vec<Cell>>
      |
      +-------> scrollback_lines(from, count) [new accessor]
      |                |
      |                v
      |         pump reads scrollback + current_epoch
      |         under terminal_state mutex (no torn read)
      |                |
      |         ScrollbackPage { epoch_at_snapshot, lines }
      |                |
      |         send to bounded mpsc (512-line cap)
      |         [pump drops oldest if full — M-6 fix]
      |                |
      |         scrollback sender task (separate tokio::spawn)
      |         [accepts only quinn::SendStream]
      |                |
      |         encode ScrollbackPage frames
      |         write to channel SendStream
      |         [paused when remaining_credit == 0]
      |
      v
  Client RecvStream (scrollback channel task)
      |
  decode ScrollbackPage
      |
  local line buffer (Vec<Vec<Cell>> in view state)
      |
      +-- scrollback view state machine (in run_pump)
      |         |
      |   ESC[5;2~ (Shift-PageUp): enter / page up
      |   ESC[6;2~ (Shift-PageDown): page down / exit
      |   any other key: snap to live + forward to shell
      |
      |   Display path: render from local buffer + scroll offset
      |   (live datagram apply suspended for DISPLAY only;
      |    epoch acks still sent so server pump never stalls)
      |
      v
  live datagram (epoch >= epoch_at_snapshot) → exit scrollback mode
```

### Recommended Project Structure

No structural changes to the crate layout. Changes are in-file additions to existing modules:

```
crates/
├── nosh-proto/src/
│   └── messages.rs          # Append ScrollbackRequest / ScrollbackPage / ScrollbackCredit after ChannelClose (discriminant 14)
│   └── codec.rs             # Update message_discriminant_order_is_stable test to add new variants
├── nosh-server/src/
│   ├── terminal.rs          # Add scrollback_lines(from, count) -> Vec<Vec<Cell>> accessor
│   └── channel.rs           # Add run_scrollback_sender_task() (mirrors run_echo_loop shape)
│   └── server.rs            # Accept ChannelType::Scrollback (both run_session + run_reattach_session);
│                            # spawn scrollback sender task with bounded mpsc; feed mpsc from pump
├── nosh-client/src/
│   ├── channel.rs           # Add scrollback client drain task (delivers pages to view buffer)
│   └── main.rs              # run_pump: intercept ESC[5;2~ / ESC[6;2~; scrollback view state machine;
│                            # reattach_session: re-open Scrollback channel after ResumeComplete
```

### Pattern 1: New Wire Variants — Append After `ChannelClose` (discriminant 14)

**What:** Three new `Message` variants appended after `ChannelClose` in `nosh-proto/src/messages.rs`.

**When to use:** Any time a new mux protocol message is needed — always append, never insert.

**Example:**
```rust
// Source: crates/nosh-proto/src/messages.rs (append after ChannelClose, discriminant 14)
// APPEND-ONLY from here — Phase 22 scrollback variants.

/// Client → server: request a page of scrollback lines (SCROLL-01).
/// Travels on the scrollback channel's data stream (not the control stream).
ScrollbackRequest {
    channel_id: u32,
    /// Index of the first line to fetch, counting from the newest line backwards.
    /// 0 = line just above the live viewport.
    from_line: u64,
    /// Number of lines requested.
    count: u32,
},

/// Server → client: a page of scrollback lines (SCROLL-01 / S-5).
/// Travels on the scrollback channel's SendStream (reliable, never datagrams).
ScrollbackPage {
    channel_id: u32,
    /// The index of the first line in this page (same coordinate as from_line).
    from_line: u64,
    /// How many total lines are available in the server's scrollback buffer at
    /// the time of this snapshot (so the client knows when it has hit the top).
    total_available: u64,
    /// The datagram epoch at the moment this page was snapshotted (LOCKED — S-5).
    /// Client applies history up to (not including) this epoch, then waits for
    /// a live datagram with epoch >= epoch_at_snapshot before resuming live grid.
    epoch_at_snapshot: u64,
    /// Per-line content. Each line is the Vec<Cell> serialised as char/style tuples.
    lines: Vec<ScrollbackLine>,
},

/// Either direction: grants additional byte-credit on the scrollback channel
/// (SCROLL-02 / MUX-03). Byte-granular, consistent with ChannelCredit.
ScrollbackCredit {
    channel_id: u32,
    bytes: u64,
},
```

**Discriminant-stability test update** (must be the first commit of this phase, extending the Phase 21 test):
```rust
// Source: crates/nosh-proto/src/codec.rs (append to message_discriminant_order_is_stable)
// Phase 22 scrollback variants — discriminants 15–17:
(15, Message::ScrollbackRequest { channel_id: 2, from_line: 0, count: 256 }),
(16, Message::ScrollbackPage {
    channel_id: 2, from_line: 0, total_available: 100,
    epoch_at_snapshot: 42, lines: vec![] }),
(17, Message::ScrollbackCredit { channel_id: 2, bytes: 256 * 1024 }),
```

### Pattern 2: `TerminalState` Read Accessor

**What:** Add `scrollback_lines(from: u64, count: usize) -> (Vec<Vec<Cell>>, u64, u64)` to `TerminalState`, returning `(lines, total_len, epoch_at_snapshot)`. The third field is not the epoch — the epoch must be read externally by the pump under the same lock. The accessor's job is only to return a contiguous slice of the `VecDeque`.

**Key insight from source reading:** The `scrollback` field is a `VecDeque<Vec<Cell>>` indexed from oldest (front = `[0]`) to newest (back). The CONTEXT.md coordinate system is "indexed from the newest scrollback line backwards, 0 = line just above the live viewport." So `from_line = 0, count = N` means the N most-recent lines: indices `[scrollback.len() - N .. scrollback.len()]`.

**When to use:** Any server code that reads scrollback. Never expose the `VecDeque` or `Vec<Cell>` rows directly outside `terminal.rs`.

**Example:**
```rust
// Source: crates/nosh-server/src/terminal.rs (new public method)
/// Return a page of scrollback lines.
///
/// `from_line` is the line index from the newest line backwards (0 = most
/// recent). `count` is the number of lines requested.
///
/// Returns `(lines, total_available)`:
/// - `lines`: the requested page (may be shorter than `count` at the top).
/// - `total_available`: the total number of lines currently in scrollback.
///
/// The epoch must be captured by the CALLER under the same lock acquisition
/// that calls this method — `epoch_at_snapshot` belongs in the caller, not here.
pub fn scrollback_lines(&self, from_line: u64, count: usize) -> (Vec<Vec<Cell>>, u64) {
    let total = self.scrollback.len() as u64;
    if from_line >= total || count == 0 {
        return (vec![], total);
    }
    // Newest line is at index total-1; from_line=0 is index total-1.
    let newest_idx = (total - 1 - from_line) as usize;
    let oldest_idx = newest_idx.saturating_sub(count - 1);
    let lines: Vec<Vec<Cell>> = (oldest_idx..=newest_idx)
        .map(|i| self.scrollback[i].clone())
        .collect();
    (lines, total)
}
```

### Pattern 3: Scrollback Sender Task (Server Side)

**What:** A new variant of `run_channel_task_inner` for `ChannelType::Scrollback`. The task owns a `quinn::SendStream` only — it never touches `send_datagram`.

**Key integration point:** The pump feeds a bounded `mpsc::channel::<ScrollbackSnapshot>(512)` where `ScrollbackSnapshot` carries `(from_line, lines, epoch_at_snapshot, total_available)`. The pump sends to this channel via `try_send`; on `Err(Full)`, the oldest item is dropped (the bounded channel is created with `bounded(512)`, and the pump uses `try_send` — if full, it pops the front via a secondary `VecDeque` used as a queue, or simply logs and discards).

**Alternative simpler approach:** Rather than a pre-loaded mpsc, the scrollback sender task itself can request lines by querying the `SessionSlot.terminal_state` directly when a `ScrollbackRequest` arrives. This avoids the "drop oldest" complexity entirely and naturally reads the latest scrollback state at request time. The mpsc is only needed if the pump needs to push lines proactively. Given that the protocol is client-driven pull, the sender task can:

1. Wait for a `ScrollbackRequest` on the channel's RecvStream.
2. Lock `slot.terminal_state`, call `scrollback_lines(from, count)`, capture `current_epoch` from the pump via an `Arc<AtomicU64>` or a dedicated mpsc response, then release the lock.
3. Encode and write `ScrollbackPage` over `ch_send`.
4. Repeat.

The "bounded mpsc with drop-oldest" pattern from CONTEXT.md applies to proactive push; for pull it applies to the pending-request queue if multiple requests arrive before the sender can process them. The simplest correct approach for the locked decision is: the sender task holds a `Receiver<(from_line, count)>` from the pump (bounded 16 requests), processes them sequentially, reads from `SessionSlot` directly when processing each request. If the request queue fills (client sending requests faster than the server can respond), oldest requests are dropped by using `try_send` in the pump.

```rust
// Source: crates/nosh-server/src/channel.rs (new function, production-gated)
// Not cfg(test) — this is a production channel.
async fn run_scrollback_sender_task(
    channel_id: u32,
    slot: Arc<crate::registry::SessionSlot>,
    ch_send: &mut quinn::SendStream,
    ch_recv: &mut quinn::RecvStream,
    events: &mut mpsc::Receiver<ChannelEvent>,
    control_tx: &mpsc::Sender<Message>,
) {
    let mut remaining_credit: u64 = INITIAL_CREDIT;
    // ... loop: read ScrollbackRequest from ch_recv, lock terminal_state,
    // snapshot lines + epoch_at_snapshot, encode ScrollbackPage,
    // write to ch_send while pausing on credit == 0.
}
```

**Pump changes:** In `server.rs`, the `ChannelType::Scrollback` arm in `run_session` changes from `accept = false` to `accept = true`. After `ChannelAccept`, the task is spawned with access to `slot.clone()` in addition to the existing `channel_id`, `task_rx`, and `channel_ctrl_tx.clone()`. The server needs to pass `slot` to the scrollback task — this means the scrollback channel task function signature differs from the generic echo task.

**The cleanest approach:** Add a new variant to `ChannelEvent`:
```rust
pub enum ChannelEvent {
    Stream(quinn::SendStream, quinn::RecvStream),
    Credit(u64),
    Close,
    // Phase 22: pass the session slot to the scrollback task on startup.
    // Only meaningful for ChannelType::Scrollback.
    // (Alternative: add slot as a parameter to run_scrollback_sender_task directly.)
}
```

Or, more directly: spawn a separate `run_scrollback_channel_task` function that takes `slot` as an additional argument and does NOT go through the generic `run_channel_task`.

### Pattern 4: Client Scrollback View State Machine

**What:** A `ScrollbackViewState` enum in `run_pump` (or a small struct) that tracks whether the client is in live mode or scrollback mode.

**Key integration point:** In `run_pump`'s stdin arm, the raw bytes currently pass through `EscapeState::process()`. Shift-PageUp/Down are raw CSI sequences that must be intercepted BEFORE the escape machine (or the escape machine must be extended to pass them through without consuming them — but the simpler path is to pre-scan the raw stdin bytes for the specific byte sequences).

**The CSI sequences:**
- Shift-PageUp: `ESC [ 5 ; 2 ~` = `[0x1b, 0x5b, 0x35, 0x3b, 0x32, 0x7e]`
- Shift-PageDown: `ESC [ 6 ; 2 ~` = `[0x1b, 0x5b, 0x36, 0x3b, 0x32, 0x7e]`

These are distinct from plain PageUp/Down (`ESC [ 5 ~` / `ESC [ 6 ~`) and from arrow keys. The scan must handle the case where the sequence is split across two `stdin.read()` calls (buffered in an accumulator, not matched byte-by-byte in a single call).

**Implementation approach:** Add a small `EscapeAccumulator` or extend the existing stdin buffer handling with a prefix-match state machine. Given that Shift-PageUp/Down are 6 bytes, a simple ring-buffer accumulation of up to 6 pending bytes handles the split-call case.

**State machine:**
```rust
enum ScrollbackView {
    /// Live mode: datagrams apply and display normally.
    Live,
    /// In scrollback: local line buffer displayed; live datagrams acked but not displayed.
    Active {
        lines: Vec<Vec<Cell>>,   // fetched historical lines (oldest first)
        offset: usize,           // how many lines from the bottom are being displayed
        pending_request: bool,   // true while a ScrollbackRequest is in-flight
        epoch_at_snapshot: u64,  // from the last ScrollbackPage — gate for exit
    },
}
```

**Exit condition:** When in `Active` and a datagram arrives with `diff.epoch >= epoch_at_snapshot`, the client exits scrollback mode and applies the datagram normally.

**Display in scrollback mode:** The client redraws the terminal from `lines` + `offset` rather than from `screen.render_with_predictor`. The scrollback view renders directly to stdout using the same cursor-positioning escape sequences. The predictor is suspended (no `on_input` during scrollback — keystrokes that snap back immediately call the predictor after exiting).

### Anti-Patterns to Avoid

- **Reading scrollback inline in the session pump:** Stalls the pump while iterating a potentially large VecDeque. Use the channel task.
- **Writing `ScrollbackPage` via `conn.send_datagram`:** Violates S-1. Type-level enforcement: the scrollback sender function takes `&mut quinn::SendStream`, not `&quinn::Connection`.
- **Snapshotting epoch and scrollback in two separate lock acquisitions:** Causes S-5 torn-read. Always acquire the `terminal_state` lock once, read both `scrollback_lines()` and pass the epoch from the pump as a parameter, or read the epoch from a separate atomic that the pump updates under the lock.
- **Forwarding channel-control frames (`ChannelCredit`) directly from the channel task:** Violates A4. All outbound control frames go through `control_tx`.
- **Advancing `confirmed_epoch` or altering epoch cadence in the scrollback path:** Would reintroduce the R-2 / SEC-1 noecho-epoch regression. The scrollback sender never calls `send_datagram` and has no epoch authority.
- **Replaying channel state on reattach:** Violates MUX-05 / M-5. Channel state resets on reconnect; the client re-opens the Scrollback channel after `ResumeComplete`.

---

## Don't Hand-Roll

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| Scrollback credit pacing | Custom windowing scheme | Existing `INITIAL_CREDIT` = 256 KiB + `ChannelEvent::Credit` + `ChannelCredit` message | Already designed and tested in Phase 21 echo loop; variable-size pages map cleanly onto byte credits |
| Channel lifecycle (open/accept/close) | New handshake | `client::open_channel()` + `client::await_channel_accept()` | Already implemented in `client.rs:760–783`; used by Phase 21 integration tests |
| Channel-id varint prefix on bidi streams | Custom encoding | `postcard::to_allocvec(&channel_id)` + `send.write_all(&prefix)` | Already established in `open_channel()` and `read_varint_u32()` |
| Drop-oldest bounded queue | Ring buffer from scratch | `tokio::sync::mpsc::channel(N)` with `try_send` + discard on `Err(Full)` | `mpsc::bounded` already provides the capacity limit; `try_send` is the non-blocking path |
| Discriminant stability enforcement | Manual serialisation check | Extend the existing `message_discriminant_order_is_stable` test in `codec.rs` | Test is already the Phase 21 gating commit; appending entries is one line per variant |

---

## Common Pitfalls

### Pitfall 1 (S-5): Torn epoch / scrollback read

**What goes wrong:** The pump reads `current_epoch` in one statement and calls `slot.with_terminal_state(|ts| ts.scrollback_lines(...))` in a second statement. Between the two calls the diff tick fires, increments `current_epoch`, and the epoch no longer corresponds to the scrollback snapshot the sender delivers.

**Why it happens:** `current_epoch` is a local variable in the pump's stack frame, not inside the `terminal_state` mutex. The mutex only guards `TerminalState`; the epoch is independent.

**How to avoid:** The scrollback read must capture the epoch at the same moment as the scrollback lines — either by passing the epoch into the accessor (the accessor returns it as part of the tuple), or by doing both reads inside the same `slot.with_terminal_state` closure. Concretely: the pump reads `current_epoch` into a local, then enters `with_terminal_state`, reads the lines and total, and passes the captured epoch as `epoch_at_snapshot` to the `ScrollbackPage`. Since `with_terminal_state` is synchronous (no `.await` inside), the epoch cannot change while the closure is running.

**Warning signs:** Integration test showing duplicate lines at the scrollback/live boundary; or a client that never exits scrollback mode because `epoch_at_snapshot` is in the future relative to live datagrams.

---

### Pitfall 2 (M-6): Scrollback sender blocking the pump

**What goes wrong:** The scrollback sender task is implemented inline in the `run_session` select! loop rather than as a separate `tokio::spawn`. Writing `ScrollbackPage` frames to a QUIC `SendStream` may block if the stream's flow-control window is exhausted. Blocking inside a select! arm stalls all other arms: PTY input backs up, diff ticks are missed, and the terminal model stops updating.

**Why it happens:** This is the documented M-6 pitfall from `PITFALLS.md`. The channel task model in `channel.rs` exists precisely to avoid this.

**How to avoid:** The scrollback sender is always a `tokio::spawn`ed task (matches `run_channel_task` shape). The pump sends `ChannelEvent::Stream(send, recv)` to the task via the existing mpsc; the task owns the stream handles exclusively and never shares them with the pump.

**Warning signs:** PTY input latency > 5 ms during a scrollback transfer; the 16 ms diff tick starts missing; the `M-6 PTY-latency isolation test` fails.

---

### Pitfall 3 (S-1): Scrollback sent over datagrams

**What goes wrong:** A refactor moves scrollback delivery to `conn.send_datagram()` for performance. Lines are silently dropped on lossy paths. The client receives history with random gaps and no way to detect them.

**Why it happens:** Datagrams are the fast path in this codebase; it is tempting to use them for all state sync.

**How to avoid:** Type-level enforcement: the scrollback sender task's function signature accepts `&mut quinn::SendStream` with no access to `Connection`. The in-order/no-gap integration test under 20% simulated packet loss proves sequential integrity.

**Warning signs:** Scrollback lines arrive out of order or with gaps on high-loss paths.

---

### Pitfall 4 (S-2): Alt-screen contamination

**What goes wrong:** The `!alt_screen` gate on `scroll_up()` is removed or bypassed during a refactor of `terminal.rs`. Alt-screen lines (vim, htop) appear in the scrollback buffer.

**Why it happens:** The gate is an `if` inside `scroll_up()` at line 631 and inside `resize()` at line 489. A future change that adds a new scroll path (e.g. a `scroll_n()` helper) might forget to add the gate.

**How to avoid:** The unit test from ROADMAP success criterion 3 (write to primary, activate alt screen, force scroll lines, deactivate, assert only primary lines appear) is the regression guard. This test must be a required CI gate.

**Warning signs:** Scrollback viewer shows vim/htop control sequences; `scrollback.len()` grows while `echo_state.alt_screen == true`.

---

### Pitfall 5 (cold reattach): Channel state replayed instead of re-opened

**What goes wrong:** On cold reattach the client does not call `open_channel(ChannelType::Scrollback)` after `ResumeComplete`. Either the channel is never re-opened (scrollback unavailable post-reattach, SCROLL-05 failure) or the client tries to re-use the old channel-id on the new QUIC connection (the server's channel map was cleared on orphan, so this gets `ChannelReject`).

**Why it happens:** `reattach_session` currently has no channel infrastructure. Phase 21's reattach re-open is only exercised in the integration test; the production reattach path (`reattach_session` in `main.rs`) does not currently call `open_channel` for anything.

**How to avoid:** Add `open_channel(conn, &mut send, &mut recv, channel_id, ChannelType::Scrollback)` immediately after the `await_reattach_reply` returns `ReattachOutcome::Ok` and before `run_pump`. The channel_id can be freshly allocated by the `EvenIdAllocator` held in the caller's scope. The server's reattach path (`run_reattach_session`) already handles `ChannelType::Scrollback` — currently rejects it (line 1819); this phase changes that to `accept = true`.

**Warning signs:** Reattach integration test shows `ChannelReject` on the Scrollback channel; or `SCROLL-05` "scrollback content is viewable immediately after a cold reattach" fails.

---

### Pitfall 6: CSI escape split across `stdin.read()` calls

**What goes wrong:** The Shift-PageUp CSI sequence `ESC[5;2~` is 6 bytes. If the OS delivers it split across two consecutive `stdin.read()` calls (e.g. `ESC[5` then `;2~`), a simple `bytes.windows(6).any(|w| w == ...)` check on each read individually misses the match.

**Why it happens:** Terminal emulators typically send escape sequences as one atomic write, but the POSIX pipe buffer and `tokio::io::stdin` do not guarantee arrival in one read call.

**How to avoid:** Maintain a small rolling accumulator (`[u8; 8]` or similar) in the scrollback view state machine. On each stdin read, append to the accumulator and scan for the 6-byte sequences. Once matched, consume the bytes from the accumulator. This is a stateful prefix-match, not a stateless `windows()` scan.

**Warning signs:** Shift-PageUp only works when typed slowly (not after a burst of input); or the sequence is split by a concurrent datagram arrival that interrupts the tokio select! run.

---

### Pitfall 7: `ScrollbackRequest` on the control stream introduces M-2 deadlock risk

**What goes wrong:** If `ScrollbackRequest` is routed over the control stream (the same bidi stream carrying `PtyData`, `ChannelOpen`, `ChannelCredit` etc.), and the scrollback sender task blocks writing `ScrollbackPage` on the channel's data stream while the pump is blocked reading the next `ScrollbackRequest` from the control stream, you get a head-of-line deadlock between the two streams.

**Why it happens:** The M-2 pitfall: flow-control on one logical stream stalls the read of control frames from another.

**How to avoid:** Route `ScrollbackRequest` on the channel's own data stream (the `RecvStream` of the scrollback channel), not the control stream. The server's channel task reads `ScrollbackRequest` from `ch_recv`; it writes `ScrollbackPage` to `ch_send`. The control stream carries only `ChannelCredit` and `ChannelClose` (via `control_tx`). This keeps the control stream free of data-plane backpressure.

**Warning signs:** Integration test hangs with the scrollback channel open and an active request in-flight; the pump's control stream read never unblocks.

---

## Code Examples

### Existing `scroll_up()` alt-screen gate (verified, do not re-implement)

```rust
// Source: crates/nosh-server/src/terminal.rs:626-639
fn scroll_up(&mut self) {
    if self.rows == 0 {
        return;
    }
    let top_row = self.grid.remove(0);
    if !self.echo_state.alt_screen {
        // Primary screen: push to scrollback with cap enforcement.
        self.scrollback.push_back(top_row);
        if self.scrollback.len() > SCROLLBACK_LINE_CAP {
            self.scrollback.pop_front();
        }
    }
    // Alt screen: top_row is dropped here (no scrollback for alt grid).
    self.grid.push(vec![Cell::default(); self.cols as usize]);
}
```

### Existing `open_channel()` in `client.rs` (verified — re-use for scrollback channel open)

```rust
// Source: crates/nosh-client/src/client.rs:760-783
pub async fn open_channel(
    conn: &quinn::Connection,
    control_send: &mut quinn::SendStream,
    control_recv: &mut quinn::RecvStream,
    channel_id: u32,
    channel_type: ChannelType,
) -> anyhow::Result<Option<(quinn::SendStream, quinn::RecvStream)>> {
    send_channel_open(control_send, channel_id, channel_type).await?;
    match await_channel_accept(control_recv, channel_id).await? {
        ChannelAcceptOutcome::Rejected => Ok(None),
        ChannelAcceptOutcome::Accepted => {
            let (mut send, recv) = conn.open_bi().await.context("open channel bidi stream")?;
            let prefix = postcard::to_allocvec(&channel_id)
                .context("encode channel-id varint prefix")?;
            send.write_all(&prefix)
                .await
                .context("write channel-id varint prefix")?;
            Ok(Some((send, recv)))
        }
    }
}
```

### Existing echo loop credit pattern (template for scrollback sender task)

```rust
// Source: crates/nosh-server/src/channel.rs:198-259
// The scrollback sender task reuses this credit-pause / Credit-event / write pattern.
// The key difference: writes are ScrollbackPage frames (variable size), not echo bytes.
// remaining_credit must be decremented by the actual byte count written to ch_send.
async fn run_echo_loop(/* ... */) {
    let mut remaining_credit: u64 = INITIAL_CREDIT;
    loop {
        if remaining_credit == 0 {
            // Wait for ChannelEvent::Credit before attempting any reads or writes.
            match events.recv().await {
                Some(ChannelEvent::Credit(n)) => {
                    remaining_credit = remaining_credit.saturating_add(n);
                }
                Some(ChannelEvent::Close) | None => break,
                _ => {}
            }
            continue;
        }
        tokio::select! {
            read_res = ch_recv.read(&mut buf[..read_cap]) => { /* ... */ }
            ev = events.recv() => { /* handle Credit / Close */ }
        }
    }
}
```

### `with_terminal_state` pattern (for atomic epoch + scrollback snapshot)

```rust
// Source: crates/nosh-server/src/registry.rs (existing public method)
// The pump uses this to read scrollback + pass current_epoch atomically.
// current_epoch is a local variable; it cannot change inside the synchronous closure.
let epoch_at_snapshot = current_epoch;  // capture before entering lock
let (lines, total_available) = slot.with_terminal_state(|ts| {
    ts.scrollback_lines(from_line as u64, count as usize)
});
// Now epoch_at_snapshot is consistent with lines — no diff tick can fire
// inside the synchronous closure (it would be in the same task's select! arm).
```

### Current server `ChannelType::Scrollback` rejection (lines to change)

```rust
// Source: crates/nosh-server/src/server.rs:1060-1067 (run_session)
ChannelType::Scrollback => {
    // Phase 22 consumer — not yet handled.
    tracing::debug!(channel_id, "ChannelOpen for Scrollback; rejecting (Phase 22)");
    false
}
// Change to: true, then spawn run_scrollback_channel_task with slot.clone()

// Source: crates/nosh-server/src/server.rs:1819 (run_reattach_session)
ChannelType::Scrollback => false,
// Change to: true (same logic)
```

### Discriminant-stability test current state (to extend)

```rust
// Source: crates/nosh-proto/src/codec.rs:276-306
// Current last entry: (14, Message::ChannelClose { channel_id: 2 })
// Phase 22 must add:
(15, Message::ScrollbackRequest { channel_id: 2, from_line: 0, count: 256 }),
(16, Message::ScrollbackPage {
    channel_id: 2, from_line: 0, total_available: 0,
    epoch_at_snapshot: 0, lines: vec![] }),
(17, Message::ScrollbackCredit { channel_id: 2, bytes: 0 }),
```

---

## State of the Art

| Old Approach | Current Approach | When Changed | Impact |
|--------------|------------------|--------------|--------|
| Scrollback channel rejected (`false`) in both `run_session` and `run_reattach_session` | Accept + spawn scrollback sender task | Phase 22 | Enables SCROLL-01 through SCROLL-05 |
| `run_pump` stdin arm: all bytes pass through `EscapeState` | Pre-scan for `ESC[5;2~` / `ESC[6;2~` before `EscapeState` | Phase 22 | Enables Shift-PageUp/Down scrollback navigation (SCROLL-04) |
| `run_pump` reliable-stream arm: `PtyData` bytes discarded, `Ok(_)` other frames ignored | Handle `ScrollbackPage` frames in channel task; deliver to scrollback view buffer | Phase 22 | Enables actual scrollback content delivery |
| `reattach_session` has no channel infrastructure | Re-opens `ChannelType::Scrollback` after `ResumeComplete` | Phase 22 | SCROLL-05 cold reattach support |
| `terminal.rs` has no public scrollback read accessor | Add `scrollback_lines(from, count)` accessor | Phase 22 | Encapsulates cap and index logic; `VecDeque` never exposed |

---

## Runtime State Inventory

> This is a feature-addition phase, not a rename/refactor. No runtime state inventory required.

---

## Open Questions

1. **`ScrollbackRequest` routing — control stream or channel data stream?**
   - What we know: CONTEXT.md says "implementer's call, provided it does not reintroduce the M-2 control/data flow-control deadlock."
   - What's unclear: whether writing `ScrollbackPage` responses on `ch_send` while reading `ScrollbackRequest` from `ch_recv` creates any backpressure loop on the same bidi stream.
   - Recommendation: Route `ScrollbackRequest` on the channel's own `RecvStream` (`ch_recv`). The sender task reads requests from `ch_recv` and writes pages to `ch_send` — these are independent halves of the same bidi stream; QUIC flow-control is per-direction, so writing to `ch_send` does not stall reading from `ch_recv`. This avoids any control-stream pollution and is the natural fit for a request/response channel.

2. **How does the server scrollback sender task get access to `current_epoch`?**
   - What we know: `current_epoch` is a local stack variable in `run_session` / `run_reattach_session`, not in `SessionSlot`.
   - What's unclear: whether to move it into the `SessionSlot` (under a separate `Mutex<u64>`) or pass it to the task via the `ChannelEvent` mpsc.
   - Recommendation: Keep `current_epoch` in the pump and pass a snapshot in the `ChannelEvent::ScrollbackRequest { from_line, count, epoch_snapshot }` message (pump reads the request from the task's mpsc, reads `current_epoch` and scrollback together, then sends `ScrollbackPage` back via the `SendStream`). This avoids adding epoch to the `SessionSlot` API. Alternatively, an `Arc<AtomicU64>` is clean if the epoch must be read inside the task itself.

3. **Keystroke snap-back while request is in-flight:**
   - What we know: CONTEXT.md says "any non-paging keystroke immediately returns to live AND is delivered to the shell."
   - What's unclear: if the client issues a `ScrollbackRequest` and receives a `ScrollbackPage` response while the user has already snapped back to live mode, the page response arrives on the channel's data stream and must be discarded (not displayed).
   - Recommendation: The client scrollback view state machine tracks whether it is in `Active` or `Live` mode. If a `ScrollbackPage` arrives while in `Live` mode, it is simply dropped. The channel task continues draining to maintain flow control (credit is still granted), but the payload is not forwarded to the view buffer.

---

## Validation Architecture

### Test Framework

| Property | Value |
|----------|-------|
| Framework | `cargo nextest` (integration tests in `crates/nosh-client/tests/`) + `cargo test` (unit tests inline in `terminal.rs`, `channel.rs`, `codec.rs`) |
| Config file | `.cargo/nextest.toml` or workspace root (existing) |
| Quick run command | `cargo nextest run --test channel_mux -p nosh-client` |
| Full suite command | `cargo nextest run` |

### Phase Requirements → Test Map

| Req ID | Behavior | Test Type | Automated Command | File |
|--------|----------|-----------|-------------------|------|
| SCROLL-01 | Server serves scrollback lines on request | integration | `cargo nextest run --test channel_mux scrollback_basic_fetch` | `crates/nosh-client/tests/channel_mux.rs` |
| SCROLL-02 | Scrollback over reliable stream only; PTY latency < 5 ms during transfer | integration | `cargo nextest run --test channel_mux scrollback_pty_latency_isolation` | `crates/nosh-client/tests/channel_mux.rs` |
| SCROLL-02 | Drop-oldest when mpsc full; no pump stall | integration | `cargo nextest run --test channel_mux scrollback_backpressure_drop_oldest` | `crates/nosh-client/tests/channel_mux.rs` |
| SCROLL-02 | In-order / no-gap under 20% packet loss | integration | `cargo nextest run --test channel_mux scrollback_inorder_under_loss` | `crates/nosh-client/tests/channel_mux.rs` |
| SCROLL-03 | Alt-screen content excluded from scrollback | unit | `cargo test -p nosh-server scrollback_excludes_alt_screen` | `crates/nosh-server/src/terminal.rs` |
| SCROLL-03 | Resize while in alt-screen does not contaminate scrollback | unit | `cargo test -p nosh-server resize_alt_screen_no_scrollback_contamination` | `crates/nosh-server/src/terminal.rs` |
| SCROLL-04 | Shift-PageUp enters scrollback; Shift-PageDown exits; any key snaps to live | integration | `cargo nextest run --test channel_mux scrollback_keybinding_snap_back` | `crates/nosh-client/tests/channel_mux.rs` |
| SCROLL-05 | No gap or duplicate lines at epoch boundary | integration | `cargo nextest run --test channel_mux scrollback_epoch_handoff_no_gap` | `crates/nosh-client/tests/channel_mux.rs` |
| SCROLL-05 | Scrollback viewable after cold reattach | integration | `cargo nextest run --test channel_mux scrollback_post_reattach` | `crates/nosh-client/tests/channel_mux.rs` |
| MUX-06 | Discriminant-stability covers new variants | unit | `cargo test -p nosh-proto message_discriminant_order_is_stable` | `crates/nosh-proto/src/codec.rs` |

### Wave 0 Gaps

- [ ] `crates/nosh-proto/src/codec.rs` — extend `message_discriminant_order_is_stable` to include discriminants 15–17 (must be the first commit of this phase).
- [ ] `crates/nosh-client/tests/channel_mux.rs` — add scrollback integration test functions (currently only covers echo channel).
- [ ] `crates/nosh-server/src/terminal.rs` — add `scrollback_excludes_alt_screen` unit test (ROADMAP success criterion 3 verbatim).

---

## Security Domain

### Applicable ASVS Categories

| ASVS Category | Applies | Standard Control |
|---------------|---------|-----------------|
| V2 Authentication | no | Auth is already established before any channel opens (Phase 21 enforces `AuthLimits` semaphore) |
| V3 Session Management | no | Session persistence is Phase 5/6; scrollback inherits the session |
| V4 Access Control | yes — channel accept gate | `run_session` accept arm: `ChannelType::Scrollback` accepted only after auth; `ChannelType::PortForward` / `AgentForward` remain rejected; production builds reject `Echo` |
| V5 Input Validation | yes | `ScrollbackRequest.from_line` and `count` must be bounds-checked: `from_line >= total_available` → empty page (not panic); `count > MAX_COUNT` → cap to MAX_COUNT; varint overflow already handled by `read_varint_u32` |
| V6 Cryptography | no | Scrollback travels over the existing TLS 1.3 QUIC connection; no additional crypto |

### Known Threat Patterns for This Stack

| Pattern | STRIDE | Standard Mitigation |
|---------|--------|---------------------|
| Client requests `from_line = u64::MAX` to cause integer overflow in index computation | Tampering | `scrollback_lines` accessor: `if from_line >= total { return (vec![], total); }` — no panic |
| Client requests `count = u32::MAX` to force server to allocate 4 GB of lines | Tampering | Cap `count` to `min(count, MAX_PAGE_SIZE)` where `MAX_PAGE_SIZE` = e.g. 1024; `SCROLLBACK_LINE_CAP = 10_000` is the ultimate bound |
| Malicious client floods `ScrollbackRequest` frames faster than the server can respond, filling the request mpsc | DoS | Bounded mpsc with `try_send` + drop-oldest prevents unbounded memory; server task processes at most one request at a time |
| Server process scrollback buffer grows without bound across many sessions | DoS (server) | `SCROLLBACK_LINE_CAP = 10_000` per session is the hard bound; do not raise it |
| `SSH_AUTH_SOCK` forwarded via scrollback channel env | Privilege escalation | Scrollback channel carries only line content, no env vars; env sanitisation on shell spawn is Phase 3 / CLAUDE.md |

---

## Environment Availability

> Step 2.6: SKIPPED (no new external dependencies; this phase adds no tools, services, or runtimes beyond the existing Rust/Cargo/tokio/quinn workspace).

---

## Assumptions Log

| # | Claim | Section | Risk if Wrong |
|---|-------|---------|---------------|
| A1 | `ScrollbackRequest` routed on channel `RecvStream` (not control stream) avoids M-2 deadlock | Architecture Patterns / Pattern 3 | If the bidi stream's `ch_send` backpressure somehow bleeds into `ch_recv`, the sender task stalls; mitigation: move request to control stream (changes only the routing, not the protocol) |
| A2 | `current_epoch` captured before `with_terminal_state` is safe because both are in the same async task and the diff tick is in the same `select!` (cannot interleave within a single task) | Code Examples / `with_terminal_state` | If `current_epoch` advances after capture but before `with_terminal_state` returns, the snapshot and epoch are still internally consistent (epoch is at most stale by one diff tick, which is fine — the client will receive a live datagram with a higher epoch and transition correctly) |
| A3 | The client scrollback channel task delivers `ScrollbackPage` bytes to a shared `Arc<Mutex<Vec<Vec<Cell>>>>` or similar that `run_pump` reads from | Pattern 4 | If the channel task and `run_pump` run in the same `tokio::spawn` context, a different delivery mechanism (e.g. `mpsc`) may be needed to avoid holding a lock across an await |

**If this table is empty:** All claims in this research were verified or cited — no user confirmation needed. (Three assumptions logged above, all low-risk.)

---

## Sources

### Primary (HIGH confidence)

- `crates/nosh-proto/src/messages.rs` — `Message` enum, `ChannelType::Scrollback` variant (discriminants 0–14 verified), `ChannelCredit`, `ChannelClose`, `INITIAL_CREDIT` pattern. Read in full.
- `crates/nosh-server/src/terminal.rs` — `TerminalState.scrollback: VecDeque<Vec<Cell>>`, `SCROLLBACK_LINE_CAP = 10_000`, `scroll_up()` alt-screen gate at line 631, `resize()` alt-screen gate at line 489, `viewport_rows()` read pattern. Read lines 1–810.
- `crates/nosh-server/src/channel.rs` — `run_channel_task`, `run_echo_loop`, `ChannelEvent`, `INITIAL_CREDIT`, `read_varint_u32`. Read in full.
- `crates/nosh-client/src/channel.rs` — `run_channel_task` (client drain), `EvenIdAllocator`, `CREDIT_REPLENISH_CHUNK`, `INITIAL_CREDIT`. Read in full.
- `crates/nosh-client/src/client.rs` — `open_channel`, `send_channel_open`, `await_channel_accept`, `ChannelAcceptOutcome`. Read lines 665–784.
- `crates/nosh-client/src/main.rs` — `run_pump` stdin arm, `EscapeState` machine, `reattach_session`, `fresh_session`, scrollback stub at line 934 ("content discarded — no scrollback this milestone"). Key lines read: 600–760, 900–1000, 1196–1270.
- `crates/nosh-server/src/server.rs` — `current_epoch`, `epoch_snapshots`, `channel_map`, `channel_ctrl_tx`, `ChannelType::Scrollback` rejection at lines 1060–1067 and 1819, `accept_bi` binding loop at lines 1193–1231. Key lines read: 750–820, 1050–1165, 1790–1870.
- `crates/nosh-server/src/registry.rs` — `SessionSlot.terminal_state: Mutex<TerminalState>`, `with_terminal_state`, `server_open_tx`. Relevant lines confirmed.
- `crates/nosh-proto/src/datagram.rs` — `StateDiff.epoch: u64` (the live-grid epoch the client compares against `epoch_at_snapshot`). Lines 51–85 read.
- `crates/nosh-proto/src/codec.rs` — `message_discriminant_order_is_stable` test showing all 15 current discriminants (0–14); `mux_variants_round_trip`. Lines 260–344 read.
- `.planning/research/PITFALLS.md` — S-1 through S-5 and M-6 in full. Read in full.
- `.planning/phases/22-scrollback-sync/22-CONTEXT.md` — all locked decisions. Read in full.
- `.planning/REQUIREMENTS.md` — SCROLL-01 through SCROLL-05. Read in full.
- `.planning/ROADMAP.md` — Phase 22 success criteria and security note. Read in full.

### Secondary (MEDIUM confidence)

- `.planning/STATE.md` — Pending todos for Phase 22 start, accumulated decisions. Read in full.
- `crates/nosh-client/tests/channel_mux.rs` — `recv_channel_reply` and `spawn_ctrl_drain` helper patterns; control-stream deadlock avoidance documented in module header (lines 1–55). Read in full.

---

## Metadata

**Confidence breakdown:**
- Standard stack: HIGH — no new dependencies; all libraries are verified in the workspace.
- Architecture: HIGH — integration points read directly from source at concrete file:line locations.
- Pitfalls: HIGH — S-1 through S-5 and M-6 sourced from the project's own PITFALLS.md (first-party documented production failures and design decisions).
- Wire protocol: HIGH — discriminant ordering and postcard encoding verified against `codec.rs` tests.

**Research date:** 2026-06-12
**Valid until:** 2026-07-12 (stable — no external dependencies; validity is bounded by codebase changes, not ecosystem churn)
