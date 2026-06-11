# Phase 22: Scrollback Sync - Pattern Map

**Mapped:** 2026-06-12
**Files analyzed:** 7 (5 modified, 2 test files extended)
**Analogs found:** 7 / 7

---

## File Classification

| New/Modified File | Role | Data Flow | Closest Analog | Match Quality |
|-------------------|------|-----------|----------------|---------------|
| `crates/nosh-proto/src/messages.rs` | model/protocol | request-response | same file (Phase 21 mux variants) | exact |
| `crates/nosh-proto/src/codec.rs` | test | transform | same file (`message_discriminant_order_is_stable` test) | exact |
| `crates/nosh-server/src/terminal.rs` | model + accessor | CRUD | same file (`scroll_up`, `resize`, `with_terminal_state`) | exact |
| `crates/nosh-server/src/channel.rs` | service (task) | streaming | same file (`run_echo_loop` / `run_channel_task`) | exact |
| `crates/nosh-server/src/server.rs` | controller | event-driven | same file (Phase 21 `ChannelType::Echo` accept arm + `tokio::spawn`) | exact |
| `crates/nosh-client/src/channel.rs` | service (task) | streaming | same file (`run_channel_task` drain loop) | exact |
| `crates/nosh-client/src/main.rs` | controller | event-driven | same file (`EscapeState`, `run_pump` stdin arm, `reattach_session`) | exact |
| `crates/nosh-client/tests/channel_mux.rs` | test | request-response | same file (echo-channel integration tests, `spawn_ctrl_drain`) | exact |

---

## Pattern Assignments

### `crates/nosh-proto/src/messages.rs` (model/protocol, request-response)

**Analog:** same file, Phase 21 mux variants block (lines 177–236)

**Append-only variant pattern** (lines 177–236):
```rust
// Phase 21: Channel Multiplexing Foundation
//
// These variants are appended AFTER `TerminalControl` (discriminant 9) to
// preserve the postcard discriminant order of all existing variants.
// Inserting or reordering is NOT backward-compatible.
// APPEND-ONLY from here.

ChannelOpen { channel_id: u32, channel_type: ChannelType },
ChannelAccept { channel_id: u32 },
ChannelReject { channel_id: u32 },
ChannelCredit { channel_id: u32, bytes: u64 },
ChannelClose { channel_id: u32 },
```

**New Phase 22 variants follow this exact shape.** Append after `ChannelClose` (discriminant 14). The three new variants become discriminants 15, 16, 17.

**`variant_name()` match arm pattern** (lines 311–315 — extend this match):
```rust
Message::ChannelOpen { .. } => "ChannelOpen",
Message::ChannelAccept { .. } => "ChannelAccept",
Message::ChannelReject { .. } => "ChannelReject",
Message::ChannelCredit { .. } => "ChannelCredit",
Message::ChannelClose { .. } => "ChannelClose",
// Phase 22: add arms for ScrollbackRequest, ScrollbackPage, ScrollbackCredit
```

**`ChannelCredit` shape is the analog for `ScrollbackCredit`** (lines 218–228):
```rust
/// Either direction: grants additional byte-credit to the send side of a
/// channel (MUX-03). Sent on the control stream, not the channel's data stream.
ChannelCredit {
    channel_id: u32,
    bytes: u64,
}
```
`ScrollbackCredit` follows the same struct shape: `{ channel_id: u32, bytes: u64 }`.

---

### `crates/nosh-proto/src/codec.rs` (test, transform)

**Analog:** `message_discriminant_order_is_stable` test (lines 276–306)

**Discriminant stability test pattern** (lines 280–296):
```rust
let cases: &[(u8, Message)] = &[
    (0, Message::SessionOpen { term: "xterm".into(), cols: 80, rows: 24, env: vec![] }),
    // ... existing 15 entries ending with:
    (14, Message::ChannelClose { channel_id: 2 }),
    // Phase 22 appends here:
];
for (expected_disc, msg) in cases {
    let encoded = to_allocvec(msg).expect("encode");
    assert_eq!(
        encoded[0], *expected_disc,
        "Message::{} must encode with discriminant {}; encoded[0] = {}",
        msg.variant_name(), expected_disc, encoded[0]
    );
}
```

**Exactly three lines to add** (after discriminant 14):
```rust
(15, Message::ScrollbackRequest { channel_id: 2, from_line: 0, count: 256 }),
(16, Message::ScrollbackPage {
    channel_id: 2, from_line: 0, total_available: 0,
    epoch_at_snapshot: 0, lines: vec![] }),
(17, Message::ScrollbackCredit { channel_id: 2, bytes: 0 }),
```

This is the **first commit** of Phase 22 — before any other change.

**Round-trip test pattern** (`mux_variants_round_trip`, lines 311–343):
```rust
let msgs = [
    Message::ChannelOpen { channel_id: 2, channel_type: ChannelType::Echo },
    Message::ChannelOpen { channel_id: 4, channel_type: ChannelType::Scrollback },
    // ...
];
for msg in msgs {
    let mut buf: Vec<u8> = Vec::new();
    write_message(&mut buf, &msg).await.expect("write");
    let mut cursor = std::io::Cursor::new(buf);
    let got = read_message(&mut cursor).await.expect("read");
    assert_eq!(msg, got, "mux variant must round-trip exactly");
}
```
Add `ScrollbackRequest`, `ScrollbackPage`, and `ScrollbackCredit` to this round-trip set.

---

### `crates/nosh-server/src/terminal.rs` (model + accessor, CRUD)

**Analog:** `scroll_up()` at lines 626–640; `resize()` at lines 465–579

**`scroll_up()` alt-screen gate — DO NOT MODIFY, only verify** (lines 626–640):
```rust
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

**`resize()` alt-screen gate (D-19-09) — DO NOT MODIFY** (lines 481–496):
```rust
if !self.echo_state.alt_screen {
    // Primary screen: preserve rows in scrollback (same cap as scroll_up).
    self.scrollback.push_back(top_row);
    if self.scrollback.len() > SCROLLBACK_LINE_CAP {
        self.scrollback.pop_front();
    }
}
// Alt screen: top_row is dropped — alt content must not enter scrollback.
```

**New accessor to add — `scrollback_lines`:**

The `scrollback` field is `VecDeque<Vec<Cell>>` (line 193). Indexing convention: `scrollback[0]` is the **oldest** line, `scrollback[len-1]` is the **newest** (just above the live viewport). CONTEXT.md coordinates: `from_line = 0` = newest, `from_line = N` = Nth line above viewport.

```rust
/// Return a page of scrollback lines for the scrollback sync protocol (SCROLL-01).
///
/// `from_line` is the line index from newest backwards (0 = most recent, just
/// above the live viewport). `count` is the number of lines requested.
///
/// Returns `(lines, total_available)`:
/// - `lines`: the requested page in display order (oldest→newest), may be shorter
///   than `count` at the top of history.
/// - `total_available`: the total number of lines currently in scrollback.
///
/// The epoch MUST be captured by the CALLER under the same lock acquisition that
/// calls this method — never two separate lock calls (S-5 torn-read prevention).
pub fn scrollback_lines(&self, from_line: u64, count: usize) -> (Vec<Vec<Cell>>, u64) {
    let total = self.scrollback.len() as u64;
    if from_line >= total || count == 0 {
        return (vec![], total);
    }
    let newest_idx = (total - 1 - from_line) as usize;
    let oldest_idx = newest_idx.saturating_sub(count - 1);
    let lines: Vec<Vec<Cell>> = (oldest_idx..=newest_idx)
        .map(|i| self.scrollback[i].clone())
        .collect();
    (lines, total)
}
```

**Alt-screen exclusion unit test to add** (mirrors ROADMAP success criterion 3):
```rust
#[test]
fn scrollback_excludes_alt_screen() {
    let mut ts = TerminalState::new(80, 24);
    // Write to primary buffer and scroll lines into scrollback.
    // ... (force scroll_up calls on primary screen)
    // Activate alt screen.
    // Force scroll_up calls on alt screen — should NOT enter scrollback.
    // Deactivate alt screen.
    // Assert only primary lines appear in scrollback (none from alt).
}
```

---

### `crates/nosh-server/src/channel.rs` (service/task, streaming)

**Analog:** `run_echo_loop` (lines 199–259) and `run_channel_task` (lines 94–135)

**Full `run_channel_task` lifecycle pattern** (lines 94–135):
```rust
pub async fn run_channel_task(
    channel_id: u32,
    mut events: mpsc::Receiver<ChannelEvent>,
    control_tx: mpsc::Sender<Message>,
) {
    // Phase 1: wait for Stream bind event.
    let (mut ch_send, mut ch_recv) = loop {
        match events.recv().await {
            Some(ChannelEvent::Stream(s, r)) => break (s, r),
            Some(ChannelEvent::Close) | None => {
                let _ = control_tx.send(Message::ChannelClose { channel_id }).await;
                return;
            }
            Some(ChannelEvent::Credit(_)) => { /* keep waiting */ }
        }
    };

    // Phase 2: run the channel-specific I/O loop.
    run_channel_task_inner(channel_id, &mut ch_send, &mut ch_recv,
                           &mut events, &control_tx).await;

    // Phase 3: half-close and notify pump.
    let _ = ch_send.finish();
    let _ = tokio::time::timeout(Duration::from_secs(2), ch_send.stopped()).await;
    let _ = control_tx.send(Message::ChannelClose { channel_id }).await;
}
```

**Credit-pause / replenish loop from `run_echo_loop`** (lines 205–258 — the template for `run_scrollback_sender_task`):
```rust
let mut remaining_credit: u64 = INITIAL_CREDIT;
let mut buf = vec![0u8; 8192];

loop {
    if remaining_credit == 0 {
        // Credit exhausted: wait for replenishment before attempting
        // any reads or sends (MUX-03 back-pressure; T-21-05).
        match events.recv().await {
            Some(ChannelEvent::Credit(n)) => {
                remaining_credit = remaining_credit.saturating_add(n);
            }
            Some(ChannelEvent::Close) | None => break,
            Some(ChannelEvent::Stream(_, _)) => { /* unexpected; ignore */ }
        }
        continue;
    }

    let read_cap = remaining_credit.min(buf.len() as u64) as usize;

    tokio::select! {
        read_res = ch_recv.read(&mut buf[..read_cap]) => {
            match read_res {
                Ok(Some(n)) => {
                    if ch_send.write_all(&buf[..n]).await.is_err() { break; }
                    remaining_credit -= n as u64;
                }
                Ok(None) => break,
                Err(_) => break,
            }
        }
        ev = events.recv() => {
            match ev {
                Some(ChannelEvent::Credit(n)) => {
                    remaining_credit = remaining_credit.saturating_add(n);
                }
                Some(ChannelEvent::Close) | None => break,
                Some(ChannelEvent::Stream(_, _)) => { /* unexpected */ }
            }
        }
    }
}
```

**New `run_scrollback_sender_task` differs from echo in one key way:** it reads `ScrollbackRequest` frames from `ch_recv` (decode via `nosh_proto::codec::read_message`), locks `slot.with_terminal_state(|ts| ts.scrollback_lines(...))`, captures the epoch atomically, encodes a `ScrollbackPage` frame, and writes it to `ch_send`. The credit deduction is `encoded_frame.len() as u64`. Because writes can be large (up to ~50 KB per page), the credit check must happen before writing each page, not just inside the read loop.

**`run_scrollback_sender_task` signature:**
```rust
// Not cfg(test) — this is a production channel.
async fn run_scrollback_sender_task(
    channel_id: u32,
    slot: Arc<crate::registry::SessionSlot>,
    ch_send: &mut quinn::SendStream,
    ch_recv: &mut quinn::RecvStream,
    events: &mut mpsc::Receiver<ChannelEvent>,
    control_tx: &mpsc::Sender<Message>,
    current_epoch: u64,  // captured before entering the lock by the pump — see S-5
)
```

Or, if `current_epoch` is passed via the task's spawning infrastructure: the pump captures `current_epoch` before spawning and passes it once (the task re-reads it for each subsequent page by querying `slot` directly or via a shared `Arc<AtomicU64>`).

The cleanest pattern per RESEARCH.md: the scrollback task reads `ScrollbackRequest` from `ch_recv`, then the pump feeds back the snapshot via a dedicated `(from_line, count, epoch)` request-response over a second mpsc pair, OR the task has direct access to `Arc<SessionSlot>` and reads the epoch from a separate `Arc<AtomicU64>` that the pump updates each diff tick. See RESEARCH.md open question 2 for the trade-offs.

**`cfg` gate for production vs. test:** The echo loop uses `#[cfg(any(test, feature = "test-support"))]` to gate test-only behaviour. The scrollback sender is a production function — no cfg gate. Add it alongside `run_echo_loop` in `channel.rs`.

---

### `crates/nosh-server/src/server.rs` (controller, event-driven)

**Analog 1:** `ChannelType::Scrollback` rejection arm in `run_session` (lines 1060–1067)

**Change pattern — flip `false` to `true` and spawn the task:**
```rust
// BEFORE (lines 1060–1067):
ChannelType::Scrollback => {
    tracing::debug!(channel_id, "ChannelOpen for Scrollback; rejecting (Phase 22)");
    false
}

// AFTER (Phase 22):
ChannelType::Scrollback => true,
// Then in the post-accept spawn block, branch on channel_type to spawn
// run_scrollback_sender_task instead of run_channel_task.
```

**Analog 2:** `ChannelType::Scrollback` rejection in `run_reattach_session` (line 1819):
```rust
// BEFORE:
ChannelType::Scrollback => false,

// AFTER:
ChannelType::Scrollback => true,
// Same spawn logic as run_session.
```

**Channel spawn pattern** (lines 1106–1112 — the template):
```rust
let (task_tx, task_rx) = mpsc::channel::<ChannelEvent>(64);
channel_map.insert(channel_id, task_tx);
tokio::spawn(run_channel_task(
    channel_id,
    task_rx,
    channel_ctrl_tx.clone(),
));
```

For the scrollback channel, pass `slot.clone()` to `run_scrollback_sender_task`. The simplest approach is a separate `tokio::spawn` arm after the channel-type match rather than routing through the generic `run_channel_task`.

**`accept_bi` stream-bind pattern** (lines 1199–1243 — do NOT change, it routes streams to tasks via `ChannelEvent::Stream`):
```rust
incoming_stream = conn.accept_bi() => {
    match incoming_stream {
        Ok((ch_send, mut ch_recv)) => {
            match crate::channel::read_varint_u32(&mut ch_recv).await {
                Ok(channel_id) => {
                    if let Some(task_tx) = channel_map.get(&channel_id) {
                        if task_tx.send(ChannelEvent::Stream(ch_send, ch_recv)).await.is_err() {
                            tracing::debug!(channel_id, "accept_bi: channel task gone before stream arrived");
                        }
                    } else {
                        // CR-02 fix: reset stream so peer gets a clean signal.
                        let mut ch_send = ch_send;
                        let _ = ch_send.reset(0u32.into());
                        ch_recv.stop(0u32.into()).ok();
                    }
                }
                Err(_) => { /* malformed varint */ }
            }
        }
        // ... connection error arms
    }
}
```

**`epoch_at_snapshot` atomic-capture pattern** (from RESEARCH.md, verified against `server.rs` locals):

`current_epoch` is a local `u64` in `run_session` (line 755) and `run_reattach_session` (line 1631). The pump reads scrollback atomically like this:

```rust
// Capture epoch BEFORE entering the terminal_state lock.
let epoch_at_snapshot = current_epoch;
let (lines, total_available) = slot.with_terminal_state(|ts| {
    ts.scrollback_lines(from_line, count)
});
// epoch_at_snapshot and lines are internally consistent:
// the diff tick (which increments current_epoch) is another arm
// of the same select! and cannot interleave within one task.
```

---

### `crates/nosh-client/src/channel.rs` (service/task, streaming)

**Analog:** existing `run_channel_task` drain loop (lines 97–164)

**Drain loop pattern** (lines 106–137):
```rust
pub async fn run_channel_task(
    channel_id: u32,
    mut ch_recv: quinn::RecvStream,
    mut ch_send: quinn::SendStream,
    control_tx: mpsc::Sender<Message>,
) {
    let mut buf = vec![0u8; 8192];
    let mut drained_since_replenish: u64 = 0;

    loop {
        match ch_recv.read(&mut buf).await {
            Ok(Some(n)) => {
                drained_since_replenish += n as u64;
                if drained_since_replenish >= CREDIT_REPLENISH_CHUNK {
                    let bytes = drained_since_replenish;
                    drained_since_replenish = 0;
                    if control_tx.send(Message::ChannelCredit { channel_id, bytes }).await.is_err() {
                        break;
                    }
                }
            }
            Ok(None) => break,
            Err(e) => { tracing::debug!(channel_id, err = %e, "channel RecvStream error"); break; }
        }
    }
    // Flush remaining credit, half-close, notify pump.
    if drained_since_replenish > 0 {
        let _ = control_tx.send(Message::ChannelCredit { channel_id, bytes: drained_since_replenish }).await;
    }
    let _ = ch_send.finish();
    let _ = tokio::time::timeout(Duration::from_secs(2), ch_send.stopped()).await;
    let _ = control_tx.send(Message::ChannelClose { channel_id }).await;
}
```

**New scrollback client channel task differs from this drain:**

The generic drain discards the bytes after counting them for credit. The scrollback drain must instead parse `ScrollbackPage` frames from the byte stream (using `nosh_proto::codec::read_message`) and deliver them to the view buffer. The signature takes an additional `page_tx: mpsc::Sender<ScrollbackPage>` (or equivalent) to forward decoded pages to `run_pump`.

**`EvenIdAllocator` pattern** (lines 51–75 — unchanged, re-used as-is):
```rust
pub struct EvenIdAllocator { next: u32 }
impl EvenIdAllocator {
    pub fn new() -> Self { Self { next: 2 } }
    pub fn next_id(&mut self) -> u32 {
        let id = self.next;
        self.next = if self.next >= u32::MAX - 1 { 2 } else { self.next + 2 };
        id
    }
}
```

The scrollback channel id is allocated by the `EvenIdAllocator` instance already held by `fresh_session` / `reattach_session` callers.

**`open_channel()` re-use** (`crates/nosh-client/src/client.rs` lines 760–783):
```rust
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
            let prefix = postcard::to_allocvec(&channel_id).context("encode channel-id varint prefix")?;
            send.write_all(&prefix).await.context("write channel-id varint prefix")?;
            Ok(Some((send, recv)))
        }
    }
}
```

Call exactly this for `ChannelType::Scrollback` — no new code needed for channel open.

---

### `crates/nosh-client/src/main.rs` (controller, event-driven)

**Analog 1:** `EscapeState` machine (lines 174–245) — the existing `~.` quit detection is the structural template for Shift-PageUp/Down CSI interception.

**`EscapeState` processing pattern** (lines 192–245):
```rust
fn process(&mut self, input: &[u8]) -> EscapeResult {
    let mut out = Vec::with_capacity(input.len());
    let mut quit = false;
    for &byte in input {
        match *self {
            EscapeState::LineStart => { /* byte-by-byte state transitions */ }
            EscapeState::SeenTilde => { /* match '.' for quit, '~' for literal */ }
            EscapeState::MidLine => { /* reset to LineStart on '\n'/'\r' */ }
        }
    }
    EscapeResult { forward: out, quit }
}
```

The Shift-PageUp/Down interceptor is a new parallel state machine (`ScrollbackKeyState` or similar accumulator). Unlike `EscapeState` (which is byte-by-byte), CSI sequences are 6 bytes: `[0x1b, 0x5b, 0x35 or 0x36, 0x3b, 0x32, 0x7e]`. The interceptor maintains a rolling 6-byte accumulator to handle split-read edge cases (RESEARCH.md Pitfall 6).

**`run_pump` stdin arm pattern** (lines 923–934 inside the `tokio::select!` — the injection point):

The existing `stdin` arm:
```rust
n = stdin.read(&mut stdin_buf) => {
    // ... n bytes read into stdin_buf[..n]
    let res = escape.process(&stdin_buf[..n]);
    if res.quit { /* UserQuit */ }
    // Forward res.forward to shell via write_message(PtyData)
}
```

The scrollback interceptor runs on `stdin_buf[..n]` BEFORE `escape.process()`. Match the 6-byte CSI sequences, set `scrollback_view` state, and only pass non-paging bytes to `escape.process()`.

**Scrollback view state machine shape** (new, no analog — see RESEARCH.md Pattern 4):
```rust
enum ScrollbackView {
    Live,
    Active {
        lines: Vec<Vec<Cell>>,   // fetched historical lines, oldest first
        offset: usize,           // lines from bottom being displayed
        pending_request: bool,   // true while a ScrollbackRequest is in-flight
        epoch_at_snapshot: u64,  // from last ScrollbackPage — gate for exit
    },
}
// Start as: let mut scrollback_view = ScrollbackView::Live;
// in run_pump, at the same scope level as `let mut escape = EscapeState::new();`
```

**"Content discarded" stub to replace** (line 934):
```rust
// BEFORE (line 934):
let _ = data; // content discarded for display (no scrollback this milestone)

// AFTER: route PtyData to scrollback seq accounting; the channel task delivers
// ScrollbackPage frames via a separate mpsc, not via the control stream PtyData arm.
```

Note: `PtyData` on the control stream is the reattach-replay byte sequence — it still must increment `*highest_applied`. The scrollback content comes over the separate scrollback channel, not here.

**`reattach_session` injection point** (lines 709–755 — insert after `ReattachOutcome::Ok` block, before calling `run_pump`):

```rust
ReattachOutcome::Ok { new_token, replaying_from_seq, truncated } => {
    *token_out = Some(new_token);
    // ... existing truncation notice and highest_applied rebase ...
    let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));

    // Phase 22: re-open Scrollback channel after ResumeComplete (SCROLL-05 / MUX-05).
    // Channel state is never byte-replayed — re-open produces a fresh channel.
    let scrollback_channel_id = id_alloc.next_id();  // EvenIdAllocator
    let scrollback_streams = client::open_channel(
        conn, &mut send, &mut recv,
        scrollback_channel_id, ChannelType::Scrollback,
    ).await?;

    run_pump(conn, cols, rows, &mut send, &mut recv, ..., scrollback_streams).await
}
```

The `EvenIdAllocator` instance must be threaded into `reattach_session` (mirrors how `fresh_session` would allocate). This is a minor refactor of the call-site signature.

---

### `crates/nosh-client/tests/channel_mux.rs` (test, request-response)

**Analog:** existing echo-channel tests (lines 156+) and helpers `spawn_ctrl_drain` (lines 102–118), `recv_channel_reply` (lines 74–95), `await_channel_accept_from_drain` (lines 134–154).

**`spawn_ctrl_drain` pattern** (lines 102–118 — must be used in all scrollback tests to prevent deadlock):
```rust
fn spawn_ctrl_drain(
    mut ctrl_recv: quinn::RecvStream,
    frame_tx: mpsc::UnboundedSender<nosh_proto::Message>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match nosh_proto::read_message(&mut ctrl_recv).await {
                Ok(msg) => { if frame_tx.send(msg).is_err() { break; } }
                Err(_) => break,
            }
        }
    })
}
```

Every scrollback test that does channel data-stream I/O (sends `ScrollbackRequest`, receives `ScrollbackPage`) MUST spawn `ctrl_drain` to prevent the server pump's control-stream write from blocking.

**Test structure for `scrollback_basic_fetch`:**
```rust
#[tokio::test]
async fn scrollback_basic_fetch() {
    // 1. Open session, spawn ctrl_drain.
    // 2. Send enough PTY input to produce scrollback lines on server
    //    (feed bytes that scroll lines off the bottom of the 24-row grid).
    // 3. Open Scrollback channel via client::open_channel(..., ChannelType::Scrollback).
    //    Use await_channel_accept_from_drain to get Accept from drain mpsc.
    // 4. Send ScrollbackRequest { channel_id, from_line: 0, count: 256 } on ch_send.
    // 5. Read ScrollbackPage from ch_recv (parse via nosh_proto::codec::read_message).
    // 6. Assert page.lines is non-empty, page.epoch_at_snapshot is a reasonable epoch,
    //    page.total_available > 0.
    // 7. Grant credit via ChannelCredit through ctrl_send.
}
```

**Integration test module header annotation** (lines 1–22 — replicate the deadlock-avoidance rationale for new tests):

The scrollback tests open a data-stream channel and simultaneously read the control stream. Always use `spawn_ctrl_drain` + `await_channel_accept_from_drain` pattern, not `recv_channel_reply` (which holds `ctrl_recv` exclusively and blocks).

---

## Shared Patterns

### Channel task lifecycle (A4 single-writer invariant)

**Source:** `crates/nosh-server/src/channel.rs` lines 94–135 + comment block lines 1–22
**Apply to:** `run_scrollback_sender_task` in `channel.rs`

Channel tasks MUST NOT write to the control stream directly. All outbound control frames (`ChannelClose`, `ChannelCredit`) go back to the pump via `control_tx` mpsc. The scrollback sender must follow this invariant: `ScrollbackCredit` grants from the client arrive as `ChannelEvent::Credit(bytes)` — never direct stream writes from the task.

```rust
// CORRECT: route via control_tx
let _ = control_tx.send(Message::ChannelCredit { channel_id, bytes }).await;

// WRONG: never call write_message on any shared stream from the task
// nosh_proto::write_message(&mut shared_control_send, ...).await  // FORBIDDEN
```

### Append-only discriminant discipline (MUX-06)

**Source:** `crates/nosh-proto/src/messages.rs` lines 177–184 (comment block); `codec.rs` lines 276–306
**Apply to:** any new `Message` variants in `messages.rs`

New variants MUST be appended after the current last variant. Never insert or reorder. The `message_discriminant_order_is_stable` test in `codec.rs` is the enforcement gate — extend it in the first commit before any other change.

### Credit-pause idiom (MUX-03)

**Source:** `crates/nosh-server/src/channel.rs` lines 205–222 (`run_echo_loop` credit-exhausted block)
**Apply to:** `run_scrollback_sender_task` credit tracking

When `remaining_credit == 0`, block on `events.recv()` before attempting any send. Do NOT use `read(&mut buf[..0])` as a credit-wait — it returns `Ok(None)` and is misread as EOF.

```rust
if remaining_credit == 0 {
    match events.recv().await {
        Some(ChannelEvent::Credit(n)) => {
            remaining_credit = remaining_credit.saturating_add(n);
        }
        Some(ChannelEvent::Close) | None => break,
        _ => {}
    }
    continue;
}
```

### Half-close + ChannelClose notification

**Source:** `crates/nosh-server/src/channel.rs` lines 128–134; `crates/nosh-client/src/channel.rs` lines 149–163
**Apply to:** both the scrollback server task and the scrollback client drain task

```rust
let _ = ch_send.finish();
let _ = tokio::time::timeout(Duration::from_secs(2), ch_send.stopped()).await;
let _ = control_tx.send(Message::ChannelClose { channel_id }).await;
```

### `with_terminal_state` read pattern

**Source:** `crates/nosh-server/src/registry.rs` lines 536–542
**Apply to:** server pump code that reads scrollback for `ScrollbackPage` construction

```rust
pub fn with_terminal_state<F, R>(&self, f: F) -> R
where F: FnOnce(&TerminalState) -> R,
{
    let ts = self.terminal_state.lock().unwrap_or_else(|e| e.into_inner());
    f(&ts)
}
```

Caller contract: do NOT `.await` inside the closure. Capture epoch before entering the lock; the `current_epoch` local cannot change while the synchronous closure runs (same task, same `select!` arm).

### Scrollback vs. datagram type safety (S-1)

**Source:** architecture decision in CONTEXT.md + `run_echo_loop` which accepts only `&mut quinn::SendStream`
**Apply to:** `run_scrollback_sender_task` signature

The function must accept `&mut quinn::SendStream` (not `&quinn::Connection`). There is no `send_datagram` call path reachable from this function. This is the type-level enforcement of the reliable-only constraint.

---

## No Analog Found

All files have close analogs. No entries.

---

## Metadata

**Analog search scope:** `crates/nosh-proto/src/`, `crates/nosh-server/src/`, `crates/nosh-client/src/`, `crates/nosh-client/tests/`
**Files scanned:** 9 source files read in full or targeted excerpts
**Pattern extraction date:** 2026-06-12
