---
phase: 13-server-datagram-sender
plan: "02"
subsystem: nosh-server
tags: [datagram, epoch-ack, diff-interval, terminal-state, session-pump, resume-complete]
dependency_graph:
  requires: [13-01-SUMMARY.md]
  provides: [diff-interval arm in run_session, diff-interval arm in run_reattach_session, epoch-ack arm, ResumeComplete gate, compute_diff_runs, build_state_diff]
  affects: [13-03-PLAN.md]
tech_stack:
  added: []
  patterns: [coalesced-tick datagram emission, acked-epoch baseline, ResumeComplete sequential gate, deferred-run queue with MAX_RUNS cap]
key_files:
  created: []
  modified:
    - crates/nosh-server/src/server.rs
decisions:
  - resume_complete declared as immutable true after replay loop in run_reattach_session (not a mut false/true pair) — sequential code structure guarantees no datagram fires during replay without needing a channel or AtomicBool (Pitfall 5)
  - compute_diff_runs breaks on style change (not on first unchanged cell) to reduce run fragmentation while remaining correct under the acked-epoch self-correcting model
  - DiffTickResult.epoch field removed since caller tracks current_epoch directly — avoids dead_code warning and redundancy
  - Epoch increments at tick time when cells != last_sent_snapshot (not per PTY chunk) — decouples epoch semantics from the output buffer sequence counter (Open Question 2)
  - Pending deferred queue capped at MAX_RUNS (4096) each tick; excess oldest runs dropped (T-13-06 DoS guard, Pitfall 3)
metrics:
  duration: "~8 minutes"
  completed: "2026-06-02"
  tasks_completed: 2
  tasks_total: 2
  files_modified: 1
---

# Phase 13 Plan 02: Server Datagram Sender — Session Pump Integration Summary

16ms diff-interval tick arm and epoch-ack read_datagram arm added to both `run_session` and `run_reattach_session`, using a shared `build_state_diff` / `compute_diff_runs` helper; datagrams gated by a `resume_complete` bool that is structurally true-after-replay in `run_reattach_session` and immediately true in `run_session`.

## Tasks Completed

| Task | Name | Commit | Files |
|------|------|--------|-------|
| 1 | Shared diff-tick helper + sender/ack arms in run_session | 62a4da9 | server.rs |
| 2 | ResumeComplete-gated sender/ack arms in run_reattach_session | f877034 | server.rs |

## What Was Built

### Task 1: Shared helpers + run_session arms

**New imports added to server.rs:**
- `bytes::Bytes`
- `nosh_proto::datagram::{encode_datagram, decode_epoch_ack, StateDiff, DiffRun, MIN_CAP, MAX_RUNS}`
- `crate::terminal::Cell`

**`compute_diff_runs(current, baseline) -> Vec<DiffRun>`** — free function:
- Scans `current` rows against `baseline` (empty baseline = all cells changed = full screen)
- Starts a run on the first differing cell, extends while style/fg/bg match, breaks on attribute change
- Produces compact runs without over-fragmenting identical-style regions

**`build_state_diff(slot, current_epoch, last_acked_epoch, last_acked_snapshot, last_sent_snapshot, pending_deferred, cap) -> Option<DiffTickResult>`** — free function:
- Snapshots TerminalState via `slot.with_terminal_state(|ts| {...})` — lock released before closure returns, NO async operation inside (Pitfall 1 / Anti-Pattern #2 enforced)
- D-13-02a: returns `None` if grid unchanged and client caught up
- Increments `current_epoch` at tick time when `cells != last_sent_snapshot` (not per chunk)
- Prepends `pending_deferred` before fresh runs (cursor-proximate priority preserved)
- Caps combined run list at `MAX_RUNS = 4096` (T-13-06 DoS guard, Pitfall 3)
- Calls `encode_datagram(&diff, cap)` and returns `Some(DiffTickResult { payload, sent_cells, deferred })`

**`run_session` additions (before the select! loop):**
- `diff_interval = tokio::time::interval(Duration::from_millis(16))` with `MissedTickBehavior::Skip`
- `resume_complete = true` (fresh session, no replay window)
- `current_epoch`, `last_acked_epoch`, `last_acked_snapshot`, `last_sent_snapshot`, `pending_deferred`

**`diff_interval` arm in run_session select!:**
- Checks `if !resume_complete { continue; }` (dead code for run_session; kept for symmetry)
- Reads `conn.max_datagram_size()`, skips silently if `None` or `< MIN_CAP`
- Calls `build_state_diff`; on success sends via `conn.send_datagram(payload)`
- `SendDatagramError` match: `TooLarge => {}` (skip), `UnsupportedByPeer | Disabled | ConnectionLost(_) => break SessionEnd::TransportLost`

**Epoch-ack arm in run_session select!:**
- `datagram = conn.read_datagram() => { ... }`
- `decode_epoch_ack(&bytes)`: on `Ok(acked) if acked > last_acked_epoch` advances baseline; older/dup acks ignored (Pitfall 2 anti-regression)
- Snapshots `last_acked_snapshot` via `slot.with_terminal_state` — lock not held across any await
- `Err(_) => break SessionEnd::TransportLost`

### Task 2: run_reattach_session arms

Identical diff_interval + epoch-ack arms added to `run_reattach_session`, reusing the same helpers.

**ResumeComplete gate (D-13-03 / Pattern 4):**
- `let resume_complete = true;` declared AFTER the replay loop and AFTER the "replay complete" `tracing::info!`
- Sequential code flow: any `re_orphan + return Ok(())` inside the replay loop prevents the select! loop from ever starting — structural guarantee that all replay is complete before any datagram fires (T-13-04)
- No `Arc<AtomicBool>` or channel needed (Pitfall 5: same async task, sequential execution)

**Initialization before reattach select! loop:**
- Same `diff_interval`, `current_epoch`, `last_acked_epoch`, `last_acked_snapshot`, `last_sent_snapshot`, `pending_deferred` as run_session
- D-13-01b: empty `last_acked_snapshot` means first post-resume diff is naturally a full-screen diff — no separate keyframe path needed

**Replay loop unchanged:**
- `for (_seq, data) in &chunks { ... }` — byte-identical to before (D-13-04 additive)

## Test Results

- `cargo build -p nosh-server`: clean (0 warnings after DevX fix)
- `cargo clippy -p nosh-server -- -D warnings`: clean
- `cargo test -p nosh-server --lib`: 79 passed, 0 failed (pre-existing tests preserved)

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 1 - Bug] Changed resume_complete initialization pattern**
- **Found during:** Task 2 build (clippy/rustc `unused_assignments` warning)
- **Issue:** Plan described `let mut resume_complete = false;` before the replay loop, then `resume_complete = true;` after. Since the replay loop is sequential (not running concurrently with the select! loop), the `false` value is never read — Rust flags it as an unused assignment.
- **Fix:** Declare `let resume_complete = true;` only once, immediately after the "replay complete" log. Structural guarantee: any error inside the replay loop causes `return Ok(())`, which prevents the select! loop from starting. This is the semantically correct and warning-free equivalent.
- **Files modified:** crates/nosh-server/src/server.rs
- **Commit:** f877034 (bundled with Task 2)
- **Plan reference:** PATTERNS.md lines 134-145 also shows `let mut resume_complete = true;` set after the replay log as the intended pattern.

**2. [Rule 1 - Bug] Removed DiffTickResult.epoch field**
- **Found during:** Task 1 build (dead_code warning)
- **Issue:** `DiffTickResult` had an `epoch: u64` field that was set but never read by the callers in both session loops (callers track `current_epoch` directly via the mutable reference passed to `build_state_diff`).
- **Fix:** Removed the `epoch` field from `DiffTickResult`. The field was redundant since callers already own `current_epoch` via the `&mut u64` argument.
- **Files modified:** crates/nosh-server/src/server.rs
- **Commit:** 62a4da9 (bundled with Task 1)

## Known Stubs

None. Both session functions are fully wired: diff snapshot, run computation, datagram emission, and epoch-ack baseline advancement all execute on every qualifying tick.

## Threat Flags

No new trust boundary surfaces introduced. All threats from the plan's threat register are addressed:
- T-13-04 (Info Disclosure during replay): `resume_complete` gate enforced structurally — sequential code, select! loop starts only after all replay frames are sent
- T-13-05 (Tampering/Replay of epoch-ack): `acked > last_acked_epoch` guard in both session arms; stale/dup/forged-older acks cannot regress the baseline
- T-13-06 (DoS via deferred run accumulation): `MAX_RUNS = 4096` cap applied in `build_state_diff` each tick; excess oldest runs dropped
- T-13-07 (DoS via malformed inbound datagram): `decode_epoch_ack` returns `Err` for unknown/malformed input; both arms discard the error and continue

## Self-Check: PASSED

- `crates/nosh-server/src/server.rs` — exists; contains `diff_interval`, `build_state_diff`, `compute_diff_runs`, `decode_epoch_ack` arms, `resume_complete`
- Commits 62a4da9 and f877034 confirmed in git log
- `cargo build -p nosh-server`: exit 0
- `cargo clippy -p nosh-server -- -D warnings`: exit 0
- `cargo test -p nosh-server --lib`: 79 passed, 0 failed
