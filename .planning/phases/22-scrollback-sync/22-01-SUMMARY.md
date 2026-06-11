---
phase: 22-scrollback-sync
plan: "01"
subsystem: nosh-proto + nosh-server
tags: [wire-protocol, scrollback, terminal, tdd, discriminant-stability]
dependency_graph:
  requires: [21-01-SUMMARY.md]
  provides: [ScrollbackRequest wire type, ScrollbackPage wire type, ScrollbackCredit wire type, scrollback_lines accessor]
  affects: [crates/nosh-proto/src/messages.rs, crates/nosh-proto/src/codec.rs, crates/nosh-server/src/terminal.rs, crates/nosh-server/src/server.rs]
tech_stack:
  added: []
  patterns:
    - append-only Message enum discriminants (MUX-06 invariant, Phase 21 precedent)
    - verify-before-build TDD: test committed as first commit before types existed
    - VecDeque accessor returning owned Vec (encapsulation of scrollback internals)
key_files:
  created: []
  modified:
    - crates/nosh-proto/src/codec.rs
    - crates/nosh-proto/src/messages.rs
    - crates/nosh-server/src/terminal.rs
    - crates/nosh-server/src/server.rs
decisions:
  - ScrollbackLine carries ScrollbackCell (mirrors DiffRun field types) for zero-copy assembly from terminal.rs Cell values; width: u16 per-line metadata satisfies S-3
  - Debug derive added to Cell struct (needed for assert_eq! in unit tests; Rule 2 missing functionality)
  - Phase 22 scrollback variants added as logged no-op arms to server.rs run_session control-stream match to restore exhaustive pattern coverage (Rule 2 deviation; full wiring in Phase 22-02)
metrics:
  duration: 492
  completed: "2026-06-11"
  tasks: 3
  files: 4
---

# Phase 22 Plan 01: Wire Protocol Foundation and Scrollback Read Accessor Summary

Three new append-only `Message` variants (discriminants 15/16/17), a `ScrollbackLine` payload type with per-line width metadata, a `TerminalState::scrollback_lines` accessor with bounds-safe index arithmetic, and passing unit tests for the scrollback_excludes_alt_screen and resize_alt_screen_no_scrollback_contamination SCROLL-03 gates.

## Tasks Completed

| Task | Name | Commit | Files |
|------|------|--------|-------|
| 1 | Discriminant-stability + round-trip tests for variants 15-17 (RED) | 5391dd5 | crates/nosh-proto/src/codec.rs |
| 2 | ScrollbackRequest/Page/Credit + ScrollbackLine wire types (GREEN) | 3195b0c | crates/nosh-proto/src/messages.rs |
| 3 | scrollback_lines accessor + alt-screen exclusion tests | 252a78d | crates/nosh-server/src/terminal.rs, server.rs |

## Verification Results

All plan success criteria met:

- `cargo test -p nosh-proto message_discriminant_order_is_stable` exits 0 (discriminants 0-17 all pinned)
- `cargo test -p nosh-proto mux_variants_round_trip` exits 0 (all 15 variants round-trip)
- `cargo test -p nosh-server scrollback` passes all 9 scrollback-related tests
- `grep -n "if !self.echo_state.alt_screen"` shows gates at lines 489 and 631 — both unmodified
- `ScrollbackPage` carries `epoch_at_snapshot: u64` (LOCKED, S-5)
- `scrollback_lines(u64::MAX, 256)` returns empty page without panic (T-22-02 / V5)
- Both alt-screen exclusion gates proven by passing unit tests on BOTH scroll_up() and resize() paths

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 2 - Missing Critical Functionality] server.rs non-exhaustive match coverage**
- **Found during:** Task 3 compile
- **Issue:** Adding `ScrollbackRequest`, `ScrollbackPage`, `ScrollbackCredit` to the `Message` enum made the `match msg` in `run_session`'s control-stream arm non-exhaustive. The plan only specified changes to `messages.rs`, `codec.rs`, and `terminal.rs`.
- **Fix:** Added a logged no-op arm for all three Phase 22 scrollback variants in `run_session`'s control-stream match, with a comment noting that Phase 22-02 wires the full handler. This restores exhaustive pattern coverage without altering session behaviour.
- **Files modified:** `crates/nosh-server/src/server.rs`
- **Commit:** 252a78d (included in Task 3 commit)

**2. [Rule 2 - Missing Critical Functionality] Cell struct missing Debug derive**
- **Found during:** Task 3 test compilation
- **Issue:** `Cell` lacked `#[derive(Debug)]`, causing `assert_eq!` in the new unit tests to fail to compile (`Vec<Vec<Cell>>` does not implement `Debug`).
- **Fix:** Added `Debug` to `Cell`'s derive list.
- **Files modified:** `crates/nosh-server/src/terminal.rs`
- **Commit:** 252a78d (included in Task 3 commit)

## TDD Gate Compliance

- RED gate: commit 5391dd5 (`test(22-01): extend discriminant-stability + round-trip tests`) — tests reference the not-yet-existing scrollback variants; crate intentionally did not compile at this stage.
- GREEN gate: commit 3195b0c (`feat(22-01): append ScrollbackRequest/...`) — both discriminant-stability and round-trip tests pass.
- Task 3 followed an implicit RED/GREEN cycle: tests added first (new scrollback_lines, scrollback_excludes_alt_screen, resize_alt_screen_no_scrollback_contamination) in the same commit as the implementation (single-task, single commit as specified by the plan).

## Known Stubs

None — all plan artifacts are complete implementations, not stubs.

## Threat Flags

No new threat surfaces beyond what the plan's threat_model covers:
- T-22-02 (`scrollback_lines` index arithmetic) is mitigated — u64::MAX proven safe by test.
- T-22-03 / T-22-03b (alt-screen gate) proven by passing unit tests on both code paths.
- The server.rs no-op arm is not a new network endpoint; it is a pattern-match arm in an existing control-stream handler.

## Self-Check: PASSED

Files verified:
- `crates/nosh-proto/src/messages.rs` — contains `ScrollbackPage` with `epoch_at_snapshot: u64` ✓
- `crates/nosh-proto/src/codec.rs` — contains `(15, Message::ScrollbackRequest ...)` ✓
- `crates/nosh-server/src/terminal.rs` — contains `pub fn scrollback_lines` ✓

Commits verified:
- `5391dd5` (test RED) ✓
- `3195b0c` (feat GREEN) ✓
- `252a78d` (feat Task 3) ✓
