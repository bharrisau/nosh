# Phase 26: Migration Handover over WebTransport — Summary

**Session ID:** 75a42ad6-2e60-4381-a9d9-aec5b20461ee
**Status:** Complete (verification-driven correction applied)

## What This Plan Delivered

Phase 26 is a **test-only phase** that adds adversarial integration coverage for the migration-handover guarantees (MH-2 concurrent reattach guard) over WebTransport. The production code paths (reconnect loop, reattach, token rotation, replay) were already implemented in Phase 24/25; this phase exists solely to prove those guarantees hold with tests that would fail on regression.

### Primary Deliverable: MH-2 Concurrent Test

The core artifact is the integration test `wt06_concurrent_same_token_one_winner` in `crates/nosh-client/tests/webtransport.rs`, which races two concurrent WebTransport connections through the full inner-auth → reattach path and proves exactly one client wins.

### Verification-Driven Correction (Post-Plan)

The opus adversarial verifier (2026-06-14) identified that `wt06_concurrent_same_token_one_winner` does **not** genuinely prove the MH-2 atomic guard — the loser is excluded by token rotation, not by the `Orphaned → Reconnecting` state check. The test passed even with the guard fully removed.

To fix this coverage gap, the verification fix added:

1. **Deterministic unit test** (`crates/nosh-server/src/registry.rs::reattach_concurrent_same_token_one_winner`): Races two `SessionRegistry::reattach` calls directly at the registry lock with the same un-rotated token. The registry mutex serializes the attempts deterministically; the loser MUST be rejected by `NotOrphaned`. This test FAILS if the `state != Orphaned` guard is removed, making the guard falsifiable.

2. **Corrected integration test docs**: Updated `wt06_concurrent_same_token_one_winner`'s docstring to accurately describe what it proves (end-to-end "one winner" property) and explicitly note that the atomic guard itself is proven by the registry unit test — clarifying the division of responsibility.

## Why This Correction Was Necessary

The MH-2 atomic guard (`state != Orphaned` at registry.rs:712-713) is correct in production. The defect was purely test coverage: the Phase 26 integration test could not detect its regression. Without the deterministic unit test, a future change could accidentally remove the guard and the test suite would still pass — false confidence that would silently break the "exactly one winner" invariant.

## Files Modified

- `crates/nosh-server/src/registry.rs`: Added `reattach_concurrent_same_token_one_winner` unit test (adversarial, deterministic proof of the atomic guard).
- `crates/nosh-client/tests/webtransport.rs`: Updated docstring for `wt06_concurrent_same_token_one_winner` to accurately describe its role.

## Result

Phase 26 now genuinely proves all four success criteria with falsifiable tests:
- SC#1 (replay): Proven by `wt06_seamless_resume_over_webtransport`.
- SC#2 (MH-2 atomic guard): Proven by `reattach_concurrent_same_token_one_winner` (registry.rs).
- SC#3 (token rotation): Proven by `wt06_seamless_resume_over_webtransport`.
- SC#4 (end-to-end seamless resume): Proven by `wt06_seamless_resume_over_webtransport`.

The integration test `wt06_concurrent_same_token_one_winner` provides end-to-end validation of the full stack (inner auth + reattach) but is no longer mislabeled as the primary proof of the atomic guard itself.

---

_Summary written 2026-06-14 (verification fix)_
