---
phase: 20-repaint-pacing
fixed_at: 2026-06-11T03:30:00Z
review_path: .planning/phases/20-repaint-pacing/20-REVIEW.md
iteration: 1
findings_in_scope: 4
fixed: 4
skipped: 0
status: all_fixed
---

# Phase 20: Code Review Fix Report

**Fixed at:** 2026-06-11T03:30:00Z
**Source review:** .planning/phases/20-repaint-pacing/20-REVIEW.md
**Iteration:** 1

**Summary:**
- Findings in scope: 4 (WR-01, WR-02, WR-03, IN-02; IN-01 excluded per instruction — structurally unavoidable)
- Fixed: 4
- Skipped: 0

Build and test result: `cargo build --workspace` and `cargo test --workspace` both passed with 0 failures after all fixes were applied.

## Fixed Issues

### WR-01: epoch=0 datagram now passes `apply()` guard at initial state

**Files modified:** `crates/nosh-client/src/screen.rs`
**Commit:** 5ba41b7
**Applied fix:** Added an explicit `diff.epoch == 0` check before the monotonic guard in `apply()`. The guard is now `if diff.epoch == 0 || diff.epoch < self.last_applied_epoch`. This restores the defence-in-depth that the old `<=` guard provided implicitly: when `last_applied_epoch` is 0 at startup, the old guard rejected epoch=0 because `0 <= 0` was true; the new `<` guard (correct for same-epoch burst) would have let epoch=0 through since `0 < 0` is false. The explicit check reinstates the rejection without affecting the same-epoch burst behaviour. Added a comment explaining the rationale (D-14-05, D-20-07, and why the server never sends epoch=0).

---

### WR-02: `send_burst` ignores `TooLarge` on first datagram send

**Files modified:** `crates/nosh-server/src/server.rs`
**Commit:** 3b80b0c
**Applied fix:** Replaced the `TooLarge => {}` no-op with `return (result.deferred, false)`. When the path MTU shrinks between the `max_datagram_size()` query and `send_datagram()`, the first payload is not sent; the prior code continued into the burst loop and sent the deferred overflow runs without the baseline payload, which would corrupt the client's confirmed grid. The fix returns the full deferred state to the caller so the next tick's `build_state_diff` can recompute a fresh diff against the still-current snapshot. Added a detailed comment explaining the PMTUD scenario and why `false` (not `true`) is the correct `transport_lost` signal.

---

### WR-03: `noecho` test skips silently when `/bin/bash` is absent

**Files modified:** `crates/nosh-client/tests/predict.rs`
**Commit:** a72b44a
**Applied fix:** Replaced the `eprintln! + return` early-exit with `panic!`. The `noecho_read_dash_s_zero_predicted_chars` test is the D-20-09 mandatory security gate; silently passing when bash is absent means the security invariant goes unchecked in a bash-less CI image without any signal. The panic message names the gate, the decision reference, and the expected binary path so the failure is actionable. (Note: bash is present in this environment so the gate ran normally and the test passed.)

---

### IN-02: `drain_datagrams_with_cull` cull deferred but behaviour not documented

**Files modified:** `crates/nosh-client/tests/predict.rs`
**Commit:** 5aab467
**Applied fix:** Added a NOTE comment inside `drain_datagrams_with_cull` at the cull site explaining that this helper culls once per drain call (on the highest epoch seen) rather than once per distinct epoch within the window. The comment contrasts with `drain_datagrams_until_quiet` — which tracks `last_culled_epoch` and culls per distinct epoch — and explains when each helper is appropriate. This is a documentation-only change; no logic was altered (D-20-08).

---

## Skipped Issues

None — all four in-scope findings were fixed.

---

_Fixed: 2026-06-11T03:30:00Z_
_Fixer: Claude (gsd-code-fixer)_
_Iteration: 1_
