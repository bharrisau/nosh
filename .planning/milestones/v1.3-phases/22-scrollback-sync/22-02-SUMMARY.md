---
phase: 22-scrollback-sync
plan: "02"
subsystem: nosh-server
tags: [scrollback, channel, reliable-stream, epoch, tdd, security]
dependency_graph:
  requires: [22-01-SUMMARY.md]
  provides: [run_scrollback_sender_task, Scrollback channel accept in both session paths, epoch_src mirror for S-5]
  affects: [crates/nosh-server/src/channel.rs, crates/nosh-server/src/server.rs]
tech_stack:
  added: []
  patterns:
    - credit-pause/replenish loop (mirrors run_echo_loop, MUX-03)
    - Arc<AtomicU64> epoch mirror updated at each diff tick (S-5 atomic epoch capture)
    - stream-bind await loop before task body (same as run_channel_task phase-1)
    - match channel_type dispatch in spawn block (Scrollback vs. generic)
key_files:
  created: []
  modified:
    - crates/nosh-server/src/channel.rs
    - crates/nosh-server/src/server.rs
decisions:
  - run_scrollback_sender_task reads epoch_src.load(Acquire) into a local immediately before with_terminal_state closure — no .await between them (S-5 code-structure invariant; concurrency correctness deferred to 22-04 scrollback_epoch_handoff_no_gap)
  - epoch_src is an additive Arc<AtomicU64> mirror of current_epoch; stored with Release ordering at each diff tick in both run_session and run_reattach_session; does not change epoch cadence, confirmed_epoch logic, or datagram sends
  - Spawn block dispatches on channel_type: Scrollback spawns a closure that awaits the stream-bind event then calls run_scrollback_sender_task; all other accepted types use run_channel_task unchanged
  - count clamped to MAX_PAGE_SIZE = 1024 before scrollback_lines call (T-22-08 / V5 allocation cap)
  - Credit pre-check before write: if encoded page exceeds remaining_credit, task blocks on events.recv() for Credit before writing (inner credit-wait loop inside the ScrollbackRequest arm)
metrics:
  duration: 30
  completed: "2026-06-12"
  tasks: 2
  files: 2
---

# Phase 22 Plan 02: Server Scrollback Sender Task Summary

Production `run_scrollback_sender_task` in channel.rs (reliable SendStream-only, byte-credit paced, epoch_src Acquire-load for S-5) wired to both fresh and reattach session pumps with `ChannelType::Scrollback` accepted and `epoch_src` mirroring the diff-tick counter.

## Tasks Completed

| Task | Name | Commit | Files |
|------|------|--------|-------|
| 1 | Add run_scrollback_sender_task (reliable SendStream-only, byte-credit paced) | f687283 | crates/nosh-server/src/channel.rs |
| 2 | Flip the Scrollback accept gate + spawn the sender in both session paths | f2e3f82 | crates/nosh-server/src/server.rs |

## Verification Results

All plan success criteria met:

- `cargo build -p nosh-server` succeeds (both tasks).
- `grep -A60 "fn run_scrollback_sender_task" channel.rs | grep send_datagram` returns empty (S-1 type-level proof — no Connection parameter, no datagram-send path reachable).
- Both pump paths accept `ChannelType::Scrollback`; `PortForward`/`AgentForward` still rejected; `Echo` still test-gated.
- `epoch_src.store(current_epoch, Ordering::Release)` present at lines 930 and 1798 — one per diff-tick in `run_session` and `run_reattach_session`.
- No `mpsc::unbounded_channel` on the scrollback path; `mpsc::channel(64)` bounded throughout.
- M-6 comments present at spawn sites in both session functions.
- `cargo test -p nosh-server`: 113 passed, 0 failed (no regression in existing channel/echo tests).
- `count` clamped to `MAX_PAGE_SIZE = 1024` before `scrollback_lines` call (V5 allocation cap verified in channel.rs).
- `epoch_src.load(Acquire)` read into `epoch_at_snapshot` local immediately before `with_terminal_state` closure — no `.await` between them (S-5 code structure).
- S-5 concurrency-correctness proof deferred to 22-04 `scrollback_epoch_handoff_no_gap` integration test.

## Deviations from Plan

None — plan executed exactly as written.

## Known Stubs

None — `run_scrollback_sender_task` is a full production implementation. The server now serves scrollback pages over a reliable channel on request.

## Threat Flags

All threat surfaces introduced by this plan are covered by the plan's threat_model:

| Flag | File | Description |
|------|------|-------------|
| T-22-05 mitigated | channel.rs | `run_scrollback_sender_task` has no `send_datagram` reachable path (S-1); grep proof passes |
| T-22-06 mitigated | server.rs | Separate `tokio::spawn` task; bounded `mpsc(64)`; pump uses `try_send` + drop-oldest for any future pump-side push (M-6) |
| T-22-07 mitigated | channel.rs | `epoch_src.load(Acquire)` into local before `with_terminal_state` — no `.await` between (S-5 code structure) |
| T-22-08 mitigated | channel.rs | `count.min(MAX_PAGE_SIZE)` before `scrollback_lines` (V5) |
| T-22-09 mitigated | channel.rs | Request read on `ch_recv`; response written on `ch_send`; control traffic via `control_tx` only (M-2) |
| T-22-10 mitigated | server.rs | Only `Scrollback` flipped to accept; `PortForward`/`AgentForward` remain `false`; `Echo` test-gated |

## Self-Check: PASSED

Files verified:
- `crates/nosh-server/src/channel.rs` — contains `fn run_scrollback_sender_task` with `epoch_src: Arc<...AtomicU64>` parameter, no `send_datagram` token, `MAX_PAGE_SIZE` constant ✓
- `crates/nosh-server/src/server.rs` — contains `epoch_src.store` at two diff-tick sites, `run_scrollback_sender_task` call, `ChannelType::Scrollback => true` in both accept matches ✓

Commits verified:
- `f687283` (feat Task 1 — channel.rs) ✓
- `f2e3f82` (feat Task 2 — server.rs) ✓
