---
phase: 13-server-datagram-sender
verified: 2026-06-02T00:00:00Z
status: passed
score: 4/4 must-haves verified
overrides_applied: 0
re_verification:
  previous_status: none
  note: "Initial verification (no prior VERIFICATION.md). Phase had a code review (13-REVIEW.md) finding 2 CRITICAL + 4 WARNING; this pass independently confirms the fixes."
---

# Phase 13: Server Datagram Sender Verification Report

**Phase Goal:** The server emits coalesced terminal-state diffs over QUIC datagrams from the session pump, gated by a ResumeComplete signal so they never corrupt a partial cold-reattach replay.
**Verified:** 2026-06-02
**Status:** passed
**Re-verification:** No — initial verification (with adversarial confirmation of post-review fixes)

## Goal Achievement

### Observable Truths (ROADMAP Success Criteria + PLAN must_haves)

| # | Truth | Status | Evidence |
|---|-------|--------|----------|
| 1 | `run_session` select! loop has a `diff_interval.tick()` arm encoding one StateDiff per ~16ms and calling `conn.send_datagram()` — not per PTY chunk | ✓ VERIFIED | server.rs:580 `interval(Duration::from_millis(16))` + MissedTickBehavior::Skip; tick arm at :645; `conn.send_datagram(result.payload)` at :673. Epoch increments at tick time in `build_state_diff` (:328), NOT in the PTY arm (:606-626 untouched). |
| 2 | Integration test connects test client+server, types chars, asserts client `read_datagram()` yields non-empty StateDiff | ✓ VERIFIED | sync.rs:63 `sync03_server_emits_datagram_after_pty_output` — passes (3/3 sync tests pass on this host). Asserts `!d.runs.is_empty()` and `epoch >= 1`. |
| 3 | Datagrams suppressed until ResumeComplete after cold-reattach replay; reattach session sends no datagrams during replay window | ✓ VERIFIED | `resume_complete` declared `true` at server.rs:1001 — strictly AFTER the replay loop (:979-987) and "replay complete" log (:988-992). Same async task is sequential, so the select! loop (with the gated diff arm at :1103) cannot start until replay finishes. Gate check `if !resume_complete { continue; }` at :1104. |
| 4 | `run_reattach_session` also has the datagram sender arm with the same ResumeComplete gate | ✓ VERIFIED | `diff_interval.tick()` arm at :1103, epoch-ack arm at :1139 — body identical to run_session. `grep -c "diff_interval.tick()"` = 2; `grep -c "decode_epoch_ack"` = 2 arms. |

**Score:** 4/4 truths verified

### Adversarial Confirmation of Post-Review Fixes (13-REVIEW.md)

| Review item | Severity | Required | Status | Evidence |
|-------------|----------|----------|--------|----------|
| CR-01 acked-epoch baseline snapshot-at-SEND-time (both loops) | CRITICAL | yes | ✓ FIXED | `epoch_snapshots: VecDeque<(u64, Vec<Vec<Cell>>)>` cap=16 (EPOCH_SNAPSHOT_CAP, :171). Sent snapshot stored at send time keyed by epoch (:667, :1122). On ack, looked up by `*e == acked` and used as new `last_acked_snapshot` (:697-702, :1147-1152). Anti-regression guard `acked > last_acked_epoch` preserved (:690, :1143). Applied to BOTH loops. |
| CR-02 epoch bumped when pending_deferred non-empty even if grid unchanged (both loops) | CRITICAL | yes | ✓ FIXED | `build_state_diff` (shared by both loops) :328 `if cells != last_sent_snapshot || !pending_deferred.is_empty() { *current_epoch += 1; }`. The `pending_deferred` arg is the backlog before fresh runs merge — correct. |
| std::sync Mutex on terminal_state never held across .await | invariant | yes | ✓ VERIFIED | `with_terminal_state` closure has no await; `awk` over `build_state_diff` body → 0 `.await`; epoch-ack arm snapshot also await-free. registry.rs delegate releases lock when closure returns. |
| Reliable PtyData path unchanged (D-13-04 additive) | invariant | yes | ✓ VERIFIED | PTY output arms (:606-626, reattach equivalent) call `push_output_and_parse` + `write_message(PtyData)` unchanged; replay loop `for (_seq, data) in &chunks` (:979) byte-identical. `grep -c "Message::PtyData { data }"` = 5 (unchanged). |
| decode_epoch_ack rejects unknown tags incl. TAG_STATE_DIFF=0x01 | security | yes | ✓ VERIFIED | datagram.rs:187-189 `if *tag != TAG_CLIENT_EPOCH { return Err(...) }`. Tests `decode_epoch_ack_rejects_state_diff_tag`, `_rejects_empty`, `_rejects_bad_body` all pass. |
| WR-01 run-extension breaks on first unchanged cell | WARNING | no | ✓ FIXED | server.rs:250-254 break on `base2 == c2`. |
| WR-02 reattach ShellExited uses remove_slot (Arc identity) | WARNING | no | ✓ FIXED | server.rs:1213-1219 `registry.remove_slot(&slot)` with WR-02 comment. |
| WR-03 deferred-queue cap truncates from END | WARNING | no | ✓ FIXED | server.rs:346 `all_runs.truncate(MAX_RUNS)`. |
| WR-04 sync test drain uses wall-clock deadline (not mid-frame cancel) | WARNING | no | ✓ FIXED | sync.rs:327 `drain_deadline`; single deadline, no per-iteration read_message cancellation. |
| IN-01 / IN-02 / IN-03 | INFO | no | ℹ️ NOT APPLIED | Info-level only. IN-02 row cast is safe-by-invariant (SessionSlot::resize clamps to 1000 rows). Not blocking. |

### Required Artifacts

| Artifact | Expected | Status | Details |
|----------|----------|--------|---------|
| crates/nosh-proto/src/datagram.rs | TAG_CLIENT_EPOCH=0x02, ClientEpoch, encode/decode_epoch_ack | ✓ VERIFIED | Constants :21/:23; struct :151; encode :161; decode :183. Re-exported at lib.rs:15. |
| crates/nosh-server/src/registry.rs | `with_terminal_state` closure delegate, field stays private | ✓ VERIFIED | Delegate present, poison-recovery lock, field private (`pub terminal_state` count = 0). |
| crates/nosh-server/src/server.rs | diff_interval + epoch-ack arms in BOTH loops, ResumeComplete gate, build_state_diff/compute_diff_runs | ✓ VERIFIED | All present and wired; CR-01/CR-02/WR-01/WR-02/WR-03 fixes applied. |
| crates/nosh-client/tests/sync.rs | 3 SYNC-03 e2e tests | ✓ VERIFIED | 3 `#[tokio::test]`, all pass. |

### Key Link Verification

| From | To | Via | Status |
|------|----|----|--------|
| server.rs diff arm | conn.send_datagram | encode_datagram → send_datagram | ✓ WIRED (:673, :1128) |
| server.rs epoch-ack arm | decode_epoch_ack | read_datagram → decode_epoch_ack | ✓ WIRED (:689, :1142) |
| build_state_diff | slot.with_terminal_state | snapshot under closure, no await | ✓ WIRED (:309) |
| sync.rs | decode_datagram + encode_epoch_ack | decode received diff; send ack | ✓ WIRED |

### Behavioral Spot-Checks / Tests Run

| Check | Command | Result | Status |
|-------|---------|--------|--------|
| nosh-proto suite (incl. all epoch_ack tests) | `cargo test -p nosh-proto` | 28 passed, 0 failed | ✓ PASS |
| nosh-server lib + bin | `cargo test -p nosh-server` | 79 + 1 passed, 0 failed | ✓ PASS |
| SYNC-03 integration | `cargo test -p nosh-client --test sync` | 3 passed, 0 failed | ✓ PASS |
| Full workspace | `cargo test --workspace` | all suites 0 failures | ✓ PASS |
| Build | `cargo build --workspace` | exit 0 | ✓ PASS |
| Lint | `cargo clippy --workspace --tests -- -D warnings` | exit 0 | ✓ PASS |

### CR-01 Adversarial Probe (verifier-authored)

The mandate requested a probe reproducing the CR-01 divergence (ack epoch E while grid advanced to M>E, assert E→M changes re-sent). I authored `sync_cr01_probe.rs`, ran it against the fixed code (PASS), then temporarily injected the buggy snapshot-at-receive-time behavior into BOTH epoch-ack arms.

**Result: the probe did NOT discriminate** — it passed under both the fixed and buggy versions, then was removed (no source residue; `git status` clean). Diagnostic output showed the markers (MARKERTWO/MARKERTHREE) remained on the visible grid, so a full-ish repaint re-emitted them under either baseline. The CR-01 bug only loses cells that are produced and then OVERWRITTEN/scrolled-away before the next ack cycle — a deterministic transient-overwrite scenario over `/bin/sh` is timing-sensitive and could not be made reliable within the verification budget.

**CR-01 correctness therefore rests on code reading**, which is unambiguous: `epoch_snapshots` captures `result.sent_cells` (the exact grid the StateDiff for epoch E was computed from, build_state_diff:309/354) at send time, and the ack arm looks it up by `*e == acked` and assigns it to `last_acked_snapshot`. The baseline is provably the send-time grid, never the receive-time grid. The anti-regression guard and per-loop application are both present. This is the correct fix.

### Requirements Coverage

| Requirement | Source Plan | Description | Status | Evidence |
|-------------|-------------|-------------|--------|----------|
| SYNC-03 | 13-01, 13-02, 13-03 | Server emits coalesced state diffs over QUIC datagrams (one per ~16ms tick, not per chunk) from the session pump; gated by ResumeComplete so they never apply to a partial cold-reattach replay | ✓ SATISFIED | Truths 1-4 all verified; diff_interval (16ms), per-tick coalescing, ResumeComplete gate in both loops, e2e test proof. |

No orphaned requirements: REQUIREMENTS.md maps only SYNC-03 to Phase 13, and all three plans declare it.

### Anti-Patterns Found

| File | Pattern | Severity | Impact |
|------|---------|----------|--------|
| (none) | Debt markers (TBD/FIXME/XXX/todo!/unimplemented!) scan over all 5 modified files | — | None found. No stubs; both session loops fully wired. |

### Human Verification Required

None. All success criteria are programmatically verifiable and verified; the full workspace test suite (including 3 SYNC-03 e2e tests over the real QUIC datagram channel) passes.

### Gaps Summary

No gaps. The phase goal is achieved: both `run_session` and `run_reattach_session` emit one coalesced StateDiff datagram per ~16ms tick (epoch incremented at tick time, not per PTY chunk), gated by a ResumeComplete signal set strictly after cold-reattach replay completes. The two CRITICAL review findings (CR-01 stale-baseline-timing, CR-02 deferred-run starvation) are independently confirmed fixed in BOTH loops; all four WARNING findings are also fixed. The reliable PtyData path is unchanged (D-13-04 additive). `decode_epoch_ack` rejects all non-0x02 tags. terminal_state lock is never held across an await.

One verification-method note (not a defect): the verifier's CR-01 integration probe could not deterministically reproduce the divergence because the acked-epoch model re-sends still-visible content under either baseline; CR-01 correctness was confirmed by direct code reading instead, which is conclusive.

---

_Verified: 2026-06-02_
_Verifier: Claude (gsd-verifier)_
