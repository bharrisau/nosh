---
phase: 10-pty-reader-race-fix
fixed_at: 2026-06-01T00:00:00Z
review_path: .planning/phases/10-pty-reader-race-fix/10-REVIEW.md
iteration: 1
findings_in_scope: 6
fixed: 4
skipped: 0
status: all_fixed
---

# Phase 10: Code Review Fix Report

**Fixed at:** 2026-06-01
**Source review:** `.planning/phases/10-pty-reader-race-fix/10-REVIEW.md`
**Re-verification input:** `.planning/phases/10-pty-reader-race-fix/10-VERIFICATION-REVERIFY.md`
**Iteration:** 1

**Summary:**
- Findings in scope: 6 (CR-01, D-04 test rewrite from REVERIFY, WR-01, WR-02, IN-01, IN-02/IN-03 folded into D-04 rewrite)
- Fixed: 4 atomic commits (all findings addressed)
- Skipped: 0

## Fixed Issues

### CR-01: EINTR retry in poll()

**Files modified:** `crates/nosh-server/src/pty_io.rs`
**Commit:** `c440b48`
**Applied fix:** Added `Err(nix::errno::Errno::EINTR) => continue` arm before the catch-all `Err(_) => break` in the `match poll(...)` block inside `unix_reader_loop`. POSIX mandates re-issuing `poll()` on EINTR; the previous catch-all exited the PTY reader whenever SIGCHLD (or any other signal) interrupted the blocking call.

---

### D-04 (IN-02/IN-03): Rewrite shutdown-barrier test to use pipe, not PTY EOF

**Files modified:** `crates/nosh-server/src/pty_io.rs`
**Commit:** `2be8cce`
**Applied fix:** Rewrote `reader_exits_on_shutdown_barrier` to keep `sess` (MasterPty) and `writer` alive for the duration of each per-cycle spawn task. The master PTY fd is NOT dropped before `signal_shutdown()` fires; the reader is blocked in `poll([master, pipe])` and can only be woken by the shutdown pipe byte. Dropping both resources is deferred to inside the async task, after `join.await` completes. Updated the doc comment to explain the invariant and why the prior `drop(sess)` ordering was the "wrong-but-green" failure mode the D-04 verifier proved.

---

### WR-02: O_CLOEXEC on shutdown self-pipe

**Files modified:** `crates/nosh-server/src/pty_io.rs`, `crates/nosh-server/Cargo.toml`
**Commit:** `fb3a313`
**Applied fix:** Replaced `nix::unistd::pipe()` with `nix::unistd::pipe2(nix::fcntl::OFlag::O_CLOEXEC)` to set CLOEXEC atomically at pipe creation. Added the `"fs"` feature to the nix dependency in `Cargo.toml` (required for `pipe2` and `OFlag` in nix 0.29 — confirmed the feature gate from the compiler diagnostic).

---

### WR-01 / IN-01: Remove no-op abort() calls; document double signal_shutdown

**Files modified:** `crates/nosh-server/src/server.rs`
**Commit:** `d9ec47b`
**Applied fix:** Removed both `input_writer.abort()` calls in `run_session` (line 609) and `run_reattach_session` (line 881). Replaced each with an explanatory comment documenting why abort() is a no-op on spawn_blocking tasks (Pitfall 6) and that drop(in_tx) is what unblocks the writer. Also updated the "best-effort" `signal_shutdown()` comments at the end of both functions to explain that the second call on the TransportLost path writes to a closed-read-end pipe, returns EPIPE silently ignored by signal_shutdown(), and does not warrant structural refactoring (IN-01).

---

## Build and Test Results

```
cargo build -p nosh-server  →  Finished (clean, 0 errors, 0 warnings)
cargo test -p nosh-server   →  ok. 24 passed; 0 failed; 0 ignored (finished in 0.08s)
  includes: pty_io::tests::reader_exits_on_shutdown_barrier  →  ok
            (verified via pipe, not PTY EOF — master kept alive across signal)
```

---

_Fixed: 2026-06-01_
_Fixer: Claude (gsd-code-fixer)_
_Iteration: 1_
