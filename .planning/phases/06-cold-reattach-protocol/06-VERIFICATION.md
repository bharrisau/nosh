# Phase 6 Verification Report

**Phase:** 06 - Cold Reattach Protocol
**Date:** 2026-05-30
**Executed by:** Claude Sonnet 4.6 (autonomous, no subagents)
**Status:** PASSED

---

## Plans Executed

| Plan | Title | Status |
|------|-------|--------|
| 06-01 | Reattach protocol messages (nosh-proto) | ✓ Complete |
| 06-02 | Registry reattach core | ✓ Complete |
| 06-03 | Server dispatch, SessionOpened, reattach rebind | ✓ Complete |
| 06-04 | Client reconnect supervisor + integration tests | ✓ Complete |

**Total:** 4 plans, 15 tasks across 4 serial waves.

---

## Test Results

### `cargo test --workspace` (final run)

All test suites green after full implementation:

| Crate | Suite | Count | Status |
|-------|-------|-------|--------|
| nosh-auth | unit | 11 passed, 1 ignored | ✓ |
| nosh-proto | unit | 5 passed | ✓ |
| nosh-client (auth.rs) | integration | 6 passed, 1 ignored | ✓ |
| nosh-client (persistence.rs) | integration | 3 passed | ✓ |
| nosh-client (session.rs) | integration | 6 passed | ✓ |
| nosh-client (transport.rs) | integration | 4 passed, 1 ignored | ✓ |
| nosh-client (reattach.rs) | integration | 3 passed | ✓ |
| nosh-server (lib) | unit | 22 passed | ✓ |
| nosh-server (main) | unit | 1 passed | ✓ |

**Total: 61 tests, 0 failed, 3 ignored (require live ssh-agent/path)**

### `cargo clippy --workspace --all-targets -- -D warnings`

Clean — no errors.

---

## Roadmap Success Criteria Coverage

| SC# | Criterion | Test | Result |
|-----|-----------|------|--------|
| SC#1 | Replay continuity: no duplicated or dropped bytes | `reattach_replays_unacked_output_byte_exact` | ✓ PASS |
| SC#2/#3 | Two-factor auth + no-oracle negative test | `reattach_wrong_key_rejected_like_bad_token` | ✓ PASS |
| SC#4 | Mutual exclusion (Active session rejects reattach) | `reattach_rejected_while_session_active` | ✓ PASS |

Registry-level coverage (unit tests):
- `reattach_matches_token_within_identity` — Arc::ptr_eq same instance ✓
- `reattach_wrong_identity_is_notfound` — no-oracle (both Err) ✓
- `reattach_active_or_reconnecting_is_rejected` — Active and Reconnecting both NotOrphaned ✓

---

## Key Design Decisions Honored

### D-12 (Mutual exclusion)
`SessionRegistry::reattach` acquires the registry lock once for the entire
lookup + state transition. Orphaned → Reconnecting is atomic. Active and
Reconnecting slots return `ReattachReject::NotOrphaned`. Verified by unit
test `reattach_active_or_reconnecting_is_rejected` and SC#4 e2e test.

### D-07 (No oracle / uniform opaque rejection)
`ReattachErr` is fieldless. All rejection causes (`NotFound`, `IdentityMismatch`,
`NotOrphaned`) map to the same opaque wire frame. The server logs only the
identity fingerprint and a generic outcome label; token is never logged.
`reattach_wrong_key_rejected_like_bad_token` asserts both rejections are
structurally identical (`ReattachOutcome::Err == ReattachOutcome::Err`).

### D-05 (Single-use rotated token)
`SessionSlot::rotate_token()` generates a fresh `Uuid::new_v4().into_bytes()`
on every successful reattach. The previous token is immediately invalidated.
`rotate_token_changes_token` unit test verifies the value changes.

### Arc::ptr_eq safety (load-bearing correctness point)
`registry.reattach()` returns the SAME `Arc<SessionSlot>` instance by cloning
the stored arc (never constructing a new `SessionSlot`). The orphan-exit watcher
spawned in the `TransportLost` path uses `remove_slot(Arc::ptr_eq)` which
remains valid across reattaches. Verified by `reattach_matches_token_within_identity`.

### last_acked_seq off-by-one convention (flagged in 06-RESEARCH §6)
**Locked to: highest output sequence number the client has APPLIED.**
Server replays all chunks with `seq > last_acked_seq` (strictly greater than).
This is documented on the `Message::Reattach` doc-comment:
> `last_acked_seq`: the highest output sequence number the client has APPLIED;
> the server replays `seq > last_acked_seq`.
Assertions: `replay_from_returns_only_unacked_in_order` pushes seqs 0..4 and
calls `replay_from(2)`, asserting returned seqs are exactly [3, 4]. Two test
cases cover "client applied nothing" (`replay_from(0)` when ring starts at 3
→ truncated) and "client applied some output" (trim_acked(2) then replay_from(2)
→ seqs [3, 4, 5]).

### D-08 (Continuous acking, trim_acked does NOT set truncated)
`SequencedOutputBuffer::trim_acked` drops ring entries with `seq <= acked_seq`
but explicitly does NOT set `self.truncated`. Only cap-overflow in `push()`
sets `truncated`. Verified by `trim_acked_drops_acked_and_keeps_unacked_and_does_not_truncate`.

### D-09 (Truncation indicator)
`replay_from(last_acked_seq)` detects when the ring's front seq is strictly
greater than `want_from = last_acked_seq + 1` and returns `truncated_below_request = true`
with `replaying_from_seq = ring_front_seq`. Verified by
`replay_from_signals_truncation_when_request_predates_ring`.

---

## Deviations and Notes

1. **PTY writer recovery via oneshot channel**: The plan suggested storing the
   writer in the slot or using try_clone. The implementation uses a oneshot
   channel to return the writer from the blocking input task on TransportLost
   (within a 200ms timeout). This is transparent to correctness.

2. **Reattach pump exit code approximation**: When the shell exits during a
   reattach session, the exit code is sent as 0 (approximate) since the original
   wait_task holds the real exit code. This matches the plan's "approximate"
   guidance.

3. **SC#1 test design**: The initial design used `reattach_collect` which waits
   for `SessionClose`. Redesigned to use a long-running shell (sleep 60) and
   manual stream reading to avoid blocking on session exit. The shell is cleaned
   up by sending `exit 0` after assertion.

4. **run_session signature refactored**: To satisfy clippy's 7-argument limit,
   `run_session`'s 11 parameters were grouped into a `SessionOpenParams` struct,
   and `run_reattach_session`'s 8 parameters were consolidated with a tuple for
   `(token, last_acked_seq)`.

---

## Verification Status

**PASSED** — all 4 roadmap success criteria have passing tests, `cargo test
--workspace` is green, and `cargo clippy --workspace --all-targets -- -D warnings`
is clean. No human review items identified.
