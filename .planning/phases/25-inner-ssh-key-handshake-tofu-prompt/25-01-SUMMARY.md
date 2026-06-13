---
phase: 25-inner-ssh-key-handshake-tofu-prompt
plan: "01"
subsystem: nosh-proto / nosh-auth
tags: [wire-format, inner-auth, tdd, crypto-helpers, transcript-constants]
dependency_graph:
  requires: []
  provides:
    - InnerAuthChallenge/Response/Complete/Fail wire variants (discriminants 18-21)
    - NoshTransport::export_keying_material default-Err trait method
    - INNER_AUTH_EKM_LABEL / INNER_AUTH_EKM_CONTEXT / INNER_AUTH_LABEL_CLIENT / INNER_AUTH_LABEL_SERVER constants
    - check_authorized_key helper
    - verify_ed25519_spki helper
  affects:
    - crates/nosh-proto/src/messages.rs
    - crates/nosh-proto/src/codec.rs
    - crates/nosh-proto/src/transport_trait.rs
    - crates/nosh-proto/src/lib.rs
    - crates/nosh-auth/src/keys.rs
    - crates/nosh-auth/src/lib.rs
    - crates/nosh-server/src/server.rs
tech_stack:
  added: []
  patterns:
    - Append-only enum extension (postcard discriminant stability, WF-1)
    - Default-Err trait method (mirrors rtt()/is_closed() precedent)
    - TDD RED/GREEN cycle for crypto helper functions
    - Fieldless variant no-oracle invariant (mirrors ReattachErr)
key_files:
  created: []
  modified:
    - crates/nosh-proto/src/messages.rs
    - crates/nosh-proto/src/codec.rs
    - crates/nosh-proto/src/transport_trait.rs
    - crates/nosh-proto/src/lib.rs
    - crates/nosh-auth/src/keys.rs
    - crates/nosh-auth/src/lib.rs
    - crates/nosh-server/src/server.rs
decisions:
  - "Vec<u8> used for 64-byte signature fields (client_sig, server_sig) because serde Deserialize only covers fixed arrays up to [T; 32]; callers validate length at the protocol handler layer"
metrics:
  duration_seconds: 691
  completed_date: "13/06/2026"
  tasks_completed: 3
  files_modified: 7
requirements: [WT-04]
---

# Phase 25 Plan 01: Wire + Crypto Foundation Summary

Four inner-auth Message variants appended at discriminants 18-21 (append-only after ScrollbackCredit=17), with discriminant stability and fieldless-oracle tests; RFC 9266 EKM trait method + canonical transcript constants in nosh-proto; check_authorized_key and verify_ed25519_spki helpers added to nosh-auth via TDD.

## Tasks

| Task | Name | Commit | Files |
|------|------|--------|-------|
| 1 | Append inner-auth variants 18-21 + stability/fieldless tests | 3cbabea | messages.rs, codec.rs |
| 2 | Add export_keying_material + transcript label constants | 1fd49cc | transport_trait.rs, lib.rs, server.rs |
| 3 RED | Failing tests for check_authorized_key + verify_ed25519_spki | 1ac8b15 | keys.rs |
| 3 GREEN | Implement check_authorized_key + verify_ed25519_spki | 1c6db5b | keys.rs, lib.rs |

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 1 - Bug] `[u8; 64]` not serde-serializable — changed to Vec<u8>**
- **Found during:** Task 1 (compile error on first test run)
- **Issue:** `serde_core` only implements `Deserialize` for fixed arrays up to `[T; 32]`. The `InnerAuthResponse.client_sig` and `InnerAuthComplete.server_sig` fields were specified as `[u8; 64]` in the plan, which fails to compile.
- **Fix:** Changed both signature fields to `Vec<u8>` with doc comments requiring callers to validate `len() == 64`. This preserves the wire format semantics (postcard serialises `Vec<u8>` with a length prefix, which is unambiguous) while staying within the serde constraint. No new dependencies added.
- **Files modified:** `crates/nosh-proto/src/messages.rs`, `crates/nosh-proto/src/codec.rs` (test cases)
- **Commit:** 3cbabea

**2. [Rule 3 - Blocking] Non-exhaustive match in server.rs after variants 18-21 added**
- **Found during:** Task 2 verification (`cargo test --workspace --no-run`)
- **Issue:** `crates/nosh-server/src/server.rs:1002` has a match on `Message` that became non-exhaustive once variants 18-21 were added. The server would not compile.
- **Fix:** Added arms for `InnerAuthChallenge`, `InnerAuthResponse`, `InnerAuthComplete`, and `InnerAuthFail` that log a warning and break the session — correct behaviour since these frames are only valid during the pre-session inner-auth handshake in wt_transport.rs, not in a live session.
- **Files modified:** `crates/nosh-server/src/server.rs`
- **Commit:** 1fd49cc

## TDD Gate Compliance

Plan task 3 followed the mandatory RED/GREEN cycle:
- RED commit: `1ac8b15` — 5 failing tests (functions not yet implemented)
- GREEN commit: `1c6db5b` — 5 passing tests (implementation added)
- REFACTOR: not needed (implementation was minimal and clean)

## Verification Results

- `cargo test -p nosh-proto --lib message_discriminant_order_is_stable inner_auth_fail_is_fieldless`: PASSED
- `cargo test -p nosh-auth --lib check_authorized_key verify_ed25519_spki`: 5/5 PASSED
- `cargo test --workspace`: all tests green (no regressions)

## Known Stubs

None. All deliverables are fully implemented and tested. The `export_keying_material` default-Err impl is intentionally not a stub — it is the correct default that concrete WebTransport transports will override in downstream plans (25-02 server wiring, 25-03 client wiring).

## Threat Flags

None. The new wire surface (discriminants 18-21) is additive. The threat mitigations documented in the plan's STRIDE register were all applied:
- T-25-01-WF1: `message_discriminant_order_is_stable` extended in the same commit as the enum change.
- T-25-01-ORACLE: `inner_auth_fail_is_fieldless` test asserts 1-byte encoding mechanically.
- T-25-01-LOG: `variant_name()` arms return static strings only; no sig/nonce bytes in arm bodies.
- T-25-01-CB: Four canonical transcript constants in `nosh-proto/src/lib.rs`.
- T-25-01-SC: No new external packages added.

## Self-Check: PASSED

- FOUND: crates/nosh-proto/src/messages.rs
- FOUND: crates/nosh-proto/src/codec.rs
- FOUND: crates/nosh-proto/src/transport_trait.rs
- FOUND: crates/nosh-proto/src/lib.rs
- FOUND: crates/nosh-auth/src/keys.rs
- FOUND: crates/nosh-auth/src/lib.rs
- FOUND commit 3cbabea (Task 1)
- FOUND commit 1fd49cc (Task 2)
- FOUND commit 1ac8b15 (Task 3 RED)
- FOUND commit 1c6db5b (Task 3 GREEN)
- InnerAuthFail at line 409 is fieldless (trailing comma, no braces)
