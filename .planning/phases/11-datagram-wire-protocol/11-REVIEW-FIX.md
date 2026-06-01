---
phase: 11-datagram-wire-protocol
fixed_at: 2026-06-01T19:35:00Z
review_path: .planning/phases/11-datagram-wire-protocol/11-REVIEW.md
iteration: 1
findings_in_scope: 6
fixed: 6
skipped: 0
status: all_fixed
---

# Phase 11: Code Review Fix Report

**Fixed at:** 2026-06-01
**Source review:** `.planning/phases/11-datagram-wire-protocol/11-REVIEW.md`
**Iteration:** 1

**Summary:**
- Findings in scope: 6 (CR-01, CR-02, WR-01, WR-02, IN-01, IN-02)
- Fixed: 6
- Skipped: 0
- IN-03 explicitly deferred to Phase 13 per task instructions

## Fixed Issues

### CR-01: encode_datagram cap invariant violated for cap < 8

**Files modified:** `crates/nosh-proto/src/codec.rs`, `crates/nosh-proto/src/datagram.rs`, `crates/nosh-proto/src/lib.rs`
**Commit:** `0555ed8`
**Applied fix:**
- Added `ProtoError::CapTooSmall(usize, usize)` variant to `codec.rs`
- Added `pub const MIN_CAP: usize = 8` to `datagram.rs` with full documentation explaining the 7-byte header floor
- Added early-return `if cap < MIN_CAP { return Err(ProtoError::CapTooSmall(cap, MIN_CAP)); }` at the top of `encode_datagram`
- Updated the `encode_datagram` doc comment to document the MIN_CAP precondition and CapTooSmall error variant
- Re-exported `MIN_CAP` and `MAX_RUNS` from `lib.rs`

---

### WR-02: debug_assert is the only release-build guard of the cap invariant

**Files modified:** `crates/nosh-proto/src/datagram.rs`
**Commit:** `b7c0b64`
**Applied fix:**
- Promoted `debug_assert!` at the fill-loop invariant check to a hard `assert!`
- Added explanatory comment: after CR-01 ensures cap >= MIN_CAP at the API boundary, this invariant can only be violated by an implementation bug in the fill loop, not by caller error — a hard assert is appropriate

---

### CR-02: heterogeneous_continue_past_rejection test is vacuous for its stated purpose

**Files modified:** `crates/nosh-proto/src/datagram.rs`
**Commit:** `88df965`
**Applied fix:**
- Changed `cap = 120` to `cap = 19`

**Why cap=19 (not the reviewer's suggested cap=90):**
The reviewer's cap=90 suggestion did not account for the split path in `encode_datagram`. At cap=90 (body_cap=89), the whole-run check rejects the large 80-char run (body=92 >= 89), but the split path fires with `remaining = 89 - 6 - 12 = 71` bytes of budget, fitting 71 ASCII chars as a prefix. The split accepts 71 chars of the large run and continues — so the "x" assertion passes but NOT because of continue-past-rejection.

At cap=19 (body_cap=18): remaining = 18 - 6 - 12 = 0 → the split path's `remaining > 0` guard is false → the large run is **fully deferred** with no partial encoding. The small "x" runs (size 13 < 18) are accepted when the loop continues. Under a break-on-first-rejection bug: the large run triggers rejection and breaks → 0 "x" runs → test fails correctly (red under the bug).

Also added:
- Assertion that the large run appears in `deferred` (fully deferred, not partially encoded)
- Assertion that no large-chars run appears in the decoded payload
- Corrected the comment that said "cap > 93" means rejection — that is the FITS condition; cap < 93 is the rejection condition

---

### WR-01: start_col + prefix_chars as u16 can overflow in split path

**Files modified:** `crates/nosh-proto/src/datagram.rs`
**Commit:** `8f1e3a8`
**Applied fix:**
- Changed `run.start_col + prefix_chars as u16` to `run.start_col.saturating_add(prefix_chars as u16)` in the right_run construction in the split path
- Added comment explaining the saturation rationale: a run at `start_col` near `u16::MAX` with a long prefix is a degenerate terminal state; saturating at `u16::MAX` is the least-surprising fallback

---

### IN-02: fg=0/bg=0 as "default color" collides with palette index 0 (black)

**Files modified:** `crates/nosh-proto/src/datagram.rs`
**Commit:** `8d2f942`
**Applied fix:**
- Changed `pub fg: u8` → `pub fg: Option<u8>` and `pub bg: u8` → `pub bg: Option<u8>` in `DiffRun`
- `None` = terminal default color; `Some(n)` = ANSI 256-color palette index n
- Updated all `DiffRun` constructors in the module (tests and split path):
  - `fg: 0` → `fg: None` (terminal default foreground)
  - `bg: 0` → `bg: None` (terminal default background)
  - `fg: 2` → `fg: Some(2)` (explicit palette index)
  - `bg: 3` → `bg: Some(3)` (explicit palette index)
- Updated doc comments on both fields to explain the Option semantics
- `Option<u8>` serializes to 1 or 2 bytes under postcard (tag byte + optional payload), keeping wire overhead minimal

This fix is cheap now (datagram.rs is the only consumer; Phases 12–14 not built) and expensive later.

---

### IN-01: char_byte_offset docstring claims function panics but it never does

**Files modified:** `crates/nosh-proto/src/datagram.rs`
**Commit:** `ed512be`
**Applied fix:**
- Replaced the false "Panics if n > s.chars().count()" claim with an accurate description: returns `s.len()` for out-of-bounds n (the end-of-string sentinel), yielding an empty split tail. Added note that `fit_chars_in_bytes` callers need not worry about bounds.

---

## Skipped Issues

None — all 6 in-scope findings were fixed.

## Explicitly Deferred (per task instructions)

### IN-03: O(n²) serialized_size calls in fill loop

**Reason:** Deferred to Phase 13 per explicit task constraint. The Phase 13 tick loop will introduce a MAX_ENCODE_RUNS bound (e.g., 256) before the fill loop. Adding it in Phase 11 without the Phase 13 context would be premature. The issue is benign for the typical 24-run terminal (≤ 576 serialization calls).

## Final Test Results

`cargo build -p nosh-proto`: clean (0 errors, 0 warnings)
`cargo test -p nosh-proto`: **22 passed, 0 failed**

```
test result: ok. 22 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

## Commit Summary

| Finding | Commit | Description |
|---------|--------|-------------|
| CR-01 | `0555ed8` | Add MIN_CAP const + CapTooSmall early-return |
| WR-02 | `b7c0b64` | Promote debug_assert to hard assert |
| CR-02 | `88df965` | Fix vacuous test — use cap=19 (true falsifier) |
| WR-01 | `8f1e3a8` | Use saturating_add for start_col in split path |
| IN-02 | `8d2f942` | Change fg/bg to Option<u8> |
| IN-01 | `ed512be` | Correct char_byte_offset docstring |

---

_Fixed: 2026-06-01_
_Fixer: Claude (gsd-code-fixer)_
_Iteration: 1_
