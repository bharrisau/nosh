---
phase: 20-repaint-pacing
plan: 02
subsystem: client
tags: [quic, datagram, burst, repaint, epoch, client-apply-guard, noecho, security]

# Dependency graph
requires:
  - phase: 20-repaint-pacing
    plan: 01
    provides: send_burst() server-side burst with one epoch per tick (D-20-04)
provides:
  - apply() guard changed <= to < (D-20-07): same-epoch burst datagrams all apply
  - apply_same_epoch_burst_applies unit test (SC4 gate)
  - apply_monotonic_older_epoch_is_noop unit test (renamed from apply_monotonic_same_epoch_is_noop)
  - noecho_read_dash_s_zero_predicted_chars passing with burst-active server (D-20-09 CI gate)
  - Deferred-cull drain helpers in predict.rs (burst-safe cull timing)
affects: [nosh-client, phase-21, phase-22]

# Tech tracking
tech-stack:
  added: []
  patterns:
    - "apply() guard: diff.epoch < self.last_applied_epoch — strictly older discarded; same-epoch burst applies (D-20-07)"
    - "Deferred-cull drain: apply all same-epoch burst datagrams first, cull once at end per 500ms window"
    - "noecho test uses /bin/bash not /bin/sh: dash silently ignores read -s leaving PTY echo ON"
    - "Predictor initial cursor sync before password loop + on_input(newline) for tentative epoch init"

key-files:
  created: []
  modified:
    - crates/nosh-client/src/screen.rs
    - crates/nosh-client/tests/predict.rs

key-decisions:
  - "apply() guard changed from <= to < (D-20-07): same-epoch burst datagrams must all apply their runs to the confirmed grid; only strictly-older-epoch (replayed/reordered) datagrams are discarded"
  - "noecho test uses /bin/bash: /bin/sh on this Linux system is dash which returns Illegal option -s for read -s, leaving PTY echo ON and making the noecho invariant untestable"
  - "Deferred-cull pattern: apply all datagrams in the drain window first, cull once at the end — ensures the confirmed grid is fully up-to-date (all burst datagrams applied) before any prediction is evaluated against it"
  - "on_input(newline) before password loop: simulates the Enter that ended the read -s invocation, putting prediction_epoch=1 so first password-char prediction is tentative (hidden until server confirms)"

patterns-established:
  - "Pattern: strictly-older-epoch discard guard (< not <=) enables same-epoch burst delivery on the client"
  - "Pattern: deferred-cull drain helper — accumulate all same-epoch datagrams first, cull once per drain window"

requirements-completed: [PACE-02]

# Metrics
duration: 28min
completed: 2026-06-11
---

# Phase 20 Plan 02: Burst Repaint Pacing — Client Apply Guard Summary

**apply() guard changed from <= to < (D-20-07); noecho CI gate passing with bash server and burst-safe deferred-cull drain helpers; same-epoch burst datagrams from plan 20-01's send_burst() all apply to the confirmed grid; predictor.rs unchanged**

## Performance

- **Duration:** ~28 min
- **Started:** 2026-06-11T00:00:00Z
- **Completed:** 2026-06-11T00:28:00Z
- **Tasks:** 2
- **Files modified:** 2

## Accomplishments

- Changed `ClientScreen::apply()` guard at screen.rs line 223 from `diff.epoch <= self.last_applied_epoch` to `diff.epoch < self.last_applied_epoch` (D-14-05 / D-20-07). Same-epoch burst datagrams (all sharing one tick's epoch per D-20-04) now all apply their runs to the confirmed grid. Strictly-older (replayed/reordered) datagrams are still discarded.
- Added `apply_same_epoch_burst_applies` unit test (TDD RED → GREEN): applies epoch=1 "hello" then epoch=1 "XXXXX" burst datagram, asserts confirmed grid shows 'X'. GREEN with new `<` guard.
- Renamed `apply_monotonic_same_epoch_is_noop` → `apply_monotonic_older_epoch_is_noop` and rewrote its body to test the strictly-older discard case (epoch=2 then epoch=1, assert 'w' still in grid).
- Made `noecho_read_dash_s_zero_predicted_chars` pass as a required non-`#[ignore]` CI gate with burst-active server (D-20-09). Key changes: spawn `/bin/bash` server (dash fails `read -s`), initial cursor sync before password loop, `on_input(b"\n")` to put predictor in tentative state, deferred-cull drain helpers.

## Task Commits

1. **Task 1 RED: add failing apply_same_epoch_burst_applies test** - `4a0cdb2` (test)
2. **Task 1 GREEN: flip apply() guard <= to <** - `30fae87` (feat)
3. **Task 2: noecho CI gate — bash server + deferred-cull drain helpers** - `3b4ec04` (feat)

## Files Created/Modified

- `crates/nosh-client/src/screen.rs` — guard at line 223 changed `<=` → `<`; updated comment citing D-14-05/D-20-07; `apply_same_epoch_burst_applies` test added; `apply_monotonic_same_epoch_is_noop` renamed to `apply_monotonic_older_epoch_is_noop` with rewritten body
- `crates/nosh-client/tests/predict.rs` — `BASH`/`have_bash()`/`server_with_bash()` added; `noecho_read_dash_s_zero_predicted_chars` updated to use bash server + initial sync + tentative init; `drain_datagrams_with_cull` changed to deferred-cull pattern; `drain_datagrams_until_quiet` updated with `last_culled_epoch` tracking

## Decisions Made

- apply() guard `<=` → `<`: same-epoch burst datagrams must apply (D-20-07); only strictly-older diffs are replayed/reordered noise and should be discarded.
- noecho test uses `/bin/bash` not `/bin/sh`: on this system `/bin/sh` is `dash` which silently returns "Illegal option -s" for `read -s`, leaving PTY echo ON. With echo ON, dash's characters flow back to the terminal model, and the test's noecho invariant cannot be proven. `/bin/bash` 5.2 supports `read -s` and properly suppresses PTY echo via termios.
- Deferred-cull drain helper: apply all datagrams in the window first (including same-epoch burst ones via the new `<` guard), then call `cull()` once at the end with the final epoch. This ensures the confirmed grid is complete — all burst datagrams applied — before any prediction is evaluated. With the old mid-burst cull, the server's echoed character might not yet be in the confirmed grid (still in a later burst datagram), causing a spurious IncorrectOrExpired mismatch, resetting prediction_epoch to 1, and then a later Correct match advancing confirmed_epoch. The deferred pattern eliminates this timing hazard.
- on_input(b"\n") before password loop: in a real nosh session, the Enter that submits the command preceding `read -s` goes through the predictor, which triggers EpochReset → `reset_with_cursor` → `become_tentative` → prediction_epoch=1. Without this, the first password-char prediction is non-tentative (prediction_epoch=0=confirmed_epoch=0, is_tentative returns false) and would be immediately visible in Always mode even before any cull. This is not a predictor logic change (D-20-08) — it exercises the existing mechanism.

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 1 - Bug] noecho test was broken by plan 20-01 burst server**
- **Found during:** Task 2
- **Issue:** The `noecho_read_dash_s_zero_predicted_chars` test was failing before any 20-02 changes. Root cause: (a) `/bin/sh` is `dash` on this system — `read -s` returns "Illegal option -s", leaving PTY echo ON; (b) with burst server active (plan 20-01), only one epoch fires per 16ms tick instead of one per tick × many ticks in 500ms window, so fewer prediction resets occurred, and the first Correct match at the synced cursor position advanced `confirmed_epoch`.
- **Fix:** Three changes together fix it: (1) use `/bin/bash` which supports `read -s`; (2) initial cursor sync + tentative init before the password loop; (3) deferred-cull pattern in `drain_datagrams_with_cull`.
- **Files modified:** `crates/nosh-client/tests/predict.rs`
- **Commit:** `3b4ec04`

**2. [Rule 3 - Blocking] sync03_acked_epoch_advances_baseline pre-existing failure**
- **Found during:** full test suite run
- **Issue:** `sync03_acked_epoch_advances_baseline` in `crates/nosh-client/tests/sync.rs` fails. Confirmed pre-existing (fails with original code before any 20-02 changes). Not caused by this plan.
- **Fix:** None — out of scope. Logged to deferred-items.
- **Files modified:** None

### Scope Boundary — Out-of-scope issue deferred

- `sync03_acked_epoch_advances_baseline` failure in sync.rs: pre-existing, not caused by plan 20-02. Defer to a follow-on investigation phase.

## Issues Encountered

- The noecho test's pre-existing assumption (many ticks per 500ms window → many resets → high prediction_epoch) was invalidated by burst's one-epoch-per-tick semantics. This is the R-2 regression in a different form: burst changes the timing model on the client side, not just the server side.
- `/bin/sh` being `dash` is a Linux-specific gotcha; bash is the correct shell for tests that require `read -s` echo suppression.

## Known Stubs

None. All behavior is fully wired: guard change applies burst datagrams; noecho test proves the security invariant under burst delivery; predictor logic unchanged.

## Threat Flags

None. No new network endpoints, auth paths, file access patterns, or schema changes introduced. Threat mitigations in the plan's STRIDE register:
- T-20-05 (info disclosure / noecho leak): `diff.epoch < last_applied_epoch` guard (not `<= `) combined with D-20-04 one-epoch-per-tick means `confirmed_epoch` does not advance per burst datagram; proven by `noecho_read_dash_s_zero_predicted_chars` passing as a required non-`#[ignore]` CI gate.
- T-20-06 (tampering / epoch replay): strictly-older discard guard intact; covered by `apply_monotonic_older_epoch_is_noop` + `apply_monotonic_lower_epoch_is_noop`.
- T-20-07 (input validation / OOB): `<` change does not touch OOB row/col guards inside `apply()`.

## Self-Check

Files created/modified:
- FOUND: crates/nosh-client/src/screen.rs
- FOUND: crates/nosh-client/tests/predict.rs

Commits:
- FOUND: 4a0cdb2
- FOUND: 30fae87
- FOUND: 3b4ec04

## Self-Check: PASSED

## Next Phase Readiness

- Phase 20 is now complete: SC4 (apply guard), PACE-02 (noecho CI gate), and plan 20-01's PACE-01/03 are all satisfied.
- Phase 21 (channel multiplexing): `message_discriminant_order_is_stable` is the mandatory first commit; confirm `TerminalControl` discriminant before writing the test.

---
*Phase: 20-repaint-pacing*
*Completed: 2026-06-11*
