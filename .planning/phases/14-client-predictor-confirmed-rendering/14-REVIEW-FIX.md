---
phase: 14-client-predictor-confirmed-rendering
fixed_at: 2026-06-02T00:00:00Z
review_path: .planning/phases/14-client-predictor-confirmed-rendering/14-REVIEW.md
iteration: 1
findings_in_scope: 3
fixed: 3
skipped: 2
status: all_fixed
---

# Phase 14: Code Review Fix Report

**Fixed at:** 2026-06-02
**Source review:** `.planning/phases/14-client-predictor-confirmed-rendering/14-REVIEW.md`
**Iteration:** 1

**Summary:**
- Findings in scope (critical + warning): 3
- Fixed: 3
- Skipped (out of scope — info level): 2

## Fixed Issues

### CR-01: Unconstrained `StateDiff.cols`/`rows` enables client OOM crash

**Files modified:** `crates/nosh-client/src/screen.rs`
**Commit:** `eac7b3e`
**Applied fix:**

Added two module-level constants at the top of `screen.rs` (after the `use` block):

```rust
const MAX_TERMINAL_COLS: u16 = 512;
const MAX_TERMINAL_ROWS: u16 = 256;
```

Added a dimension guard at the top of `apply()`, immediately after the monotonic epoch check and before any `resize()` call. Diffs with `cols == 0`, `rows == 0`, `cols > 512`, or `rows > 256` are logged at `tracing::warn!` (with epoch/cols/rows fields) and returned early — consistent with the loss-tolerant datagram model (dropping a diff is safe; the next valid diff will bring state up to date).

Added five unit tests:
- `apply_oversized_cols_is_rejected_grid_unchanged` — cols = MAX+1, verifies epoch and grid unchanged
- `apply_oversized_rows_is_rejected_grid_unchanged` — rows = MAX+1, verifies epoch and grid unchanged
- `apply_zero_cols_is_rejected_no_panic` — cols = 0
- `apply_zero_rows_is_rejected_no_panic` — rows = 0
- `apply_max_allowed_dimensions_is_accepted` — cols = 512, rows = 256 (exactly at cap, must succeed)

All 19 unit tests pass. Worst-case two-grid allocation at cap: `512 × 256 × 12 × 2 ≈ 3 MB`.

---

### WR-01: `physical` committed before `stdout.write_all` — display model diverges on write error

**Files modified:** `crates/nosh-client/src/main.rs`
**Commit:** `e04c83b`
**Applied fix:**

In the datagram arm of `run_pump`, replaced the two silent `let _ = stdout.write_all(...)` / `let _ = stdout.flush()` discards with explicit error-checking arms:

```rust
if let Err(e) = stdout.write_all(&buf).await {
    tracing::warn!("stdout write_all failed: {e} — forcing full repaint");
    screen.reset_physical();
} else if let Err(e) = stdout.flush().await {
    tracing::warn!("stdout flush failed: {e} — forcing full repaint");
    screen.reset_physical();
}
```

On write failure, `screen.reset_physical()` resets the physical grid to blank so the next successful render emits a full repaint instead of silently diverging (where `physical == desired` would produce an empty diff forever).

`render_to_stdout()` still commits `physical` internally (consistent with its existing contract). Recovery is in the caller as the reviewer's guidance recommended.

---

### WR-02: Datagram error arm maps all quinn errors to `TransportDrop`, masking recoverable conditions

**Files modified:** `crates/nosh-client/src/main.rs`
**Commit:** `b6fa03e`
**Applied fix:**

Changed both error arms from wildcard pattern `Err(_)` to named `Err(e)`, adding `tracing::warn!` before the `TransportDrop` return:

Reliable-stream arm (previously line 668):
```rust
Err(e) => {
    tracing::warn!("reliable stream error, triggering reconnect: {e}");
    return Ok(PumpOutcome::TransportDrop);
}
```

Datagram arm (previously line 709):
```rust
Err(e) => {
    tracing::warn!("datagram channel error, triggering reconnect: {e}");
    return Ok(PumpOutcome::TransportDrop);
}
```

Both `quinn::ConnectionError` and `nosh_proto::read_message` errors implement `Display`, so the format string `{e}` produces useful diagnostic output.

---

**Additional fix (pre-existing clippy lint):**

A pre-existing `clippy::too_many_arguments` lint on `run_pump` (8 arguments, cap is 7) caused `cargo clippy -p nosh-client --tests -- -D warnings` to fail. This lint pre-dated all phase 14 changes. Added `#[allow(clippy::too_many_arguments)]` with an explanatory comment. Committed separately as `7de6e2a`.

---

## Skipped Issues

### IN-01: `last_col.unwrap_or(col)` in `render_to_stdout` — dead `unwrap_or` branch

**File:** `crates/nosh-client/src/screen.rs:318`
**Reason:** Info-level finding — out of scope for `fix_scope: critical_warning`.
**Original issue:** `last_col.unwrap_or(col)` at the column advance step is always `Some` at that point (set unconditionally earlier in the same iteration), so the `unwrap_or` fallback is unreachable. Not wrong; just obscures the invariant.

---

### IN-02: Missing test coverage for resize-during-render interaction (cursor clamping)

**File:** `crates/nosh-client/src/screen.rs:499–521`
**Reason:** Info-level finding — out of scope for `fix_scope: critical_warning`.
**Original issue:** The cursor-clamping logic in `resize()` (`confirmed_cursor.row.min(rows.saturating_sub(1))`) is not covered by a unit test. Incorrect clamping (e.g., using `rows` instead of `rows.saturating_sub(1)`) would produce an out-of-bounds cursor position causing visual corruption.

---

_Fixed: 2026-06-02_
_Fixer: Claude (gsd-code-fixer)_
_Iteration: 1_
