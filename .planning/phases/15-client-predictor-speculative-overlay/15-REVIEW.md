---
phase: 15-client-predictor-speculative-overlay
reviewed: 2026-06-02T12:00:00Z
depth: deep
files_reviewed: 6
files_reviewed_list:
  - crates/nosh-client/src/predictor.rs
  - crates/nosh-client/src/screen.rs
  - crates/nosh-client/src/main.rs
  - crates/nosh-client/tests/predict.rs
  - crates/nosh-client/Cargo.toml
  - crates/nosh-client/src/lib.rs
findings:
  critical: 3
  warning: 5
  info: 0
  total: 8
status: findings
---

# Phase 15: Code Review Report

**Reviewed:** 2026-06-02
**Depth:** deep
**Files Reviewed:** 6
**Status:** findings

## Summary

The Phase 15 prediction engine implements the Mosh epoch/Validity state machine cleanly in most
respects: the `classify_input` bulk-before-escape ordering is correct, the `>=` epoch check for
dropped datagrams is correct (Pitfall 4), the `CellStyle::UNDERLINE` flagging path is correct,
and `width_cjk()` is correctly NOT used (D-15-03). The RTT hysteresis, bracketed-paste
detection, and wide-char right-edge `become_tentative()` guard are all implemented as specified.

Three blockers were found: one that breaks every prediction on a non-blank-screen terminal,
one that violates the core "never render worse than no prediction" invariant on backspace, and
one that creates a one-frame window where a password character can be rendered to stdout before
the noecho mismatch is detected. Five warnings were also found covering code quality and test
coverage gaps.

---

## Critical Issues

### CR-01: `predicted_cursor.row` never initialized from or synced to confirmed cursor

**File:** `crates/nosh-client/src/predictor.rs:183-204, 260-278`

**Issue:** `predicted_cursor` is initialized to `CursorPos { row: 0, col: 0 }` in `new()` and
`predicted_cursor.row` is never written after that. Only `.col` is ever modified (by
`PredictChar`, `PredictBackspace`, cursor-movement handlers, and `find_line_end`). There is also
no API to sync `predicted_cursor` from `ClientScreen.confirmed_cursor` (which is private and has
no public getter).

Practical consequence: every char prediction is placed at `row = 0` regardless of where the
actual terminal cursor is. On a real interactive shell, after the initial prompt is printed, the
confirmed cursor is at `(row=0, col=N)` for some `N > 0` (e.g., past the prompt characters).
The predictor inserts char predictions at `(0, 0)` — overlaying the start of the prompt line —
while `predicted_cursor()` returns column `1`, placing the cursor override at `(0, 1)`.

On the next datagram, `cull()` compares `screen.confirmed_cell(0, 0).ch` (the prompt character,
e.g. `'$'`) against `predicted_ch` (`'a'`); they differ → `IncorrectOrExpired` (non-tentative,
since `confirmed_epoch == tentative_until_epoch == 0` initially) → `reset()`. The prediction
briefly shows garbage at `(0, 0)`, then clears on every single keystroke. This violates the
core "never render worse than no prediction" invariant for every keystroke after the shell
prompt appears.

On multi-row screens (e.g., command on row 5), the bug is the same: all predictions land on
row 0 and immediately reset. The predictor contributes only display noise, never correct
speculative echo. The phase-gate D-15-04 matrix tests all run on a blank screen starting at
`(0, 0)`, so they do not exercise this path.

**Fix:** Add a public `confirmed_cursor()` method to `ClientScreen`, or add a
`sync_cursor(pos: CursorPos)` method to `PredictionOverlay`. Call it in `cull()` whenever
`pending` becomes empty (safe sync point) and in `reset()`. Alternatively, pass the current
confirmed cursor into `on_input` and use it to initialize `predicted_cursor` at the start of
each new prediction epoch:

```rust
// In PredictionOverlay:
pub fn sync_cursor_from_confirmed(&mut self, confirmed: CursorPos) {
    if self.pending.is_empty() {
        self.predicted_cursor = confirmed;
    }
}

// In ClientScreen — add a public getter:
pub fn confirmed_cursor(&self) -> CursorPos {
    self.confirmed_cursor
}

// In run_pump datagram arm, after cull():
predictor.sync_cursor_from_confirmed(screen.confirmed_cursor());
```

And in `reset()`:
```rust
pub fn reset(&mut self) {
    self.pending.clear();
    self.become_tentative();
    // Do NOT reset predicted_cursor here — caller must sync from confirmed.
}
```

---

### CR-02: Backspace leaves stale char prediction visible at the vacated column

**File:** `crates/nosh-client/src/predictor.rs:280-285`

**Issue:** `PredictBackspace` only decrements `predicted_cursor.col` by 1. It does NOT remove
or invalidate the `PendingPrediction` entry at the vacated column. After:

```
on_input(b"a")   // pushes prediction 'a' at (row, col=0); cursor → col=1
on_input(&[0x7f]) // PredictBackspace: cursor → col=0; 'a' prediction remains at col=0
```

`cell_at(row, 0)` still returns `Some(Cell { ch: 'a', … })`. The user deleted the character,
but the speculative overlay continues to display it.

**Why this violates the invariant:** Without prediction, `confirmed_cell(row, 0)` returns `' '`
(blank) until the server confirms the backspace. With prediction active, the overlay shows `'a'`
(the deleted char). The prediction renders WORSE than no prediction — the core design invariant
from D-15-01 and CONTEXT.md is violated.

The research document (PITFALL 4 / Open Question 2) acknowledges cursor-only backspace as the
chosen conservative approach, but the intent was "predict cursor move left, mark the vacated
column as `unknown` (do not render)." The implementation moves the cursor but omits the
"mark unknown" step, causing the deleted char to remain in the overlay.

**Fix:** In `PredictBackspace`, remove the prediction at the vacated column from `pending`
before moving the cursor:

```rust
InputAction::PredictBackspace => {
    if self.predicted_cursor.col > 0 {
        let vacated_col = self.predicted_cursor.col - 1;
        let row = self.predicted_cursor.row;
        // Remove any char prediction at the vacated position so it stops showing.
        self.pending.retain(|p| !(p.row == row && p.col == vacated_col));
        self.predicted_cursor.col = vacated_col;
    }
}
```

---

### CR-03: PREDICT-04 one-frame render window before noecho is structurally detected

**File:** `crates/nosh-client/src/main.rs:785, 803-814`

**Issue:** In `run_pump`'s stdin arm, `predictor.on_input()` is called and then immediately
`screen.render_with_predictor()` flushes the prediction to stdout — before any server datagram
can arrive to detect the noecho mismatch. The sequence is:

```
1. predictor.on_input(bytes, &screen)   // prediction enqueued; potentially non-tentative
2. render_with_predictor(...)           // prediction rendered to stdout  ← PREDICT-04 gap
3. send_input(bytes)                    // keystroke sent to server
4. [later] datagram arm: cull()         // mismatch detected, reset()
```

When the user first enters a noecho context (e.g., `read -s` is running), the prediction epoch
may equal `confirmed_epoch` (both are 0, or both advanced to N after previous correct echoes).
The first typed character gets `tentative_until_epoch = N`, and since `N <= confirmed_epoch`,
`is_tentative()` is `false` — the prediction is immediately visible. Step 2 writes it to stdout.
Only at step 4 does `cull()` detect the noecho mismatch and call `reset()`, making subsequent
predictions tentative.

Result: for the first keystroke after entering noecho mode, the predicted char is written to
stdout for one render frame (one `write_all` call), visible to the user, before it is removed.
This is a direct violation of PREDICT-04: "display **zero** predicted characters when server is
not echoing."

The D-15-04 live `noecho_read_dash_s_zero_predicted_chars` test does not catch this because it
asserts `cell_at(0, col) == None` only AFTER `drain_datagrams_with_cull()` has run, which
means cull() has already reset the state before the assertion fires. The one-frame write to
stdout is not observable through `cell_at` after the fact.

**Fix:** Before calling `render_with_predictor` in the stdin arm, check whether the last applied
epoch is the same as the epoch that would confirm the new prediction. If `cull()` has not been
called since the last `on_input`, consider deferring the render until after the next datagram
arm cull. The simplest mitigation: set a `dirty` flag in `on_input` and render only in the
datagram arm after cull. This defers every prediction render by one RTT but is safe:

```rust
// stdin arm — no immediate render:
predictor.on_input(&result.bytes_to_forward, &screen);
// Defer render to datagram arm (after cull confirms/denies the prediction).

// datagram arm — render after cull:
predictor.cull(&screen, diff.epoch, rtt_ms);
let mut buf: Vec<u8> = Vec::new();
screen.render_with_predictor(&mut buf, &predictor)...;
```

Alternatively, add a `pending_confirmation_required: bool` flag to `PredictionOverlay` that
prevents rendering any non-tentative prediction until at least one `cull()` call has run since
the prediction was enqueued. This preserves the low-latency render on subsequent keystrokes
after noecho status is confirmed.

---

## Warnings

### WR-01: `PredictionOverlay.term_cols`/`term_rows` not updated on terminal resize

**File:** `crates/nosh-client/src/predictor.rs:203-204`, `crates/nosh-client/src/main.rs:829-837`

**Issue:** `PredictionOverlay` is constructed once in `run_pump` with the initial `cols`/`rows`
from `crossterm::terminal::size()`. When the user resizes the terminal, `ClientScreen` calls
`resize()` updating its internal dimensions, but there is no corresponding update to
`predictor.term_cols` or `predictor.term_rows`. The stale values persist for the session:

- Terminal shrinks (e.g., 80→40): `predictor.term_cols = 80`. The wide-char right-edge check
  (`col.saturating_add(col_width) > self.term_cols`) won't fire until `col >= 80`, allowing
  predictions at columns 40–79 that are beyond the actual terminal — resulting in a
  `MoveTo(col > 39, row)` cursor position that wraps or positions off-screen.
- Terminal grows (e.g., 80→120): `predictor.term_cols = 80`. Predictions near col 79 may call
  `become_tentative()` unnecessarily, and `PredictCursorRight` won't advance past col 79.

**Fix:** Add a `set_size(cols: u16, rows: u16)` method to `PredictionOverlay`. Call it (and
also call `predictor.reset()`) in the datagram arm when `screen.apply()` triggers a resize:

```rust
// In run_pump datagram arm, after screen.apply(&diff):
if diff.cols != screen_cols_before || diff.rows != screen_rows_before {
    predictor.set_size(diff.cols, diff.rows);
    predictor.reset();
}

// In PredictionOverlay:
pub fn set_size(&mut self, cols: u16, rows: u16) {
    self.term_cols = cols;
    self.term_rows = rows;
}
```

---

### WR-02: `cull()` second loop uses `to_remove.contains(&i)` — O(n²)

**File:** `crates/nosh-client/src/predictor.rs:375-384`

**Issue:** The second loop in `cull()` iterates `self.pending.iter().enumerate()` and calls
`to_remove.contains(&i)` for each element. This is O(n) inside an O(n) loop → O(n²) overall.
For typical `pending` sizes this is negligible (n is bounded by keystrokes-since-last-confirm,
usually < 10), but the logic also introduces fragility: the second loop re-derives conditions
already computed in the first loop, creating a maintenance surface and risk of divergence.

The entire `epochs_to_kill` collection pass could be eliminated by integrating epoch pruning
directly into the first loop (e.g., using a `HashSet<u64>` for epochs to kill, updated in the
first loop alongside `to_remove`) and then applying `retain` once after the first loop.

**Fix:** Refactor to collect epochs-to-kill in the first loop using a `HashSet`:

```rust
let mut epochs_to_kill: std::collections::HashSet<u64> = std::collections::HashSet::new();

for (i, pred) in self.pending.iter().enumerate() {
    if pred.epoch_required <= new_epoch {
        let confirmed_ch = screen.confirmed_cell(pred.row, pred.col).ch;
        match Self::check_validity(confirmed_ch, pred.predicted_ch) {
            Validity::Correct => {
                if pred.tentative_until_epoch > self.confirmed_epoch {
                    self.confirmed_epoch = pred.tentative_until_epoch;
                }
                to_remove.push(i);
            }
            Validity::CorrectNoCredit => { to_remove.push(i); }
            Validity::IncorrectOrExpired => {
                if self.is_tentative(pred) {
                    epochs_to_kill.insert(pred.tentative_until_epoch);
                } else {
                    self.reset();
                    return;
                }
            }
            _ => {}
        }
    }
}
for epoch in epochs_to_kill {
    self.kill_epoch(epoch);
}
// rebuild to_remove or use retain instead of indexed removal
```

---

### WR-03: `PendingPrediction.is_cursor_move` is always `false` — dead field

**File:** `crates/nosh-client/src/predictor.rs:155, 276`

**Issue:** `PendingPrediction.is_cursor_move` is declared as a `pub` field with a doc comment,
but the only place it is set is `is_cursor_move: false` in the `PredictChar` arm of `on_input`
(line 276). Cursor-motion inputs (`PredictBackspace`, `PredictCursorLeft`, `PredictCursorRight`,
`PredictLineStart`, `PredictLineEnd`) do NOT push to `pending`, so there are no predictions with
`is_cursor_move: true`. The field is never read in any logic path — including `cull()`,
`cell_at()`, or `predicted_cursor()`.

This creates reader confusion (the field implies a meaning that is never used) and wastes 1 byte
per `PendingPrediction` struct.

**Fix:** Remove the field or, if cursor-move tracking is planned for a future phase, replace it
with a `#[allow(dead_code)]` attribute and a TODO comment documenting the planned use:

```rust
// Option 1: remove entirely
pub struct PendingPrediction {
    pub row: u16,
    pub col: u16,
    pub predicted_ch: char,
    pub col_width: u16,
    pub epoch_required: u64,
    pub tentative_until_epoch: u64,
    // is_cursor_move removed: cursor moves are not tracked in pending (Open Question 2)
}

// Option 2: annotate with intent
/// Reserved for Phase 17 cursor-prediction tracking. Always false in Phase 15.
#[allow(dead_code)]
pub is_cursor_move: bool,
```

---

### WR-04: Noecho security test asserts only `row=0` — structural suppression not tested for non-zero cursor rows

**File:** `crates/nosh-client/tests/predict.rs:711`

**Issue:** The live-server `noecho_read_dash_s_zero_predicted_chars` test asserts:

```rust
for col in 0..80u16 {
    assert!(predictor.cell_at(0, col).is_none(), ...);
}
```

This checks only `row=0`. On a real PTY after running `read -s X\n`, the shell cursor is
almost certainly NOT at row 0 (it has advanced through the prompt and the echoed command).
The actual predictions (from CR-01) are also placed at `row=0` — meaning the test is checking
the same row where predictions happen to land due to the `row=0` bug, not the row where
the password prompt is. The test may pass because: (a) the `cull()` inside
`drain_datagrams_with_cull` already reset state (noecho mismatch at row 0), or (b) because
predicted chars at row 0 were already gone after cull.

The test does NOT verify that no prediction is shown on the row where the `read -s` prompt
actually appears. This means the structural noecho suppression is not adversarially validated
for the realistic cursor-position scenario.

**Fix:** After CR-01 is fixed (predicted_cursor.row synced from confirmed cursor), update the
test to assert across ALL 24 rows (or at minimum, the confirmed cursor's row) to ensure no
predicted char appears anywhere:

```rust
let cursor_row = screen.confirmed_cursor().row; // requires confirmed_cursor() getter
for row in 0..24u16 {
    for col in 0..80u16 {
        assert!(
            predictor.cell_at(row, col).is_none(),
            "SECURITY VIOLATION: cell_at({row},{col}) returned Some during 'read -s' noecho"
        );
    }
}
```

---

### WR-05: No test covers the type-then-backspace stale-prediction scenario

**File:** `crates/nosh-client/tests/predict.rs`

**Issue:** The D-15-04 validation matrix includes `vim insert (iHello<Esc>)` and `Ctrl-C
mid-line` but does NOT include a case for typing a char then pressing backspace. The
`PredictBackspace` bug from CR-02 — where the char prediction at the vacated column remains
visible — would not be caught by any existing test in either `predictor.rs` unit tests or
`predict.rs` integration tests.

**Fix:** Add a unit test to `predictor.rs` (or `predict.rs`) that verifies backspace removes
the char from the overlay:

```rust
#[test]
fn backspace_removes_char_prediction_from_overlay() {
    let screen = make_screen(80, 24);
    let mut overlay = PredictionOverlay::new(PredictDisplayMode::Always, 80, 24);

    // Type 'a' — prediction at (0, 0).
    overlay.on_input(b"a", &screen);
    assert!(overlay.cell_at(0, 0).is_some(), "prediction must be visible before backspace");

    // Backspace — cursor moves to col 0; prediction at col 0 must be gone.
    overlay.on_input(&[0x7f], &screen);

    assert!(
        overlay.cell_at(0, 0).is_none(),
        "after backspace, cell_at(0,0) must be None — deleted char must not remain in overlay"
    );
}
```

---

_Reviewed: 2026-06-02_
_Reviewer: Claude (gsd-code-reviewer)_
_Depth: deep_
