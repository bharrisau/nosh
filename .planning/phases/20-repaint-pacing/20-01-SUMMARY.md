---
phase: 20-repaint-pacing
plan: 01
subsystem: server
tags: [quic, datagram, burst, repaint, epoch, server-tick]

# Dependency graph
requires:
  - phase: 19-full-screen-tui-rendering-correctness
    provides: terminal state model with alt-screen + grapheme support
provides:
  - send_burst() free function draining deferred runs via encode_datagram-only per tick
  - Extended DiffTickResult with cols/rows/cursor/alt_screen geometry for burst iterations
  - BURST_CAP = 64 safety cap constant (D-20-02)
  - burst_drains_when_grid_differs_from_acked_baseline unit test (PACE-03 RED/GREEN gate)
  - one_epoch_per_tick unit test (PACE-02 server-side gate)
  - Both run_session and run_reattach_session tick arms burst-capable (Pitfall 6 fixed)
affects: [20-02-client-apply-guard, nosh-server, phase-21, phase-22]

# Tech tracking
tech-stack:
  added: []
  patterns:
    - "Burst drain: build_state_diff called once per tick, encode_datagram-only for iterations 2..N (R-1 fix)"
    - "One epoch per tick: all burst datagrams share result.epoch, never incrementing current_epoch in the burst loop (R-2 fix)"
    - "send_burst() free function (not a method) — quinn::Connection is not mockable; testable indirectly via unit tests on encode_datagram-only drain"
    - "datagram_send_buffer_space() as per-iteration budget gate (D-20-01)"
    - "BURST_CAP = 64 as safety bound (D-20-02; ~2.5-4x full 80x24 repaint)"

key-files:
  created: []
  modified:
    - crates/nosh-server/src/server.rs

key-decisions:
  - "BURST_CAP = 64 chosen as ~2.5-4x a full 80x24 repaint (16-24 datagrams at 1200-byte MTU); bound is generous enough to never trip in normal TUI use but caps pathological diffs at ~76KB per tick"
  - "send_burst() returns (Vec<DiffRun>, bool) — leftover deferred + transport-lost flag — so the tick arm can assign pending_deferred and break SessionEnd::TransportLost without any await inside the function"
  - "std::mem::take(&mut deferred) inside the burst loop avoids Rust move-after-use; the loop still terminates correctly when encode_datagram returns Err (unreachable in practice)"
  - "Unit tests use have_sh() guard + real /bin/sh session to call build_state_diff; populate 80x24 terminal via VT escape sequences rather than spawning a real interactive PTY session"

patterns-established:
  - "Pattern: burst drain is encode_datagram-only; build_state_diff is called at most once per tick (outside any loop)"
  - "Pattern: epoch_snapshots.push_back called exactly once per tick, outside the burst loop"

requirements-completed: [PACE-01, PACE-02, PACE-03]

# Metrics
duration: 25min
completed: 2026-06-11
---

# Phase 20 Plan 01: Burst Repaint Pacing — Server Tick Summary

**send_burst() helper drains a full StateDiff in one tick via encode_datagram-only, with BURST_CAP=64 safety bound and one epoch per tick, architecturally preventing the R-1 infinite-spin and R-2 noecho-epoch regressions from the reverted 999.4 attempt**

## Performance

- **Duration:** ~25 min
- **Started:** 2026-06-11T00:00:00Z
- **Completed:** 2026-06-11T00:25:00Z
- **Tasks:** 2
- **Files modified:** 1

## Accomplishments

- Extended `DiffTickResult` with `cols`, `rows`, `cursor`, `alt_screen` geometry fields so send_burst() can construct burst `StateDiff` objects without re-locking the terminal slot (D-20 Pitfall 4 designed out).
- Added `send_burst()` free function that sends the first datagram from `build_state_diff` then drains remaining `DiffRun`s via `encode_datagram` only — no `build_state_diff` call inside the loop (D-20-03 / R-1 architectural fix); all burst datagrams share the tick's single epoch (D-20-04 / R-2 architectural fix).
- Wired `send_burst()` into both `run_session` and `run_reattach_session` tick arms, with `epoch_snapshots.push_back` called exactly once per tick outside the loop (Pitfall 5 / EPOCH_SNAPSHOT_CAP not blown by burst size).
- Added `burst_drains_when_grid_differs_from_acked_baseline` and `one_epoch_per_tick` unit tests; both green as part of the 108-test nosh-server suite.

## Task Commits

1. **Task 1: RED-before burst drain + one-epoch-per-tick unit tests** - `ab2cf7a` (test)
2. **Task 2: Extend DiffTickResult + add send_burst() wired into both tick arms** - `4171147` (feat)

## Files Created/Modified

- `crates/nosh-server/src/server.rs` — `DiffTickResult` extended with geometry fields; `BURST_CAP = 64` constant; `send_burst()` free function; both diff-tick arms in `run_session` and `run_reattach_session` updated to call `send_burst()`; two unit tests added

## Decisions Made

- `BURST_CAP = 64`: ~2.5–4x a full 80x24 repaint at typical MTU (1200 bytes); generous enough to never fire in normal TUI use (vim startup needs ~16–24 datagrams), bounds pathological diffs at ~76 KB per tick (well within the default 1 MiB buffer). T-20-02 (DoS) mitigated.
- `send_burst()` returns `(Vec<DiffRun>, bool)` instead of `Result<...>`: separates leftover deferred from transport-lost signal cleanly; the tick arm assigns `pending_deferred = leftover` and breaks on the flag, matching the existing code shape.
- Unit tests guard with `have_sh()` and create a real `/bin/sh` session to access `build_state_diff` (which takes a `SessionSlot`). The terminal is populated via VT escape sequences through `push_output_and_parse` — no interactive PTY I/O needed during the test.

## Deviations from Plan

**[Rule 1 - Bug] Fixed Rust move-after-use in burst loop**
- **Found during:** Task 2 (first compile attempt)
- **Issue:** `runs: deferred` inside `StateDiff { ... }` moved `deferred` out of the loop variable; after the `Err(_) => break` arm, `deferred` was used in the return statement despite having been moved.
- **Fix:** Changed `runs: deferred` to `runs: std::mem::take(&mut deferred)` so `deferred` is replaced with an empty Vec (via mem::take) and then reassigned from `next_deferred` each iteration. When the Err arm breaks, `deferred` is empty (the move consumed it), which is the correct leftover state.
- **Files modified:** `crates/nosh-server/src/server.rs` (send_burst burst loop)
- **Verification:** `cargo build -p nosh-server` clean; all tests pass.
- **Committed in:** `4171147` (Task 2 commit)

---

**Total deviations:** 1 auto-fixed (Rule 1 — bug)
**Impact on plan:** The fix is mechanical (Rust borrow checker enforcement); correctness is unchanged. The `deferred` Vec is empty after the take() when encode_datagram returns Err (unreachable at runtime), which is the right leftover state to carry to the next tick.

## Issues Encountered

None beyond the borrow-checker fix above.

## Known Stubs

None. The burst loop is fully wired. `pending_deferred` is correctly updated from `send_burst()`'s leftover return value in both tick arms.

## Threat Flags

None. No new network endpoints, auth paths, file access patterns, or schema changes introduced. All threat mitigations in the plan's STRIDE register (T-20-01 through T-20-04) are implemented:
- T-20-01 (info disclosure via epoch): send_burst reuses `result.epoch`, never calls `build_state_diff` (which increments epoch).
- T-20-02 (DoS via burst): `datagram_send_buffer_space() >= cap` gate + BURST_CAP = 64 bound.
- T-20-03 (R-1 infinite spin): encode_datagram-only drain; `burst_drains_when_grid_differs_from_acked_baseline` test asserts finite termination.
- T-20-04 (epoch_snapshots overflow): one push_back per tick; EPOCH_SNAPSHOT_CAP unchanged.

## Self-Check

Files created/modified:
- FOUND: crates/nosh-server/src/server.rs

Commits:
- FOUND: ab2cf7a
- FOUND: 4171147

## Self-Check: PASSED

## Next Phase Readiness

- Plan 20-02 (client apply() guard `<=` → `<`, `apply_same_epoch_burst_applies` test, `apply_monotonic_same_epoch_is_noop` rename) can proceed immediately — this plan's server-side burst is active.
- `noecho_read_dash_s_zero_predicted_chars` is the mandatory D-20-09 CI gate (non-`#[ignore]`, owned by 20-02); must pass with burst code active before the phase is complete.
- SC1 (80x24 repaint drains in one tick): `burst_drains_when_grid_differs_from_acked_baseline` passes, providing structural proof; live RTT timing is a manual/observational check (0-RTT loopback trivially satisfies the ≤2 RTT criterion).

---
*Phase: 20-repaint-pacing*
*Completed: 2026-06-11*
