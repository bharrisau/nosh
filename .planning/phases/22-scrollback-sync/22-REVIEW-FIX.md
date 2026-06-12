---
phase: 22-scrollback-sync
fixed_at: 2026-06-12T00:00:00Z
review_path: .planning/phases/22-scrollback-sync/22-REVIEW.md
iteration: 1
findings_in_scope: 13
fixed: 12
skipped: 1
status: partial
---

# Phase 22 — Scrollback Sync — Code Review Fix Report

**Fixed at:** 2026-06-12
**Source review:** .planning/phases/22-scrollback-sync/22-REVIEW.md
**Iteration:** 1

**Summary:**
- Findings in scope: 13
- Fixed: 12
- Skipped: 1

## Fixed Issues

### CR-C-01: `epoch_at_snapshot: 0` makes scrollback non-functional

**Files modified:** `crates/nosh-client/src/main.rs`
**Commit:** caefd27
**Applied fix:** Replaced `epoch_at_snapshot: 0` with `screen.last_applied_epoch()` at the `ScrollbackView::Live → Active` transition. Added `epoch_at_snapshot_seeded_to_zero_causes_premature_exit` test that demonstrates the broken behaviour (seed=0 causes immediate gate fire on live_epoch=7) and the correct behaviour (seed=7 keeps Active until catchup_epoch=7).

### WR-P-01: S-5 epoch/scrollback TOCTOU

**Files modified:** `crates/nosh-server/src/channel.rs`
**Commit:** dcb4f15
**Applied fix:** Moved the `epoch_src.load(Ordering::Acquire)` call inside the `with_terminal_state` closure. The closure now returns a triple `(epoch, lines, total)` so both values are read under the same mutex acquisition, eliminating the window where a PTY task could push new scrollback lines between the atomic load and the mutex lock.

### WR-S-01: `try_send(ChannelEvent::Close)` can silently drop close signal

**Files modified:** `crates/nosh-server/src/server.rs`
**Commit:** 3fe9a5a
**Applied fix:** Changed both `ChannelClose` arms (in `run_session` and `run_reattach_session`) from `task_tx.try_send(ChannelEvent::Close)` to `task_tx.send(ChannelEvent::Close).await`. This ensures the close signal is delivered even when the 64-slot queue is momentarily full, mirroring the existing WR-01 Stream-event fix.

### WR-S-02: inner credit-wait loop can block indefinitely

**Files modified:** `crates/nosh-server/src/channel.rs`
**Commit:** 5a8cbc4
**Applied fix:** Replaced the unbounded `events.recv().await` in the inner credit-wait loop with `tokio::time::timeout_at(credit_wait_deadline, events.recv())` using a 30-second deadline. On timeout, the task logs a warning and closes the channel cleanly via `ch_send.finish()` + `control_tx.send(ChannelClose)`.

### WR-C-01: credit fallback of 0 understates consumed bytes

**Files modified:** `crates/nosh-client/src/channel.rs`
**Commit:** 3e1e718
**Applied fix:** Changed the `Err(_)` arm of the re-encode match in `run_scrollback_drain_task` from returning `0` to returning `nosh_proto::codec::MAX_FRAME_LEN as u64 + 4`. An overcount grants more credit than strictly needed (safe) whereas undercounting to 0 would permanently starve the server's flow-control window.

### WR-C-02: Active entered even when scrollback channel unavailable

**Files modified:** `crates/nosh-client/src/main.rs`
**Commit:** 04923c7
**Applied fix:** Introduced `scrollback_available: bool` set from `scrollback_streams.is_some()` before consuming the streams. The `ScrollbackView::Live → Active` transition now checks `if !scrollback_available` and stays in Live with a debug log when the channel was not successfully opened. Also fixed IN-C-01 simultaneously (see below).

### WR-P-02: `ChannelType` discriminants unguarded by stability test

**Files modified:** `crates/nosh-proto/src/codec.rs`
**Commit:** f3d9717
**Applied fix:** Added `channel_type_discriminant_order_is_stable` test to `codec.rs` mirroring `message_discriminant_order_is_stable`. Pins Echo=0, Scrollback=1, PortForward=2, AgentForward=3. Test passes.

### IN-P-02: `cells` doc says trailing blanks "may be omitted"

**Files modified:** `crates/nosh-proto/src/messages.rs`
**Commit:** 8399e60
**Applied fix:** Updated the `ScrollbackLine.cells` doc comment to say "All cells in the line are always sent; length == `width`" instead of "Length ≤ `width` (trailing blank cells may be omitted)".

### IN-S-01: stale "until Phase 22-02 wires the scrollback handler" comment

**Files modified:** `crates/nosh-server/src/server.rs`
**Commit:** 162c112
**Applied fix:** Rewrote the comment and log string at the scrollback-frames-on-control-stream arm in `run_session` to describe the current (complete) invariant rather than a deferred future state.

### IN-S-02: `run_reattach_session` drops scrollback frames with bare `Ok(_) => {}`

**Files modified:** `crates/nosh-server/src/server.rs`
**Commit:** 1797ce2
**Applied fix:** Added an explicit `Ok(Message::ScrollbackRequest) | Ok(Message::ScrollbackPage) | Ok(Message::ScrollbackCredit)` match arm with a `tracing::debug!` log before the catch-all `Ok(_) => {}`, mirroring the explicit arm in `run_session`.

### IN-C-01: `scrollback_ctrl_tx.clone()` where original is unused

**Files modified:** `crates/nosh-client/src/main.rs`
**Commit:** 04923c7 (combined with WR-C-02)
**Applied fix:** Changed `scrollback_ctrl_tx.clone()` to `scrollback_ctrl_tx` (move) in the `run_scrollback_drain_task` spawn, since the original sender is never used after that point.

### IN-C-02: CSI `pending` accumulator is unbounded without assertion

**Files modified:** `crates/nosh-client/src/main.rs`
**Commit:** 7f263ff
**Applied fix:** Added `debug_assert!(self.pending.len() <= 16, ...)` after the `extend_from_slice` in `CsiAccumulator::process` to catch any violation of the "at most 8 bytes" invariant in debug builds.

## Skipped Issues

### IN-P-01: `ScrollbackLine.width` carries post-resize width (S-3 metadata)

**File:** `crates/nosh-proto/src/messages.rs`
**Reason:** Reviewer explicitly noted "leave as-is (pre-existing representation limitation)". The `Vec<Cell>` representation cannot carry the original live width after a resize without a protocol change. No code change made per review instruction.

---

**Build result:** `cargo build --workspace` — clean, 0 errors, 0 warnings
**Test result:** `cargo test --workspace` — all tests pass (0 failures across all crates)

---

_Fixed: 2026-06-12_
_Fixer: Claude (gsd-code-fixer)_
_Iteration: 1_
