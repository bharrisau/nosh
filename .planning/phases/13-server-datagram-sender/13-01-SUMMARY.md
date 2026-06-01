---
phase: 13-server-datagram-sender
plan: "01"
subsystem: nosh-proto, nosh-server
tags: [datagram, epoch-ack, wire-format, terminal-state, tdd]
dependency_graph:
  requires: [12-02-SUMMARY.md]
  provides: [ClientEpoch wire type, encode_epoch_ack, decode_epoch_ack, SessionSlot::with_terminal_state]
  affects: [13-02-PLAN.md, 13-03-PLAN.md]
tech_stack:
  added: []
  patterns: [postcard tag-prefixed datagram, closure delegate with Mutex poison recovery]
key_files:
  created: []
  modified:
    - crates/nosh-proto/src/datagram.rs
    - crates/nosh-proto/src/lib.rs
    - crates/nosh-server/src/registry.rs
decisions:
  - ClientEpoch derives Copy (single u64, no heap) unlike StateDiff which is Clone-only
  - with_terminal_state uses poison-recovery (unwrap_or_else) consistent with push_output_and_parse (WR-01)
  - terminal_state field stays private; with_terminal_state is the sole read access path
  - Pre-existing nosh-client clippy failure (doc-lazy-continuation in client.rs) is out of scope; only nosh-proto and nosh-server are modified in this plan
metrics:
  duration: "~8 minutes"
  completed: "2026-06-02"
  tasks_completed: 2
  tasks_total: 2
  files_modified: 3
---

# Phase 13 Plan 01: Epoch-Ack Wire Format and with_terminal_state Delegate Summary

ClientEpoch wire type (TAG_CLIENT_EPOCH=0x02, encode/decode with tag-mismatch rejection) and SessionSlot::with_terminal_state closure delegate added as pure prerequisites for the server datagram sender in Plan 02.

## Tasks Completed

| Task | Name | Commit | Files |
|------|------|--------|-------|
| 1 | Add ClientEpoch epoch-ack wire format to nosh-proto (TDD) | 7520a08 | datagram.rs, lib.rs |
| 2 | Add with_terminal_state read-only delegate to SessionSlot | 03ce853 | registry.rs |

## What Was Built

### Task 1: ClientEpoch wire format (nosh-proto)

- Activated `TAG_CLIENT_EPOCH: u8 = 0x02` (was a reserved comment on line 22)
- Added `pub struct ClientEpoch { pub epoch: u64 }` deriving `Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize` — `Copy` is appropriate since it is a single `u64` field
- Added `pub fn encode_epoch_ack(epoch: u64) -> Bytes` (infallible: postcard serialization of a `u64` cannot fail)
- Added `pub fn decode_epoch_ack(bytes: &[u8]) -> Result<u64, ProtoError>` with the security-relevant tag check: any tag != 0x02 including TAG_STATE_DIFF (0x01) returns `Err` and is never deserialized as a ClientEpoch (T-13-01 guard)
- Re-exported `ClientEpoch`, `decode_epoch_ack`, `encode_epoch_ack` from `nosh_proto` crate root
- 5 new unit tests in `datagram::tests`:
  - `epoch_ack_roundtrip` — epoch=1 round-trip
  - `epoch_ack_roundtrip_extremes` — epoch=0 and u64::MAX
  - `decode_epoch_ack_rejects_state_diff_tag` — TAG_STATE_DIFF (0x01) returns Err
  - `decode_epoch_ack_rejects_empty` — empty slice returns Err (no panic)
  - `decode_epoch_ack_rejects_bad_body` — correct tag but garbage body returns Err (no panic)
  - `encode_epoch_ack_first_byte_is_tag` — first byte is exactly 0x02

### Task 2: with_terminal_state delegate (nosh-server)

- Added `pub fn with_terminal_state<F, R>(&self, f: F) -> R where F: FnOnce(&TerminalState) -> R` to `impl SessionSlot`
- Lock acquired via `self.terminal_state.lock().unwrap_or_else(|e| e.into_inner())` (WR-01 poison recovery)
- Rustdoc documents the Anti-Pattern #2 invariant: caller MUST NOT `.await` inside the closure
- `terminal_state` field remains private — this delegate is the only permitted read path
- Unit test `with_terminal_state_returns_snapshot_from_closure`: pushes "hi" via `push_output_and_parse`, snapshots `(cols, rows, cursor)` via the closure, asserts 80x24 default size and `cursor.col > 0`

## Test Results

- `cargo test -p nosh-proto`: 28 passed (22 pre-existing + 6 new epoch-ack tests)
- `cargo test -p nosh-server --lib`: 79 passed (78 pre-existing + 1 new with_terminal_state test)
- `cargo clippy -p nosh-proto -- -D warnings`: clean
- `cargo clippy -p nosh-server -- -D warnings`: clean
- `cargo build --workspace`: succeeded

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 1 - Bug] Fixed pre-existing clippy::unnecessary_lazy_evaluations in decode_datagram**
- **Found during:** Task 1 clippy run
- **Issue:** `decode_datagram` used `ok_or_else(|| ProtoError::Postcard(...))` where the value is not expensive to construct — clippy -D warnings catches this
- **Fix:** Changed to `ok_or(ProtoError::Postcard(...))` matching the pattern used in my new `decode_epoch_ack`
- **Files modified:** `crates/nosh-proto/src/datagram.rs`
- **Commit:** 7520a08 (bundled with Task 1 implementation)

### Known Out-of-Scope Pre-existing Issue

A pre-existing `clippy::doc_lazy_continuation` error exists in `crates/nosh-client/src/client.rs` line 296 (not modified by this plan). `cargo clippy --workspace -- -D warnings` fails because of this. Per scope boundary rules, this is not fixed here — only the two crates modified by this plan (`nosh-proto`, `nosh-server`) are required to pass clippy, which they do.

## Threat Flags

All threats addressed per plan's threat model:
- T-13-01 (Tampering): tag check enforced — `decode_epoch_ack` rejects any tag != 0x02 including TAG_STATE_DIFF (0x01). Enforced by `decode_epoch_ack_rejects_state_diff_tag` test.
- T-13-02 (DoS): `split_first()` on empty returns Err; truncated body returns Err from postcard. ClientEpoch is a fixed single-u64 (≤9 bytes) with no Vec — no allocation-amplification vector. Enforced by `decode_epoch_ack_rejects_empty` and `decode_epoch_ack_rejects_bad_body` tests.
- T-13-03 (Spoofing/Replay): accepted by design — replayed acked epoch is harmless (monotonic u64); convergence guaranteed by baseline-advance guard in Plan 02.

## Known Stubs

None. Both additions are fully functional with no placeholder values or deferred wiring.

## Self-Check: PASSED

- `crates/nosh-proto/src/datagram.rs` — exists with TAG_CLIENT_EPOCH, ClientEpoch, encode_epoch_ack, decode_epoch_ack
- `crates/nosh-proto/src/lib.rs` — re-exports confirmed
- `crates/nosh-server/src/registry.rs` — with_terminal_state present in impl SessionSlot
- Commits 7520a08 and 03ce853 confirmed in git log
