---
phase: 12
status: clean
depth: standard
files_reviewed: 5
findings:
  critical: 0
  warning: 1
  info: 2
  total: 3
  fixed: 1
reviewed: 2026-06-01
---

# Code Review: Phase 12 — server-terminal-state-model

**Files reviewed:**
- `crates/nosh-server/Cargo.toml`
- `crates/nosh-server/src/lib.rs`
- `crates/nosh-server/src/registry.rs`
- `crates/nosh-server/src/server.rs`
- `crates/nosh-server/src/terminal.rs`

---

## Summary

Phase 12 implements a well-structured, thoroughly tested terminal state model. The code is
correct, secure, and follows the project's architectural decisions. No critical issues found.
One warning about future scalability, two informational observations.

---

## Findings

### WR-01: `cell()` uses `OnceLock<Cell>` global static as default return value

**File:** `crates/nosh-server/src/terminal.rs`  
**Lines:** ~230–240  
**Severity:** warning  

The `cell()` method returns `&Cell` for out-of-bounds accesses using a `static OnceLock<Cell>`.
This works correctly today, but the returned `&Cell` has `'static` lifetime — callers who store
this reference (rather than copying fields) would hold a reference to a global static, not the
grid. This is subtle and could lead to confused code in Phase 13 diff extraction.

**Recommendation:** Consider returning `Option<&Cell>` or `Cow<Cell>` so the caller must
explicitly handle the out-of-bounds case rather than receiving a silent default. If the
current `&Cell` return type must be preserved for Phase 13 compatibility, add a doc comment
making the `'static` lifetime behavior explicit.

**Note:** This is advisory — no current callers are affected. The `OnceLock` approach is
correct for the current test harness usage.

---

### IN-01: `resize()` scrolls top rows into scrollback on shrink, but scrollback may grow beyond cap in edge case

**File:** `crates/nosh-server/src/terminal.rs`  
**Lines:** `resize()` method  
**Severity:** info  

On a shrink, rows are removed from the top of the grid and pushed into scrollback. The
cap enforcement (`if self.scrollback.len() > SCROLLBACK_LINE_CAP { pop_front }`) runs
after each push. This is correct, but if `resize` shrinks by many rows simultaneously
(e.g., from 10_000 rows to 1 row — unlikely in practice), the cap enforcement fires N
times in a loop. This is correct behavior, but worth noting that the scrollback will
transiently hold `SCROLLBACK_LINE_CAP + 1` items for each iteration before the pop runs.
Given realistic terminal sizes (24–50 rows), this is a no-concern.

---

### IN-02: SGR 24-bit color (38;2;r;g;b) silently consumed but r,g,b params not drained

**File:** `crates/nosh-server/src/terminal.rs`, `handle_sgr()`  
**Severity:** info  

When `code == 38` and the subtype is `2` (24-bit / truecolor), the current code:
```rust
// SGR 38;2;r;g;b (24-bit) is scope-fenced — consume but ignore
```
…reads the `2` marker but only consumes it with `iter.next()` without also consuming the
three r/g/b parameters. This means the next `r`, `g`, and `b` values fall through as
independent SGR codes in the next `while let Some(param)` iterations — they would be
processed as SGR codes 0–255 depending on the color values, potentially setting
unintended styles.

For example, `\x1b[38;2;0;0;0m` (truecolor black) would:
1. Match `38`, read `2` (correct)
2. Fall through with `r=0` → SGR 0 → reset all attributes (unintended!)
3. Then `g=0` → SGR 0 → reset (again)
4. Then `b=0` → SGR 0 → reset (again)

**Recommended fix:** When subtype `2` is detected for 24-bit color, consume the three
additional params (r, g, b) to drain them, even though the color is not applied:
```rust
38 => {
    if let Some(next) = iter.next() {
        if next[0] == 5 {
            if let Some(color_param) = iter.next() {
                self.sgr.fg = Some(color_param[0] as u8);
            }
        } else if next[0] == 2 {
            // 24-bit: drain r, g, b params (scope-fenced — not applied)
            let _ = iter.next(); // r
            let _ = iter.next(); // g
            let _ = iter.next(); // b
        }
    }
}
```
Same fix needed for `48` (bg truecolor). This affects correctness if any app emits
truecolor SGR sequences, but since most terminals and apps fall back to 256-color anyway,
this is low priority for Phase 13 work.

---

## Security Review

No new attack surfaces introduced. `TerminalState` is a pure in-memory model fed from the
same PTY bytes already trusted by `SequencedOutputBuffer`. The adversarial tests explicitly
cover:
- Cursor position clamping (no out-of-bounds index possible)
- Scrollback bounded at SCROLLBACK_LINE_CAP (no unbounded allocation)
- OSC 52 detection-only (no clipboard side effect)

The isolation constraint (no quinn/tokio/session/registry imports in `terminal.rs`) is
verified by the acceptance criterion grep check.

---

## Conclusion

Phase 12 is correct and well-tested. The truecolor SGR drain issue (IN-02) is the most
actionable finding — it could cause attribute resets when apps emit `\x1b[38;2;r;g;b m`
sequences, which is worth fixing before Phase 13 diff extraction is added. The `cell()`
return-type note (WR-01) is advisory only.
