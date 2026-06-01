---
phase: 10-pty-reader-race-fix
verified: 2026-06-01T00:00:00Z
status: gaps_found
score: 3/3 roadmap success criteria functionally achieved in CODE; 1 test-coverage gap
verifier: gsd-verifier (independent adversarial re-verification, opus)
re_verification:
  previous_status: passed
  previous_score: 7/7
  previous_verifier: gsd-verifier (inline, sonnet)
  gaps_closed: []
  gaps_remaining:
    - "D-04 completion-barrier test exits readers via drop(sess) EOF, not the shutdown pipe"
  regressions: []
gaps:
  - truth: "D-04 deterministic completion-barrier test proves the SHUTDOWN-PIPE interrupt stops the reader while the shell is still alive"
    status: partial
    reason: >
      The shipped test `pty_io::tests::reader_exits_on_shutdown_barrier` calls
      `drop(sess)` BEFORE writing to the shutdown pipe. Dropping the Session drops
      the MasterPty, which gives the cloned reader an EOF/HUP and exits the reader
      on its own. A verifier probe that replicated the shipped test's exact sequence
      with the shutdown-pipe write REMOVED still reached `exit_count == N` (all 10
      readers exited). Therefore the test would stay GREEN even if
      `signal_shutdown()` were a complete no-op — it does not exercise the
      shutdown-pipe interrupt that is the entire deliverable of this phase. This is
      the exact "wrong-but-green" failure mode the phase context (D-04) warned
      against: the test must orphan while a real /bin/sh child is still running and
      assert the reader exits via the pipe.
    artifacts:
      - path: "crates/nosh-server/src/pty_io.rs"
        issue: >
          Test at lines 248-259: `drop(sess)` at line 248 precedes the pipe write at
          line 256. The reader exits on EOF from the dropped master, not the pipe.
          The PRIMARY assertion (exit_count == N) cannot distinguish pipe-exit from
          EOF-exit.
    missing:
      - >
        A test that keeps the Session (MasterPty + live shell) ALIVE and asserts (a)
        the reader stays parked in poll() with no shutdown signal, then (b) exits
        only after signal_shutdown(). A verifier-authored probe of exactly this shape
        (master kept alive) PASSED against the current production code, confirming the
        fix works — but no such assertion is committed to the repo, so a future
        regression of signal_shutdown() to a no-op would go undetected.
---

# Phase 10: PTY Reader Race Fix — Independent Re-Verification (Adversarial, opus)

**Phase Goal:** Orphaned sessions cleanly terminate their PTY reader threads — a
blocked `read()` is interruptible, so the server's blocking-thread count stays
bounded under repeated session orphan/drop cycles.

**Verified:** 2026-06-01
**Status:** gaps_found (1 test-coverage gap; production code is correct)
**Re-verification:** Yes — independent opus pass disputing the inline sonnet 7/7 PASS.

## Headline

The **production code achieves the goal** — independently proven by a verifier probe.
But the **shipped D-04 test does not test the mechanism it claims to test**: it exits
the reader via `drop(sess)` EOF, not the shutdown pipe, and would pass even if
`signal_shutdown()` were a no-op. The previous inline verification's T4/T5 evidence
("D-04 test … shutdown pipe byte makes pipe_read_fd readable") is a misread of what
the test actually exercises.

## Goal Achievement — Observable Truths

| # | Truth | Status | Evidence |
|---|-------|--------|----------|
| 1 | Reader stops within one polling interval when orphaned **while shell alive** (no SIGHUP, master kept open) | ✓ VERIFIED (probe) | Verifier probe `reader_parks_while_shell_alive_then_exits_on_pipe`: with `sess` kept alive, reader did NOT exit in 700 ms with no signal; exited <3 s after `signal_shutdown()`. Proves the pipe (not EOF) stops it. |
| 2 | Blocking-thread count stays bounded under repeated orphan cycles | ✓ VERIFIED | Production teardown (server.rs:559-560, 865-866) signals + awaits join before orphan; each live session = one reader thread that exits on signal. D-04 loop reaches exit_count==N. |
| 3 | `cargo test` passes, no regressions | ✓ VERIFIED | `cargo test -p nosh-server` → `ok. 24 passed; 0 failed`. (Inline verification claimed 25; actual is 24 — minor count discrepancy.) |
| 4 | D-04 test deterministically proves the **shutdown-pipe** interrupt | ✗ PARTIAL (BLOCKER-as-test-gap) | Probe replicating the shipped test sequence WITHOUT the pipe write still hit exit_count==N — the shipped test passes via `drop(sess)` EOF, not the pipe. |

**Functional score: 3/3 roadmap success criteria met in code. Test rigor: 1 gap.**

## Production Code Verification (the fix itself — CORRECT)

| Site | Requirement | Status | Evidence |
|------|-------------|--------|----------|
| `pty_io::unix_reader_loop` | poll([master, pipe]); exit on pipe-readable | ✓ | pty_io.rs:144-181; `fds[1].any()` → break (line 165) checked before PTY data |
| `run_session` TransportLost | signal + await join BEFORE orphan | ✓ | server.rs:559-560 then `registry.orphan` at 573 |
| `run_reattach_session` TransportLost | signal + await join BEFORE orphan | ✓ | server.rs:865-866 then `registry.orphan` at 872 |
| Old `output_reader.abort()` anti-pattern | removed | ✓ | no `reader.abort()` in server.rs |
| Master fd never closed on orphan | Pitfall 7 | ✓ | reader holds copied `i32`, never an owner; no `libc::close`/drop of master in pty_io.rs |
| Self-pipe fds owned (OwnedFd), closed once | Pitfall 4 | ✓ | pty_io.rs:49,109,134 — both ends OwnedFd; read end moved into closure, write end in handle |
| No O_NONBLOCK / F_SETFL | D-01 | ✓ | grep: only the doc comment mentions it; no call |
| W2 writer handback unchanged | D-03a | ✓ | `in_rx.blocking_recv` ×2, `return_pty_writer` ×4 in server.rs |
| `Session::master` stays private; raw fd via accessor | T3 | ✓ | session.rs:123 `master:` (private); `master_raw_fd` delegates; registry copies i32 under brief lock |

## Adversarial Probes Run (verifier-authored, then removed)

| Probe | Result | Meaning |
|-------|--------|---------|
| `reader_parks_while_shell_alive_then_exits_on_pipe` — keep sess ALIVE, no signal for 700 ms, then signal | PASS | Production fix is CORRECT: reader parks in poll() until the pipe wakes it; EOF is not the cause. |
| `drop_sess_alone_may_or_may_not_eof_reader` — drop sess, NO pipe signal, observe | reader exited via drop alone = **true** | `drop(sess)` alone causes reader EOF-exit — the shipped test's `drop(sess)` (line 248) is sufficient to end the reader. |
| `shipped_test_passes_with_NO_pipe_signal` — replicate shipped D-04 sequence, omit pipe write | PASS (exit_count==N) | The shipped D-04 test would be green even if `signal_shutdown()` did nothing. It does not test the deliverable. |

## Behavioral / Test Results

```
cargo test -p nosh-server : ok. 24 passed; 0 failed; 0 ignored
  includes: pty_io::tests::reader_exits_on_shutdown_barrier (passes — but via EOF, not pipe)
```

## Anti-Patterns Found

| File | Line | Pattern | Severity | Impact |
|------|------|---------|----------|--------|
| pty_io.rs | 246-248 | Test comment "avoids a PTY EOF race interfering with the shutdown test" + `drop(sess)` before signalling | ⚠️ Warning | The EOF the comment tries to "avoid" is in fact what ends the reader; the pipe path is never exercised. Misleading green test. |

## Gaps Summary

One gap, and it is about **test rigor, not shipped behavior**:

- The phase goal IS achieved by the production code — independently confirmed by a
  master-kept-alive probe that the shipped test does not perform.
- The shipped D-04 completion-barrier test is a false-confidence test: it orphans by
  `drop(sess)`, which closes the master and EOF-exits the reader, so it passes
  regardless of whether the shutdown pipe works. The phase context's D-04 requirement
  ("orphan while a real /bin/sh child is still running and assert exit via the pipe")
  is not met by the committed test.

**Recommended fix (small):** Add (or replace the existing) D-04 assertion with the
master-kept-alive shape proven above — keep `sess`/MasterPty alive across the orphan,
assert the reader is still parked before `signal_shutdown()`, and assert it exits only
after the signal. The production code already passes this; only the test needs
strengthening. This is a coverage hardening item, not a functional blocker — route to
backlog unless the team wants the regression guard in place before Phase 11 builds on
this callsite.

---

_Verified: 2026-06-01_
_Verifier: Claude (gsd-verifier), independent adversarial opus pass_
