---
phase: 15-client-predictor-speculative-overlay
fixed_at: 2026-06-02T00:30:00Z
review_path: .planning/phases/15-client-predictor-speculative-overlay/15-REVIEW.md
iteration: 1
findings_in_scope: 8
fixed: 8
skipped: 0
status: all_fixed
---

# Phase 15: Code Review Fix Report

**Fixed at:** 2026-06-02
**Source review:** `.planning/phases/15-client-predictor-speculative-overlay/15-REVIEW.md`
**Iteration:** 1

**Summary:**
- Findings in scope: 8
- Fixed: 8
- Skipped: 0

## Fixed Issues

### CR-01: `predicted_cursor.row` never initialized from or synced to confirmed cursor

**Files modified:** `crates/nosh-client/src/screen.rs`, `crates/nosh-client/src/predictor.rs`, `crates/nosh-client/src/main.rs`
**Commit:** db7e1ca
**Applied fix:**
- Added `ClientScreen::confirmed_cursor() -> CursorPos` public getter in `screen.rs`.
- Added `PredictionOverlay::sync_cursor_from_confirmed(confirmed: CursorPos)` method in `predictor.rs`. Syncs `predicted_cursor` from the confirmed position when `pending` is empty (safe sync point), and also clears `awaiting_first_cull` (the CR-03 flag).
- In `run_pump`'s datagram arm in `main.rs`, call `predictor.sync_cursor_from_confirmed(screen.confirmed_cursor())` after every `cull()` call.
- Added mandatory regression test `cr01_prediction_lands_on_correct_nonzero_row` (unit) and `screen_confirmed_cursor_getter_returns_correct_position` (unit): a NON-ZERO confirmed cursor row is used; prediction lands at the correct row; confirmed when server echoes there.

### CR-02: Backspace leaves stale char prediction visible at the vacated column

**Files modified:** `crates/nosh-client/src/predictor.rs`, `crates/nosh-client/tests/predict.rs`
**Commit:** db7e1ca
**Applied fix:**
- In `PredictBackspace` arm of `on_input()`, call `self.pending.retain(|p| !(p.row == row && p.col == vacated_col))` before moving the cursor. This removes any prediction at the vacated column so `cell_at` no longer returns the deleted char.
- Additional: if backspace empties `pending`, also clears `awaiting_first_cull` (so cursor navigation after a type-then-backspace still works correctly).
- Added mandatory regression test `cr02_backspace_removes_char_prediction_from_overlay` (unit) and `backspace_removes_stale_char_prediction` (integration): type 'a' then backspace; `cell_at(0,0)` returns `None` after backspace.

### CR-03: PREDICT-04 one-frame render window before noecho is structurally detected

**Files modified:** `crates/nosh-client/src/predictor.rs`
**Commit:** db7e1ca
**Applied fix:**
- Added `awaiting_first_cull: bool` field to `PredictionOverlay` (initialized to `false`).
- Set `awaiting_first_cull = true` in `PredictChar` arm when `pending.is_empty()` before pushing the new prediction (i.e., first prediction of a fresh window).
- Set `awaiting_first_cull = true` in `reset()` (called on EpochReset, BulkSuppressed, BracketedPasteStart, non-tentative mismatch).
- Clear `awaiting_first_cull = false` at the top of `cull()` (before any early-return path). If `reset()` is called within `cull()` (non-tentative mismatch), `reset()` re-sets it to `true` — correctly requiring another `cull()` before predictions become visible again.
- `cell_at()` and `predicted_cursor()` both return `None` when `awaiting_first_cull` is `true`, preventing any prediction from being rendered between `on_input()` and the first datagram arm `cull()`.
- Updated three existing tests (`cell_at_returns_underline_when_flagging`, `cell_at_no_underline_when_not_flagging`, `render_with_predictor_overlays_predicted_cell`) to call `cull(&screen, 0, ...)` before checking `cell_at`.
- Updated six predict.rs integration tests that checked `predicted_cursor().is_some()` or `cell_at().is_some()` immediately after `on_input()` without a prior `cull()`.
- Added mandatory regression test `cr03_noecho_first_keystroke_not_rendered_before_cull` (unit): first keystroke of a fresh epoch returns `None` from `cell_at` before `cull()` runs; after noecho cull, still `None`.

**Note:** This fix adds one RTT of latency to the FIRST keystroke of each new prediction window (subsequent keystrokes in an ongoing epoch are unaffected). This is the documented safe tradeoff.

## Warning Issues

### WR-01: `PredictionOverlay.term_cols`/`term_rows` not updated on terminal resize

**Files modified:** `crates/nosh-client/src/predictor.rs`, `crates/nosh-client/src/main.rs`
**Commit:** db7e1ca
**Applied fix:**
- Added `PredictionOverlay::set_size(cols: u16, rows: u16)` method.
- Removed `#[allow(dead_code)]` from `term_rows` field (now actively used).
- In `run_pump`'s datagram arm, capture terminal dims before `screen.apply(&diff)`, compare after apply, and call `predictor.set_size(cols_after, rows_after)` + `predictor.reset()` when a resize is detected.

### WR-02: `cull()` second loop uses `to_remove.contains(&i)` — O(n²)

**Files modified:** `crates/nosh-client/src/predictor.rs`
**Commit:** db7e1ca
**Applied fix:**
- Eliminated the second loop entirely. `epochs_to_kill` is now a `HashSet<u64>` collected inline during the first loop when `IncorrectOrExpired` is detected on a tentative prediction. This reduces the second pass from O(n²) to zero extra iterations.

### WR-03: `PendingPrediction.is_cursor_move` is always `false` — dead field

**Files modified:** `crates/nosh-client/src/predictor.rs`
**Commit:** db7e1ca
**Applied fix:**
- Removed `is_cursor_move: bool` field from `PendingPrediction` struct and its doc comment.
- Removed the only construction site (`is_cursor_move: false` in `PredictChar` arm).

### WR-04: Noecho security test asserts only `row=0`

**Files modified:** `crates/nosh-client/tests/predict.rs`
**Commit:** db7e1ca
**Applied fix:**
- Updated the security assertion loop in `noecho_read_dash_s_zero_predicted_chars` to check all 24 rows × 80 columns (not just row=0).
- Added `predictor.sync_cursor_from_confirmed(screen.confirmed_cursor())` call after each `drain_datagrams_with_cull()` so the predictor tracks the real cursor row.
- The assertion message now includes the confirmed cursor row for diagnosis.

### WR-05: No test covers the type-then-backspace stale-prediction scenario

**Files modified:** `crates/nosh-client/tests/predict.rs`
**Commit:** db7e1ca
**Applied fix:**
- Added `backspace_removes_stale_char_prediction` integration test.
- Type 'a', cull to make visible, then backspace. Asserts `cell_at(0,0).is_none()` after backspace (would have returned `Some('a')` before CR-02 fix).
- Also asserts `pending_len() == 0`.

---

## Build / Test / Clippy Status

**`cargo build --workspace`:** GREEN

**`cargo test -p nosh-client`:** GREEN
- Unit tests (lib): 75 passed, 0 failed (includes new cr01/cr02/cr03 regression tests)
- predict.rs integration tests: 11 passed, 0 failed (includes live noecho and e2e echo tests)
- All other workspace test suites: GREEN

**`cargo clippy --workspace --all-targets -- -D warnings`:** GREEN

---

_Fixed: 2026-06-02_
_Fixer: Claude (gsd-code-fixer)_
_Iteration: 1_
