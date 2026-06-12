---
phase: "22"
plan: "05-gap"
subsystem: nosh-client
tags: [scrollback, render, gap-closure, SCROLL-01]
dependency_graph:
  requires: [22-03-SUMMARY.md]
  provides: [SCROLL-01-display-complete]
  affects: [crates/nosh-client/src/main.rs]
tech_stack:
  added: []
  patterns: [ansi-escape-render, crossterm-queue, reset_physical-before-scrollback]
key_files:
  modified: [crates/nosh-client/src/main.rs]
decisions:
  - render_scrollback_to_buf bypasses ClientScreen compositor and predictor entirely
  - reset_physical called on every scrollback render to guarantee full live repaint on snap-back
  - macro wbytes! used to extend Vec<u8> and sidestep AsyncWriteExt::write_all ambiguity
metrics:
  duration: "~45 minutes"
  completed: "2026-06-12"
  tasks_completed: 1
  files_modified: 1
---

# Phase 22 Plan 05 (Gap Closure): Scrollback Render Path Summary

Implements the missing render path for `ScrollbackView::Active` identified by the opus verifier as SCROLL-01's display half being unmet.

## Gap Identified

The 22-03 executor deferred the render step, leaving a `TODO: render scrollback view from lines+offset once the rendering path is wired (SCROLL-01 display)` at what was main.rs:1802. The client correctly fetched, decoded, and buffered scrollback pages and suppressed live datagrams while in `ScrollbackView::Active`, but never painted the held buffer to the terminal. Users pressing Shift-PageUp would trigger a channel open and page requests but see no historical content — the screen remained on live output.

This closed ROADMAP Success Criterion #1 (SCROLL-01 display half).

## What Was Implemented

### New function: `render_scrollback_to_buf`

`crates/nosh-client/src/main.rs`, inserted before the CSI accumulator section (~line 187).

Renders the visible window of a scrollback buffer (`&[ScrollbackLine]`) at a given `offset` to a `Vec<u8>` using raw ANSI escape sequences:

- Emits `\x1b[2J\x1b[H` to clear the screen and home the cursor.
- Computes the visible window: `bottom = lines.len() - offset`, `top = bottom - rows` (clamped). Rows above available history are emitted as blank lines.
- For each terminal row: emits `MoveTo(0, row)` via `crossterm::QueueableCommand`, resets SGR (`\x1b[0m`), then iterates cells emitting SGR params (bold, italic, underline, reverse, 256-color fg/bg) and the Unicode char.
- Blanks the remainder of each row with spaces after the last cell.
- Parks cursor at bottom-left when done.

Deliberately bypasses `ClientScreen`'s compositor, predictor, and loss overlay — historical content is not predicted and has no live overlay.

Uses a local `wbytes!` macro (`extend_from_slice`) instead of `std::io::Write::write_all` to avoid ambiguity with `tokio::io::AsyncWriteExt::write_all` which is in scope from the top-level import.

### Caller responsibility: `screen.reset_physical()`

Every call site calls `screen.reset_physical()` immediately after `render_scrollback_to_buf`. This marks the physical model as blank, so when the view returns to Live the first `render_to_stdout` / `render_with_predictor` call diffs against a blank physical model and emits the complete live grid.

### Wire-up points (five locations)

1. **Active entry** (Shift-PageUp from Live, ~line 2018): renders an initially blank scrollback frame immediately on entry while the first page request is in flight. This clears the live grid and signals to the user that scrollback mode is active.

2. **Shift-PageUp while Active** (~line 2064): renders the updated offset after incrementing. Reads `lines` and `offset` from `scrollback_view` after the offset mutation via a second borrow.

3. **Shift-PageDown while Active, page down** (~line 2095): renders the updated offset after decrementing.

4. **Shift-PageDown exit to Live** (~line 2091): instead of rendering scrollback, calls `screen.render_to_stdout` to force a full live grid repaint immediately on exit (reset_physical was already called on the last scrollback render).

5. **`page_rx` arm while Active** (~line 1901): replaces the TODO. After prepending the new page and updating metadata, renders the updated buffer at current offset.

### Snap-back: keystroke path

When a non-paging keystroke arrives while Active, the old code set `scrollback_view = Live` then fell through to the escape machine. The keystroke path calls `render_with_predictor` only when `bytes_to_forward` is non-empty, and that render might produce an empty diff if the physical model wasn't reset. Fixed by:

- Capturing `was_active` before mutating `scrollback_view`.
- After setting Live, calling `screen.render_to_stdout` to immediately paint the live grid before processing the keystroke through the predictor. The keystroke's own `render_with_predictor` call then produces an incremental update on top.

### Snap-back: epoch-gate path (SCROLL-05)

Already correct: setting `scrollback_view = Live` falls through to `screen.apply(&diff)` + `render_with_predictor`. Since `reset_physical` was called on every scrollback render, the physical model is zeroed and `render_with_predictor` emits a full repaint automatically. No change needed.

## Tests Added

Module `scrollback_render_tests` (7 tests, all passing):

| Test | What it verifies |
|------|-----------------|
| `empty_lines_emits_clear_screen` | Output starts with `\x1b[2J\x1b[H`; non-empty even with no lines |
| `single_line_content_appears_in_output` | Text chars from a line appear in render output |
| `offset_zero_shows_most_recent_lines` | Both lines visible when offset=0 and viewport=2 |
| `offset_rows_shifts_viewport_up` | Paging shifts window; newest line not visible at offset=2 |
| `large_offset_produces_blank_rows_without_panic` | Offset past all lines: no panic, blank rows, preamble present |
| `bold_cell_emits_bold_sgr` | Bold style produces `;1` in SGR sequence |
| `fg_color_cell_emits_256_color_sgr` | 256-color fg produces `38;5;N` in SGR sequence |

## Commit

`5d999d3` — feat(22-gap): implement scrollback render path (SCROLL-01 display)

## How This Satisfies SCROLL-01 Display Half

SCROLL-01 requires: "User can press Shift-PageUp to enter scrollback mode and view terminal history."

Before this fix: entering scrollback mode initiated page requests and suppressed live output, but the terminal screen was never updated to show historical content. The user saw no change on screen.

After this fix: pressing Shift-PageUp clears the screen and immediately renders the available scrollback buffer (blank initially, then populated as pages arrive). Each Shift-PageUp/Down keypress re-renders the updated offset. On snap-back (any other keystroke, or paging past the live boundary), the live grid is immediately repainted.

## Files Changed

- `crates/nosh-client/src/main.rs`: +388 lines, -4 lines (net +384)
  - New function `render_scrollback_to_buf` (~100 lines)
  - New test module `scrollback_render_tests` (~150 lines)
  - Five call-site wire-ups with repaint logic (~130 lines)
  - Removed TODO comment and updated match arm to capture `offset`

## Self-Check: PASSED

- `render_scrollback_to_buf` exists in main.rs: confirmed
- Commit `5d999d3` exists: confirmed
- `cargo build --workspace`: green
- `cargo test --workspace`: green (all 7 new tests pass; no regressions)
- TODO at former line 1802: removed
- STATE.md / ROADMAP.md: not modified
