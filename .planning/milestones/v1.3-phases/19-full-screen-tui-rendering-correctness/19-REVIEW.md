---
phase: 19-full-screen-tui-rendering-correctness
reviewed: 2026-06-07T00:00:00Z
depth: standard
files_reviewed: 10
files_reviewed_list:
  - crates/nosh-server/src/terminal.rs
  - crates/nosh-server/src/server.rs
  - crates/nosh-server/Cargo.toml
  - crates/nosh-proto/src/datagram.rs
  - crates/nosh-client/src/main.rs
  - crates/nosh-client/src/predictor.rs
  - crates/nosh-client/src/screen.rs
  - crates/nosh-client/tests/render.rs
  - crates/nosh-client/tests/predict.rs
  - fuzz/fuzz_targets/osc_accumulation.rs
findings:
  critical: 3
  warning: 7
  info: 4
  total: 14
status: issues_found
---

# Phase 19: Code Review Report

**Reviewed:** 2026-06-07
**Depth:** standard
**Files Reviewed:** 10
**Status:** issues_found

## Summary

Phase 19 introduces the two-grid alt-screen model, wide-character cursor arithmetic, server-side OSC accumulation pre-bounding (SEC-03), predictor suppression on alt-screen entry, and the `alt_screen` field added to the `StateDiff` wire format. The core machinery is well-structured and the documented security invariants are implemented. However, there are three blocking defects — two correctness bugs and one security-relevant data-loss issue — plus several warnings of varying severity.

---

## Critical Issues

### CR-01: OSC pre-filter loses trailing ESC at slice boundary — parser permanently stays in OscString

**File:** `crates/nosh-server/src/terminal.rs:382-386`

**Issue:** When the pre-filter sees a lone `0x1B` at the very last byte of a slice while `in_osc` is true, the code falls through to the "Regular OSC payload byte" path (increments `osc_byte_count`). However, it does not commit the updated `in_osc`/`osc_byte_count` back to `self` before returning — those are committed by the bottom of the loop when `i < bytes.len()`. The ESC byte IS counted. But the critical flaw is different: when the *next* chunk begins with `0x5C` (`\`), the pre-filter has already set `in_osc = true` (from the prior call) and sees `0x5C` as a plain payload byte instead of the second byte of an ST terminator. So an `ESC \` split across two `advance()` calls never closes the OSC in the shadow tracker.

After a split-boundary `ESC \` the shadow `in_osc` flag stays `true` indefinitely. Every subsequent byte is counted against `osc_byte_count`. Eventually any long non-OSC output accumulates past `OSC_ACCUMULATION_MAX` and triggers a spurious truncation and parser reset — corrupting the parse of whatever was on screen at the time. In the worst case a crafted server process can emit `ESC ]2;` followed by content up to 1 MiB – 1 bytes, then `ESC` at the end of a 4 KiB pipe read, and then `\` in the next read. The pre-filter will never reset `in_osc` and all subsequent terminal state (cursor, grid) will be corrupted once the byte count wraps.

The `vte` parser itself handles this correctly via its internal state machine; only the shadow tracker is broken.

**Fix:**
```rust
// In osc_prefilter, replace the "ESC at very end of slice" comment/fall-through
// with an explicit in-OSC-continue that ALSO handles a next-byte lookahead into
// the next call. The simplest correct fix: track a `pending_esc` flag:
if b == 0x1B {
    if i + 1 < bytes.len() {
        // ... (existing logic, unchanged)
    } else {
        // ESC at very end: do NOT count it as payload — it might be ST.
        // Leave in_osc = true, do NOT increment osc_byte_count.
        // Signal to the next call that the previous call ended mid-ESC.
        // Easiest: add a `self.pending_esc: bool` field and set it here,
        // checked at the top of the next osc_prefilter call to handle
        // the potential ST completion before the normal scan begins.
        i += 1;
        continue; // do not fall through to osc_byte_count increment
    }
}
```

Add a `pending_esc: bool` field to `TerminalState`, reset it in `esc_dispatch` (RIS) and on overflow, and handle it at the start of `osc_prefilter`.

---

### CR-02: Wide-char cursor arithmetic in `print_char` wraps on the same line instead of wrapping after the continuation cell

**File:** `crates/nosh-server/src/terminal.rs:631-637`

**Issue:** After writing a wide (width-2) glyph, the cursor is advanced by `col_width` (2). The wrap check is:
```rust
self.cursor.col = self.cursor.col.saturating_add(col_width);
if self.cursor.col >= self.cols {
    self.cursor.col = 0;
    self.lf();
}
```

Consider a terminal with `cols=80` and the cursor at `col=79`. Writing a wide char: `col_width=2`. The right-edge guard in the write block checks `col + col_width > self.grid[row].len()` to suppress the continuation cell write (T-19-04), which fires correctly. But then `cursor.col = 79 + 2 = 81`, which is `>= 80`, so the code wraps: `cursor.col = 0` and calls `lf()`. The cursor moves to row+1, col 0 — which looks correct for the wrap case.

The deeper issue is at `cols=80`, cursor at `col=78`. Writing a wide char: `cont_col = 79 < 80`, so the continuation cell IS written at col 79. Then `cursor.col = 78 + 2 = 80 >= 80`, so it wraps to `(row+1, 0)`. That is correct.

The real defect: cursor at `col=79`, cols=80. `col + col_width = 79 + 2 = 81 > 80`, so the continuation cell write is suppressed (T-19-04 guard). But the primary glyph IS written at col 79. Now the cursor wraps to `(row+1, 0)`. On the client side (`screen.rs:apply`), the DiffRun carries the wide char at `start_col=79`, and the client's apply loop iterates `(79..).zip(chars())` writing only the single scalar. The client does NOT write a continuation cell (apply doesn't know about width). The client cursor from `diff.cursor` will be at `(row+1, 0)`. So far consistent.

However in `compute_diff_runs` (server.rs:224-276): if a wide-char cell at col=79 has `wide: false` (the primary glyph was written there), and there is no continuation cell after it, the run scan includes col=79 char and advances `col` by 1. That is fine. But after a subsequent render where col=79 is the primary glyph and the diff encoder sees it as changed, a DiffRun is emitted with `start_col=79` and `chars = "中"`. On the client, `apply` tries to write this at `row_cells[79]`. The client `apply` does NOT set `wide=true` anywhere — the `wide` field is always `false` in client `Cell` construction (screen.rs:263-269). This means the client `physical` grid after rendering will show the glyph at col=79 and the terminal itself is positioned past col=79 by the terminal's own wide-char rendering, potentially creating a one-column desync between the client's physical model and the actual terminal state.

This is a latent correctness issue: the client renderer doesn't propagate `wide=true` into the `confirmed` or `physical` grids (screen.rs `apply` never sets `wide` to anything but `false`), so `emit_diff` will never skip a continuation-cell position in the physical model, and could re-emit a space at col=79 in a later render thinking the physical cell changed when it hasn't.

**Fix:** In `screen.rs:apply`, when the DiffRun's char has unicode width 2, set `wide: true` on the cell at `col+1` if in bounds:
```rust
for (col, ch) in (start..).zip(run.chars.chars()) {
    if col >= row_cells.len() { break; }
    row_cells[col] = Cell { ch, style: run.style, fg: run.fg, bg: run.bg, wide: false };
    // Set continuation marker for wide glyphs
    use unicode_width::UnicodeWidthChar;
    if UnicodeWidthChar::width(ch) == Some(2) && col + 1 < row_cells.len() {
        row_cells[col + 1] = Cell { ch: ' ', style: run.style, fg: run.fg, bg: run.bg, wide: true };
    }
}
```
Without this, the client's `emit_diff` will not skip continuation cells and may generate spurious space writes.

---

### CR-03: `resize()` for the saved primary grid discards rows into the ACTIVE screen's scrollback

**File:** `crates/nosh-server/src/terminal.rs:478-484`

**Issue:** When the terminal is resized while the alternate screen is active, `resize()` handles the saved primary grid. On shrink, excess top rows are pushed to `self.scrollback`:
```rust
let top = prim_grid.remove(0);
self.scrollback.push_back(top);
```

`self.scrollback` is the scrollback for the ACTIVE viewport. When the alternate screen is active (`echo_state.alt_screen == true`), `scroll_up()` explicitly gates on `!self.echo_state.alt_screen` to prevent alt-screen content from contaminating primary scrollback (D-19-09). But here in `resize()`, the saved primary rows are pushed into `self.scrollback` unconditionally regardless of which screen is active.

This means: after a resize-while-alt-active, the scrollback buffer contains rows from the primary screen that were pushed during resize. When the user later exits the alt screen and scrolls back, they will see these rows mixed in. More seriously, the `SCROLLBACK_LINE_CAP` check here uses `self.scrollback.len()` which already includes active-screen-period content — the accounting is crossed.

The other direction is also wrong: if the alt screen was larger before resize and its *active* rows scrolled into `self.scrollback` (via `scroll_up` during alt-screen) — no, `scroll_up` correctly discards those. But the resize path does not have the gate.

**Fix:** In the `saved_primary` resize path, push excess rows into a separate temporary Vec that is discarded (rows that fall off the primary grid when the primary is in the background should be lost, not merged into alt-screen scrollback), or defer the primary resize until `exit_alt_screen` is called:
```rust
// Replace self.scrollback.push_back(top) with:
drop(top); // primary rows that no longer fit are discarded; they were off-screen
           // while the alt-screen was active anyway
```
Since the primary was not visible during the alt-screen session, its truncated rows have not been shown to the user and do not belong in the scrollback the user can scroll through.

---

## Warnings

### WR-01: `cull()` index tracking is invalidated by `kill_epoch()` internal `retain`

**File:** `crates/nosh-client/src/predictor.rs:620-626`

**Issue:** `cull()` collects indices of predictions to remove into `to_remove: Vec<usize>`. It also calls `self.kill_epoch(epoch)` which calls `self.pending.retain(...)`, removing elements from `pending` and potentially shifting all subsequent indices. After `kill_epoch`, the previously collected indices in `to_remove` may refer to wrong elements in the new `pending`. The subsequent loop:
```rust
for &i in to_remove.iter().rev() {
    if i < self.pending.len() {
        self.pending.remove(i);
    }
}
```
guards against out-of-bounds, but may remove the wrong element (an element that shifted into a now-invalid index). In the common case this only occurs when tentative and non-trivially-correct predictions are in the same cull pass, which is unusual. But the guard `if i < self.pending.len()` is not sufficient — an index may be in-bounds but point to a different element than intended.

**Fix:** Use a stable identity rather than index. Collect the actual `tentative_until_epoch` values for kill and the row/col of predictions to remove, then use `retain` to remove them:
```rust
self.pending.retain(|p| {
    !epochs_to_kill.contains(&p.tentative_until_epoch)
    && !to_remove_positions.contains(&(p.row, p.col, p.epoch_required))
});
```
Or, more simply: call `kill_epoch` and the to_remove loop in separate passes, where `to_remove` is re-indexed after `kill_epoch` (re-collect after the kill).

---

### WR-02: `osc_prefilter` counts the ESC byte of a potential ST terminator against `osc_byte_count` when it appears at end of slice

**File:** `crates/nosh-server/src/terminal.rs:382-399`

**Issue:** At line 382, when `in_osc` is true and `b == 0x1B` and `i + 1 >= bytes.len()` (ESC at end of slice), the code falls through to the `osc_byte_count += 1` increment below. A lone ESC byte that is actually the first byte of an ST terminator (`ESC \`) is counted against the payload accumulation budget. This is a double-accounting issue: the ESC contributes to the 1 MiB cap even though it may be a terminator. In most cases this is negligible (1 extra byte), but combined with the CR-01 tracking bug it contributes to the incorrect in_osc state.

**Fix:** Add `continue` after the "ESC at end of slice" comment block to skip the `osc_byte_count += 1` increment, and record the pending ESC as noted in CR-01.

---

### WR-03: `compute_diff_runs` emits a DiffRun starting at a wide-char continuation cell when style changes mid-wide-glyph

**File:** `crates/nosh-server/src/server.rs:236-238`

**Issue:** The inner extension loop of a run (lines 250-271) skips continuation cells with `col += 1; continue`. After this skip, the loop checks if the *next* cell has a different style and may break the run. But if the continuation cell is the last cell in the row, `col` is incremented past the end and the while loop exits normally — this is fine.

The outer loop (lines 224-232) at the top has a guard:
```rust
if cell.wide { col += 1; continue; }
```
This prevents a DiffRun from starting at a continuation cell. That is correct.

However, the inner extension loop's handling of a continuation cell that immediately follows a style-boundary is subtly wrong: when `c2.style != style || c2.fg != fg || c2.bg != bg` is checked at a continuation cell, the continuation cell is NOT a style-break candidate (it has the same style as its primary because both were written with `self.sgr` at write time). But if the NEXT non-continuation cell after this wide glyph has a different style, the run is broken correctly. So in practice this is benign. However, when a `wide:true` cell at the end of the current run is encountered, `col` is advanced and the next iteration starts fresh — which is correct.

The actual warning is: if `col_width == 2` and `col + 1 == self.cols`, the continuation cell is suppressed server-side (T-19-04), but the primary cell's `wide` flag is still `false` (no continuation). The DiffRun for that row will include the primary char in `chars`. The client side does not know the char was wide. As noted in CR-02, this requires the client to reconstruct width from the character itself.

**Fix:** Document in `DiffRun` that consumers must derive cell width from the char's Unicode width, not from any field in the struct. Alternatively, carry a `widths: Vec<u8>` field. For now, the client's `apply` method needs to set `wide=true` on cont cells as described in CR-02.

---

### WR-04: `PredictCursorRight` does not use `saturating_add`

**File:** `crates/nosh-client/src/predictor.rs:422-425`

**Issue:**
```rust
InputAction::PredictCursorRight => {
    if self.predicted_cursor.col + 1 < self.term_cols {
        self.predicted_cursor.col += 1;
    }
```
If `self.predicted_cursor.col` is `u16::MAX` (65535) and `self.term_cols` is 0 (which is rejected by the server but not by the predictor's own `set_size`), `col + 1` overflows in debug mode (panic) or wraps in release. In normal operation `term_cols` is bounded by `MAX_TERMINAL_COLS = 512`, so `col` is always at most 511 and `col + 1` cannot overflow a `u16`. However the check `col + 1 < self.term_cols` has a potential overflow if col is close to `u16::MAX` — this can happen if `set_size` is called with a large `cols` value (the predictor does not cap `term_cols`).

**Fix:** Use saturating arithmetic:
```rust
if self.predicted_cursor.col.saturating_add(1) < self.term_cols {
    self.predicted_cursor.col += 1;
}
```

---

### WR-05: `PredictBatch` exit from inner loop does not call `trace_exit!()` when `become_tentative` fires

**File:** `crates/nosh-client/src/predictor.rs:533-537`

**Issue:** Inside the `PredictBatch` arm, when the right-edge guard fires (`become_tentative`), the code calls `trace_exit!()` and returns early. This is correct for early-return tracing. However the `trace_exit!()` macro captures `pending_before` at the point `on_input` was entered, but `pending_after = self.pending.len()` at exit. Since `become_tentative` does NOT add to or remove from `pending`, the trace will show `predictions_dropped = 0` even if the batch partially predicted some chars before hitting the edge. This is misleading for diagnostics — the trace event should reflect the chars already predicted in the batch before the early exit.

This is a minor quality/observability issue rather than a correctness defect, but it affects the reliability of the D-03 tracing.

**Fix:** Consider emitting the trace after the `for (ch, col_width) in chars` loop to capture the actual final state rather than on each early exit from inside the loop.

---

### WR-06: `find_line_end` always scans the full row from right-to-left even if the terminal is very wide

**File:** `crates/nosh-client/src/predictor.rs:754-762`

**Issue:**
```rust
fn find_line_end(&self, row: u16, screen: &ClientScreen) -> u16 {
    for col in (0..self.term_cols).rev() {
        let cell = screen.confirmed_cell(row, col);
        if cell.ch != ' ' {
            return col + 1;
        }
    }
    0
}
```
`term_cols` is a `u16` and the loop iterates `0..self.term_cols`. If `term_cols == 0`, `(0..0).rev()` is empty and returns 0 — safe. But if `term_cols == u16::MAX` (which the predictor allows since it doesn't cap), this loops 65535 times per End/Ctrl-E keystroke. In practice `term_cols` comes from `MAX_TERMINAL_COLS=512` via the validated resize path so this is bounded. The predictor's `set_size` has no cap, so a future caller could set `term_cols` to 65535.

**Fix:** Cap `term_cols` in `set_size` to `MAX_TERMINAL_COLS` or document that `term_cols` must be validated before passing to `PredictionOverlay::new`/`set_size`.

---

### WR-07: `run_reattach_session` does not propagate `OscTitle`/`OscClipboard` terminal control frames in the select! loop

**File:** `crates/nosh-server/src/server.rs:1135-1175` (reattach pump loop, `out_rx.recv()` arm)

**Issue:** The reattach session pump in `run_reattach_session` calls `slot.drain_terminal_control()` and forwards the results, mirroring the `run_session` pattern. This looks correct. However, examining the reattach `msg` arm (line 1247), it handles `Message::SessionClose` and `Message::Ack` but does NOT handle `Message::SessionOpen { .. }` as a protocol error. The fresh session's `msg` arm (line 784) breaks with `SessionEnd::ClientClosed` on `SessionOpen`. The reattach session ignores it via the `Ok(_) => {}` catch-all at line 1268. An unexpected `SessionOpen` mid-session is a protocol violation that should be treated as `ClientClosed` rather than silently ignored.

**Fix:** In the reattach loop's `msg` arm, add explicit handling:
```rust
Ok(Message::SessionOpen { .. }) => {
    break SessionEnd::ClientClosed; // protocol violation in reattach
}
```

---

## Info

### IN-01: `screen.rs:apply` does not propagate `wide` field — all client cells have `wide: false`

**File:** `crates/nosh-client/src/screen.rs:259-270`

**Issue:** The `apply` method constructs every `Cell` with `wide: false` hardcoded. This is documented ("Wide character handling is deferred") but is directly at odds with Phase 19's `wide` field addition to the server-side `Cell`. The client `emit_diff` at line 468 skips `want.wide` cells to avoid double-writing — but since the client never sets `wide: true`, this guard is permanently dead. If CR-02 is fixed and the client starts setting `wide: true` on continuation cells, this guard will become active. Currently it is unreachable code.

**Fix:** When CR-02 fix is applied, the `wide` guard in `emit_diff` will activate automatically. No separate action needed beyond the CR-02 fix.

---

### IN-02: `osc_prefilter` resets `osc_byte_count` to 0 at OSC start but leaves it accumulating the introducer bytes

**File:** `crates/nosh-server/src/terminal.rs:341-355`

**Issue:** When an OSC start is detected (`0x9D` or `ESC ]`), `osc_byte_count` is reset to 0. The introducer bytes themselves are not counted. This means the 1 MiB cap is applied only to payload bytes, not to the overall OSC including the `ESC ]` prefix. This is fine and intentional (the prefix is 1-2 bytes out of 1 MiB). However the comment at line 74-77 says "this bounds what vte ALLOCATES while parsing" — vte does allocate for the introducer as well, so the true maximum is `OSC_ACCUMULATION_MAX + 3 bytes` (2-byte ESC ] + 1 count byte). This is negligibly above the documented cap.

**Fix:** No code change required; add a comment clarifying that the cap applies to payload bytes only and the total vte allocation may be `OSC_ACCUMULATION_MAX + 3 bytes`.

---

### IN-03: `fuzz/fuzz_targets/osc_accumulation.rs` SEC-03 multi-chunk test does not verify the post-overflow parser handles normal non-OSC sequences

**File:** `fuzz/fuzz_targets/osc_accumulation.rs:71`

**Issue:** After the 10 MiB overflow and resync, the fuzz target checks that a subsequent OSC 2 title parses (`state2.advance(b"\x1b]2;OK\x07")`) and that OSC 52 still dispatches. It does not verify that plain text output (e.g. `state2.advance(b"hello")`) advances the cursor correctly after resync. If the parser resync left any phantom state (e.g. `in_osc` still `true` due to CR-01), `hello` would be silently swallowed into the shadow OSC accumulator.

**Fix:** Add after the existing resync assertions:
```rust
state2.advance(b"hello");
// Verify cursor advanced (plain text after resync must not be consumed by OSC accumulator).
assert_eq!(state2.cursor().col, 5, "plain text after OSC overflow resync must advance cursor");
```

---

### IN-04: `Cargo.toml` Cargo.toml comment for vte acknowledges an outstanding security issue without a tracking resolution

**File:** `crates/nosh-server/Cargo.toml:33-47`

**Issue:** The Cargo.toml comment at line 33-46 acknowledges that Phase 19 is supposed to fix OSC-OOM and references "phase 999.7". This comment describes the problem being solved by Phase 19's SEC-03 work. After Phase 19 lands, this comment should be updated to reflect that the mitigation is in place and to remove the "(tracked + fix in phase 999.7)" language, which is now stale. Leaving it creates confusion for future readers about whether the issue is still open.

**Fix:** Update the comment to reflect that SEC-03 is implemented and point to the relevant constants and advance() method for the implemented solution.

---

_Reviewed: 2026-06-07_
_Reviewer: Claude (gsd-code-reviewer)_
_Depth: standard_
