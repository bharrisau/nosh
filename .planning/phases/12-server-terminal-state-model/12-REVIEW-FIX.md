---
phase: 12-server-terminal-state-model
fixed_at: 2026-06-01T00:00:00Z
review_path: .planning/phases/12-server-terminal-state-model/12-REVIEW.md
iteration: 1
findings_in_scope: 7
fixed: 6
skipped: 1
status: partial
---

# Phase 12: Code Review Fix Report

**Fixed at:** 2026-06-01
**Source review:** `.planning/phases/12-server-terminal-state-model/12-REVIEW.md`
**Iteration:** 1

**Summary:**
- Findings in scope: 7 (CR-01, CR-02, CR-03, WR-01, WR-02, IN-01, IN-02)
- Fixed: 6
- Skipped: 1 (IN-01 — already resolved before this run)

Final test count: **78 passed, 0 failed** (up from 67 before fixes; 11 new regression tests added).

---

## Fixed Issues

### CR-01: u16 overflow in CSI B/C cursor motion

**Files modified:** `crates/nosh-server/src/terminal.rs`
**Commit:** `2de1aa8`
**Applied fix:** Replaced raw `+` with `saturating_add` in the CSI B (cursor-down) and CSI C (cursor-right) handlers. With `cursor.row = 23` and `n = 65535` (vte max), the previous `self.cursor.row + n` overflowed u16 — panicking in debug builds (DoS on adversarial PTY output) and silently wrapping in release (wrong cursor position). `saturating_add` clamps at `u16::MAX` before the subsequent `.min(rows - 1)` produces the correct in-bounds result.

Added three permanent regression tests (promoted from the adversarial verifier's probe pattern):
- `adversarial_csi_b_max_count_from_nonzero_row_clamps_no_panic` — `CSI 24;80H` then `CSI 65535B`
- `adversarial_csi_c_max_count_from_nonzero_col_clamps_no_panic` — `CSI 24;80H` then `CSI 65535C`
- `adversarial_huge_repeat_from_nonzero_position_clamps` — both in sequence, assertions on exact clamped values

Audited all other cursor/coordinate arithmetic in terminal.rs: CSI A/D already use `saturating_sub` (correct); CSI H/f uses `max(1).saturating_sub(1)` on 1-based params (correct, no overflow possible since params arrive as u16 from vte and the subtraction is bounded); scroll math uses `cursor.row + 1 >= self.rows` without arithmetic on `n` (correct). Only CSI B/C needed the fix.

---

### CR-02: resize OOM DoS via unbounded TerminalState grid allocation

**Files modified:** `crates/nosh-server/src/registry.rs`
**Commit:** `45f6a1b`
**Applied fix:** Added `MAX_COLS = 1000` and `MAX_ROWS = 1000` constants inside `SessionSlot::resize` and clamped both client-supplied dimensions before passing to either `Session::resize` (PTY ioctl) or `TerminalState::resize`. An unauthenticated… authenticated client sending `Resize{cols:65535, rows:65535}` previously triggered a 65535×65535 grid allocation (~34 GB), OOMing the server and killing all sessions. At 1000×1000 × 8 bytes/Cell, worst-case allocation is ~8 MB/session — generous for any real terminal.

Added two regression tests:
- `resize_oom_cap_clamps_client_supplied_dimensions` — no PTY required; verifies the cap constants and bounded TerminalState allocation
- `slot_resize_clamps_huge_dimensions_to_cap` — slot-level (/bin/sh guarded); calls `slot.resize(65535, 65535)` and asserts `actual_cols <= 1000 && actual_rows <= 1000`

---

### CR-03: unbounded OSC buffer (vte std feature)

**Files modified:** `crates/nosh-server/Cargo.toml`, `crates/nosh-server/src/terminal.rs`
**Commit:** `6c7d292`
**Applied fix:** Changed `vte = "0.15"` to `vte = { version = "0.15", default-features = false }` in Cargo.toml. With the `std` feature enabled, vte's `action_osc_put` has no `is_full()` guard, accumulating OSC bytes in an unbounded `Vec<u8>` before `osc_dispatch` is called. With `default-features = false`, vte uses `ArrayVec<u8, 1024>` and silently truncates beyond 1024 bytes. The `Perform` trait, `Parser` struct, and all public APIs remain available — only the OSC accumulation buffer changes. Build confirmed clean after the change.

Added two regression tests (64 KiB OSC sequences fed to the parser; no panic/OOM; assertions on bounded retained payload):
- `adversarial_large_osc_title_is_bounded_no_panic`
- `adversarial_large_osc52_is_bounded_no_panic`

**PHASE 16 CONSIDERATION — OSC52 size cap:** The 1024-byte vte OSC buffer cap limits the total OSC 52 frame size (including `\x1b]52;c;` framing overhead, ~8 bytes). Effective base64 payload cap is ~1016 bytes, encoding ~762 bytes of raw clipboard data. Phase 16 plans to forward OSC 52 "no MTU limit." Large clipboard writes (e.g. copy of a multi-KB file) will be silently dropped by vte before `osc_dispatch` is called, so `osc52_pending` will be `None` rather than holding the payload. Phase 16 must choose one of:

  **(a) Custom OSC accumulation path (preferred):** Bypass vte's OSC buffer for clipboard payloads by pre-processing raw PTY bytes before calling `advance()`, detecting and extracting OSC 52 sequences manually with a larger bounded cap (e.g. 64 KiB). This is complex but avoids the `std` feature risk.

  **(b) Re-enable vte std + explicit cap in osc_dispatch (fallback):** Revert to `vte = "0.15"` (std enabled) and add `if title.len() <= MAX_TITLE` and `let data = &data[..data.len().min(MAX_OSC52)]` guards in `osc_dispatch`. This caps what nosh *retains* in `TerminalState` but leaves vte's transient `Vec<u8>` accumulation as a residual concern: a sufficiently large OSC 52 (multi-MB) still inflates vte's internal buffer transiently even though we discard it on dispatch.

The 1024-byte vte cap is conservative and safe for the current phase scope. Phase 16 must explicitly revisit this before implementing clipboard forwarding.

---

### WR-01: Mutex poisoning cascade on panic inside advance()

**Files modified:** `crates/nosh-server/src/registry.rs`
**Commit:** `a626955`
**Applied fix:** Replaced `.lock().unwrap()` with `.lock().unwrap_or_else(|e| e.into_inner())` at both `terminal_state` lock sites in `SessionSlot`: `push_output_and_parse` and `resize`. If `advance()` panics while holding the Mutex (which CR-01 eliminates as the only known panic source, but future code could introduce others), the Mutex becomes poisoned and all subsequent `.unwrap()` calls propagate the panic to every async task touching the session — permanently disabling it. With `unwrap_or_else`, we recover the poisoned guard and continue; the TerminalState may be partially corrupted but the session remains alive. This is belt-and-suspenders hardening.

No new tests added (poison recovery is behavior-under-panic; the existing suite verifies normal operation is unaffected).

---

### WR-02: 256-color index truncation

**Files modified:** `crates/nosh-server/src/terminal.rs`
**Commit:** `53e4b57`
**Applied fix:** Added `if color_param[0] <= 255` guard before the `as u8` cast in both the `38;5;n` (fg) and `48;5;n` (bg) SGR handlers. Previously, out-of-range indices (vte delivers up to u16::MAX) were silently truncated: index 300 became 44 (300 mod 256), setting a wrong palette entry. After the fix, out-of-range indices are ignored (no color attribute set), matching the VT spec "ignore invalid parameter" behavior. `Option<u8>` type for `Cell.fg`/`bg` is preserved.

Added four regression tests:
- `sgr_256_color_fg_boundary_255_accepted` — max valid index accepted
- `sgr_256_color_fg_out_of_range_256_rejected` — first out-of-range (was truncated to 0)
- `sgr_256_color_fg_out_of_range_300_rejected` — truncation victim (was truncated to 44)
- `sgr_256_color_bg_out_of_range_256_rejected` — bg path covered

---

### IN-02: doc note on cell() static return

**Files modified:** `crates/nosh-server/src/terminal.rs`
**Commit:** `8b866bc`
**Applied fix:** Expanded the doc comment on `cell()` to explicitly document that out-of-bounds access returns a shared `&'static Cell` (a global sentinel, not a grid reference), with a specific warning for Phase 13 callers to copy fields rather than hold the reference across grid mutations.

---

## Skipped Issues

### IN-01: Throwaway adversarial probe file left in tests/

**File:** `crates/nosh-server/tests/zz_adversarial_probe.rs`
**Reason:** skipped — file does not exist in the repository; already removed before this fix run.
**Original issue:** The adversarial verifier's probe file was identified as throwaway and left in the repo. However, inspection of the actual working tree and git status confirms no `zz_adversarial_probe.rs` or equivalent file exists anywhere in `crates/nosh-server/`. The REVIEW.md's description of this finding matched the verification report's note that "all throwaway probe files removed; tree verified clean via git status." No action required.

The probe scenarios (CSI B/C overflow from nonzero cursor) are covered by the three permanent regression tests added under CR-01.

---

_Fixed: 2026-06-01_
_Fixer: Claude (gsd-code-fixer)_
_Iteration: 1_
