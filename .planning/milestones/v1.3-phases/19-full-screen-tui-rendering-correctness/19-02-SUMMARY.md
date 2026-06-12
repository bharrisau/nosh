---
phase: 19-full-screen-tui-rendering-correctness
plan: "02"
subsystem: server/terminal, server/diff-encoder, client/render
tags: [tui, wide-char, unicode-width, cell, diff-encoder, render]
dependency_graph:
  requires: [19-01]
  provides: [TUI-03-wide-char-cursor-accuracy, D-19-04-width-policy, D-19-05-zero-width, D-19-06-continuation-marker]
  affects:
    - crates/nosh-server/Cargo.toml
    - crates/nosh-server/src/terminal.rs
    - crates/nosh-server/src/server.rs
    - crates/nosh-client/src/screen.rs
    - crates/nosh-client/src/predictor.rs
tech_stack:
  added:
    - "unicode-width = \"0.2\" in nosh-server"
  patterns:
    - UnicodeWidthChar::width(c) dispatch for D-19-04 default-width policy
    - Cell.wide bool continuation marker (D-19-06)
    - Outer + inner wide-skip in compute_diff_runs for clean DiffRun.chars
    - want.wide skip in emit_diff with unconditional physical-grid commit
key_files:
  created: []
  modified:
    - crates/nosh-server/Cargo.toml
    - crates/nosh-server/src/terminal.rs
    - crates/nosh-server/src/server.rs
    - crates/nosh-client/src/screen.rs
    - crates/nosh-client/src/predictor.rs
decisions:
  - "Cell.wide: bool added to both server and client Cell structs for field-for-field parity (T-19-05)"
  - "Default width() used (not width_cjk()) — East Asian Ambiguous chars measure 1, matching client predictor (D-19-04)"
  - "Outer loop in compute_diff_runs skips wide:true run-start candidates; inner loop skips and advances col counter"
  - "emit_diff skips want.wide but unconditional physical-grid commit still syncs phys_cell"
  - "Right-edge guard: col+1 OOB write suppressed silently, not panicked (T-19-04)"
metrics:
  duration_minutes: 25
  completed_date: "2026-06-07"
  tasks_completed: 2
  tasks_total: 2
  files_modified: 5
---

# Phase 19 Plan 02: Wide-Character Width Accuracy and Continuation-Cell Consistency Summary

CJK width-2 glyphs now advance the server cursor by two columns with an explicit `wide:true` continuation marker at `col+1`; zero-width combining marks do not advance the cursor; the server diff encoder and client renderer both skip continuation cells so CJK lines round-trip with no column drift.

## What Was Built

### Task 1: unicode-width dep, Cell.wide field, and width-aware print_char

Added `unicode-width = "0.2"` to `crates/nosh-server/Cargo.toml` alongside the existing `vte` entry.

Added `pub wide: bool` to the `Cell` struct in `terminal.rs` with `wide: false` in `Default`. The new field is documented as the D-19-06 continuation marker — `true` only at `col+1` for a width-2 glyph; the `ch` value of a continuation cell is `' '` (irrelevant since both encoder and renderer skip it).

`print_char` now dispatches on `UnicodeWidthChar::width(c)` (default policy per D-19-04):

- `Some(0)` (combining accents, ZWJ sequences) → `return` immediately; cursor unchanged (D-19-05).
- `Some(2)` (CJK wide) → write primary cell with `wide: false` at `col`; write continuation cell `Cell { ch: ' ', wide: true, .. }` at `col+1` if in bounds (T-19-04 right-edge guard); advance cursor by 2 via `saturating_add(2)`.
- `Some(1)` or `None` (narrow/control-ish) → unchanged single-column behaviour.

Only one Cell literal site (the primary cell write in `print_char` itself) needed `wide: false` — all other Cell construction goes through `Cell::default()`.

### Task 2: Wide-continuation skip in server diff encoder and client render path

`compute_diff_runs` (server.rs): two-level wide skip:
1. Outer scan loop: if the first changed cell is `wide:true`, skip it and continue (prevents a continuation cell from becoming the `start_col` of a new run).
2. Inner extend loop: if `c2.wide`, `col += 1; continue` — column counter advances past the spacer so the next run starts at the correct column, but `chars.push` is not called (D-19-06; T-19-05).

Client `Cell` (screen.rs): added `pub wide: bool` with `wide: false` in `Default`, matching the server field-for-field (T-19-05). Fixed three Cell literal sites with `wide: false`: `ConnectionLossOverlay::cell_at`, `apply` (confirmed-grid update from DiffRun), and `PredictionOverlay::cell_at` in predictor.rs.

`emit_diff` (screen.rs): after the `want == have` early-out, added `if want.wide { continue; }` — the physical-grid commit loop at the end still sets `*phys_cell = des_cell.clone()` unconditionally so no spurious diff persists across renders.

`apply` (screen.rs): no change needed — `DiffRun.chars` already carries only logical glyphs (continuation cells are never in the wire chars stream), so the existing `(start..).zip(run.chars.chars())` loop writes cells at the correct columns without modification.

## Tests Added

Task 1 (TDD RED/GREEN):
- `wide_char_cjk_advances_cursor_by_two_and_writes_continuation` — U+4E2D '中' advances cursor.col by 2; col 0 has `wide:false`; col 1 has `wide:true`
- `zero_width_combining_mark_does_not_advance_cursor` — U+0301 combining acute after 'a' leaves cursor.col at 1
- `wide_char_at_right_edge_does_not_panic` — '中' at col 3 of a 4-wide terminal does not panic (T-19-04)

Task 2 (TDD RED/GREEN):
- `compute_diff_runs_wide_char_produces_one_scalar_in_chars` — 2-column grid `['中', CONT]` with empty baseline produces exactly 1 DiffRun with exactly 1 char ('中'); continuation is not in the chars string

Total terminal tests: 65 (filtered); total nosh-server lib tests: 98 (all passing).

## Commits

- `b0c9709` test(19-02): add failing tests for Cell.wide, width-aware print_char, and right-edge safety
- `ec32470` feat(19-02): add Cell.wide field, unicode-width dep, and width-aware print_char
- `f5a6b14` test(19-02): add failing test for compute_diff_runs wide-char skip
- `70c60c3` feat(19-02): skip wide-continuation cells in server diff encoder and client render

## Deviations from Plan

None. Plan executed exactly as written. The only Cell literal site requiring `wide: false` outside `print_char` itself was found in three client files (`screen.rs` × 2, `predictor.rs` × 1) — fixing these is the expected discovery mechanism described in the plan's interface contract.

## Known Stubs

None. Width-aware print_char, diff encoder skip, and client render skip are all fully wired and tested.

## Threat Flags

None. Both threat mitigations from the plan's STRIDE register are verified:
- T-19-04 (right-edge crash) — `col + 1 < self.grid[row].len()` bounds check before continuation write; `saturating_add(col_width)` for cursor advance; `wide_char_at_right_edge_does_not_panic` test covers this.
- T-19-05 (server/client drift) — field-for-field Cell parity; both encoder and renderer skip `wide:true`; `compute_diff_runs_wide_char_produces_one_scalar_in_chars` round-trip test.

## Self-Check: PASSED

Files exist:
- `crates/nosh-server/Cargo.toml` — FOUND (unicode-width added)
- `crates/nosh-server/src/terminal.rs` — FOUND (Cell.wide, print_char updated)
- `crates/nosh-server/src/server.rs` — FOUND (compute_diff_runs wide skips)
- `crates/nosh-client/src/screen.rs` — FOUND (Cell.wide, emit_diff skip)
- `crates/nosh-client/src/predictor.rs` — FOUND (Cell literal fixed)

Commits exist:
- `b0c9709` — FOUND (test(19-02): add failing tests...)
- `ec32470` — FOUND (feat(19-02): add Cell.wide field...)
- `f5a6b14` — FOUND (test(19-02): add failing test for compute_diff_runs...)
- `70c60c3` — FOUND (feat(19-02): skip wide-continuation cells...)

All 98 nosh-server lib tests pass. nosh-client and nosh-server build clean.
