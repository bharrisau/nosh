# Phase 10: PTY Reader Race Fix — Research

**Researched:** 2026-06-01
**Domain:** Rust async runtime / Unix PTY / blocking-thread interruption
**Confidence:** HIGH

---

<user_constraints>
## User Constraints (from CONTEXT.md)

### Locked Decisions

- **D-01:** Unix implementation uses **`AsyncFd` + `O_NONBLOCK`** on the PTY master fd — drive PTY reads from async via `tokio::io::unix::AsyncFd` so there is no blocking thread at all for the output reader.
- **D-01a (fallback):** If master fd is not reachable / O_NONBLOCK cannot be set safely, fall back to **signal-fd interrupt**: the reader thread `poll()`/`select()`s on `[pty_master_fd, shutdown_pipe_fd]` and async code writes one byte to the shutdown pipe to wake it.
- **D-02:** Implement the interrupt behind a **small trait / abstraction boundary** (an "interruptible PTY reader") so the Windows ConPTY server (Phase 17 / M6) can slot in its own implementation without reworking the session pump.
- **D-02a:** `AsyncFd` is a Unix-only Tokio primitive — it does NOT compile on Windows. Cross-platform support comes from D-02's trait boundary. M4 ships only the Unix impl.
- **D-03:** Fix the **output reader** interrupt AND the **reattach reader-clone path** (`session.rs:167` `try_clone_reader`): ensure a reattach gets a fresh reader while the orphaned session's prior reader has cleanly exited. Closes the PTY master fd not closed on reattach (old reader clone still live) leak.
- **D-03a:** The **input writer** (`server.rs:385`) needs no change — it is already interruptible. Confirm this remains true; do not regress the W2 writer-handback fix.
- **D-04:** Prove success criterion #2 with a **completion-barrier test**: loop N create→orphan cycles and assert every output-reader exits within one polling interval via a shared counter / `oneshot` signal. Deterministic, no unstable build flags.
- **D-04a:** Do NOT depend on `tokio_unstable` `RuntimeMetrics` or a purely time-based "exits within 1s" probe as the primary assertion.

### Claude's Discretion

- Exact trait shape / module placement for the interruptible-reader boundary.
- Buffer sizes, channel capacities, and how the async read task integrates with the existing `out_tx`/`out_rx` plumbing and the `tokio::select!` session loop.
- Test harness mechanics (how N orphan cycles are driven; whether a dedicated test PTY/shell stub is used) — provided the assertion is the deterministic completion barrier of D-04.

### Deferred Ideas (OUT OF SCOPE)

- Native Windows/ConPTY interruptible-reader implementation — Phase 17 / M6. This phase only defines the trait boundary; the Windows impl is out of scope.
- Datagram/state-sync emission from the PTY output callsite — Phase 12/13. This phase touches the same callsite but adds no datagram behavior.
</user_constraints>

---

## Summary

This phase fixes a latent resource-leak bug in `nosh-server`: every orphaned session leaves a `spawn_blocking` PTY reader thread permanently blocked in `read()`. `tokio::task::JoinHandle::abort()` has no effect on an executing `spawn_blocking` task (documented in tokio's API), so the current `output_reader.abort()` call at server.rs:607 and server.rs:879 does nothing. Under the default `idle_timeout=0` every orphaned session accumulates one permanently-stuck blocking thread; at ~512 orphans (tokio's default pool limit) the server becomes unresponsive to new `spawn_blocking` calls.

The fix is to make the blocking read interruptible. The locked decision (D-01) is to use `tokio::io::unix::AsyncFd` + `O_NONBLOCK` to drive reads from async, eliminating the blocking thread entirely. **The critical D-01a feasibility question — does `portable-pty` 0.9.0 expose the master fd? — is ANSWERED: YES.** `MasterPty::as_raw_fd() -> Option<unix::RawFd>` is a trait method on the `MasterPty` trait (defined in `portable_pty/src/lib.rs` line 114) and implemented in `UnixMasterPty::as_raw_fd()` returning `Some(self.fd.0.as_raw_fd())`. However, there is an **O_NONBLOCK side-effect constraint** that the planner MUST account for (see Pitfall 1 below).

Because `O_NONBLOCK` on a dup-shared file description affects all readers and writers sharing that PTY master fd — including the blocking writer in `spawn_blocking` — the **self-pipe fallback (D-01a) is equally feasible and avoids that constraint entirely**. The research documents both paths concretely so the planner can choose the one that minimizes code change while satisfying D-01's intent.

**Primary recommendation:** Implement the self-pipe / `nix::poll` path (HARDEN-01 spec mechanism). It is simpler, avoids O_NONBLOCK side-effects on the writer, requires only adding `"poll"` to the nix feature list, and eliminates the zombie without touching the writer path. If strictly zero blocking threads is required, the AsyncFd path is feasible with the O_NONBLOCK writer fix documented below.

---

## Architectural Responsibility Map

| Capability | Primary Tier | Secondary Tier | Rationale |
|------------|-------------|----------------|-----------|
| PTY read interrupt signal (Unix) | Server — blocking thread (`spawn_blocking`) | Async task (signals via pipe write) | The blocking thread owns the read loop; the async side only needs to send one byte to wake it |
| AsyncFd read loop (alternative) | Server — async task | — | If AsyncFd path chosen, read loop moves entirely out of `spawn_blocking` into a spawned async task |
| Trait abstraction boundary | Server — new module (e.g. `crates/nosh-server/src/pty_io.rs`) | — | D-02 requires the abstraction be at the server crate level, not in session.rs or registry.rs |
| Orphan teardown coordination | Server — `run_session` / `run_reattach_session` TransportLost arm | Registry (`orphan()`) | The shutdown signal must be sent AFTER `in_tx` is dropped and the input task has handed back the writer |
| Test completion barrier | `crates/nosh-server/src/` unit test | — | D-04 test is a unit test (not integration), using `Arc<AtomicUsize>` or `oneshot` channels |

---

## Standard Stack

### Core (already in project — no new crates needed for self-pipe path)

| Library | Version | Purpose | Why Standard |
|---------|---------|---------|--------------|
| `nix` | 0.29 (existing) | `nix::unistd::pipe()`, `nix::poll::poll()`, `nix::unistd::write()` for self-pipe | Already in nosh-server; only needs `"poll"` feature added |
| `tokio` | 1.52.x (existing) | `AsyncFd` (if D-01 AsyncFd path chosen) | Already in workspace; `AsyncFd` gated on `cfg(all(unix, feature = "net"))` which is satisfied |
| `portable-pty` | 0.9.0 (existing) | `MasterPty::as_raw_fd() -> Option<RawFd>` | Confirmed in source; exposes the master fd for both approaches |

### Feature changes required

**Self-pipe path:** Add `"poll"` to nix features in `crates/nosh-server/Cargo.toml`:

```toml
nix = { version = "0.29", default-features = false, features = ["signal", "user", "poll"] }
```

`nix::unistd::pipe()` is NOT feature-gated — it is always available. `nix::poll::poll()` requires the `"poll"` feature. [VERIFIED: portable-pty 0.9.0 source]

**AsyncFd path:** No new features needed. `tokio::io::unix::AsyncFd` is already enabled by `feature = "net"` in workspace Cargo.toml. [VERIFIED: tokio 1.52.3 source, `cfg_net_unix!` macro gates `AsyncFd` on `cfg(all(unix, feature = "net"))`]

---

## Package Legitimacy Audit

> No new packages are introduced by this phase. The only change is adding a feature flag to an existing dependency (`nix`). Package legitimacy gate: N/A.

| Package | Change | Disposition |
|---------|--------|-------------|
| `nix` 0.29 (existing) | Add `"poll"` feature | No new package — feature-only addition. Approved. |

---

## D-01a Feasibility Determination (THE LOAD-BEARING ANSWER)

**VERDICT: AsyncFd + O_NONBLOCK is FEASIBLE but carries an O_NONBLOCK side-effect on the writer. Self-pipe is equally feasible and avoids this. Both paths are viable.**

### Evidence

**`portable-pty` 0.9.0 master fd exposure:**
[VERIFIED: portable-pty 0.9.0 source at `~/.cargo/registry/src/.../portable-pty-0.9.0/src/lib.rs:114`]

```rust
// In MasterPty trait (lib.rs):
#[cfg(unix)]
fn as_raw_fd(&self) -> Option<unix::RawFd>;

// UnixMasterPty implementation (unix.rs:366):
fn as_raw_fd(&self) -> Option<RawFd> {
    Some(self.fd.0.as_raw_fd())
}
```

The master fd IS accessible via `session.master.as_raw_fd()` (through the `Session::master: Box<dyn MasterPty + Send>` field inside the `SessionSlot::session: Mutex<Session>`).

**The reader (`Box<dyn Read + Send>`) does NOT expose AsRawFd:**
`try_clone_reader()` returns `Box<dyn Read + Send>` — a trait object that erases the concrete `PtyFd` type. [VERIFIED: portable-pty 0.9.0 source] This means the reader handle itself cannot be passed to `AsyncFd::new()` directly. The master fd from `as_raw_fd()` must be used instead.

**O_NONBLOCK is a file-description property shared by all dup'd fds:**
[ASSUMED — standard POSIX semantics, not re-verified from a specific document in this session]
`try_clone_reader()` calls `self.fd.try_clone()` which internally uses `F_DUPFD_CLOEXEC` (a dup syscall). The writer (`UnixMasterWriter`) also uses a dup of the same fd. All dup-derived fds share the same open file description. Setting `O_NONBLOCK` via `fcntl(F_SETFL)` on any one of them changes it for all. Setting `O_NONBLOCK` on the master fd therefore makes the writer's `write_all()` potentially return `EAGAIN/EWOULDBLOCK`, breaking the current blocking writer loop.

**Consequence for D-01 (AsyncFd path):** If AsyncFd + O_NONBLOCK is chosen, the writer (`spawn_blocking` loop in `server.rs:385-396`) MUST be updated to handle `EAGAIN` (e.g., retry with `std::thread::park` or move the writer to async too). This is a larger change than D-01 implies.

**Consequence for D-01a (self-pipe path):** `nix::unistd::pipe()` creates a fresh pipe with two independent fds. `nix::poll::poll()` on `[master_fd, pipe_read_fd]` waits for either to be readable. The master fd is accessed via `session.master.as_raw_fd()` through the session mutex (brief lock, no await). Writing one byte to `pipe_write_fd` from async wakes the blocked poll. No O_NONBLOCK is set anywhere; the writer path is unchanged.

---

## Architecture Patterns

### System Architecture Diagram

```
Async task (run_session / run_reattach_session)
│
├── [D-01a SELF-PIPE PATH]
│   ├── creates ShutdownPipe { read_fd: OwnedFd, write_fd: OwnedFd }
│   ├── passes read_fd + master_raw_fd to spawn_blocking reader
│   ├── on TransportLost: writes 1 byte to write_fd  ──────────────┐
│   │                                                               │
│   └─────────────────────────────────────────────────────────────▼
│       spawn_blocking reader loop:
│           nix::poll(&mut [PollFd(master_fd, POLLIN),
│                           PollFd(pipe_read_fd, POLLIN)], NONE)
│           if PTY readable → read → send to out_tx
│           if pipe readable → break (clean exit)
│
├── [D-01 ASYNCFD PATH — alternative]
│   ├── gets master_raw_fd via slot.session.lock().master.as_raw_fd()
│   ├── sets O_NONBLOCK via fcntl(F_SETFL, O_NONBLOCK)  [affects writer!]
│   ├── creates AsyncFd<OwnedRawFdRef> wrapping master_raw_fd
│   ├── spawns async task (NOT spawn_blocking):
│   │       loop {
│   │           async_fd.readable().await?
│   │           match async_fd.try_io(|fd| syscall::read(fd.as_raw_fd(), ...)) { ... }
│   │       }
│   └── cancels task by dropping the JoinHandle (natural async cancellation)
│
└── [D-02 TRAIT BOUNDARY]
    InterruptiblePtyReader trait (Unix impl / Windows impl placeholder):
    ├── start_reading(out_tx: Sender<Vec<u8>>) -> ReaderHandle
    └── ReaderHandle::shutdown() — sends interrupt signal

Output pump (both paths):
    out_tx → out_rx (mpsc channel, capacity 64, unchanged)
    async task drains out_rx → PtyData frames → QUIC stream  [unchanged]

Input pump (unchanged — writer-handback W2 fix preserved):
    in_tx → in_rx → spawn_blocking writer
    drop(in_tx) on any session end → blocking_recv() returns None → writer exits
    writer stores self back to slot on exit (W2 fix)
```

### Recommended Project Structure

```
crates/nosh-server/src/
├── lib.rs              # exports: pub mod pty_io (new)
├── server.rs           # calls pty_io to create reader (change output pump sites x2)
├── session.rs          # Session::try_clone_reader (confirm API; no functional change)
├── registry.rs         # unchanged (slot::orphan already correct)
└── pty_io.rs           # NEW: InterruptiblePtyReader trait + Unix impl
```

`pty_io.rs` is the D-02 trait boundary module. It is added to lib.rs as `pub mod pty_io`. The Windows impl is a placeholder `todo!()` or a compile-time stub.

### Pattern 1: Self-Pipe Interrupt (Recommended for D-01a)

**What:** A Unix pipe pair whose read end is passed to the blocking reader thread alongside the PTY master fd. The reader uses `nix::poll()` to block on both. Async code writes one byte to the write end to wake and exit the thread.

**When to use:** Primary implementation for Phase 10 on Linux. Avoids O_NONBLOCK side-effects on the writer.

```rust
// Source: VERIFIED — nix 0.29.0 source at ~/.cargo/registry/.../nix-0.29.0/src/poll.rs
// and ~/.cargo/registry/.../nix-0.29.0/src/unistd.rs

use std::os::fd::{AsRawFd, OwnedFd};
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::unistd::{pipe, write};

fn interruptible_pty_reader_loop(
    master_raw_fd: i32,       // from session.master.as_raw_fd().unwrap()
    pipe_read_fd: OwnedFd,    // read end of the shutdown pipe
    out_tx: mpsc::SyncSender<Vec<u8>>,
    mut reader: Box<dyn Read + Send>,  // the actual read handle (still used for Read::read)
) {
    let mut buf = [0u8; 8 * 1024];
    let pipe_raw = pipe_read_fd.as_raw_fd();
    loop {
        // SAFETY: BorrowedFd lifetimes are 'a tied to this stack frame; poll does not close them.
        let master_pfd = unsafe {
            PollFd::new(BorrowedFd::borrow_raw(master_raw_fd), PollFlags::POLLIN)
        };
        let pipe_pfd = unsafe {
            PollFd::new(BorrowedFd::borrow_raw(pipe_raw), PollFlags::POLLIN)
        };
        let mut fds = [master_pfd, pipe_pfd];
        match poll(&mut fds, PollTimeout::NONE) {
            Err(_) => break,  // EINTR or other error → exit
            Ok(0) => continue, // spurious wakeup (should not happen with NONE timeout)
            Ok(_) => {}
        }
        // Check shutdown pipe first
        if fds[1].any().unwrap_or(false) {
            break;  // shutdown signal received
        }
        // PTY readable
        if fds[0].any().unwrap_or(false) {
            match reader.read(&mut buf) {
                Ok(0) => break,  // PTY EOF
                Ok(n) => {
                    if out_tx.send(buf[..n].to_vec()).is_err() {
                        break;  // receiver dropped
                    }
                }
                Err(_) => break,
            }
        }
    }
    // reader and pipe_read_fd dropped here — no fd leak
}

// Shutdown: from async context, write 1 byte to pipe_write_fd:
fn send_shutdown(pipe_write_fd: &OwnedFd) {
    let _ = write(pipe_write_fd.as_raw_fd(), b"x");
}
```

### Pattern 2: AsyncFd Reader (D-01 primary — conditional on writer fix)

**What:** Register the PTY master fd with tokio's I/O driver via `AsyncFd`. Reads become fully async; no `spawn_blocking`. Orphan teardown drops the `JoinHandle` (natural async cancellation).

**When to use:** When strictly zero blocking threads for the reader is required. Requires updating the writer to handle `EAGAIN`.

**Feasibility gate:** The writer (`server.rs:385-396`) uses `write_all()` in `spawn_blocking`. After setting `O_NONBLOCK` on the master fd, `write_all()` may fail with `EAGAIN` on any dup sharing the file description. The writer loop must be changed to retry on `io::ErrorKind::WouldBlock` or `EAGAIN`:

```rust
// Writer loop modification needed if O_NONBLOCK is set:
while let Some(bytes) = in_rx.blocking_recv() {
    let mut offset = 0;
    while offset < bytes.len() {
        match writer.write(&bytes[offset..]) {
            Ok(n) => offset += n,
            Err(e) if e.raw_os_error() == Some(libc::EAGAIN)
                   || e.kind() == io::ErrorKind::WouldBlock => {
                // Brief spin-wait: O_NONBLOCK writer retry.
                std::thread::yield_now();
            }
            Err(_) => return,  // break loop
        }
    }
}
```

**AsyncFd creation (wrapping a borrowed raw fd without taking ownership):**

```rust
// Source: [ASSUMED — pattern based on tokio AsyncFd docs]
// tokio::io::unix::AsyncFd requires T: AsRawFd + 'static
// To wrap a raw fd we don't own, use a struct that does NOT close on drop:
struct BorrowedFdForAsyncFd(i32);
impl AsRawFd for BorrowedFdForAsyncFd { fn as_raw_fd(&self) -> i32 { self.0 } }
// NOTE: Do NOT implement Drop to close, since the fd is owned by MasterPty.

let master_fd: i32 = slot.session.lock().unwrap().master_raw_fd(); // via as_raw_fd()
// Set O_NONBLOCK on master fd (affects all dup holders):
unsafe { libc::fcntl(master_fd, libc::F_SETFL, libc::O_NONBLOCK) };
let async_fd = tokio::io::unix::AsyncFd::new(BorrowedFdForAsyncFd(master_fd))?;
// Drive reads from async select! loop:
loop {
    let mut guard = async_fd.readable().await?;
    guard.try_io(|fd| {
        // read via libc::read or the cloned reader handle
        ...
    })?;
}
```

### Anti-Patterns to Avoid

- **Calling `output_reader.abort()` on a `spawn_blocking` JoinHandle:** Has no effect once the task is executing. The current code at server.rs:607 and server.rs:879 is the bug. Remove these calls.
- **Closing the MasterPty or master fd to interrupt reads:** This sends SIGHUP to the shell — exactly the orphan semantics violation (Pitfall #7). Never close `master` from the interrupt path.
- **Closing the reader clone fd to interrupt reads:** The self-pipe pattern (PITFALL-6 stopgap) — may cause EIO on the blocking read, but also means reattach `try_clone_reader()` may behave differently. The self-pipe is cleaner and explicit.
- **Holding the session `Mutex` across the poll/wait:** The `nix::poll()` call must be done AFTER releasing the session lock. Only the raw fd value is needed inside the blocking thread.
- **`unsafe { BorrowedFd::borrow_raw(fd) }` lifetimes:** `PollFd::new` takes `BorrowedFd<'fd>` — the lifetime must outlive the `poll()` call. Both the master fd and pipe fd are owned by the calling frame, so this is safe, but must be documented.

---

## Don't Hand-Roll

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| Polling multiple fds for readability | Custom epoll or libc select loop | `nix::poll::poll()` | Standard POSIX; nix wraps it safely with BorrowedFd lifetimes |
| Creating a shutdown pipe | Manual `libc::pipe2` call | `nix::unistd::pipe()` returns `(OwnedFd, OwnedFd)` | Already in the dependency; handles FD_CLOEXEC |
| Non-blocking fd I/O in async | Custom reactor | `tokio::io::unix::AsyncFd` | tokio's supported API for exactly this use case |

---

## Common Pitfalls

### Pitfall 1: O_NONBLOCK Side-Effect on Writer (AsyncFd Path)

**What goes wrong:** Setting `O_NONBLOCK` on the PTY master fd (or any dup of it) changes it for ALL file descriptors sharing the same open file description. The writer (`UnixMasterWriter`) uses a dup clone of the master fd. After `O_NONBLOCK` is set, `write_all()` in the blocking writer task returns `Err(EAGAIN)` when the PTY's write buffer is transiently full (e.g., during high-output shell commands), causing the writer to silently discard input.

**Why it happens:** POSIX: `O_NONBLOCK` is stored in the open file description (kernel `struct file`), not the fd number. `dup`/`dup2`/`F_DUPFD` create new fd entries pointing to the same file description. `fcntl(F_SETFL)` on any one fd changes the shared flag.

**How to avoid:** Either (a) use the self-pipe path which never sets `O_NONBLOCK`, or (b) update the writer loop to retry on `EAGAIN`/`WouldBlock` before considering it an error.

**Warning signs:** Shell input is randomly dropped under load after `O_NONBLOCK` is set; no regression test for writer-path EAGAIN.

### Pitfall 2: Accessing Master fd While Session Mutex is Held

**What goes wrong:** The master fd is accessed via `slot.session.lock().unwrap().master.as_raw_fd()`. If the lock is held while spawning the `spawn_blocking` task or while the blocking poll runs, it will deadlock (the async task that needs to send the shutdown signal may also need the session lock for resize).

**How to avoid:** Extract the raw fd value (`i32`) while holding the lock, then release the lock before spawning. The raw fd number is just an integer — no lifetime constraint once the value is copied.

```rust
let master_raw_fd: i32 = {
    let guard = slot.session.lock().unwrap();
    guard.master_raw_fd().expect("master fd must be available")
};
// Lock is released here. Pass master_raw_fd (i32) to spawn_blocking.
```

### Pitfall 3: Reader Not Exiting Before try_clone_reader() on Reattach

**What goes wrong (D-03):** On reattach, `slot.clone_pty_reader()` (which calls `try_clone_reader()`) is called at `run_reattach_session:719`. If the prior orphaned reader thread is still running (zombie), two threads are simultaneously reading from the same PTY master fd. Bytes are non-deterministically split between the two readers — the reattaching client receives garbled or missing output.

**How to avoid:** The shutdown signal MUST be sent before or during the `TransportLost` teardown, and the reattach pump MUST NOT call `clone_pty_reader()` until the prior reader has exited. Implementation options:

1. **Completion barrier:** The `InterruptiblePtyReader` trait exposes a `join()` method that the `TransportLost` arm awaits before orphaning. Only after the reader thread has confirmed exit does `registry.orphan()` store the slot in the reattachable state.
2. **Slot flag:** Store a `Arc<AtomicBool>` or `oneshot::Receiver` in the slot that the reader signals on exit; `clone_pty_reader()` checks this flag and returns an error if a prior reader is still live.

The simplest option: in the `TransportLost` arm, after sending the shutdown signal, do:
```rust
// Wait for reader thread to exit (bounded)
let _ = tokio::time::timeout(Duration::from_secs(5), output_reader).await;
// Now orphan — reader is definitely exited.
registry.orphan(&slot);
```
This is analogous to the existing `input_writer` await pattern (W2 fix at server.rs:570).

### Pitfall 4: Shutdown Pipe fd Leak on Reattach

**What goes wrong:** The self-pipe pair (`pipe_read_fd`, `pipe_write_fd`) is created per-session-pump invocation. If the `OwnedFd` values are not correctly dropped when the session ends, fds accumulate. In particular, if `pipe_write_fd` is cloned into the `JoinHandle` closure AND kept in the calling async function, there are two owners.

**How to avoid:** Structure ownership so `pipe_write_fd` is held only in the async frame (for sending the signal), and `pipe_read_fd` is moved into the `spawn_blocking` closure. When the blocking thread exits, `pipe_read_fd` drops. When the async frame returns, `pipe_write_fd` drops. Both `OwnedFd` values close automatically on drop — no explicit close needed.

### Pitfall 5: `nix::poll` PollFd Lifetime Issue

**What goes wrong:** `nix::poll::PollFd::new` takes `BorrowedFd<'fd>`. If the underlying `OwnedFd` is dropped while `PollFd` is alive (e.g., passed to a helper function that doesn't bound lifetimes correctly), the fd is closed and poll operates on a closed fd.

**How to avoid:** Create `PollFd` values in the same stack frame as the `poll()` call. The `BorrowedFd::borrow_raw(raw_fd: i32)` unsafe constructor bypasses the lifetime check — only use it if you can manually prove the fd is live for the duration of the call.

---

## Code Examples

### Creating the shutdown pipe (self-pipe path)

```rust
// Source: VERIFIED — nix 0.29.0 src/unistd.rs line 1183
use nix::unistd::pipe;
use std::os::fd::OwnedFd;

let (pipe_read_fd, pipe_write_fd): (OwnedFd, OwnedFd) = pipe()?;
// pipe_read_fd → move into spawn_blocking reader closure
// pipe_write_fd → keep in async frame for shutdown signal
```

### Sending the shutdown signal from async

```rust
use std::os::fd::AsRawFd;
use nix::unistd::write;

fn signal_reader_shutdown(pipe_write_fd: &OwnedFd) {
    // Single byte is enough to wake poll(). Error (e.g., EBADF if already closed) is ignored.
    let _ = write(pipe_write_fd.as_raw_fd(), b"x");
}
```

### nix::poll with PTY + shutdown pipe

```rust
// Source: VERIFIED — nix 0.29.0 src/poll.rs
use nix::poll::{poll, PollFd, PollFlags, PollTimeout};
use std::os::fd::BorrowedFd;

fn poll_pty_or_shutdown(master_raw_fd: i32, pipe_raw_fd: i32) -> PollResult {
    // SAFETY: both fds are live for the duration of this stack frame.
    let master_pfd = unsafe {
        PollFd::new(BorrowedFd::borrow_raw(master_raw_fd), PollFlags::POLLIN)
    };
    let pipe_pfd = unsafe {
        PollFd::new(BorrowedFd::borrow_raw(pipe_raw_fd), PollFlags::POLLIN)
    };
    let mut fds = [master_pfd, pipe_pfd];
    // PollTimeout::NONE = block indefinitely; woken by data or shutdown signal.
    poll(&mut fds, PollTimeout::NONE)
}
```

### Accessing master raw fd via Session (brief lock, then release)

```rust
// Source: VERIFIED — portable-pty 0.9.0 src/lib.rs:114 and src/unix.rs:366
// Session::master is Box<dyn MasterPty + Send>
// MasterPty::as_raw_fd() -> Option<unix::RawFd>  [#[cfg(unix)]]

let master_raw_fd: i32 = {
    let guard = slot.session.lock().unwrap();
    guard.master.as_raw_fd()
        .expect("Unix MasterPty must return a raw fd")
    // Lock released immediately after this block.
};
```

Note: `Session::master` is private (`master: Box<dyn MasterPty + Send>`). A new method `Session::master_raw_fd() -> i32` must be added to `session.rs` that locks and calls `self.master.as_raw_fd()`.

### InterruptiblePtyReader trait boundary (D-02)

```rust
// Proposed trait shape — Claude's discretion per CONTEXT.md
// Source: [ASSUMED — design recommendation]
#[cfg(unix)]
pub mod pty_io {
    use tokio::sync::mpsc;

    /// Handle to a running interruptible PTY reader task/thread.
    pub struct PtyReaderHandle {
        /// JoinHandle of the blocking thread (for awaiting clean exit — D-03).
        pub join: tokio::task::JoinHandle<()>,
        /// Shutdown signal (write end of self-pipe, or equivalent).
        shutdown_tx: std::os::fd::OwnedFd,
    }

    impl PtyReaderHandle {
        /// Send the interrupt signal. Does NOT wait for the reader to exit.
        pub fn signal_shutdown(&self) {
            let _ = nix::unistd::write(self.shutdown_tx.as_raw_fd(), b"x");
        }

        /// Signal shutdown and await clean exit (bounded by caller's timeout).
        pub async fn shutdown_and_join(self) {
            self.signal_shutdown();
            let _ = self.join.await;
        }
    }

    /// Start an interruptible PTY reader.
    ///
    /// - `master_raw_fd`: the PTY master fd (from `Session::master_raw_fd()`).
    /// - `reader`: the `Box<dyn Read + Send>` from `try_clone_reader()`.
    /// - `out_tx`: channel to send PTY output chunks.
    pub fn start_interruptible_reader(
        master_raw_fd: i32,
        reader: Box<dyn std::io::Read + Send>,
        out_tx: mpsc::Sender<Vec<u8>>,
    ) -> anyhow::Result<PtyReaderHandle> {
        let (pipe_read_fd, shutdown_tx) = nix::unistd::pipe()?;
        let join = tokio::task::spawn_blocking(move || {
            // ... poll loop as shown in Pattern 1 ...
        });
        Ok(PtyReaderHandle { join, shutdown_tx })
    }
}
```

### D-04 Deterministic Completion-Barrier Test

```rust
// Source: [ASSUMED — test design recommendation per D-04]
#[tokio::test]
async fn pty_reader_exits_on_orphan_completion_barrier() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    // Skip if /bin/sh unavailable.
    if !std::path::Path::new("/bin/sh").exists() { return; }

    const N: usize = 10;
    let exit_count = Arc::new(AtomicUsize::new(0));

    // Spawn N interruptible readers, each connected to a real /bin/sh PTY.
    let mut handles = Vec::new();
    for _ in 0..N {
        let counter = exit_count.clone();
        // Open PTY, create reader via try_clone_reader, set up shutdown pipe...
        let handle = start_interruptible_reader(...);
        // Wrap handle so it increments counter on exit:
        let counting_handle = tokio::spawn(async move {
            handle.join.await.ok();
            counter.fetch_add(1, Ordering::Release);
        });
        handles.push((handle.shutdown_tx, counting_handle));
    }

    // Send shutdown to all N readers.
    for (shutdown_tx, _) in &handles {
        // signal_shutdown via the tx end
        ...
    }

    // PRIMARY assertion: wait for all N to exit (deterministic, no time-based check).
    // The secondary time safety net (5s) is NOT the pass criterion.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if exit_count.load(Ordering::Acquire) == N { break; }
        if std::time::Instant::now() > deadline {
            panic!("completion barrier: {} of {} readers exited within 5s", 
                   exit_count.load(Ordering::Acquire), N);
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(exit_count.load(Ordering::Acquire), N, "all N readers must exit on shutdown");
}
```

---

## Runtime State Inventory

> Not applicable. This is a code fix with no rename, data migration, or runtime-state change. PTY fd handles and blocking threads are in-process ephemeral state; no external system stores them.

---

## Environment Availability

| Dependency | Required By | Available | Version | Fallback |
|------------|------------|-----------|---------|----------|
| `/bin/sh` | D-04 completion-barrier test | ✓ | system | Skip guard in test |
| `nix::poll` feature | Self-pipe reader interrupt | ✓ (after Cargo.toml change) | nix 0.29.0 | N/A |
| `tokio::io::unix::AsyncFd` | D-01 AsyncFd path | ✓ | tokio 1.52.3 (feature = "net" ✓) | N/A |
| Linux (Unix) | Both paths | ✓ | 6.8.0-117-generic | N/A — phase is Linux-only |

**Missing dependencies with no fallback:** None.

**Missing dependencies with fallback:** None.

---

## State of the Art

| Old Approach | Current Approach | When Changed | Impact |
|--------------|------------------|--------------|--------|
| `spawn_blocking` reader + `JoinHandle::abort()` | Interruptible reader with shutdown signal | Phase 10 | Eliminates zombie blocking threads on orphan |
| `output_reader.abort()` (no-op) | `reader_handle.signal_shutdown()` + await exit | Phase 10 | Reader guaranteed to exit before slot is orphaned |

**Deprecated/outdated in this phase:**
- `output_reader.abort()` call at `server.rs:607` — must be replaced with `signal_shutdown()` + await.
- `output_reader.abort()` call at `server.rs:879` (reattach path) — same.

---

## Open Questions (RESOLVED)

All three resolved by the locked decisions in `10-CONTEXT.md` (D-01..D-04). Marked here for plan-checker Dimension 11.

1. **AsyncFd vs. self-pipe as primary implementation** — **RESOLVED:** self-pipe + `nix::poll` (CONTEXT.md D-01). The user accepted the research recommendation; AsyncFd is explicitly NOT implemented this phase (its O_NONBLOCK writer side-effect is avoided). AsyncFd remains a documented future upgrade behind the D-02 trait boundary.
   - Original analysis: Both feasible. AsyncFd eliminates blocking threads but requires an O_NONBLOCK writer retry-on-EAGAIN fix; self-pipe keeps one interruptible blocking thread, writer untouched.

2. **Session::master_raw_fd() method** — **RESOLVED:** add `Session::master_raw_fd() -> Option<i32>` (through the session mutex) AND `SessionSlot::master_raw_fd()` delegating to it (CONTEXT.md D-03; mirrors the `clone_pty_reader` delegation at registry.rs:287-292). Minimum surface that avoids leaking the private `master` field.

3. **try_clone_reader() vs. master fd directly for reads** — **RESOLVED:** pass BOTH into the blocking thread — raw master fd for `nix::poll()` readability detection, `Box<dyn Read + Send>` (from `try_clone_reader()`) for the actual `Read::read()`. Preserves the EIO→EOF translation in `PtyFd::read()` and the portable-pty abstraction.

---

## Security Domain

> `security_enforcement` key absent from config — treated as enabled.

| ASVS Category | Applies | Standard Control |
|---------------|---------|-----------------|
| V2 Authentication | No | Phase 10 is server-internal, no new auth surface |
| V5 Input Validation | No | No new user input paths |
| V6 Cryptography | No | No crypto operations |
| V4 Access Control | Indirectly | Orphan cleanup must not close the master fd (no SIGHUP) — this is the existing Pitfall #7 invariant; the reader interrupt must not breach it |

### Known Threat Patterns for this fix

| Pattern | STRIDE | Standard Mitigation |
|---------|--------|---------------------|
| Shutdown pipe write triggering spurious wake → reader exits prematurely | Tampering (internal) | Reader checks WHICH fd caused poll to return; only exits on pipe_read_fd being readable, not on every wakeup |
| Double-close of pipe fd (OwnedFd + manual close) | Denial of service | Use only `OwnedFd` — do not call `libc::close()` manually; `OwnedFd` drops and closes exactly once |
| Setting O_NONBLOCK on master fd → writer discards shell input | Tampering (accidental) | Mitigation: use self-pipe path; if AsyncFd, update writer to retry on EAGAIN |

---

## Assumptions Log

| # | Claim | Section | Risk if Wrong |
|---|-------|---------|---------------|
| A1 | O_NONBLOCK is a property of the open file description, shared by all dup'd fds (standard POSIX) | D-01a Feasibility, Pitfall 1 | If per-fd instead of per-file-description, the O_NONBLOCK side-effect concern on the writer is eliminated; AsyncFd path becomes cleaner. But all POSIX implementations share this behavior — LOW risk. |
| A2 | `nix::unistd::pipe()` returns `(OwnedFd, OwnedFd)` and is not feature-gated in nix 0.29 | Standard Stack | Low risk — confirmed by reading nix 0.29 source. |
| A3 | `BorrowedFd::borrow_raw(i32)` is `unsafe` and available in Rust stable std | Code Examples (PollFd creation) | If API changed, alternative is `PollFd::new_with_raw(i32)` or similar — but `borrow_raw` has been stable since Rust 1.63. LOW risk. |
| A4 | Deterministic completion-barrier test shape (AtomicUsize counter) for D-04 | D-04 test example | Shape is Claude's discretion per CONTEXT.md; planner may choose `oneshot` channels instead. LOW risk. |

---

## Sources

### Primary (HIGH confidence)

- `portable-pty` 0.9.0 source — `~/.cargo/registry/src/.../portable-pty-0.9.0/src/lib.rs` and `src/unix.rs` — MasterPty::as_raw_fd() trait method and UnixMasterPty implementation confirmed [VERIFIED]
- `nix` 0.29.0 source — `~/.cargo/registry/src/.../nix-0.29.0/src/poll.rs` and `src/unistd.rs` — poll(), PollFd, pipe() APIs and feature gates confirmed [VERIFIED]
- `tokio` 1.52.3 source — `~/.cargo/registry/src/.../tokio-1.52.3/src/io/mod.rs` and `src/macros/cfg.rs` — AsyncFd available under `cfg(all(unix, feature = "net"))` confirmed [VERIFIED]
- nosh-server source files (`server.rs`, `session.rs`, `registry.rs`) — current bug locus, writer-handback (W2), orphan semantics [VERIFIED by direct inspection]
- tokio docs: `spawn_blocking` abort semantics — https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html [CITED]
- tokio docs: `AsyncFd` — https://docs.rs/tokio/latest/tokio/io/unix/struct.AsyncFd.html [CITED]

### Secondary (MEDIUM confidence)

- `.planning/research/PITFALLS.md` §"Pitfall 6: PTY Reader Zombie Race" — root cause analysis and candidate fixes [CITED — project research]
- `.planning/phases/10-pty-reader-race-fix/10-CONTEXT.md` — locked decisions D-01 through D-04 [CITED — project decisions]

### Tertiary (LOW confidence)

- POSIX O_NONBLOCK / open file description sharing semantics [ASSUMED — universal POSIX; not re-verified from a specific authoritative source in this session]

---

## Metadata

**Confidence breakdown:**
- D-01a feasibility (master fd exposure): HIGH — directly verified in portable-pty 0.9.0 source
- O_NONBLOCK writer side-effect: HIGH (POSIX standard; LOW risk of being wrong)
- Self-pipe API (nix): HIGH — verified in nix 0.29.0 source
- AsyncFd availability: HIGH — verified in tokio 1.52.3 source
- Test design (D-04): MEDIUM — shape is Claude's discretion; mechanics are standard AtomicUsize patterns

**Research date:** 2026-06-01
**Valid until:** 2026-07-01 (stable Rust ecosystem; tokio/nix APIs won't change within 30 days)
