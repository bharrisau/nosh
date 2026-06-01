---
phase: 14-client-predictor-confirmed-rendering
reviewed: 2026-06-02T00:00:00Z
depth: standard
files_reviewed: 4
files_reviewed_list:
  - crates/nosh-client/src/screen.rs
  - crates/nosh-client/src/lib.rs
  - crates/nosh-client/src/main.rs
  - crates/nosh-client/tests/render.rs
findings:
  critical: 1
  warning: 2
  info: 2
  total: 5
status: issues_found
---

# Phase 14: Code Review Report

**Reviewed:** 2026-06-02
**Depth:** standard
**Files Reviewed:** 4
**Status:** issues_found

## Summary

Phase 14 implements `ClientScreen` — a dual-grid (confirmed + physical) ANSI-diff
compositor — and wires it into `run_pump` as the sole display path. The overall
architecture is sound: the monotonic epoch guard, OOB row/col guards in `apply()`,
the single display path invariant, and the datagram/stream ack separation are all
implemented correctly.

One BLOCKER was found: `apply()` has no upper bound on `diff.cols`/`diff.rows` before
invoking `resize()`, which allocates two grids proportional to `cols × rows`. A
compromised server can send a `StateDiff` with `cols=65535, rows=65535`, triggering an
attempt to allocate ~100 GB and crashing the client with OOM. This directly contradicts
the phase's stated threat model ("malicious/compromised server hardening — a crafted
StateDiff … must NOT panic or index out-of-bounds the client") — OOM crash is an
equivalent failure mode.

Two warnings were also found: (1) `stdout.write_all` errors are silently discarded while
`physical` has already been committed inside `render_to_stdout`, which permanently
desynchronises the client's display model; and (2) the datagram error arm maps ALL quinn
datagram errors to `PumpOutcome::TransportDrop` including transient errors that should
not trigger a full reconnect.

No nosh-server imports in production code, correct MoveTo `(col, row)` argument order,
correct epoch-ack on the datagram channel (distinct from `Ack{seq}` on the stream), and
correct `select!` cancellation-safety were all confirmed.

---

## Critical Issues

### CR-01: Unconstrained `StateDiff.cols`/`rows` enables client OOM crash

**File:** `crates/nosh-client/src/screen.rs:155–165`
**Issue:** `apply()` calls `self.resize(diff.cols, diff.rows)` (line 163) without first
bounding `diff.cols` or `diff.rows`. `resize()` calls `Self::make_grid(cols, rows)`
(line 224) which allocates `rows × cols` `Cell` structs. A `Cell` is approximately
12 bytes. With `diff.cols = 65535` and `diff.rows = 65535`:

```
65535 × 65535 × 12 bytes × 2 grids ≈ 103 GB
```

This will exhaust virtual memory and crash the client process (OOM). The process
cannot catch `std::alloc::AllocError` in safe Rust; the Rust allocator panics or the
OS kills the process. A server that has been compromised can exploit this to crash any
connected client with a single datagram.

The phase threat model (T-14-01) explicitly covers "a crafted StateDiff … must NOT panic
or index out-of-bounds the client". OOM-induced process termination is the same class of
failure as a panic.

Note: `decode_datagram` already caps the run-count at `MAX_RUNS` (4096) as a DoS guard
(T-11-02), but applies no analogous guard to the terminal dimensions.

**Fix:** Add dimension constants and validate before resizing:

```rust
/// Maximum allowed terminal width — any larger value in a StateDiff is rejected.
/// Matches xterm's practical upper bound and keeps the grid allocation <≈10 MB.
const MAX_TERMINAL_COLS: u16 = 512;
/// Maximum allowed terminal height.
const MAX_TERMINAL_ROWS: u16 = 256;

pub fn apply(&mut self, diff: &StateDiff) {
    if diff.epoch <= self.last_applied_epoch {
        return;
    }
    // T-14-01 extension: reject implausible terminal dimensions before resize.
    if diff.cols == 0 || diff.rows == 0
        || diff.cols > MAX_TERMINAL_COLS
        || diff.rows > MAX_TERMINAL_ROWS
    {
        tracing::warn!(
            epoch = diff.epoch,
            cols = diff.cols,
            rows = diff.rows,
            "StateDiff dimensions out of range — discarding"
        );
        return;
    }
    if diff.cols != self.cols || diff.rows != self.rows {
        self.resize(diff.cols, diff.rows);
    }
    // ... rest unchanged
}
```

Choose `MAX_TERMINAL_COLS`/`MAX_TERMINAL_ROWS` values to match project requirements;
512 × 256 keeps the worst-case two-grid allocation at `512 × 256 × 12 × 2 ≈ 3 MB`.

---

## Warnings

### WR-01: `physical` committed before `stdout.write_all` — display model diverges on write error

**File:** `crates/nosh-client/src/main.rs:685–691` and `crates/nosh-client/src/screen.rs:324–332`

**Issue:** `render_to_stdout` writes ANSI bytes into `out: &mut Vec<u8>` and then
**commits `physical = desired` inside the function** (lines 327–332 of `screen.rs`) before
returning. The caller in `run_pump` then writes `buf` to tokio stdout:

```rust
screen.render_to_stdout(&mut buf).unwrap_or_else(|e| { tracing::warn!(...) });
if !buf.is_empty() {
    let _ = stdout.write_all(&buf).await;   // error silently dropped
    let _ = stdout.flush().await;           // error silently dropped
}
```

If `write_all` fails (broken pipe, terminal closed), the bytes were never written to the
terminal, but `physical` was already advanced to `desired`. On the next render call, the
diff is empty (physical == desired), so no cells are re-emitted. The displayed terminal
stays stale, and the client has no mechanism to detect or recover within the current
session. `reset_physical()` would force a full repaint, but it is never called on
`write_all` failure.

**Fix:** Propagate the `write_all` error and call `screen.reset_physical()` on failure so
the next render re-emits everything:

```rust
screen.render_to_stdout(&mut buf).unwrap_or_else(|e| {
    tracing::warn!("render_to_stdout error: {e}");
});
if !buf.is_empty() {
    if let Err(e) = stdout.write_all(&buf).await {
        tracing::warn!("stdout write_all failed: {e} — forcing full repaint");
        screen.reset_physical();
    } else if let Err(e) = stdout.flush().await {
        tracing::warn!("stdout flush failed: {e} — forcing full repaint");
        screen.reset_physical();
    }
}
```

Alternatively, move the physical commit out of `render_to_stdout` and into the caller,
after the successful write.

---

### WR-02: Datagram error arm maps all quinn errors to `TransportDrop`, masking recoverable conditions

**File:** `crates/nosh-client/src/main.rs:704–707`

**Issue:** The datagram `select!` arm handles `Err(_)` from `conn.read_datagram()` by
returning `PumpOutcome::TransportDrop`, which triggers a full reconnect with exponential
backoff:

```rust
Err(_) => {
    // Transport drop on datagram channel — mirror reliable-stream behavior.
    return Ok(PumpOutcome::TransportDrop);
}
```

`quinn::Connection::read_datagram()` returns `quinn::ConnectionError`. All variants of
`ConnectionError` do indicate the connection is gone (VersionMismatch, TransportError,
Reset, TimedOut, LocallyClosed, CidGenerationFailure). This is therefore correct in the
general case — there is no recoverable variant of `ConnectionError`.

However, the error is completely suppressed (`_`). If the connection drops for an
unexpected reason, there is no log entry: operators and developers have no visibility into
why reconnects are occurring. The reliable-stream arm at line 669 has the same pattern
(`Err(_) => return Ok(PumpOutcome::TransportDrop)`).

**Fix:** Log the error at warn level before returning:

```rust
Err(e) => {
    tracing::warn!("datagram channel error, triggering reconnect: {e}");
    return Ok(PumpOutcome::TransportDrop);
}
```

Apply the same fix to the reliable-stream `Err` arm at line 669.

---

## Info

### IN-01: `last_col.unwrap_or(col)` in `render_to_stdout` — dead `unwrap_or` branch

**File:** `crates/nosh-client/src/screen.rs:318`

**Issue:** After `last_col` is set to `Some(col)` at line 302 (within the same iteration),
line 318 reads:

```rust
last_col = Some(last_col.unwrap_or(col) + 1);
```

At line 318, `last_col` is always `Some(col)` (set unconditionally at line 302 before any
write). The `unwrap_or(col)` fallback is never reachable. It is not wrong, but it
obscures the invariant.

**Fix:** Replace with `.expect()` or a direct unwrap to make the invariant explicit, or
simplify to:

```rust
last_col = Some(col + 1);
```

This is safe because `col` here is the loop variable `c as u16`, which is always the
current cell's column.

---

### IN-02: Missing test coverage for resize-during-render interaction (cursor clamping)

**File:** `crates/nosh-client/src/screen.rs:499–521` (existing `apply_resize_changes_dims_and_resets_physical` test)

**Issue:** The existing resize test (`apply_resize_changes_dims_and_resets_physical`)
verifies that grid dimensions change and physical is reset, but does not verify cursor
clamping when the cursor position is outside the new bounds. The clamping logic at
lines 230–232:

```rust
self.confirmed_cursor.row = self.confirmed_cursor.row.min(rows.saturating_sub(1));
self.confirmed_cursor.col = self.confirmed_cursor.col.min(cols.saturating_sub(1));
```

is tested by no unit test. If `confirmed_cursor.row = 23` when shrinking to `rows = 5`,
the clamp must reduce it to 4. An incorrect clamp (e.g., using `rows` instead of
`rows.saturating_sub(1)`) would leave cursor at row 5 — out of bounds for a 5-row grid —
and `render_to_stdout` would emit `MoveTo(col, 5)` which positions the cursor on row 6,
causing visual corruption.

**Fix:** Add a unit test:

```rust
#[test]
fn resize_clamps_cursor_to_new_bounds() {
    let mut screen = ClientScreen::new(80, 24);
    // Place cursor at bottom-right of original grid.
    let diff = StateDiff {
        epoch: 1, cols: 80, rows: 24,
        cursor: CursorPos { row: 23, col: 79 },
        runs: vec![],
    };
    screen.apply(&diff);
    // Shrink to 10x5 — cursor must clamp to (4, 9).
    let diff2 = StateDiff {
        epoch: 2, cols: 10, rows: 5,
        cursor: CursorPos { row: 23, col: 79 }, // old cursor carried in diff
        runs: vec![],
    };
    screen.apply(&diff2);
    // After apply, confirmed_cursor must be clamped.
    let mut buf = Vec::new();
    screen.render_to_stdout(&mut buf).unwrap();
    // MoveTo(9, 4) must appear somewhere in the ANSI output — not MoveTo(79, 23).
    let s = String::from_utf8_lossy(&buf);
    assert!(s.contains("\x1b[5;10H") || s.contains("\x1b[5;10H"),
        "cursor must be clamped to (row=4, col=9); got: {:?}", s);
}
```

(Note: crossterm `MoveTo(col, row)` emits VT100 `\x1b[row+1;col+1H` because VT100 uses
1-based coordinates. Verify the exact escape sequence by inspection.)

---

_Reviewed: 2026-06-02_
_Reviewer: Claude (gsd-code-reviewer)_
_Depth: standard_
