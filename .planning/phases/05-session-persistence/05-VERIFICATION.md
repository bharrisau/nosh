---
phase: 05-session-persistence
verified: 2026-05-30T00:00:00Z
verifier: Claude Opus 4.8 (1M) — independent re-verification (EXTRA SCRUTINY)
status: gaps_found
score: 4/5 success criteria verified
re_verification:
  previous_verifier: sonnet (inline)
  previous_status: passed
  previous_score: 5/5
  regressions: []
  newly_found:
    - "SC#5 reaper exit-detection is non-functional in the production path (child taken before orphaning → slot.try_wait() always None → exited orphans never removed → slot/MasterPty leak)"
gaps:
  - truth: "A background zombie-reaper task calls child.try_wait() on all orphaned sessions; exited orphans are removed (no zombies, no leaked slots)"
    status: partial
    reason: >
      In the real server path the shell child is moved out of the Session
      (Session::take_child) into the wait_task BEFORE the session is orphaned.
      On transport loss the wait_task is detached (drop, not abort), so the
      child Box lives in the detached blocking thread, NOT in the slot. The
      reaper's exit check, slot.try_wait() -> Session::child_mut(), therefore
      always returns None for every real orphan, so the "shell exited" branch
      of reap_once() can never fire. With the default idle_timeout=0 an
      orphaned session whose shell has exited is RETAINED in the registry
      forever (leaked SessionSlot + MasterPty). The detached wait_task does
      reap the OS process (so literal zombies do not accumulate), but the SC's
      stated mechanism (reaper calling try_wait to clear exited orphans) is
      dead code in production. Empirically reproduced: orphan_count stays 1
      after the shell exits + reap_once() in the take-child path.
    artifacts:
      - path: "crates/nosh-server/src/server.rs:303-312"
        issue: "take_child() unconditionally moves the child into wait_task before any orphaning"
      - path: "crates/nosh-server/src/server.rs:480-482"
        issue: "TransportLost path: drop(wait_task) detaches the child; slot retains no child"
      - path: "crates/nosh-server/src/registry.rs:206-217"
        issue: "SessionSlot::try_wait() returns None whenever child_mut() is None (child already taken) — i.e. always for real orphans"
      - path: "crates/nosh-server/src/registry.rs:384"
        issue: "reap_once() relies on slot.try_wait().is_some() for exit detection — never true for real orphans"
      - path: "crates/nosh-server/src/registry.rs:786-822"
        issue: "Unit test reaper_removes_exited_orphan() orphans a slot WITHOUT taking the child, so it does not exercise the production child-ownership path and gives false confidence"
    missing:
      - "Make exit detection independent of the taken child. Options: (a) have the detached wait_task notify the registry on exit (e.g. call registry.remove on completion), or (b) record exit status into the slot (AtomicBool / Mutex<Option<i32>>) from wait_child and have reap_once consult that, or (c) keep the child in the slot and let the reaper own try_wait. Any of these makes reap_once actually remove exited orphans."
      - "Add an integration/unit test that orphans via the REAL server path (child taken) then asserts the reaper removes the slot after the shell exits."
human_verification:
  - test: "Long-running orphan-then-exit leak check"
    expected: "After a client transport-loss disconnect and the orphaned shell later exiting on its own (e.g. a backgrounded script that finishes), the server's orphan_count for that identity returns to 0 within a few reaper cycles."
    why_human: "Requires a live server, a real transport drop, and a shell that exits while orphaned — not covered by any automated test; current code will leak the slot."
---

# Phase 5: Session Persistence — Verification Report (Opus Re-Verification)

**Phase Goal:** An orphaned session (PTY + shell + output buffer) survives QUIC disconnect and waits for a client to reattach, within the per-identity session cap.
**Verified:** 2026-05-30 (independent opus re-verification with extra scrutiny)
**Status:** gaps_found
**Re-verification:** Yes — prior sonnet inline pass reported PASSED 5/5; this pass finds one genuine BLOCKER the prior pass missed.

## Summary of Disposition

4 of the 5 roadmap success criteria are genuinely satisfied in the code. One — the zombie-reaper criterion (SC#5 / the reaper half of PERSIST-01) — is **broken in the production path**: the reaper's exit-detection can never fire for a real orphan because the shell child is moved out of the Session before the session is orphaned. This causes exited orphans to leak their `SessionSlot` + `MasterPty` forever under the default `idle_timeout=0`. The defect is masked by a unit test that does not exercise the real child-ownership path. Empirically reproduced (see "Reaper trace" below).

All other locked decisions (D-01..D-11) are honored. `cargo test --workspace` is green (45 passed, 3 ignored), `cargo clippy --workspace --all-targets -- -D warnings` is clean.

## Observable Truths

| # | Truth (Success Criterion) | Status | Evidence |
|---|---------------------------|--------|----------|
| 1 | On QUIC drop, MasterPty stays open, shell not SIGHUP'd; live shell remains in an orphaned session | ✓ VERIFIED | On `TransportLost` (server.rs:465-483) the `Session` (and its `master: Box<dyn MasterPty>`) stays inside the slot held by the registry; `registry.orphan(&slot)` only marks state + enforces cap and never calls `sighup()`. `drop(wait_task)` detaches without aborting the child. Test `transport_loss_orphans_without_sighup` (persistence.rs:135) confirms orphan_count==1 and no HUP-trap file. (Test is weaker than its docstring — it does not probe /proc for liveness — but the ownership trace proves the master is retained.) |
| 2 | Orphaned sessions accumulate outgoing PTY chunks with monotonic u64 seq from session open, in a 64 KiB ring | ✓ VERIFIED | `SequencedOutputBuffer` (registry.rs:41-114): seq starts at 0, monotonic; `ring.len() > 1` guard keeps newest; drop-oldest under 64 KiB; truncation flag + `lowest_retained_seq`. Fed on every send via `slot.push_output(&data)` (server.rs:360, 423). 5 unit tests cover ordering, overflow, newest-survives, truncation marker. No seq gaps (only front popped → contiguous suffix retained). |
| 3 | Configurable idle timeout, default 0 = disabled; tested at 0 and finite | ✓ VERIFIED | `--idle-timeout-secs` (`env = NOSH_IDLE_TIMEOUT_SECS`, default 0) at main.rs:53; clap env gives CLI > env > default precedence, asserted by `cli_env_precedence_idle_timeout`. `reap_once` gates idle reaping on `idle_timeout > Duration::ZERO` (registry.rs:385). Tests `idle_timeout_zero_never_reaps_on_idle` and `finite_idle_timeout_reaps_old_orphan` cover both. |
| 4 | Per-identity cap (default 5) enforced before first orphan; deterministic eviction, not silent drop | ✓ VERIFIED | `orphan()` (registry.rs:279-329): `mark_orphaned()` then under the registry lock counts Orphaned slots; if `> max_per_identity` evicts the min-`last_active` orphan EXCLUDING the just-orphaned slot (by session_id), with `tracing::warn!` (never silent). Off-by-one correct: cap N allows exactly N orphans (N+1 → evict 1). Active slots excluded from count and never selected (D-07). Tests `cap_evicts_least_recently_active_orphan`, `active_slot_never_evicted`, `different_identities_are_independent`. |
| 5 | Background zombie-reaper calls try_wait() on all orphaned sessions; no zombies / exited orphans cleared | ✗ FAILED (partial) | Reaper EXISTS and is spawned (registry.rs:427, server.rs:118), but its exit-detection is **dead code for real orphans**: the child is `take_child()`'d into wait_task before orphaning (server.rs:303-306), so `slot.try_wait()` → `child_mut()` is always `None` (registry.rs:206-217). With default `idle_timeout=0`, exited orphans are never removed → `SessionSlot` + `MasterPty` leak. Literal OS zombies are avoided only because the detached wait_task reaps the process — incidental, not the SC's mechanism. **Empirically reproduced** (orphan_count stays 1 after exit+reap in the take-child path). The passing unit test `reaper_removes_exited_orphan` orphans WITHOUT taking the child, so it does not cover the production path. |

**Score:** 4/5 truths verified.

## Reaper trace (the load-bearing finding)

Production ownership path:
1. `run_session` moves `Session` into `SessionSlot::new(sess)` (server.rs:295). Session owns `master` (MasterPty).
2. `take_child()` removes the child from the Session and moves it into `wait_task = tokio::spawn(wait_child(child))` (server.rs:303-312). **From now on the slot's Session has `child == None`.**
3. On `TransportLost`: `registry.orphan(&slot)` (slot retained, master open, no SIGHUP ✓) then `drop(wait_task)` — detaches the blocking thread that owns the child (server.rs:480-482).
4. Reaper `reap_once()` checks `slot.try_wait().is_some()` (registry.rs:384). `SessionSlot::try_wait()` → `Session::child_mut()` → `None` (child was taken) → always `None`. The "shell exited" removal branch is unreachable for real orphans.
5. Default `idle_timeout=0` ⇒ `idle_expired` always false ⇒ the slot is **never removed**.

Empirical confirmation: a temporary probe test mirroring the production path (take child → register → orphan → kill+wait child → `reap_once`) asserted `orphan_count == 0` and **failed with `left: 1`** — the exited orphan was not reaped. Probe removed after confirmation; no source left modified (`git status` clean for `crates/`).

Consequence: the cap (PERSIST-03) bounds *concurrent slots per identity*, but dead-but-retained orphans count against that cap and consume an LRU slot. A recently-exited orphan (recent `last_active`) can crowd out a still-live orphan under the LRU policy. This is a real correctness/resource defect, not cosmetic.

## Required Artifacts

| Artifact | Expected | Status | Details |
|----------|----------|--------|---------|
| `crates/nosh-server/src/registry.rs` | SequencedOutputBuffer, SessionSlot, SessionRegistry, reaper | ⚠️ PARTIAL | All types substantive and wired; `reap_once` exit-detection non-functional in production (see above) |
| `crates/nosh-server/src/server.rs` | SessionEnd split, orphan vs remove routing | ✓ VERIFIED | `SessionEnd{ShellExited,ClientClosed,TransportLost}`; routing correct; wired to registry |
| `crates/nosh-server/src/session.rs` | master held open, child_mut seam, sighup by pid | ✓ VERIFIED | `child_pid` retained after `take_child` so `ClientClosed` SIGHUP still works |
| `crates/nosh-server/src/main.rs` | `--idle-timeout-secs` (+env), `--max-sessions-per-identity` | ✓ VERIFIED | clap env precedence; registry built from config |
| `crates/nosh-client/tests/persistence.rs` | 3 disconnect-outcome integration tests | ⚠️ ADEQUATE | 3 tests pass; transport-loss test does not probe shell liveness; NO test covers reaper removing a real (taken-child) exited orphan |

## Key Link Verification

| From | To | Via | Status |
|------|----|----|--------|
| run_session output pump | SequencedOutputBuffer | `slot.push_output(&data)` (server.rs:360,423) | ✓ WIRED |
| TransportLost | registry.orphan (no SIGHUP) | server.rs:480 | ✓ WIRED |
| ShellExited/ClientClosed | registry.remove | server.rs:448,462 | ✓ WIRED |
| run_accept_loop | reaper | `registry.spawn_reaper()` (server.rs:118) | ✓ WIRED (but reaper exit-path ineffective) |
| main.rs config | SessionRegistry::new | main.rs:105 | ✓ WIRED |

## Decision Compliance (D-01..D-11)

- D-01 (clean close/shell exit tears down): ✓ `ClientClosed`/`ShellExited` → SIGHUP+reap+remove (server.rs:412-463).
- D-02 (transport loss orphans, no SIGHUP, master open): ✓ verified by ownership trace + test.
- D-03 (single `last_active` drives idle + LRU): ✓ exactly one `last_active: Mutex<Instant>` per slot (registry.rs:149); used by both `reap_once` and `orphan` LRU. No second timestamp.
- D-04 (Active→Orphaned→reaped state machine): ✓ `SlotState{Active,Orphaned}`.
- D-05 (cap default 5): ✓ `DEFAULT_MAX_PER_IDENTITY=5`, CLI default 5.
- D-06 (LRU evict oldest orphan, logged): ✓ min `last_active`, `tracing::warn!`.
- D-07 (Active never evicted / never counted): ✓ filtered to Orphaned in count and victim selection; test `active_slot_never_evicted`.
- D-08 (idle default 0 = disabled): ✓.
- D-09 (CLI > env > default): ✓ via clap `env`.
- D-10 (monotonic u64 from open): ✓.
- D-11 (64 KiB drop-oldest, truncation marker, newest survives): ✓.
- Pitfall #7 (no SIGHUP on transport loss): ✓.
- **Pitfall #6 (zombie reaper):** ⚠️ OS zombies avoided incidentally by the detached wait_task, but the SC#5 reaper mechanism does not clear exited orphans → slot/MasterPty leak. This is the gap.

## Concurrency / Locking Review (clean)

- `SessionSlot` and `SessionRegistry` use `std::sync::Mutex` only for brief field/Vec operations; no lock is held across `.await`. `orphan()` and `reap_once()` both collect victims under the lock and `sighup()`/`drop()` AFTER releasing it (registry.rs:284-328, 376-422). No lock-ordering hazard (single registry mutex; slot mutexes are leaf locks taken individually). No mutex-poisoning path observed beyond `unwrap()` on healthy locks (acceptable for this phase). `mark_orphaned()` runs before the registry lock but the cap count is computed under the lock, so the cap check/insert is not a TOCTOU — no double-eviction or N+1 leak. Clippy `-D warnings` clean confirms no held-lock-across-await lint.

## Test Results

```
cargo test --workspace  → 45 passed, 3 ignored, 0 failed
  nosh-auth      11 passed, 1 ignored
  nosh-client     6 passed, 1 ignored
  persistence     3 passed
  session (it)    6 passed
  transport (it)  4 passed, 1 ignored
  auth (it)       4 passed
  nosh-server    14 passed (incl. registry + server + session unit)
  main.rs         1 passed
cargo clippy --workspace --all-targets -- -D warnings → clean
```

## Requirements Coverage

| Requirement | Status | Evidence |
|-------------|--------|----------|
| PERSIST-01 (orphan survives, master open, reaper prevents zombies) | ⚠️ PARTIAL | Orphan survival + master-open + no-SIGHUP verified; reaper does not clear exited orphans (slot leak). OS zombies avoided incidentally. |
| PERSIST-02 (idle timeout default 0) | ✓ SATISFIED | CLI/env precedence tested at 0 and finite. |
| PERSIST-03 (per-identity cap before first orphan, deterministic) | ✓ SATISFIED | Cap enforced in `orphan()`, LRU eviction logged. (Note: leaked dead orphans consume cap slots — secondary effect of the SC#5 gap.) |

## Gaps Summary

One BLOCKER: the SC#5 zombie-reaper mechanism is non-functional in the production code path. Because the shell child is taken out of the `Session` (into the detached `wait_task`) before the session is orphaned, `SessionSlot::try_wait()` is permanently `None` for every real orphan, so `reap_once()` can never detect shell exit. Under the default `idle_timeout=0`, exited orphans are retained in the registry indefinitely, leaking `SessionSlot` + `MasterPty` and consuming per-identity cap slots. Literal OS zombies are avoided only as a side effect of the detached wait task reaping the process — not via the reaper the SC describes. The passing unit test gives false confidence because it orphans a slot without taking the child, bypassing the actual ownership path. Fix by routing exit notification from the detached wait_task back into the registry, or by recording the exit status into the slot for the reaper to consult, and add a test that orphans via the real (child-taken) path.

---

_Verified: 2026-05-30 — Claude Opus 4.8 (1M), independent re-verification_
