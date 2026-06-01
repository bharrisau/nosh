---
phase: 12-server-terminal-state-model
reviewed: 2026-06-01T12:00:00Z
depth: deep
files_reviewed: 4
files_reviewed_list:
  - crates/nosh-server/src/terminal.rs
  - crates/nosh-server/src/registry.rs
  - crates/nosh-server/src/server.rs
  - crates/nosh-server/Cargo.toml
findings:
  critical: 3
  warning: 2
  info: 2
  total: 7
status: issues_found
---

# Phase 12: Code Review Report

**Reviewed:** 2026-06-01T12:00:00Z
**Depth:** deep
**Files Reviewed:** 4
**Status:** issues_found

## Summary

Phase 12 introduces a ~1300-line `TerminalState` with a `vte::Perform` implementation, wires it into `SessionSlot`, and converts three `push_output` callsites to `push_output_and_parse`. The truecolor SGR drain bug (previously identified as IN-02) was correctly fixed. The isolation tests are thorough for ordinary input.

However, adversarial-input analysis reveals three critical defects introduced by this phase. Two of them (u16 overflow in cursor-motion arithmetic and unbounded memory in `TerminalState::resize`) are confirmed by the existence of a throwaway adversarial probe file (`zz_adversarial_probe.rs`) that was left in the repo after verification, documenting that the bugs were identified but never fixed. The third (vte's unbounded OSC buffer in `std` mode) is a latent DoS vector that allows a single session's PTY output to exhaust server memory.

The prior review (`status: clean`) missed all three critical findings.

---

## Structural Findings (fallow)

None provided.

---

## Narrative Findings (AI reviewer)

## Critical Issues

### CR-01: u16 Integer Overflow in CSI B/C Cursor Motion — Debug Panic, Release Wrong Position

**File:** `crates/nosh-server/src/terminal.rs:473,478`

**Issue:** Cursor-down (`CSI B`) and cursor-right (`CSI C`) perform raw `u16 + u16` addition without overflow protection:

```rust
// Line 473
self.cursor.row = (self.cursor.row + n).min(self.rows.saturating_sub(1));
// Line 478
self.cursor.col = (self.cursor.col + n).min(self.cols.saturating_sub(1));
```

`cursor.row` and `n` are both `u16`. `vte` saturates CSI parameters at `u16::MAX` (65535) via `saturating_mul`/`saturating_add` in its accumulator. On an 80×24 terminal, if the cursor is at the bottom row (`cursor.row = 23`) and the parameter is 65535, the addition is `23 + 65535 = 65558`, which exceeds `u16::MAX` (65535).

In **debug builds** (the build mode used during development and CI): this panics with "attempt to add with overflow", which kills the QUIC connection handler task — a DoS via adversarial PTY output. In **release builds**: the value wraps silently (`65558 mod 65536 = 22`), placing the cursor at the wrong row (22 instead of 23), which corrupts subsequent terminal state.

The throwaway adversarial probe `crates/nosh-server/tests/zz_adversarial_probe.rs` (specifically `probe_csi_repeat_after_motion`) was written during phase verification to test this exact path and was never removed, indicating the bug was identified but left unfixed.

This affects any PTY output containing `CSI <large-n> B` or `CSI <large-n> C`. A program running in the shell (or a compromised terminal multiplexer) can trigger this with e.g. `printf '\033[65535B'`.

**Fix:** Replace raw addition with `saturating_add` at both sites:

```rust
// Line 473 — cursor down
self.cursor.row = self.cursor.row
    .saturating_add(n)
    .min(self.rows.saturating_sub(1));

// Line 478 — cursor right
self.cursor.col = self.cursor.col
    .saturating_add(n)
    .min(self.cols.saturating_sub(1));
```

The same adversarial probe (`probe_csi_overflow_add`, `probe_csi_repeat_after_motion`, `probe_huge_repeat_param_csi_b_clamps`) in `zz_adversarial_probe.rs` should be promoted to permanent tests in the isolation suite rather than deleted.

---

### CR-02: `TerminalState::resize` Allocates Unbounded Grid — OOM DoS via Authenticated Client

**File:** `crates/nosh-server/src/registry.rs:443-446`, `crates/nosh-server/src/terminal.rs:220-259`

**Issue:** `SessionSlot::resize` unconditionally passes client-supplied dimensions to `TerminalState::resize`, with no upper bound:

```rust
// registry.rs:443-446
pub fn resize(&self, cols: u16, rows: u16) -> anyhow::Result<()> {
    let result = self.session.lock().unwrap().resize(cols, rows);
    self.terminal_state.lock().unwrap().resize(cols, rows); // ALWAYS called, regardless of result
    result
}
```

`cols` and `rows` come from the client's `Message::Resize` frame, which carries plain `u16` fields with no protocol-level bounds. An authenticated client sends `Resize { cols: 65535, rows: 65535 }`, which triggers `TerminalState::resize(65535, 65535)`.

Inside `TerminalState::resize`, each of the 24 existing rows is extended to 65535 cells, then 65511 new rows of 65535 cells each are allocated. Estimated allocation: `65535 × 65535 × 8 bytes ≈ 34 GB`, which OOMs the server process. This kills all sessions, not just the attacker's.

`Session::resize` (PTY `ioctl TIOCSWINSZ`) succeeds with any `u16` value — the kernel does not reject oversized dimensions — so the PTY resize result does not gate the `TerminalState` resize.

This is new attack surface introduced by Phase 12: prior to this phase, `SessionSlot::resize` only called `Session::resize`; the `TerminalState::resize` allocation path did not exist.

**Fix:** Clamp dimensions to a sane maximum before passing to `TerminalState::resize`. A reasonable cap matching common terminal emulator limits is 512 columns × 512 rows (or whatever the project decides). Apply the clamp in `SessionSlot::resize` so it is enforced at a single point regardless of which server path calls it:

```rust
pub fn resize(&self, cols: u16, rows: u16) -> anyhow::Result<()> {
    const MAX_COLS: u16 = 512;
    const MAX_ROWS: u16 = 512;
    let cols = cols.min(MAX_COLS);
    let rows = rows.min(MAX_ROWS);
    let result = self.session.lock().unwrap().resize(cols, rows);
    self.terminal_state.lock().unwrap().resize(cols, rows);
    result
}
```

---

### CR-03: vte `std` Feature Uses Unbounded `Vec<u8>` for OSC Content — OOM via Large PTY Output

**File:** `crates/nosh-server/src/terminal.rs:625,636`, `crates/nosh-server/Cargo.toml:32`

**Issue:** `vte` ships two implementations of its internal OSC byte accumulator:

- **`no_std`**: `ArrayVec<u8, 1024>` — hard cap at 1024 bytes, enforced by `is_full()` check before every push.
- **`std`** (the default, used by nosh-server): `Vec<u8>` with **no size check** (`is_full()` is only compiled for `no_std`).

```
# Cargo.toml:32
vte = "0.15"   # uses default features which includes "std"
```

From vte's source (`lib.rs:544-551`):
```rust
fn action_osc_put(&mut self, byte: u8) {
    #[cfg(not(feature = "std"))]
    { if self.osc_raw.is_full() { return; } }  // only in no_std!
    self.osc_raw.push(byte);                    // unbounded in std mode
}
```

This means a single OSC sequence in PTY output can accumulate an arbitrarily large payload in vte's internal buffer. The nosh server then clones this into `TerminalState` fields:

- **OSC 0/2 (title):** `self.title = Some(title.to_owned())` at line 625 — clones the full string.
- **OSC 52 (clipboard detection):** `data.to_vec()` at line 636 — clones the full base64 payload.

A program running in the authenticated user's shell emitting `printf '\033]52;c;%s\007' "$(python3 -c "print('A'*10_000_000)")"` causes vte to accumulate 10 MB in `osc_raw`, then `osc52_pending` holds a 10 MB `Vec<u8>`. The next OSC 52 replaces it, but while it is held, server memory is inflated by the payload size. For a multi-session server this is a memory-exhaustion vector.

**Fix:** Either:
1. Disable vte's `std` feature to restore the `ArrayVec<1024>` cap: `vte = { version = "0.15", default-features = false }` (then verify all needed features are still available), or
2. Clamp the title and OSC 52 data at the `osc_dispatch` handler before storing:

```rust
// title cap — 1024 bytes is generous for any real terminal title
if let Ok(title) = std::str::from_utf8(title_bytes) {
    if title.len() <= 1024 {
        self.title = Some(title.to_owned());
    }
}

// osc52 data cap — 64 KiB covers any clipboard paste that matters
const MAX_OSC52_DATA: usize = 64 * 1024;
let data = params.get(2).copied().unwrap_or(b"");
let data = &data[..data.len().min(MAX_OSC52_DATA)];
self.osc52_pending = Some((selection.to_vec(), data.to_vec()));
```

---

## Warnings

### WR-01: `terminal_state` Mutex Poisoning Cascade on Panic Inside `advance()`

**File:** `crates/nosh-server/src/registry.rs:370-372`

**Issue:** `push_output_and_parse` acquires the `terminal_state` Mutex and calls `advance()` while holding it:

```rust
pub fn push_output_and_parse(&self, chunk: &[u8]) -> u64 {
    let seq = self.output_buf.lock().unwrap().push(chunk);
    self.terminal_state.lock().unwrap().advance(chunk);   // Mutex held here
    seq
}
```

If `advance()` panics while the Mutex is held (e.g., from the u16 overflow in CR-01 in debug mode), the Mutex becomes **poisoned**. All subsequent calls to `push_output_and_parse`, `resize`, and the test helper that accesses `slot.terminal_state` will then panic unconditionally on `.unwrap()`, propagating the panic to every async task that calls these methods — including the main session pump loop. This means a single malformed PTY byte sequence can permanently disable a session's output handling even after the initial panic is caught.

The `mem::take` borrow-split in `advance()` partially mitigates this: if `advance()` panics, `self.parser` is left in `Default` state (fresh parser, lost state), but the Mutex is poisoned before `self.parser` can be restored, making the state loss moot.

**Fix:** Fix CR-01 (the overflow) to eliminate the panic source. Additionally, consider using `lock().unwrap_or_else(|e| e.into_inner())` (poison recovery) in `push_output_and_parse` and `resize` so that a single panic does not permanently disable subsequent calls:

```rust
self.terminal_state
    .lock()
    .unwrap_or_else(|e| e.into_inner())
    .advance(chunk);
```

---

### WR-02: `color_param[0] as u8` Silently Truncates Out-of-Range 256-Color Indices

**File:** `crates/nosh-server/src/terminal.rs:737,756`

**Issue:** The 256-color index from `CSI 38;5;<n>m` and `CSI 48;5;<n>m` is cast directly from `u16` to `u8`:

```rust
self.sgr.fg = Some(color_param[0] as u8);   // line 737
self.sgr.bg = Some(color_param[0] as u8);   // line 756
```

vte accumulates the parameter value as `u16`. If a terminal emits `CSI 38;5;300m` (index 300, invalid per the 256-color spec), `color_param[0]` is `300u16`. The cast `300u16 as u8` truncates to `44` (300 mod 256), silently storing a wrong color. This produces incorrect diff data for Phase 13 extraction.

A test for the boundary (`38;5;255` works, `38;5;256` should be ignored or clamped) does not exist.

**Fix:** Clamp or ignore out-of-range indices:

```rust
// Clamp to valid 256-color range:
if color_param[0] <= 255 {
    self.sgr.fg = Some(color_param[0] as u8);
}
// OR simply clamp: Some((color_param[0].min(255)) as u8)
```

Same fix needed at line 756 for `bg`.

---

## Info

### IN-01: Throwaway Adversarial Probe File Left in `tests/` Directory

**File:** `crates/nosh-server/tests/zz_adversarial_probe.rs`

**Issue:** The file opens with `//! THROWAWAY adversarial probes for Phase 12 verification. Delete after run.` It documents the exact overflow scenarios identified in CR-01 (`probe_csi_overflow_add`, `probe_csi_repeat_after_motion`, `probe_huge_repeat_param_csi_b_clamps`) but marks them for deletion rather than promoting them to permanent regression tests. The file currently ships with the codebase and will run in CI as an integration test.

**Fix:** Do not delete this file. Instead, remove the "THROWAWAY" comment and move the relevant tests into `terminal.rs`'s `#[cfg(test)]` module as permanent adversarial-robustness coverage. This ensures the bugs they document are permanently regression-tested.

---

### IN-02: `cell()` Returns `&'static Cell` for Out-of-Bounds Access

**File:** `crates/nosh-server/src/terminal.rs:335-342`

**Issue:** The `cell()` method uses a `static OnceLock<Cell>` to return a default cell for out-of-bounds coordinates. The returned reference has `'static` lifetime — it appears to be a grid reference but is actually a global static. Phase 13 diff extraction will iterate `cell()` heavily; if any call-site stores the reference rather than copying fields, it holds a `'static` reference to a constant value rather than the actual grid, which could produce stale diff data silently.

**Fix:** Add a `'static` note to the doc comment to make the behavior explicit, or change the return type to `Cow<'_, Cell>` to force callers to handle both cases.

---

_Reviewed: 2026-06-01T12:00:00Z_
_Reviewer: Claude (gsd-code-reviewer)_
_Depth: deep_
