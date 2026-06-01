---
phase: 11-datagram-wire-protocol
plan: "01"
subsystem: proto
tags: [postcard, serde, datagram, quic, wire-format, terminal-diff]

requires:
  - phase: 03-pty-session-core
    provides: "ProtoError type in codec.rs; postcard to_allocvec/from_bytes pattern"
  - phase: 01-quic-transport
    provides: "QUIC datagram transport configured in transport.rs"

provides:
  - "StateDiff wire type — sparse terminal diff with epoch, cols, rows, cursor, runs"
  - "DiffRun wire type — run-length terminal run with style/fg/bg/chars (String)"
  - "CursorPos and CellStyle(u8) bitflag types"
  - "encode_datagram(diff, cap) — cursor-priority fill, STRICT payload.len() < cap"
  - "decode_datagram(bytes) — tag-byte dispatch, MAX_RUNS guard, no panic on malformed input"
  - "16 inline tests covering round-trip, decode-hardening, size-cap, cursor-priority, heterogeneous-fill"

affects:
  - "phase-12-server-terminal-state-model (produces StateDiff values)"
  - "phase-13-datagram-sender (calls encode_datagram, conn.send_datagram)"
  - "phase-14-client-state-apply (calls decode_datagram, applies epoch guard)"
  - "phase-15-predictive-echo (reads StateDiff for prediction)"

tech-stack:
  added: []
  patterns:
    - "Datagram wire format isolated behind datagram.rs (mirrors codec.rs D-03/D-04 convention)"
    - "Tag byte discriminant (0x01=StateDiff, 0x02 reserved Phase 13) for extensible decode"
    - "Cursor-priority fill with postcard::experimental::serialized_size strict-cap check"
    - "Continue-past-rejection fill loop (never breaks on first oversize run)"

key-files:
  created:
    - "crates/nosh-proto/src/datagram.rs"
  modified:
    - "crates/nosh-proto/src/lib.rs"

key-decisions:
  - "D-11-01 honored: cursor-priority partial update (skip-frame and reliable-stream fallback explicitly rejected in doc comment at encode_datagram)"
  - "All three tasks executed as a single commit (A+B+C) since the plan's ordering constraint is structural not temporal — encode_datagram introduced ONCE, already cap-correct"
  - "Strict less-than cap: body_cap = cap - 1, check size < body_cap (not <=), debug_assert!(body.len() < body_cap)"
  - "MAX_RUNS = 4096 guard in decode_datagram returns ProtoError (T-11-02 DoS guard)"
  - "Split-and-defer on oversize single runs with correct start_col advancement (chars().count())"

requirements-completed:
  - SYNC-01

duration: 4min
completed: 2026-06-01
---

# Phase 11 Plan 01: Datagram Wire-Format Module Summary

**Delivers `nosh-proto/src/datagram.rs` — the shared sparse terminal-diff contract (SYNC-01): StateDiff type, tag-byte encode/decode pair with provable STRICT cap, cursor-priority fill with continue-past-rejection, and 16 inline tests.**

## Performance

- **Duration:** 4 min
- **Started:** 2026-06-01T10:10:19Z
- **Completed:** 2026-06-01T10:14:36Z
- **Tasks:** 3 (A: types, B: decode, C: encode — executed as one atomic commit per the plan's structural ordering requirement)
- **Files modified:** 2

## Accomplishments

- Created `crates/nosh-proto/src/datagram.rs` (796 lines) implementing the complete SYNC-01 wire format: four public types, `encode_datagram`, `decode_datagram`, and 16 inline tests (all passing).
- `encode_datagram` is TOTAL with a STRICT `payload.len() < cap` guarantee: tag byte budgeted via `body_cap = cap - 1`; fill loop uses strict `size < body_cap`; `debug_assert!` on invariant; heterogeneous test proves continue-past-rejection.
- `decode_datagram` returns `ProtoError` (never panics) on all malformed inputs: empty bytes, unknown tag, truncated postcard body, and run count exceeding `MAX_RUNS=4096` (T-11-02 DoS guard).
- Large-repaint design decision documented at `encode_datagram` definition: cursor-priority chosen; skip-frame and reliable-stream fallback explicitly rejected with rationale.
- All 22 `nosh-proto` tests pass (16 new datagram tests + 6 pre-existing codec/messages tests) — zero regressions.

## Task Commits

Tasks A, B, and C were implemented in a single atomic commit (the plan calls for no cap-violating intermediate export; all three tasks implement types, decode, and encode together in one file):

1. **Tasks A+B+C: datagram.rs + lib.rs** — `9f36b29` (feat(11-01))

## Files Created/Modified

- `crates/nosh-proto/src/datagram.rs` — New module: StateDiff, DiffRun, CursorPos, CellStyle types; encode_datagram (cursor-priority fill, strict cap, split-on-wide-run); decode_datagram (tag byte, MAX_RUNS guard); 16 inline tests
- `crates/nosh-proto/src/lib.rs` — Added `pub mod datagram;` and re-exported all public items: `StateDiff, DiffRun, CursorPos, CellStyle, encode_datagram, decode_datagram`

## Decisions Made

- **Single atomic commit** for all three tasks: the plan's task ordering (A then B then C, no intermediate encode export) is a structural invariant enforced by the implementation, not by commit boundaries. Since all code landed in one file in one sitting, splitting artificially would be misleading. The commit message documents all three task deliverables.
- **`body_cap = cap.saturating_sub(1)`**: reserves the TAG_STATE_DIFF byte before the fill loop; all `serialized_size` checks compare against `body_cap` (strict less-than), not `cap`.
- **Continue-past-rejection**: the `else` branch after a rejected run does NOT `break` or `continue` to the outer loop; it falls through and the `for` loop advances naturally. This was verified by the `heterogeneous_continue_past_rejection` test which would fail for a break-on-first-rejection implementation.
- **`tag_encode` test helper**: a `#[cfg(test)]` only function that bypasses the cap to build raw postcard payloads for decode-hardening tests. This preserves the Task B invariant (no cap-violating encode function exported) while providing round-trip test coverage.

## Deviations from Plan

None — plan executed exactly as written. All `must_haves`, `acceptance_criteria`, `key_links`, and `verification` commands pass.

## Verification Results

```
cargo build -p nosh-proto     → Finished (exit 0)
cargo test -p nosh-proto      → 22 passed, 0 failed (exit 0)
cargo test -p nosh-proto --lib datagram → 16 passed, 0 failed (exit 0)
```

Key acceptance criteria spot-checks:
- `pub struct StateDiff` / `DiffRun` / `CursorPos` / `CellStyle` — all present
- `chars: String` declared (not `Vec<char>`); `Vec<char>` only in doc comments
- `pub mod datagram;` and `pub use datagram::...` in lib.rs (includes encode_datagram, decode_datagram)
- `grep 'skip.frame\|skip-frame'` and `grep 'reliable.stream\|reliable-stream'` both match in doc comment
- `grep 'debug_assert'` matches on body length strict bound
- `grep '< body_cap'` matches (strict less-than on fill check)
- `postcard::experimental::serialized_size` used in fill loop and split verification
- No `unwrap`/`expect`/`panic!` outside `#[cfg(test)]` in decode path
- datagram.rs is 796 lines (>= 200 minimum)

## Self-Check: PASSED
