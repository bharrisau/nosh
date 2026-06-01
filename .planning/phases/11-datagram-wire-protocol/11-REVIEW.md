---
phase: 11-datagram-wire-protocol
reviewed: 2026-06-01T00:00:00Z
depth: deep
files_reviewed: 2
files_reviewed_list:
  - crates/nosh-proto/src/datagram.rs
  - crates/nosh-proto/src/lib.rs
findings:
  critical: 2
  warning: 2
  info: 3
  total: 7
status: issues_found
---

# Phase 11: Code Review Report

**Reviewed:** 2026-06-01
**Depth:** deep (per-file analysis with call-chain tracing and empirical probing)
**Files Reviewed:** 2
**Status:** issues_found

## Summary

The implementation is structurally sound: the type layout is correct, the
decode path cannot panic on untrusted input, the tag-byte budget is correctly
deducted, and the split path advances `start_col` by char count (not bytes).

Two critical defects were found through adversarial analysis:

1. `encode_datagram` violates its stated "payload.len() < cap for ANY input"
   guarantee when `cap` is smaller than the header-only serialized size plus
   the tag byte. This is not caught in release builds (debug_assert only).

2. The `heterogeneous_continue_past_rejection` test — explicitly marked as the
   BLOCKER guard against break-on-first-rejection bugs — does not actually
   falsify that bug. A break-on-first-rejection implementation passes the test
   because the large run is never rejected at cap=120. The test is vacuous for
   its stated purpose.

Both are provable through simulation (see per-finding detail below).

## Critical Issues

### CR-01: `encode_datagram` cap invariant violated for `cap <= 7` (empty-diff
minimum payload is 7 bytes)

**File:** `crates/nosh-proto/src/datagram.rs:164-314`

**Issue:** The function's documented guarantee states "For ANY input,
`payload.len() < cap`." This is false. An empty-runs `StateDiff` serialises to
exactly 6 bytes under postcard (epoch=1-byte varint, cols=1, rows=1,
cursor.row=1, cursor.col=1, runs.len=1). Adding the 1-byte tag prefix gives a
minimum payload of 7 bytes. When `cap <= 7`, the function returns a payload
whose length equals or exceeds `cap`.

Empirical confirmation — serialised byte sequence for a minimal empty diff:

```
[0x01, 0x50, 0x18, 0x00, 0x00, 0x00]   // 6 bytes, values epoch=1 cols=80 rows=24 cur=(0,0) runs=[]
```

With `cap=7`, `body_cap = cap.saturating_sub(1) = 6`. The fill loop runs zero
iterations (no runs). At line 300, `to_allocvec` produces a 6-byte body.
The `debug_assert` at line 303 checks `6 < 6` — false — and panics in debug
builds. In release builds the assert is elided and the function silently
returns a 7-byte payload against a cap of 7, breaking the `< cap` strict
guarantee.

The function offers no documented precondition on the minimum safe cap, and
makes no API-level error return when cap is too small.

In practice, `cap` derives from `max_datagram_size()` (negotiated QUIC MTU,
always >= 1200 bytes), so real callers are unaffected. However, the public API
contract is wrong and will mislead Phase 13/14 callers about robustness.

**Fix:** Add a validated minimum at the top of `encode_datagram` and document it:

```rust
/// Minimum valid `cap` value — the header-only (zero-run) payload is 7 bytes
/// (1 tag + up to 6 body bytes). Any cap <= this value cannot satisfy the
/// strict < guarantee.
const MIN_CAP: usize = 8;

pub fn encode_datagram(
    diff: &StateDiff,
    cap: usize,
) -> Result<(Bytes, Vec<DiffRun>), ProtoError> {
    if cap < MIN_CAP {
        // Return the header-only payload even though it may equal cap;
        // caller is responsible for not calling with a sub-minimum cap.
        // OR: return Err(ProtoError::CapTooSmall) and add a variant.
        return Err(ProtoError::Postcard(postcard::Error::SerializeSeqLengthUnknown));
    }
    // ... existing code unchanged ...
```

Alternatively, change `debug_assert!` to `assert!` to catch this in release
builds at the cost of a panic instead of a contract violation. The cleanest
fix is a new `ProtoError::CapTooSmall` variant with an early return.

---

### CR-02: `heterogeneous_continue_past_rejection` test does not falsify
break-on-first-rejection — the test is vacuous for its stated purpose

**File:** `crates/nosh-proto/src/datagram.rs:679-740`

**Issue:** The test is labelled "(BLOCKER 1)" and described as the guard that
proves the fill loop continues past the first rejected run. The test places a
large run (80-char row, `row=0`) and 23 small single-char runs (`rows 1–23`)
with `cursor` at `row=0 col=0` (so the large run sorts first). `cap=120`.

Measured serialised sizes at `cap=120` (`body_cap=119`):

| State | Serialised size | Outcome |
|-------|-----------------|---------|
| header only | 6 | — |
| + large run (80 chars) | 92 | 92 < 119 → **ACCEPTED** |
| + large + 1 small | 99 | 99 < 119 → ACCEPTED |
| + large + 2 small | 106 | 106 < 119 → ACCEPTED |
| + large + 3 small | 113 | 113 < 119 → ACCEPTED |
| + large + 4 small | 120 | 120 ≥ 119 → rejected |

The large run at index 0 is **never rejected** — it fits within `body_cap=119`.
Rejection first occurs at run index 4 (a small run), by which point runs 1–3
(all `"x"` strings) are already accepted. A break-on-first-rejection
implementation would break after run index 4 and still return 4 runs with
three `"x"` runs. The test assertion `decoded.runs.iter().any(|r| r.chars ==
"x")` passes with both the correct implementation and the buggy one.

Simulation confirms:

- Correct (continue) implementation at `cap=120`: 4 accepted runs (rows 0,1,2,3),
  `has_x = true`.
- Break-on-first-rejection at `cap=120`: identical 4 accepted runs,
  `has_x = true`.

The test is **not a falsifying test** for the bug it claims to guard.

**Fix:** Change the test's `cap` so the large run is rejected first and small
runs fit after it. The large run body is 92 bytes; `body_cap` must be ≤ 92 to
reject it, and > 13 (small run + header size) to accept a small run. Any `cap`
in the range `[15, 93]` creates the intended scenario. `cap = 90` works:

```rust
// At cap=90 (body_cap=89):
// large run: size=92, 92 < 89 = false → REJECTED (index 0)
// small run: size=13, 13 < 89 = true  → ACCEPTED (continue past rejection)
let cap = 90;  // was 120 — broken; 90 creates the actual rejection
```

With `cap=90`, a break-on-first-rejection implementation returns 0 accepted
runs (`has_x = false`), and the test correctly fails. The continue
implementation returns 11 small runs (`has_x = true`), and the test correctly
passes.

The comment block (lines 710–716) stating "large row body = ~92 bytes; we need
cap > 93" also contains a logic error — that comment describes the bound for
the large row to **fit**, not to be **rejected**. The comment should be
updated to match the corrected cap.

## Warnings

### WR-01: `start_col + prefix_chars as u16` can overflow `u16` in the run-split
path, causing debug-mode panic or silent wrap in release

**File:** `crates/nosh-proto/src/datagram.rs:250`

**Issue:**

```rust
start_col: run.start_col + prefix_chars as u16,
```

`run.start_col` is a `u16` (max 65535). `prefix_chars` is a `usize`.
`prefix_chars as u16` truncates silently; more critically, the subsequent
addition is a plain `u16 +` which panics on overflow in debug mode and wraps
silently in release mode.

In practice, `prefix_chars` comes from `fit_chars_in_bytes(&run.chars, remaining)`,
where `remaining ≤ body_cap - current_size - 12`. With a typical QUIC MTU of
1200–9000 bytes, `remaining` is bounded to roughly 1200 bytes, meaning
`prefix_chars ≤ 1200` (all-ASCII). A run at `start_col = 64356` with
`prefix_chars = 1180` produces `64356 + 1180 = 65536`, which overflows `u16`.

This cannot happen on a real terminal wider than 65535 columns, but the
`DiffRun` type is a public type with no column-bound validation. Any caller
that constructs a `StateDiff` with a high `start_col` and a long `chars` field
can trigger the panic (debug) or the wrap (release). Because `DiffRun` will be
constructed from the server's terminal model in Phase 13, this should be fixed
before that wiring lands.

**Fix:** Use `saturating_add` or `checked_add` with a fallback:

```rust
start_col: run.start_col.saturating_add(prefix_chars as u16),
```

`saturating_add` caps at `u16::MAX` (65535) rather than wrapping; a run that
can't be described within `u16` columns is a degenerate terminal state and
saturating is the least-surprising behaviour. Alternatively, validate
`start_col + run.chars.chars().count() <= u16::MAX` at encode entry and return
an error.

---

### WR-02: `debug_assert!` at line 303 is the only in-binary guard of the cap
invariant — the invariant is undetectable in release builds

**File:** `crates/nosh-proto/src/datagram.rs:302-308`

**Issue:** The invariant "body.len() < body_cap" is checked only via
`debug_assert!`, which is compiled out in release mode. If the cap invariant is
violated (see CR-01), the function silently returns an oversized payload in
release builds. Downstream code (Phase 13's tick loop) would call
`connection.send_datagram(payload)` with a payload larger than
`max_datagram_size`, causing quinn to return `SendDatagramError::TooLarge`
which the tick loop then needs to handle. This is a latent, production-only
failure mode: it is completely invisible in test/debug runs.

**Fix:** Promote to a hard `assert!` for now:

```rust
assert!(
    body.len() < body_cap,
    "encode_datagram invariant violated: body {} >= body_cap {}",
    body.len(),
    body_cap
);
```

This makes the invariant violation visible in release builds. Once CR-01 is
fixed (early return for cap < MIN_CAP), the assert becomes an internal
sanity check that should only fire on implementation bugs, making a hard assert
appropriate.

## Info

### IN-01: `char_byte_offset` docstring claims "Panics if n > s.chars().count()"
but the implementation does not panic

**File:** `crates/nosh-proto/src/datagram.rs:364-371`

**Issue:** The function docstring states:

```
/// Panics if `n > s.chars().count()` (callers must ensure `n` is in bounds).
```

The implementation uses `.unwrap_or(s.len())`, which returns `s.len()` for any
`n` at or past the end of the string — it never panics. The docstring is
wrong. This is not a safety issue (the implementation is safer than the doc
claims), but it documents a false precondition that may mislead future
maintainers.

**Fix:** Correct the docstring:

```rust
/// Returns `s.len()` if `n >= s.chars().count()` (the end-of-string sentinel,
/// yielding an empty split tail). Callers that pass a count from
/// `fit_chars_in_bytes` need not worry about out-of-bounds.
```

---

### IN-02: `fg = 0` / `bg = 0` as "default color" sentinel collides with ANSI
256-color index 0 (black)

**File:** `crates/nosh-proto/src/datagram.rs:87-94` (DiffRun struct),
`datagram.rs:104` (CellStyle::NONE)

**Issue:** The field docs state "`0` = default terminal color" for both `fg`
and `bg`. In ANSI 256-color tables, palette index 0 is black. There is no
in-band way to distinguish "use the terminal's default foreground/background"
from "use palette color index 0 (black)". Any run with `fg=0` will be
ambiguously interpreted as either "default" or "black" depending on whether the
Phase 14 renderer checks `fg == 0` as a special case or treats it as a raw
index.

This is a design decision that will need resolution before Phase 14 builds the
terminal renderer. The common approach is to use a sentinel outside the valid
range (e.g., `Option<u8>` where `None` = default, or a `u16` where `256` =
default).

**Fix:** No action required in Phase 11 scope. Record this as a known design
limitation for Phase 14. If the semantic is "0 means default-color" (not black),
document that convention explicitly on the field and ensure the Phase 14
renderer implements it consistently.

---

### IN-03: O(n²) `serialized_size` calls in the fill loop — relevant for
MAX_RUNS-scale inputs

**File:** `crates/nosh-proto/src/datagram.rs:196-293`

**Issue:** For each run in `sorted_runs`, the fill loop calls
`postcard::experimental::serialized_size` on a clone of the growing candidate
`StateDiff`. In the rejection path, it calls it twice more (once for
`current_size`, once to verify the split prefix). The total work is O(n²) in
the number of runs.

For a standard 80×24 terminal (≤ 24 runs), this is ~576 operations and is
negligible. However, `MAX_RUNS = 4096` and the QUIC max datagram size is 65535
bytes, which allows up to ~10,000 empty-chars runs per packet. An attacker
cannot trigger this on the decode path (since `decode_datagram` guards
MAX_RUNS), but the encode path has no corresponding bound: if Phase 13 calls
`encode_datagram` on a `StateDiff` built from a server-side terminal model that
somehow accumulates thousands of runs (e.g., due to a bug in the diff
algorithm), the encode loop will spin for millions of iterations.

This is an acknowledged tradeoff (review scope notes "acceptable for n<=~24 but
note if worse"), called out here because MAX_RUNS=4096 is 170× larger than the
24-run typical case and there is no corresponding guard on the encode path.

**Fix:** No action required in Phase 11 scope. Add a MAX_ENCODE_RUNS constant
(e.g., `256`) and truncate `sorted_runs` before the fill loop if it exceeds
that bound. This is a Phase 13 concern when the tick loop is wired.

---

_Reviewed: 2026-06-01_
_Reviewer: Claude (gsd-code-reviewer)_
_Depth: deep_
