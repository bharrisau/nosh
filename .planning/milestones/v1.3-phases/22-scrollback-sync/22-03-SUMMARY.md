---
phase: 22-scrollback-sync
plan: "03"
subsystem: nosh-client
tags: [scrollback, channel, csi, tdd, state-machine]
dependency_graph:
  requires: ["22-01", "22-02"]
  provides: ["scrollback-client-drain", "scrollback-view-state-machine", "csi-interception", "reattach-reopen"]
  affects: ["crates/nosh-client/src/channel.rs", "crates/nosh-client/src/main.rs"]
tech_stack:
  added: []
  patterns:
    - tokio::select! bidirectional drain (concurrent req write + page read on same QUIC bidi stream)
    - rolling CSI accumulator for split-read-safe sequence detection (Pitfall 6)
    - mpsc-gated ScrollbackRequest routing to avoid M-2 deadlock (Open Question 1)
    - epoch-gated display suspension with ack-flow continuity (SCROLL-05 / T-22-12)
key_files:
  created: []
  modified:
    - crates/nosh-client/src/channel.rs
    - crates/nosh-client/src/main.rs
decisions:
  - "ScrollbackRequest travels on channel's own ch_send (not control stream) via req_rx → drain task, avoiding M-2 deadlock"
  - "run_scrollback_drain_task uses tokio::select! to concurrently handle inbound requests (ch_send) and outbound pages (ch_recv)"
  - "Scrollback channel opened inside run_pump so both fresh and reattach paths converge on the same re-open logic"
  - "Datagram arm while Active emits epoch acks but suppresses display; exits on epoch >= epoch_at_snapshot (SCROLL-05)"
metrics:
  duration: "~45m (continuation executor)"
  completed: "2026-06-12"
  tasks_total: 4
  tasks_completed: 4
  files_changed: 2
---

# Phase 22 Plan 03: Scrollback Client Drain + View State Machine Summary

Client scrollback implemented end-to-end: a drain task decoding ScrollbackPage frames from the dedicated QUIC channel, a ScrollbackView state machine intercepting Shift-PageUp/Down CSI sequences in run_pump, and a cold-reattach re-open path that always starts in Live mode.

## Tasks Completed

| Task | Name | Commit | Files |
|------|------|--------|-------|
| 1 | run_scrollback_drain_task (decode + credit) | c53325c | channel.rs |
| 2a | ScrollbackView + CsiAccumulator + CSI interception | 95ffa7a | main.rs |
| 2b | page_rx arm + epoch gate + lazy prefetch | d724efa + 68b0fbe | main.rs |
| 3 | Cold-reattach re-open (Task 3 + Open Question 1 wiring) | 68b0fbe | channel.rs, main.rs |

## What Was Built

### channel.rs — run_scrollback_drain_task (6-arg)

The drain task uses `tokio::select!` to concurrently:
- Write `ScrollbackRequest` frames arriving from `req_rx` (sent by `run_pump`) onto `ch_send` — the channel's own QUIC send stream. This is the M-2-safe routing path (Open Question 1): requests travel on the dedicated channel stream, not the shared control stream, so they cannot deadlock control-stream flow control.
- Read `ScrollbackPage` frames from `ch_recv`, track byte consumption, replenish credit via `control_tx` (never via direct stream write — A4), and deliver pages to `run_pump` via `page_tx.try_send` (drop-on-full when Live — T-22-14).

### main.rs — ScrollbackView state machine

`enum ScrollbackView { Live, Active { lines, offset, pending_request, epoch_at_snapshot, total_available } }` declared at module level. Freshly initialised as `Live` at the top of every `run_pump` call — no residual Active state can leak across reattach (T-22-13).

`CsiAccumulator` maintains a rolling 8-byte buffer to detect the 6-byte Shift-PageUp (`ESC [ 5 ; 2 ~`) and Shift-PageDown (`ESC [ 6 ; 2 ~`) sequences across split stdin reads (Pitfall 6 / T-22-11).

Stdin arm processing order (SCROLL-04):
1. `CsiAccumulator::process` scans for paging CSI sequences first.
2. Shift-PageUp: if Live → enter Active + send initial `ScrollbackRequest` via `scrollback_req_tx`; if already Active → increase offset + lazy prefetch if near top.
3. Shift-PageDown: decrease offset; if `offset <= rows` → auto-exit to Live.
4. Non-paging remainder: if Active → snap to Live first (T-22-15 LOCKED: keystroke delivered, not swallowed), then process through `EscapeState` + forward to shell.

### main.rs — page_rx arm (Task 2b)

While Active: prepend new lines to the front of the view buffer (oldest-first ordering), update `total_available`, `epoch_at_snapshot`, clear `pending_request`. While Live: drop page silently (T-22-14 / Open Question 3).

### main.rs — datagram epoch gate (Task 2b / SCROLL-05)

Inside the datagram arm, before display processing:
- If Active and `diff.epoch < epoch_at_snapshot`: suppress display, still emit epoch ack (T-22-12 — server pump never stalls), `continue` to next loop iteration.
- If Active and `diff.epoch >= epoch_at_snapshot`: set `scrollback_view = Live`, fall through and apply the diff normally (handoff).

### main.rs — Lazy prefetch (Task 2b / SCROLL-01)

On each Shift-PageUp while Active: if `!pending_request && lines.len() < total_available`, send `ScrollbackRequest { from_line: lines.len(), count: 256 }` via `scrollback_req_tx` and set `pending_request = true`. At true top-of-history (`lines.len() == total_available`) further prefetch is a no-op.

### Task 3 — Cold reattach re-open

`reattach_session` calls `run_pump`, which on entry creates a fresh `EvenIdAllocator::new()`, allocates channel id 2 (first even id, always fresh), and calls `client::open_channel(ChannelType::Scrollback)`. This converges fresh and reattach paths on a single re-open site. No old channel id from a prior connection is ever reused (Pitfall 5). `scrollback_view = ScrollbackView::Live` is declared at `run_pump` entry — each call starts clean (T-22-13).

## Threat Mitigations Applied

| Threat ID | Status | Notes |
|-----------|--------|-------|
| T-22-11 (CSI split-read) | Mitigated | CsiAccumulator rolling 8-byte accumulator |
| T-22-12 (server pump stall) | Mitigated | Epoch acks continue while Active; display only suspended |
| T-22-13 (reattach view leak) | Mitigated | scrollback_view = Live at every run_pump entry |
| T-22-14 (unbounded buffer) | Mitigated | try_send drop-on-full in drain task |
| T-22-15 (keystroke swallowed) | Mitigated | snap-back always forwards the triggering byte |

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 2 - Missing critical functionality] Open Question 1 — ScrollbackRequest routing on channel's own stream**

- Found during: Task 3 / continuation executor starting point
- Issue: The committed Task 2a stub had a TODO comment noting requests need to travel on `ch_send`, but only provided a 5-arg drain task with no receive path for requests. Requests sent on the control stream would violate the M-2 flow-control safety invariant.
- Fix: Updated `run_scrollback_drain_task` to a 6-arg signature adding `req_rx: mpsc::Receiver<Message>`; restructured the inner loop to use `tokio::select!` for concurrent request writes (ch_send) and page reads (ch_recv). Added `(scrollback_req_tx, scrollback_req_rx)` mpsc pair in `run_pump`; Shift-PageUp now sends `ScrollbackRequest` via `scrollback_req_tx`.
- Files modified: `channel.rs`, `main.rs`
- Commit: 68b0fbe

**2. [Rule 2 - Stub completion] Task 2b page_rx arm was a stub**

- Found during: continuation review
- Issue: The committed Task 2b had a stub `page_rx` arm that dropped all pages unconditionally.
- Fix: Implemented the full arm: prepend lines while Active (oldest-first), drop while Live; update metadata and clear pending_request.
- Commit: 68b0fbe

**3. [Rule 2 - Stub completion] Datagram epoch gate not implemented**

- Found during: continuation review
- Issue: The datagram arm had no scrollback epoch-gate logic — while Active it would apply diffs to the screen (incorrectly) and never exit to Live via the epoch boundary.
- Fix: Added the epoch gate before the display processing block. While Active and `diff.epoch < epoch_at_snapshot`: emit ack, `continue`. When `diff.epoch >= epoch_at_snapshot`: set Live and fall through.
- Commit: 68b0fbe

## Known Stubs

- Scrollback rendering: `page_rx` arm updates `lines` + `offset` state but does not yet render the history buffer to the terminal. The display path still goes through `render_with_predictor` (which shows the live screen). Rendering from the scrollback buffer is deferred to a follow-on plan — the state machine correctness and channel wiring are complete.

## Self-Check: PASSED

- `crates/nosh-client/src/channel.rs` — exists, modified ✓
- `crates/nosh-client/src/main.rs` — exists, modified ✓
- Commit 68b0fbe exists: ✓ (`git log --oneline | grep 68b0fbe`)
- `cargo build --workspace` — green ✓
- `cargo test --workspace` — all passed (nosh-client + nosh-server + nosh-proto + nosh-auth) ✓
- `enum ScrollbackView` in main.rs ✓
- `fn run_scrollback_drain_task` with 6 args in channel.rs ✓
- `req_rx` wired through drain task ✓
- Datagram epoch gate `>= epoch_at_snapshot` present ✓
- No "content discarded" stub string in main.rs ✓
- Reattach path reaches `run_pump` with fresh scrollback channel ✓
