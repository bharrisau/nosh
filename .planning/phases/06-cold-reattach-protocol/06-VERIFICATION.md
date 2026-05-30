---
phase: 06-cold-reattach-protocol
verified: 2026-05-30T00:00:00Z
status: passed
score: 4/4 success criteria verified (SC#1 blocker + 3 warnings all remediated)
verifier: Claude Opus 4.8 (1M) — independent goal-backward RE-VERIFICATION (extra scrutiny)
supersedes: prior inline "PASSED" self-verification (Sonnet 4.6)
remediation:
  applied: 2026-05-30 (opus, direct fix)
  commits: [d4fd933, 846e472, 75409e6, 99dca90]
  cargo: "test --workspace 63 passed 0 failed; clippy -D warnings clean"
  notes: >
    SC#1 fixed via next-expected-seq convention (last_acked_seq = count applied;
    server replays seq >= last_acked_seq; trim drops seq < acked_seq; client rebase
    = replaying_from_seq). New 4-cycle byte-exact reattach test (40 ordered markers)
    confirmed FAILS pre-fix / PASSES post-fix. W1 (token rotated only after ReattachOk
    sent), W2 (writer handed back to slot, no 200ms oneshot race), W3 (variant_name()
    in logs, no token) all fixed.
carry_forward:
  - "FIX 2 / W1 token-lifecycle (commit 846e472): fixer flagged it wants a human/verify look — confirm the mint_token_candidate/commit_token rotation timing under a send-failure path next session (verifier now resolves to opus)."
  - "SEPARATE pre-existing reader-zombie race surfaced during W2: spawn_blocking + abort() does NOT interrupt a blocked read(), so an old PTY reader task can keep consuming/stealing output bytes after teardown/reattach. Out of Phase-6 scope, NOT yet fixed. Investigate next session — could affect reattach correctness under timing; the byte-exact test passes today, so it's latent."
gaps:
  - truth: "SC#1 / ROAM-02 — replay continuity: buffered output since last_acked_seq is replayed with NO duplicated or dropped bytes"
    status: failed
    reason: >
      Fence-post mismatch between the client's last_acked_seq accounting and the
      server's 0-based sequence numbering. The server assigns seq starting at 0
      (SequencedOutputBuffer::push, registry.rs:72) and replays `seq > last_acked_seq`
      (registry.rs:154). The client's run_pump initialises highest_applied = 0 and
      increments it by 1 on EACH received PtyData frame (main.rs:327) — so after
      applying 0-based seq K the client reports highest_applied = K+1. It then sends
      that inflated value as Reattach.last_acked_seq (main.rs:264 via highest_applied)
      and as Ack.seq (main.rs:369). Consequence: after applying true-seq K, the
      server replays `seq > K+1`, silently DROPPING the chunk at seq K+1 — exactly
      one chunk the client never saw. Proven by probe: client applies seq 0 ("A"),
      reports highest_applied=1; replay_from(1) returns only seq 2 ("C"); seq 1 ("B")
      is dropped. The error also compounds: on each reattach the client rebases
      highest_applied = replaying_from_seq - 1 (main.rs:286), preserving the wrong
      baseline so a further chunk is lost on every reconnect cycle.
    artifacts:
      - path: "crates/nosh-client/src/main.rs:327"
        issue: "highest_applied counts received frames (start 0, +1/frame) — produces K+1 for 0-based applied seq K; off by one vs server replay boundary"
      - path: "crates/nosh-client/src/main.rs:264"
        issue: "sends inflated highest_applied as Reattach.last_acked_seq → server drops one chunk on replay"
      - path: "crates/nosh-client/src/main.rs:369"
        issue: "sends inflated highest_applied as Ack.seq → server trim_acked drops an un-applied chunk silently (no truncation flag)"
      - path: "crates/nosh-server/src/registry.rs:154"
        issue: "replay_from filters seq > last_acked_seq; correct on its own but the client's contract for last_acked_seq is off by one"
    missing:
      - "Reconcile the last_acked_seq convention end-to-end. Either (a) server numbers seq from 1 and client reports the true last APPLIED 0-based seq, or (b) client tracks the actual server seq carried with each chunk rather than counting frames from 0. Define and assert the exact mapping: applied seq K => last_acked_seq value V => server replays the correct contiguous range with no gap and no dup."
      - "Carry/echo the server seq to the client (e.g. include seq in PtyData or have the client derive it from replaying_from_seq + frame index) so highest_applied tracks real server seq, not a private frame count."
      - "Add an integration test that uses the REAL client counter (not hardcoded last_acked_seq=0) and asserts byte-exact continuity across a drop+reattach (concatenation of pre-drop applied output + replayed output == server's full output, no overlap, no gap)."
      - "Fix the Ack/trim_acked path so trimming never discards a chunk the client has not actually applied; or flag truncation when it does."
  - truth: "SC#1 integration test actually exercises replay continuity"
    status: partial
    reason: >
      reattach_replays_unacked_output_byte_exact (reattach.rs:69) hardcodes
      last_acked_seq = 0 (line 143) instead of using the client's highest_applied
      counter, so it never crosses the boundary where the fence-post bug bites. It
      also asserts only a fuzzy substring check ("at least 3 LINE markers present",
      line 196) which cannot detect a single dropped chunk. The unit tests
      (replay_from_returns_only_unacked_in_order etc.) assert replay_from in
      isolation with hand-chosen, internally-consistent seqs and never cross-check
      the client's counting convention — so the suite is green while the end-to-end
      contract is broken.
    artifacts:
      - path: "crates/nosh-client/tests/reattach.rs:143"
        issue: "last_acked_seq hardcoded to 0; does not exercise the client highest_applied path"
      - path: "crates/nosh-client/tests/reattach.rs:196"
        issue: "substring 'found_lines.len() >= 3' cannot detect a dropped/duplicated chunk"
    missing:
      - "Byte-exact end-to-end assertion driving the real client counter (see SC#1 above)."
human_verification:
  - test: "Live cold-reattach byte continuity: run an interactive session producing steady output, kill the link mid-stream, let the client auto-reattach, and confirm the rendered terminal has neither a gap nor a duplicated chunk at the seam."
    expected: "Terminal output is contiguous across the reconnect — no missing line(s), no repeated line(s)."
    why_human: "Requires a live network drop + a real TTY; the headless suite does not currently assert byte-exact continuity through the real client counter."
---

# Phase 6: Cold Reattach Protocol — Verification Report (opus re-verification)

**Phase Goal:** A client that disconnected and reconnected can resume its orphaned session in 1 RTT, with output continuity and no possibility of session hijacking.

**Status:** gaps_found — **SC#1 / ROAM-02 (replay continuity) is FAILED.**

This is an independent goal-backward re-verification on opus with extra scrutiny, prompted because prior sonnet self-verifications rubber-stamped real bugs in Phases 4 and 5. The prior inline "PASSED" verdict for Phase 6 is **superseded**. `cargo test --workspace` (61 tests) and `cargo clippy --workspace --all-targets -- -D warnings` are both green, but green tests do not equal goal achievement: the central deliverable of this phase — exactly-once replay — is broken, and the existing tests are structured so they cannot catch it.

## Observable Truths

| # | Truth (Success Criterion) | Status | Evidence |
|---|---------------------------|--------|----------|
| 1 | SC#1 / ROAM-02: replay with no duplicated or dropped bytes | ✗ **FAILED** | Fence-post: client reports K+1 for applied 0-based seq K (main.rs:327); server replays `seq > last_acked_seq` (registry.rs:154) → drops one chunk per reconnect. Probe-proven. |
| 2 | SC#2 / IDENT-02: two-factor auth (TLS handshake + token bound to identity) | ✓ VERIFIED | `registry.reattach` scopes token lookup to the TLS-authenticated identity's Vec (registry.rs:483-508); wrong identity → NotFound. e2e test `reattach_wrong_key_rejected_like_bad_token`. |
| 3 | SC#3: valid token + wrong key rejected identically to bad token (no oracle) | ✓ VERIFIED | All reject causes map to fieldless `Message::ReattachErr` via one `Err(_)` arm (server.rs:599-612); token never logged in the reattach path. Test asserts both outcomes structurally equal. |
| 4 | SC#4 / D-12: mutual exclusion — Reattach of Active/Reconnecting rejected | ✓ VERIFIED | Single registry-lock scope does find→state-check→mark_reconnecting atomically (registry.rs:485-523); no TOCTOU. Active and Reconnecting both → NotOrphaned. Tests cover both. |

**Score: 2/4 success criteria fully verified (SC#2/#3 count as one criterion pair; SC#1 FAILED, SC#4 verified).**

## High-Risk Areas — Findings

### 1. Replay exactly-once (ROAM-02 / SC#1) — BLOCKER
The single most important property of the phase is broken. Detail in the `gaps` frontmatter. Three concrete probes (now removed) demonstrated:
- Client applies seq 0, reports `highest_applied=1`; `replay_from(1)` returns `[2]` — **seq 1 dropped**.
- Client applies nothing, reports 0; with a 0-based buffer, the convention is only self-consistent when no real output exists.
- Inflated `Ack{seq}` → `trim_acked` drops a chunk the client never applied, **with `truncated` left false** — silent loss on the live path, not just on reattach.

Root cause is a 0-based-server-seq vs count-from-0-client-frames fence-post error in `main.rs` `run_pump`. `registry.rs::replay_from` / `trim_acked` are internally correct; the broken contract is on the client. The bug compounds across successive reconnects because `highest_applied` is rebased to `replaying_from_seq - 1` (main.rs:286), carrying the error forward.

### 2. Reattach generation-swap / orphan-exit watcher — OK
Traced orphan → reattach → re-orphan. `registry.reattach` returns the SAME `Arc<SessionSlot>` (verified `Arc::ptr_eq` in `reattach_matches_token_within_identity`); the original `TransportLost` watcher (server.rs:555-564) owns the original `wait_task` and only fires `remove_slot` when the shell **actually exits**. `remove_slot` is `Arc::ptr_eq`-keyed and idempotent, so a stale watcher cannot evict a live reattached slot, and it cannot fire while the shell is still running. The reattach pump does not spawn a second watcher (by design); shell exit during a reattach session is observed via PTY EOF → `ShellExited(0)` → `registry.remove(by session_id)`, with the original watcher's `remove_slot` as an idempotent backstop. No double-remove hazard, no re-leak for the shell-exit case. **No bug found here.**

### 3. Mutual exclusion / TOCTOU (D-12 / SC#4) — OK
`SessionRegistry::reattach` holds the registry `Mutex` across the entire lookup + Orphaned→Reconnecting transition (registry.rs:485-523). Two concurrent reattach attempts serialize; the loser sees `NotOrphaned`. A failed reattach re-orphans (server.rs `re_orphan`), returning the slot to `Orphaned` — verified it is not stuck in `Reconnecting` (probe). **Correct.**

### 4. Concurrency / lock discipline — OK
All `std::sync::Mutex` locks in registry.rs are released before any `.await`; `reattach`/`replay_from`/`trim_acked` are synchronous. Phase 5 lock discipline preserved. **No lock-across-await found.**

## Secondary Findings (WARNING — not blockers)

- **W1 — Token rotated before confirmed delivery (server.rs:636 then :638).** `rotate_token()` invalidates the old token *before* `ReattachOk` is sent. If the `ReattachOk` send or any replay frame send fails (lines 645/655), the slot is re-orphaned holding the NEW token while the client still holds the OLD one. The client's next reattach then fails terminally (`NotFound` → `ReattachErr` → client clears token and gives up, main.rs:267-273). Bounded (one lost session), no security impact, but undermines D-10's "retry indefinitely." Recommend rotating only after `ReattachOk` is acknowledged/flushed, or keeping the old token valid until the new one is confirmed.

- **W2 — PTY writer recovery can permanently disable reattach (server.rs:528-534 / 819-825).** Writer recovery uses a 200 ms oneshot timeout. If it times out (writer task slow to drain under load), the slot is orphaned with `pty_writer = None`. `registry.reattach` still succeeds (token/identity/state all fine → Reconnecting), but the server then fails at `take_pty_writer()` None (server.rs:679-686) and re-orphans — forever. The slot becomes permanently un-reattachable yet occupies a per-identity cap slot until idle-timeout or shell exit. Probe confirmed the state machine recovers (not stuck Reconnecting) but the writer remains absent. Recommend a more robust writer hand-back (store-in-slot / try_clone) or surfacing the failure.

- **W3 — Token can leak via Debug logging on a malformed first frame (server.rs:244).** The first-frame dispatch matches `SessionOpen`/`Reattach`; any other variant hits `tracing::warn!(?other, …)`. `Message` derives `Debug`, and `SessionOpened { token }` / `ReattachOk { new_token }` carry token bytes. A peer sending one of those as its first frame would have its token Debug-logged, violating the absolute D-07 "token is NEVER logged" invariant documented in messages.rs:124. The leaked bytes are peer-supplied (not the server's secret), so impact is low, but the invariant is stated as inviolable. Recommend redacting token-bearing variants from the `?other` log.

## Requirements Coverage

| Requirement | Status | Evidence |
|-------------|--------|----------|
| IDENT-02 (two-factor reattach bound to SSH identity, token never sole credential) | ✓ SATISFIED | Identity-scoped token lookup + uniform opaque error (SC#2/#3 verified). Minor W3 logging caveat. |
| ROAM-02 (1-RTT reattach, sequenced replay, no dup/drop) | ✗ **BLOCKED** | 1-RTT shape present, but "no duplicated or dropped bytes" is violated by the fence-post bug (SC#1 FAILED). |

## Anti-Patterns / Pitfalls Checked

| Pitfall | Result |
|---------|--------|
| #8 token-only hijack | Avoided — two-factor, identity-scoped (verified). |
| #9 seq resync dup/gap | **HIT** — drop-one-chunk fence-post (BLOCKER). |
| #10 two-clients-one-session race | Avoided — atomic Orphaned→Reconnecting under one lock (verified). |

## Verification Method

- Read all phase code: nosh-proto/messages.rs, nosh-server/registry.rs + server.rs, nosh-client/client.rs + main.rs, reattach.rs.
- Ran `cargo test --workspace` (61 pass, 3 ignored) and `cargo clippy --workspace --all-targets -- -D warnings` (clean).
- Wrote four probe tests (since removed; tree left clean) proving: drop-one-chunk on replay, drop-on-applied-nothing, single-frame boundary drop, and silent trim of an un-applied chunk via inflated Ack.

---

_Verified: 2026-05-30 — Claude Opus 4.8 (1M), independent re-verification. Working tree left clean (probe tests removed)._
