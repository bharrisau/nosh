---
phase: 10-pty-reader-race-fix
reviewed: 2026-06-01T00:00:00Z
depth: deep
files_reviewed: 5
files_reviewed_list:
  - crates/nosh-server/src/pty_io.rs
  - crates/nosh-server/src/server.rs
  - crates/nosh-server/src/session.rs
  - crates/nosh-server/src/registry.rs
  - crates/nosh-server/Cargo.toml
findings:
  critical: 1
  warning: 2
  info: 3
  total: 6
status: issues_found
---

# Phase 10: Code Review Report

**Reviewed:** 2026-06-01T00:00:00Z
**Depth:** deep
**Files Reviewed:** 5
**Status:** issues_found

## Summary

The self-pipe interruptible PTY reader (`pty_io.rs`) is structurally sound — the
OwnedFd ownership model is correct, both pipe ends are closed exactly once, the
master fd is never closed on the orphan path, and the D-03 completion-barrier
ordering (signal → join-await → orphan()) is implemented correctly in both
`run_session` and `run_reattach_session`. The W2 writer-handback and D-04 test
are present.

One blocker-level correctness bug was found: `nix::poll` returns
`Err(Errno::EINTR)` when the calling thread is interrupted by a signal. The code
treats every poll error identically (`Err(_) => break`) and terminates the reader
on EINTR. In a production server, SIGCHLD is delivered to blocking threads each
time any child process exits (the login shell, or any command the shell ran). This
silently kills the PTY output pump whenever the shell reaps a child, causing the
session to stall.

---

## Critical Issues

### CR-01: `poll()` exits on EINTR instead of retrying — PTY reader killed by SIGCHLD

**File:** `crates/nosh-server/src/pty_io.rs:158-160`

**Issue:** `nix::poll::poll()` returns `Err(Errno::EINTR)` when the blocking
thread is interrupted by an unblocked signal. The code matches all errors with a
single `Err(_) => break`, which exits the reader loop on signal interruption.

In this server, SIGCHLD is delivered to the thread pool whenever a child process
exits. The login shell reaps its own subcommands (every invocation of `ls`, `vim`,
`sleep`, etc. sends SIGCHLD). Each such SIGCHLD can interrupt the `poll()` call on
a blocking thread, causing the PTY reader to exit immediately — the session goes
silent. The client's terminal freezes, and further output is dropped.

EINTR is not a fatal error; POSIX mandates that applications re-issue `poll()` on
EINTR. All other error codes (EBADF, EFAULT, ENOMEM) are genuine failures and
should continue to break the loop.

**Fix:**
```rust
match poll(&mut fds, PollTimeout::NONE) {
    Err(nix::errno::Errno::EINTR) => continue, // signal interrupted poll(); retry
    Err(_) => break,                            // genuine error → exit
    Ok(0) => continue,                          // spurious wakeup
    Ok(_) => {}
}
```

---

## Warnings

### WR-01: `input_writer.abort()` is documented as a no-op on `spawn_blocking` tasks but still called

**File:** `crates/nosh-server/src/server.rs:609` (also line 881 in `run_reattach_session`)

**Issue:** The code's own comment at line 359 in `run_session` states: "Pitfall 6:
`abort()` on an executing `spawn_blocking` task is a no-op; this replaces it."
Yet `input_writer.abort()` is called on both the `ShellExited`/`ClientClosed`
paths (lines 609, 881). The abort call has no effect. Worse, it creates a
misleading impression that the input task is being cleanly terminated, when in
reality it continues running until `blocking_recv` returns `None` (from
`drop(in_tx)` which was already called) and the task stores the writer back into
the slot.

For `ShellExited` and `ClientClosed` the session is `registry.remove()`'d, so the
lingering task is harmless — but the slot's `Arc` (held by `slot_for_writer` in
the closure) is kept alive until the task completes. This delays the `MasterPty`
close by however long the blocking task takes to drain its channel, which is
normally near-instant. The abort() call does not shorten this.

**Fix:** Remove the `abort()` calls on `input_writer` on the non-orphan paths and
add a comment explaining why it is not needed:
```rust
// abort() on spawn_blocking is a no-op (Pitfall 6). drop(in_tx) above already
// unblocked the blocking_recv loop; the task will drain and return the writer
// to the slot on its own. Nothing further to do here.
// input_writer.abort();   <- removed: no-op, misleading
```

---

### WR-02: Shutdown self-pipe not created with `O_CLOEXEC` — defense-in-depth gap

**File:** `crates/nosh-server/src/pty_io.rs:109-110`

**Issue:** `nix::unistd::pipe()` calls `libc::pipe()`, which does not set
`O_CLOEXEC`. The pipe file descriptors are therefore inheritable. Today the
server process does not `exec()` any child directly (portable-pty does, and it
sets `CLOEXEC` on its own fds), so the shell process itself cannot inherit the
shutdown pipe fds. However, any future `exec()` in the server process would
silently inherit both pipe ends, leaking them into an unrelated subprocess. That
subprocess would prevent the POLLHUP-on-write-end-close mechanism from firing and
could corrupt the shutdown signal (by consuming the wakeup byte from the read
end).

Setting `O_CLOEXEC` atomically at pipe creation is the standard defensive practice
and has no downside. The `nix "fs"` feature (needed for `pipe2`) is not currently
in `Cargo.toml`; the simplest alternative is `fcntl(FD_CLOEXEC)` immediately after
`pipe()`.

**Fix:**
```rust
// Option 1: add "fs" to nix features and use pipe2
let (read_fd, write_fd) =
    nix::unistd::pipe2(nix::fcntl::OFlag::O_CLOEXEC)
        .context("create shutdown self-pipe")?;

// Option 2: fcntl after pipe() (no new feature required)
let (read_fd, write_fd) =
    nix::unistd::pipe().context("create shutdown self-pipe")?;
nix::fcntl::fcntl(read_fd.as_raw_fd(),
    nix::fcntl::FcntlArg::F_SETFD(nix::fcntl::FdFlag::FD_CLOEXEC))
    .context("set CLOEXEC on pipe read end")?;
nix::fcntl::fcntl(write_fd.as_raw_fd(),
    nix::fcntl::FcntlArg::F_SETFD(nix::fcntl::FdFlag::FD_CLOEXEC))
    .context("set CLOEXEC on pipe write end")?;
```

---

## Info

### IN-01: `signal_shutdown()` called twice on the `TransportLost` path

**File:** `crates/nosh-server/src/server.rs:559` and `608` (also `865`/`880` in
`run_reattach_session`)

**Issue:** The `TransportLost` arm explicitly calls `signal_shutdown()` at line
559, then awaits `reader_handle.join` at line 560. By the time execution falls
through to the post-match "best-effort" call at line 608, the reader thread has
already exited. The second `signal_shutdown()` writes one byte to a pipe whose
read end is closed (the thread dropped `_pipe_read_fd` on exit), which returns
`EPIPE`. `signal_shutdown()` silently ignores this with `let _ = ...`, so the
effect is invisible. Still, a reader of the code may not immediately understand why
the write succeeds on the first call and silently fails on the second.

**Fix:** Restructure the best-effort cleanup to skip `signal_shutdown()` for the
`TransportLost` arm (which already handled it), or add an inline comment:
```rust
// TransportLost already called signal_shutdown() + join-await above;
// this best-effort path covers ShellExited and ClientClosed only.
// The write is harmless for TransportLost (EPIPE, silently ignored) but
// avoids the need for an early-return here.
reader_handle.signal_shutdown();
```

---

### IN-02: D-04 test drops `sess` (master PTY) before reader thread exits, violating the stated safety invariant

**File:** `crates/nosh-server/src/pty_io.rs:248`

**Issue:** The safety comment in `unix_reader_loop` states: "master_raw_fd: owned
by the session slot (lives until slot is dropped, which only happens AFTER this
thread exits per D-03 teardown ordering)." The D-04 test calls `drop(sess)` at
line 248 before the reader thread is known to have exited. After `drop(sess)`, the
`MasterPty` is closed, so `master_raw_fd` is a stale integer referring to a closed
file descriptor. The `unsafe` `BorrowedFd::borrow_raw(master_raw_fd)` inside the
thread is then accessing an invalid fd.

In practice this is harmless because `poll()` on a closed fd returns `POLLNVAL`,
`fds[0].any()` returns `true`, `Read::read()` fails with `EBADF`/`EIO`, and the
thread breaks cleanly. But the test's comments say it drops the session "so the
PTY slave-side fd is closed" — which is imprecise. `drop(sess)` closes the
**master** PTY fd (and consequently causes SIGHUP on the slave side), not just the
slave fd. The test intentionally violates the production invariant and should say
so explicitly.

**Fix:** Update the comment to be precise:
```rust
// Drop sess to close the MASTER PTY (which also causes the slave side to
// receive SIGHUP). This intentionally violates the production safety
// invariant (master fd outlives the thread) to create a race with the
// shutdown signal — the reader should exit cleanly via either path.
drop(sess);
```

---

### IN-03: D-04 test exercises PTY-EOF path more than shutdown-pipe path

**File:** `crates/nosh-server/src/pty_io.rs:248-265`

**Issue:** The test drops `sess` (closing the master PTY) then immediately signals
shutdown and awaits the join. Dropping `sess` causes `POLLHUP` / `EIO` on the
master fd — the reader thread will almost always exit via the EOF branch before the
shutdown byte even arrives. As a result, the test proves "reader exits on PTY
close" more than it proves "reader exits on shutdown pipe signal." The test title
(`reader_exits_on_shutdown_barrier`) and the D-04 designation imply the shutdown
pipe is what's under test, but the actual signal may never be observed by the
thread.

A more targeted D-04 test would keep the PTY alive (hold `sess` open) and verify
that the shutdown pipe alone causes the reader to exit.

**Fix:** Keep `sess` and `_writer` alive for the duration of the per-cycle shutdown
test, and rely only on `signal_shutdown()` to cause exit:
```rust
// Don't drop sess here — keep master PTY open so the reader is only
// woken by the shutdown pipe, not by PTY EOF.
let _sess_alive = sess;  // kept alive for the duration of the spawn + join
let _writer_alive = _writer;
// ... signal shutdown, await join, then let _sess_alive drop at end of scope.
```

---

_Reviewed: 2026-06-01T00:00:00Z_
_Reviewer: Claude (gsd-code-reviewer)_
_Depth: deep_
