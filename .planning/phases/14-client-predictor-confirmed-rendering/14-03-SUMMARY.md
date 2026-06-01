---
phase: 14-client-predictor-confirmed-rendering
plan: "03"
subsystem: nosh-client (tests)
tags: [PREDICT-01, render, grid-comparison, integration-test, datagram, ClientScreen]
dependency_graph:
  requires:
    - nosh_client::screen::ClientScreen (from 14-01)
    - nosh_proto::datagram::{encode_datagram, decode_datagram, StateDiff, DiffRun, CursorPos, CellStyle}
    - nosh_server::terminal::TerminalState (dev-dependency, tests only)
    - test harness from sync.rs (server_with_key, client_endpoint_for helpers)
  provides:
    - PREDICT-01 success criterion #2: automated test proof that datagram-rendered
      ClientScreen confirmed grid matches server TerminalState visible characters
    - End-to-end integration test: real client/server datagram path produces
      confirmed grid containing typed text
    - Idempotency test: duplicate datagrams produce no new cell output
  affects: []
tech_stack:
  added: []
  patterns:
    - "viewport_rows() + coalesce-non-blank-cells pattern for building a full-repaint StateDiff"
    - "encode_datagram → decode_datagram wire round-trip before apply (real path exercise)"
    - "Full 80×24 cell-by-cell grid equality assertion (ch + fg + bg + style)"
    - "read_datagram loop with timeout + row_str.contains() scan (sync.rs integration pattern)"
key_files:
  created:
    - crates/nosh-client/tests/render.rs
  modified: []
decisions:
  - "All three tests written in a single file creation pass and committed atomically — tasks 1 and 2 both target render.rs and were naturally implemented together"
  - "encode_datagram cap=4096 used in the pure test to ensure all runs fit without deferral (testing the full-repaint path, not the cap-enforcement path)"
  - "Full 80×24 grid assertion (for row in 0..rows, for col in 0..cols) rather than a spot-check per the critical test quality directive"
  - "Integration test scans confirmed grid rows as row_str.contains(\"hello\") substring check per the directive (not single-char .ch == 'h')"
metrics:
  duration: "~5 minutes"
  completed: "2026-06-02"
  tasks_completed: 2
  files_changed: 1
---

# Phase 14 Plan 03: PREDICT-01 Render Tests Summary

**One-liner:** Three tests in `render.rs` close PREDICT-01 success criterion #2: a pure 80×24 cell-by-cell grid-equality test (encode/decode round-trip + TerminalState vs ClientScreen), an idempotency test, and a live end-to-end integration test asserting "hello" appears in the confirmed grid from real datagram apply.

## What Was Built

`crates/nosh-client/tests/render.rs` (399 lines, new test file):

**Test 1 — `confirmed_grid_matches_terminal_state_after_diff`** (PREDICT-01 criterion #2):
- Creates `TerminalState::new(80, 24)` and advances with `"hello world\r\nline two\r\n"`
- Builds a full-repaint `StateDiff` by iterating `server_ts.viewport_rows()` and coalescing consecutive non-blank cells into `DiffRun` entries (style/fg/bg-aware run coalescing)
- Round-trips through `encode_datagram(&diff, 4096)` → `decode_datagram(&bytes)` (real wire encode/decode path)
- Applies the decoded diff to `ClientScreen::new(80, 24)` via `screen.apply(&decoded)`
- Asserts full 80×24 cell-by-cell equality: `for row in 0..rows { for col in 0..cols { assert_eq!(ch), assert_eq!(fg), assert_eq!(bg), assert_eq!(style.0) } }`
- This is the load-bearing PREDICT-01 assertion — not a spot-check

**Test 2 — `duplicate_datagram_is_idempotent`** (pure, no QUIC):
- Applies a content diff at epoch=1, renders to `buf1` (non-empty ANSI)
- Applies same diff again (D-14-05 monotonic guard discards it → epoch=1 ≤ last_applied=1)
- Renders to `buf2`; asserts `buf2.len() < buf1.len()` and `buf2` contains none of `h/e/l/o` cell-content characters

**Test 3 — `render_integration_client_screen_matches_server_output`** (PREDICT-01 end-to-end):
- Spawns in-process server with `/bin/sh`; connects a real quinn client
- `open_session` → discard `SessionOpened` → `send_input(b"echo hello\n")`
- Loops `conn.read_datagram()` with 5s timeout; on each `Ok`, `decode_datagram` → `screen.apply`
- After each apply, scans all confirmed-grid rows: collect `confirmed_cell(r, c).ch` into `row_str: String` → `row_str.contains("hello")`; breaks on success
- Closes PREDICT-01 end-to-end loop: datagram display path reproduces server terminal output for user-typed input

## Test Results

```
running 3 tests
test duplicate_datagram_is_idempotent ... ok
test confirmed_grid_matches_terminal_state_after_diff ... ok
test render_integration_client_screen_matches_server_output ... ok

test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.04s
```

`cargo test -p nosh-client`: all suites passed (0 failures, no regressions in sync.rs, migration.rs, etc.)

## Commits

| Task | Commit | Files |
|------|--------|-------|
| Tasks 1 + 2 (complete render.rs) | `ff1b3aa` | `crates/nosh-client/tests/render.rs` (new, 399 lines) |

## Acceptance Criteria Verification

| Criterion | Status |
|-----------|--------|
| `cargo test -p nosh-client --test render confirmed_grid_matches_terminal_state_after_diff` exits 0 | PASS |
| `cargo test -p nosh-client --test render duplicate_datagram_is_idempotent` exits 0 | PASS |
| `grep -n "TerminalState::new" render.rs` matches | PASS (line 75) |
| `grep -n "decode_datagram" render.rs` matches | PASS (lines 19, 207, 372) |
| Full 80×24 grid loop present (`for row in 0..rows { for col in 0..cols {`) | PASS (lines 218–219) |
| `cargo test -p nosh-client --test render render_integration_client_screen_matches_server_output` exits 0 | PASS |
| `cargo test -p nosh-client --test render` (whole file) exits 0 | PASS |
| `grep -n "read_datagram" render.rs` matches | PASS (line 367) |
| `grep -n "send_input" render.rs` matches | PASS (line 354) |
| `grep -n 'contains("hello")' render.rs` matches | PASS (line 382) |

## Deviations from Plan

**1. Worktree reset required before first build**

- **Found during:** Initial `cargo test` attempt
- **Issue:** The worktree HEAD was at commit `f83093e` (Phase 9 state) while the plan's `<worktree_branch_check>` specified base `d40a3624` (Phase 14 wave 2 state). The worktree lacked `datagram.rs`, `screen.rs`, `terminal.rs`, and the `pub mod` declarations for those modules.
- **Fix:** Ran `git reset --hard d40a3624c9faa17c4590bd7bd8f24add47f571b7` per the branch-check protocol. The reset brought the codebase into the correct state; `render.rs` was preserved as an untracked file.
- **Rule:** Rule 3 (blocking issue, not an architectural change)
- **Files modified:** none (git reset)

No other deviations — plan executed as written after the initial reset.

## Known Stubs

None. All three tests are fully wired to real implementations (real TerminalState, real encode/decode, real QUIC client/server).

## Threat Flags

No new security surface introduced. This is a test-only plan:
- T-14-09 (Tampering — datagram render correctness): The full 80×24 grid-equality assertion is the adversarial check. Implemented.
- T-14-SC (cargo installs): No new dependencies introduced.

## Self-Check: PASSED

- `crates/nosh-client/tests/render.rs` exists: FOUND (399 lines)
- Commit `ff1b3aa` exists in git log: FOUND
- `cargo test -p nosh-client --test render`: 3 passed, 0 failed
- `grep -n "TerminalState::new"`: FOUND (line 75)
- `grep -n "decode_datagram"`: FOUND (lines 19, 207, 372)
- Full 80×24 grid loop `for row in 0..rows { for col in 0..cols {`: FOUND (lines 218–219)
- `grep -n "read_datagram"`: FOUND (line 367)
- `grep -n "send_input"`: FOUND (line 354)
- `grep -n 'contains("hello")'`: FOUND (line 382)
- `cargo test -p nosh-client` (full suite): 0 failures
- Line count 399 >= min_lines 80: CONFIRMED
