---
phase: 19-full-screen-tui-rendering-correctness
plan: "01"
subsystem: server/terminal
tags: [tui, alt-screen, resize, scrollback, terminal-model]
dependency_graph:
  requires: []
  provides: [D-19-09-scrollback-gate, TUI-01-two-grid-alt-screen, TUI-02-both-grids-resize]
  affects: [crates/nosh-server/src/terminal.rs]
tech_stack:
  added: []
  patterns:
    - std::mem::replace for atomic grid swap in enter_alt_screen
    - Option<(Vec<Vec<Cell>>, CursorPos, SgrState)> for saved primary state
    - !alt_screen guard in scroll_up for D-19-09 gate
key_files:
  created: []
  modified:
    - crates/nosh-server/src/terminal.rs
decisions:
  - "Two-grid model uses saved_primary: Option<(Vec<Vec<Cell>>, CursorPos, SgrState)> — single field holding all primary state"
  - "enter_alt_screen uses std::mem::replace for atomicity — avoids clone of primary grid into temp"
  - "Nested ?1049h overwrites saved_primary with current (alt) grid — xterm-divergent but safe, bounded memory (T-19-03)"
  - "Bare ?1049l with no saved_primary is handled by if let Some — leaves grid unchanged, just clears flag"
  - "scroll_up gates scrollback push on !alt_screen — alt content discarded, D-19-09 gate for Phase 22"
  - "Saved primary resize: same truncate/resize logic as active grid; shrunk rows pushed to self.scrollback"
metrics:
  duration_minutes: 20
  completed_date: "2026-06-07"
  tasks_completed: 2
  tasks_total: 2
  files_modified: 1
---

# Phase 19 Plan 01: Two-Grid Alt-Screen, Resize Correctness, and scroll_up Gate Summary

Atomic two-grid alternate screen in TerminalState: `saved_primary` field stores the full primary grid, cursor, and SGR pen on `?1049h`; both grids resize together on SIGWINCH; `scroll_up` discards alt content instead of pushing to scrollback.

## What Was Built

### Task 1: saved_primary field and atomic enter/exit_alt_screen (TUI-01)

Added `saved_primary: Option<(Vec<Vec<Cell>>, CursorPos, SgrState)>` to `TerminalState`. Two private methods:

`enter_alt_screen()` — atomically swaps the primary grid with a fresh blank alt grid using `std::mem::replace`, saves cursor and SGR pen into `saved_primary`, resets cursor to (0,0) and calls `sgr.reset()`. All three operations land together (Pitfall A-1 compliance).

`exit_alt_screen()` — takes `saved_primary` and restores grid, cursor, and SGR pen; handles bare exit (no prior enter) as a graceful no-op via `if let Some` (T-19-01). Always clears `echo_state.alt_screen`.

The `csi_dispatch` 1049 arm now calls `enter_alt_screen()`/`exit_alt_screen()` instead of toggling the flag directly. `esc_dispatch` RIS handler clears `saved_primary = None` so stale pre-reset content cannot be restored via a subsequent `?1049l`.

### Task 2: Resize both grids and scroll_up gate (TUI-02, D-19-09)

`resize()` extended with a `if let Some((ref mut prim_grid, ref mut prim_cursor, _)) = self.saved_primary` block that applies the same column truncate/extend and row remove-top-to-scrollback/push-blank logic to the saved primary grid. Cursor in saved_primary is clamped to new bounds (T-19-02). Shrunk rows are pushed to `self.scrollback` (not lost) respecting `SCROLLBACK_LINE_CAP`.

`scroll_up()` now wraps the scrollback push in `if !self.echo_state.alt_screen { ... }`. When alt-screen is active, the top row is discarded; the blank-row push always runs. This is the exact D-19-09 gate that Phase 22 scrollback sync depends on.

## Tests Added

Task 1 (TDD RED/GREEN):
- `alt_screen_enter_presents_blank_grid_at_origin` — blank grid at (0,0) on ?1049h
- `alt_screen_exit_restores_primary_grid_cursor_sgr` — full round-trip: primary content, cursor, SGR pen all restored byte-for-byte
- `alt_screen_nested_enter_no_panic` — second ?1049h while in alt-screen does not panic (T-19-03)
- `alt_screen_bare_exit_no_prior_enter_is_noop` — bare ?1049l leaves grid unchanged (T-19-01)
- `alt_screen_ris_clears_saved_primary` — RIS while in alt-screen prevents pre-reset restoration

Task 2 (TDD RED/GREEN):
- `resize_while_alt_screen_active_resizes_both_grids` — grow 80x24 to 100x30 during alt-screen; restored primary has new dimensions and content intact
- `resize_shrink_while_alt_screen_pushes_saved_primary_rows_to_scrollback` — shrink 10 rows to 5 during alt-screen; scrollback grows
- `scroll_up_in_alt_screen_does_not_push_to_scrollback` — scrollback length unchanged while alt-screen active; primary scrollback intact after exit

Total terminal tests: 94 (all passing).

## Commits

- `b7111db` feat(19-01): add saved_primary field and atomic enter/exit_alt_screen for ?1049h/?1049l
- `2dc0599` feat(19-01): resize both grids on SIGWINCH and gate scroll_up on !alt_screen

## Deviations from Plan

### Pre-existing clippy warning (out of scope)

`cargo clippy -p nosh-server --lib -- -D warnings` fails due to a pre-existing `type_complexity` warning in `registry.rs:521` (`drain_terminal_control` return type). This warning existed before this plan's changes and is unrelated to `terminal.rs`. No new clippy warnings were introduced by this plan. The pre-existing issue is logged to deferred-items.

## Deferred Items

- Pre-existing clippy `type_complexity` in `crates/nosh-server/src/registry.rs:521` — not introduced by this plan; documented for future cleanup.

## Known Stubs

None. The two-grid model is fully wired: enter, exit, resize, scroll_up gate, RIS reset are all complete and tested.

## Threat Flags

None. All three STRIDE threats from the plan's threat register are mitigated:
- T-19-01 (bare exit) — handled by `if let Some(...)` no-op path
- T-19-02 (resize bounds) — saturating cursor clamp + truncate/resize-to-default on saved grid
- T-19-03 (nested enter) — overwrites saved_primary with current grid; bounded memory, no panic

## Self-Check: PASSED

Files exist:
- `crates/nosh-server/src/terminal.rs` — FOUND

Commits exist:
- `b7111db` — FOUND (feat(19-01): add saved_primary...)
- `2dc0599` — FOUND (feat(19-01): resize both grids...)

All 94 nosh-server lib tests pass. Build succeeds.
