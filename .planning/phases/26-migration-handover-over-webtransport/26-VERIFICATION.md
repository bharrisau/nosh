---
phase: 26-migration-handover-over-webtransport
verified: 2026-06-14T00:00:00Z
status: passed
score: 4/4 success criteria genuinely proven (all falsifiable)
re_verification:
  previous_status: failed
  previous_score: 3/4
  gaps_closed:
    - "SC#2 — Concurrent same-token reattach resolves atomically to exactly one active session via the MH-2 state guard; loser gets ReattachErr (NotOrphaned)"
  gaps_remaining: []
  regressions: []
---

# Phase 26: Migration Handover over WebTransport — Verification Report

**Phase Goal:** A nosh client survives a network change in WebTransport mode by transparently reconnecting, re-authenticating, and resuming the server-side session with byte-exact replay.
**Verified:** 2026-06-14
**Status:** PASSED
**Re-verification:** Yes — after gap closure (commit 79d4eeb)

## Goal Achievement

Phase 26 is deliberately TEST-ONLY (production reconnect loop, MH-2 guard, token
rotation, replay all shipped in Phase 24/25). The verification question is therefore
NOT "was new production code written" but "do the tests genuinely PROVE the 4 success
criteria, and would they FAIL if the underlying guarantee regressed?" All four
criteria now pass that adversarial bar.

The prior verification (status: failed, 3/4) found a single blocker: SC#2's
integration test `wt06_concurrent_same_token_one_winner` gave false confidence — it
passed even with the `state != Orphaned` guard fully removed (0/10 failures), because
token rotation serialised the two attempts and excluded the loser by token mismatch
(`NotFound`) before the guard could fire. The fix (commit 79d4eeb) added a
deterministic registry-level unit test that closes this gap. This re-verification
re-ran the falsification probe directly and confirms the gap is genuinely resolved.

### Success Criteria → Assertion Map + Falsification Probe

| SC | Truth | Test + Assertion | Falsification probe | Probe result | Status |
| -- | ----- | ---------------- | ------------------- | ------------ | ------ |
| 1 | Byte-exact replay from SequencedOutputBuffer on reattach | `wt06_seamless_resume_over_webtransport` ASSERT 5 (replayed output contains `MARK26A`), wt_reattach.rs:296 | Disabled replay loop in `run_reattach_session` (server.rs:1660-1669) | Test FAILED at line 296 as expected | ✓ VERIFIED (carried forward) |
| 2 | Concurrent same-token reattach resolves atomically to exactly one active session via the MH-2 state guard; loser gets ReattachErr (NotOrphaned) | `reattach_concurrent_same_token_one_winner` (registry.rs:2141): races two `SessionRegistry::reattach` calls on the same Orphaned slot with the SAME un-rotated token; asserts `ok_count==1`, `err_count==1`, AND loser reason `== NotOrphaned` (registry.rs:2203/2208/2216) | Neutralised the `state != Orphaned` guard (registry.rs:712 → `if false && …`) so it never rejects | **Test FAILED: "expected exactly one reattach to succeed, got 2" (registry.rs:2203)** — both calls returned Ok with the guard gone; reverted → passes again | ✓ VERIFIED (gap closed) |
| 3 | Reattach token rotated on every successful reattach | `wt06_seamless_resume_over_webtransport` ASSERT 2 (`new_token != token`), wt_reattach.rs:252 | Made `mint_token_candidate` return current token (no rotation), registry.rs:454 | Test FAILED at line 252 as expected (D-07-compliant message, no token bytes) | ✓ VERIFIED (carried forward) |
| 4 | Simulated network change → seamless resume (no disruption beyond reconnect) | `wt06_seamless_resume_over_webtransport` end-to-end: drop → reconnect → inner auth → Reattach → ReattachOutcome::Ok + replay + orphan-baseline (ASSERT 1/5/6) | Covered transitively by SC#1 + SC#3 probes; ASSERT 6 (orphan count returns to baseline, wt_reattach.rs:307) distinguishes resume from fresh session | Passes; SC#1/#3 probes fail it when those guarantees regress | ✓ VERIFIED (carried forward) |

**Score:** 4/4 success criteria genuinely proven.

### SC#2 Re-Probe (the load-bearing check — re-run by the verifier this pass)

The blocker was that SC#2's test passed regardless of the guard. The fix is a new
deterministic unit test that races two `SessionRegistry::reattach` calls on the same
Orphaned slot with the SAME token, with no `run_reattach_session`/`commit_token`
between them — so token rotation cannot serialise/exclude either attempt and the
atomic `Orphaned → Reconnecting` guard is the ONLY possible rejector.

| Step | Command | Result |
| ---- | ------- | ------ |
| 1. Baseline | `cargo test -p nosh-server --lib reattach_concurrent_same_token_one_winner` | **PASSED** (1 passed) |
| 2. Falsify (guard neutralised, registry.rs:712 → `if false && state != Orphaned`) | same | **FAILED** — `assertion left == right failed: MH-2 atomic guard: expected exactly one reattach to succeed, got 2` (registry.rs:2203). Both calls returned `Ok` → guard is the only thing preventing two winners. |
| 3. Revert + re-run | `git checkout registry.rs` → same test | **PASSED** again |

This is the inverse of the prior integration test's behaviour: removing the guard now
flips the result from pass to fail. The test is genuinely falsifiable.

**Loser-reason assertion (proves guard, not rotation):** registry.rs:2214-2231 require
the losing call to fail specifically with `Err(ReattachReject::NotOrphaned)` and panic
on any other reason (`NotFound`, `IdentityMismatch`) or on two `Ok`s. Because the two
calls use the SAME un-rotated token, a `NotFound` (token-mismatch) rejection is
impossible here — the only path to a single failure is the `state != Orphaned` guard
firing after the winner's `mark_reconnecting()`. This is exactly the distinction the
prior verification flagged as missing.

### Honesty of the integration test docstring

The original integration test `wt06_concurrent_same_token_one_winner`
(webtransport.rs:289) was kept, and its docstring (lines 274-283) was corrected: it now
states the loser is excluded by TOKEN ROTATION (not the MH-2 guard), explicitly defers
the guard proof to the registry unit test, and frames its own value as end-to-end
"one winner / full stack" validation. No false claim remains.

### Probe Execution (falsification — the load-bearing step)

| Probe | Target | Command | Result | Verdict |
| ----- | ------ | ------- | ------ | ------- |
| MH-2 guard (neutralised) | `state != Orphaned` made unreachable (`if false && …`) | `cargo test -p nosh-server --lib reattach_concurrent_same_token_one_winner` | FAILED at registry.rs:2203 ("got 2") | **guard proven — test falsifies** |
| Token rotation | `mint_token_candidate` returns current token | `cargo test -p nosh-client --features webtransport --test wt_reattach` | FAILED at wt_reattach.rs:252 | rotation proven |
| Replay | replay loop disabled in `run_reattach_session` | same | FAILED at wt_reattach.rs:296 | replay proven |

### Baseline Suite (all green this pass)

| Command | Result |
| ------- | ------ |
| `cargo build --workspace` | clean (Finished dev profile) |
| `cargo test --workspace` | 387 passed, 3 ignored (24 suites) |
| `cargo test -p nosh-client --features webtransport` | 212 passed, 3 ignored (17 suites) |
| `cargo test -p nosh-server --features "test-support webtransport" --lib inner_auth` | 7 passed |
| `cargo test -p nosh-server --lib reattach_concurrent_same_token_one_winner` | 1 passed |

### Required Artifacts

| Artifact | Expected | Status | Details |
| -------- | -------- | ------ | ------- |
| `crates/nosh-server/src/registry.rs` (reattach_concurrent_same_token_one_winner, line 2141) | SC#2 deterministic MH-2 guard test | ✓ VERIFIED | Races two `SessionRegistry::reattach` on one Orphaned slot, same un-rotated token; asserts 1 Ok + 1 Err(NotOrphaned). Falsifies when the guard is removed. |
| `crates/nosh-client/tests/wt_reattach.rs` | SC#1/#3/#4 seamless-resume test | ✓ VERIFIED | Compiles, runs, passes; falsifies on replay/rotation regressions |
| `crates/nosh-client/tests/webtransport.rs` (wt06_concurrent...) | SC#2 end-to-end one-winner test | ✓ VERIFIED | Docstring corrected to claim only end-to-end value; defers guard proof to the registry unit test |
| `crates/nosh-server/src/wt_transport.rs` (doc) | Session-loss detection doc | ✓ VERIFIED | Doc block present (lines 380-397) |
| `crates/nosh-client/tests/common/mod.rs` | shared helper (WR-03 fix) | ✓ VERIFIED | `client_config_with_pinning` centralised |

### Anti-Patterns Found

None of concern. No TBD/FIXME/XXX in the modified files. 26-02-SUMMARY.md is now
populated (the prior empty-summary gap is closed).

### Tree State

All probe edits reverted. `git status --short crates/` is empty (CLEAN). The only
edit made during re-verification — neutralising the guard at registry.rs:712 — was
reverted via `git checkout` and the test re-confirmed passing.

### Gaps Summary

No gaps remain. The single prior blocker (SC#2 false confidence) is resolved by the
new deterministic registry test `reattach_concurrent_same_token_one_winner`, which
this verifier independently re-probed: it PASSES at baseline, FAILS when the
`state != Orphaned` guard is neutralised (two winners), and PASSES again after revert.
The loser-reason assertion pins the rejection to `NotOrphaned`, proving the atomic
state guard rather than token rotation. SC#1, SC#3, SC#4 were confirmed-falsifiable in
the prior pass and are carried forward as VERIFIED. All baseline suites are green.

---

_Verified: 2026-06-14_
_Verifier: Claude (gsd-verifier, opus, adversarial — re-verification)_
