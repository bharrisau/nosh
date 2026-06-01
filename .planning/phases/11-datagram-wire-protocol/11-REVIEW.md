---
phase: 11-datagram-wire-protocol
plan: "01"
status: clean
depth: standard
files_reviewed: 2
findings:
  critical: 0
  warning: 0
  info: 2
  total: 2
reviewed: 2026-06-01
---

# Phase 11 Code Review

**Files reviewed:** `crates/nosh-proto/src/datagram.rs`, `crates/nosh-proto/src/lib.rs`

**Depth:** standard (per-file analysis with language-specific checks)

**Result:** Clean — no bugs, no security issues, no blocking findings.

---

## Summary

The Phase 11 datagram wire-protocol implementation is correct and well-constructed. The critical invariants from the plan-check are all satisfied:

- `encode_datagram` is total (provable payload < cap for any input)
- Fill loop continues past rejected runs (heterogeneous test validates this)
- `decode_datagram` never panics or over-allocates on malformed input
- MAX_RUNS guard is present and test-backed (T-11-02)
- No unwrap/expect/panic on the decode input path (outside test module)
- String used for DiffRun.chars (not Vec<char>)
- Tag byte accounted in cap budget (`body_cap = cap.saturating_sub(1)`)
- Strict `<` (not `<=`) in cap check
- Split path advances deferred start_col by char count (not bytes)
- Large-repaint decision documented at encode_datagram with alternatives explicitly rejected

---

## Findings

### INFO-01: O(n²) serialized_size calls in fill loop — acceptable for current terminal sizes

**File:** `crates/nosh-proto/src/datagram.rs`  
**Lines:** 196–293  
**Severity:** info (non-blocking)

**Observation:** The fill loop calls `postcard::experimental::serialized_size` on a cloned `StateDiff` candidate for each run, and again in the split path. For an 80x24 terminal (24 runs maximum), this is O(n²) = ~576 operations — negligible. For very large terminals (e.g. 400 rows × 400 cols), the run count could reach into the thousands and the O(n²) cost would become more measurable.

**Assessment:** The RESEARCH.md explicitly notes this is O(n²) and calls it "negligible" for the 80x24 case. The plan's MAX_RUNS=4096 cap on the decode side also bounds the worst-case encode input after a round-trip. For Phase 11's stated scope (Linux, typical terminals), this is correct and the right tradeoff. No action required in this phase; a Phase 13/14 optimization note (e.g., incremental size tracking) could be recorded in backlog if empirically needed.

**No fix required.**

---

### INFO-02: `decode_max_runs_guard` test allocates MAX_RUNS+1 DiffRun values in test setup

**File:** `crates/nosh-proto/src/datagram.rs`  
**Lines:** 578–602  
**Severity:** info (non-blocking)

**Observation:** The `decode_max_runs_guard` test constructs a `StateDiff` with 4097 runs and serializes it via `tag_encode`. This is intentional (the test must produce a payload that exceeds MAX_RUNS to exercise the guard), but the runs use `i % 24` for the row index with `i` as `u16`. With `MAX_RUNS = 4096` and the range `0..=MAX_RUNS as u16`, the range is `0..=4096u16` which fits a `u16` (max 65535). No overflow concern.

**Assessment:** Correct as written. The `i % 24` keeps rows in-bounds; empty `chars` keeps the payload small. The test correctly exercises T-11-02.

**No fix required.**

---

## Security Review

| Threat | Status |
|--------|--------|
| T-11-01: Malformed postcard body → panic | Mitigated — `postcard::from_bytes` returns Err; no unwrap on decode path |
| T-11-02: Oversized `Vec<DiffRun>` → large alloc | Mitigated — MAX_RUNS guard in decode_datagram; test-backed |
| T-11-03: Empty/truncated input → panic | Mitigated — split_first + from_bytes both return Err |
| T-11-04: Stale/reordered datagram | Accepted for Phase 11 — epoch field provided; consumer check at Phase 14 |
| T-11-05: Out-of-bounds row/col indices | Accepted — bounds check at apply site (Phase 14 renderer) |
| T-11-06: Information disclosure | Accepted — QUIC TLS 1.3 handles confidentiality |

---

## Quality Assessment

| Criterion | Result |
|-----------|--------|
| All tests pass (`cargo test -p nosh-proto`) | ✓ 22/22 |
| No unwrap/expect/panic on production decode path | ✓ |
| encode_datagram total (STRICT cap for any input) | ✓ |
| Continue-past-rejection (heterogeneous test) | ✓ |
| decode_datagram hardened (empty/unknown-tag/truncated/MAX_RUNS) | ✓ |
| String (not Vec<char>) for DiffRun.chars | ✓ |
| Tag byte budgeted (body_cap = cap - 1) | ✓ |
| Strict < on fill check (not <=) | ✓ |
| debug_assert on payload length | ✓ |
| Deferred start_col advances by char count | ✓ |
| Large-repaint decision documented with rejected alternatives | ✓ |
| No new dependencies | ✓ |
| Module pattern mirrors codec.rs D-03/D-04 convention | ✓ |
