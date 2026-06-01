---
phase: 11-datagram-wire-protocol
status: passed
verified: 2026-06-01
requirements:
  - SYNC-01
must_haves_verified: 6/6
success_criteria_verified: 4/4
automated_checks: passed
human_verification: []
gaps: []
---

# Phase 11 Verification Report

**Phase goal:** A sparse, size-bounded terminal-diff wire format exists in `nosh-proto` — the shared interface that every subsequent server and client component builds on.

**Status: PASSED** — All success criteria and must-haves verified.

---

## Automated Checks

| Check | Result |
|-------|--------|
| `cargo build -p nosh-proto` | ✓ exit 0 |
| `cargo test -p nosh-proto` | ✓ 22/22 tests pass (16 new datagram tests + 6 pre-existing) |
| No regressions in codec/messages tests | ✓ |

---

## Success Criteria Verification

| # | Criterion | Status | Evidence |
|---|-----------|--------|----------|
| SC1 | `StateDiff` in `datagram.rs` carries sparse run-length runs, monotonic `epoch:u64`, dimensions (cols/rows), and cursor position | ✓ PASS | `pub struct StateDiff { epoch: u64, cols: u16, rows: u16, cursor: CursorPos, runs: Vec<DiffRun> }` at line 37; epoch documented as monotonic/never-resets |
| SC2 | `encode_datagram`/`decode_datagram` round-trip correctly — decoded == the encoded StateDiff for all valid inputs | ✓ PASS | 5 round-trip tests pass: empty-runs, single-ASCII, 80-char, multibyte-UTF-8, styled; plus `encode_decode_round_trip_via_encode_datagram` |
| SC3 | Encoded payload is provably STRICTLY < cap; size-cap test drives full 80x24 repaint at cap=1100, asserts `encoded.len() < 1100` (STRICT) plus non-empty deferred; heterogeneous test proves fill continues past rejection | ✓ PASS | `size_cap_full_80x24_repaint` and `heterogeneous_continue_past_rejection` both pass; fill check uses strict `size < body_cap` (not `<=`) |
| SC4 | Large-repaint decision documented in code comment at `encode_datagram` definition, explicitly rejecting skip-frame and reliable-stream fallback | ✓ PASS | Lines 131-137: `* **Skip-frame:** ...` and `* **Reliable-stream fallback:** ...` in the `///` doc block immediately above `pub fn encode_datagram` |

---

## Must-Haves Verification (from PLAN.md)

| # | Must-Have | Status | Evidence |
|---|-----------|--------|----------|
| MA1 | StateDiff carries sparse run-length runs, monotonic epoch:u64, terminal dims (cols/rows), and cursor position | ✓ PASS | Struct definition verified; epoch doc comment explicitly states "monotonic, never resets, DISTINCT from seq" |
| MA2 | `encode_datagram(diff, cap)` returns payload STRICTLY < cap for ANY input (including full 80x24 repaint) — tag byte accounted for, strict less-than | ✓ PASS | `body_cap = cap.saturating_sub(1)`; fill check `size < body_cap`; `debug_assert!(body.len() < body_cap)`; size-cap test asserts `encoded.len() < 1100` |
| MA3 | `decode_datagram(encode_datagram(diff).payload)` reconstructs the encoded StateDiff exactly for all valid inputs | ✓ PASS | `encode_decode_round_trip_via_encode_datagram` passes; 5 `tag_encode`-based round-trips pass |
| MA4 | `decode_datagram` returns `ProtoError` (never panics, never over-allocates) on empty bytes, unknown tag, truncated body, and runs > MAX_RUNS | ✓ PASS | 4 negative tests pass: `decode_empty_bytes_is_err`, `decode_unknown_tag_is_err`, `decode_truncated_body_is_err`, `decode_max_runs_guard`; no unwrap/expect/panic on decode path outside test module |
| MA5 | Cursor-priority fill continues past a rejected oversize run — smaller later run still included; proven by heterogeneous-size test that fails on break-on-first-rejection | ✓ PASS | `heterogeneous_continue_past_rejection` passes; fill loop falls through to next iteration after reject (no `break`); comment at line 289 names this explicitly |
| MA6 | Cursor-priority partial-update decision and rejection of skip-frame and reliable-stream fallback documented in code comment at `encode_datagram` definition | ✓ PASS | `/// ## Alternatives explicitly rejected (D-11-01a)` with `Skip-frame` and `Reliable-stream fallback` bullets in the `pub fn encode_datagram` doc block |

---

## Key-Links Verification

| Link | Status |
|------|--------|
| `datagram.rs` → `crate::codec::ProtoError` via `use crate::codec::ProtoError` | ✓ Line 17 |
| `datagram.rs` → `postcard::experimental::serialized_size` | ✓ Lines 203, 219, 263 |
| `lib.rs` → `datagram::{StateDiff, encode_datagram, decode_datagram}` via `pub use` | ✓ Line 15 |

---

## Artifacts Verification

| Artifact | Min Lines | Actual | Contains | Status |
|----------|-----------|--------|----------|--------|
| `crates/nosh-proto/src/datagram.rs` | 200 | 796 | `pub fn encode_datagram`, `pub struct StateDiff`, `chars: String` (not `Vec<char>`) | ✓ |
| `crates/nosh-proto/src/lib.rs` | — | 25 | `pub mod datagram;`, `pub use datagram::` | ✓ |

---

## Requirement Traceability

| Requirement | Status | Phase | Evidence |
|-------------|--------|-------|---------|
| SYNC-01: Sparse, size-bounded datagram wire format in `nosh-proto`; changed cells only, monotonic epoch, dims + cursor; payload capped; round-trip and size-cap tests; postcard/serde, no new deps | ✓ Satisfied | 11 | All four fields present; 16 inline tests including size-cap and round-trip; zero new crate dependencies |

---

## Test Results Summary

```
running 22 tests
test codec::tests::encode_decode_round_trip ... ok
test codec::tests::length_prefix_is_big_endian_body_len ... ok
test codec::tests::async_write_then_read_round_trip ... ok
test codec::tests::session_variants_round_trip ... ok
test codec::tests::reattach_variants_round_trip ... ok
test datagram::tests::decode_empty_bytes_is_err ... ok
test datagram::tests::decode_truncated_body_is_err ... ok
test datagram::tests::decode_unknown_tag_is_err ... ok
test datagram::tests::encode_decode_round_trip_via_encode_datagram ... ok
test datagram::tests::cursor_priority_includes_cursor_row ... ok
test datagram::tests::round_trip_empty_runs ... ok
test datagram::tests::round_trip_full_80_char_row ... ok
test datagram::tests::round_trip_multibyte_utf8_run ... ok
test datagram::tests::heterogeneous_continue_past_rejection ... ok
test datagram::tests::round_trip_single_ascii_run ... ok
test datagram::tests::round_trip_styled_run ... ok
test datagram::tests::single_cell_change_no_deferred ... ok
test datagram::tests::single_run_80_chars_serialized_size_lte_90 ... ok
test datagram::tests::tag_byte_is_0x01 ... ok
test datagram::tests::size_cap_full_80x24_repaint ... ok
test datagram::tests::decode_max_runs_guard ... ok
test messages::tests::variant_name_never_leaks_token_bytes ... ok

test result: ok. 22 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

---

## Conclusion

Phase 11 goal is **fully achieved**. The datagram wire-format module is complete, correct, and hardened:

- The shared `StateDiff` type contract is in place for Phases 12–15 to build on.
- `encode_datagram` is total with a provable STRICT payload < cap guarantee.
- `decode_datagram` is hardened against all malformed inputs (never panics, never over-allocates).
- The fill loop correctly continues past rejected runs (proven adversarially by the heterogeneous test).
- The large-repaint design decision is documented with alternatives explicitly rejected.
- No new dependencies; all existing `nosh-proto` tests continue to pass.
