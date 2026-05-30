---
phase: 07-connection-migration-validation
fixed_at: 2026-05-30T00:00:00Z
review_path: .planning/phases/07-connection-migration-validation/07-REVIEW.md
iteration: 1
findings_in_scope: 4
fixed: 4
skipped: 0
status: all_fixed
---

# Phase 7: Code Review Fix Report

**Fixed at:** 2026-05-30
**Source review:** .planning/phases/07-connection-migration-validation/07-REVIEW.md
**Iteration:** 1

**Summary:**
- Findings in scope: 4 (CR-01, WR-01, WR-02, WR-03; IN-01 excluded per fix_scope)
- Fixed: 4
- Skipped: 0

## Fixed Issues

### CR-01: Silently dropped PtyData frame when first frame is not SessionOpened

**Files modified:** `crates/nosh-client/tests/migration.rs`
**Commit:** d182671
**Applied fix:** Introduced `pending_first_frame: Option<Message>` before the first-frame read block. When the first frame is NOT `SessionOpened`, it is stored in `pending_first_frame` rather than silently discarded. The main loop now drains `pending_first_frame` on its first iteration (via `if let Some(f) = pending_first_frame.take()`) before reading from the network, so all data including a potential `LINE:0` is preserved and processed.

### WR-01: DONE detection is a no-op — break comment without break statement

**Files modified:** `crates/nosh-client/tests/migration.rs`
**Commit:** d182671
**Applied fix:** Added `done_received: bool` flag (initialized `false`) before the outer loop. The `else if trimmed == "DONE"` branch now sets `done_received = true` and adds `break` to exit the inner `for line in text.lines()` loop. After the `match` block, a new check `if done_received { break; }` exits the outer frame loop. This prevents a 30-second timeout hang when the shell exits early (before producing 80 lines).

### WR-02: Stall measurement understates actual anti-amplification stall

**Files modified:** `crates/nosh-client/tests/migration.rs`
**Commit:** d182671
**Applied fix:** Added `just_rebound: bool` flag. When the rebind is triggered inside the inner loop, `just_rebound = true` is set alongside `rebind_done = true`. The inline `t_first_post` capture inside the inner loop (which set it from lines within the same PtyData chunk as the rebind) was removed. The outer-level `t_first_post` capture (after the inner loop) now guards with `!just_rebound`, so only frames received in subsequent outer-loop iterations count as the first post-rebind data. `just_rebound = false` is reset at the start of each outer-loop iteration's post-match cleanup block. Confirmed working: the stall metric now reads 51ms / 10.2x RTT on loopback instead of the prior near-zero value.

### WR-03: rcgen is an unused production dependency in nosh-client

**Files modified:** `crates/nosh-client/Cargo.toml`
**Commit:** cd7de50
**Applied fix:** Removed `rcgen = { workspace = true }` from the `[dependencies]` section. Verified that no file under `crates/nosh-client/src/` imports or uses `rcgen`. The crate remains available transitively via `nosh-server` (a dev-dependency) for test builds.

## Skipped Issues

None — all four in-scope findings were successfully fixed.

---

## Test Results

Migration test run after all fixes:

```
test migration_survives_path_change ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 4.49s
```

Full workspace test suite: all tests pass (no regressions).

Notable observation from WR-02 fix: the `D-04` stall metric now correctly measures 51ms (10.2x RTT) on loopback, reflecting the real RFC 9000 §9.4 anti-amplification window. Previously this would have read near-zero (a few nanoseconds), masking genuine stalls in CI.

---

_Fixed: 2026-05-30_
_Fixer: Claude (gsd-code-fixer)_
_Iteration: 1_
