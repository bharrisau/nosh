# Phase 10: PTY Reader Race Fix - Pattern Map

**Mapped:** 2026-06-01
**Files analyzed:** 4 modified + 1 new
**Analogs found:** 4 / 5 (new `pty_io.rs` has no direct analog — see No Analog Found section)

---

## File Classification

| New/Modified File | Role | Data Flow | Closest Analog | Match Quality |
|-------------------|------|-----------|----------------|---------------|
| `crates/nosh-server/src/pty_io.rs` | utility/trait | event-driven (fd poll + blocking-thread bridge) | `crates/nosh-server/src/server.rs` lines 360-373 (existing spawn_blocking reader loop) | role-partial (analog is the thing being replaced) |
| `crates/nosh-server/src/server.rs` | service/pump | request-response + event-driven | self (TransportLost arm W2 fix at line 555-573 is the analog for the new reader-await pattern) | exact |
| `crates/nosh-server/src/session.rs` | model | CRUD | self (existing `try_clone_reader`, `sighup`, `take_child` methods) | exact |
| `crates/nosh-server/src/registry.rs` | service | CRUD | self (slot state machine; `orphan` / `take_pty_writer` / `return_pty_writer`) | exact |
| `crates/nosh-server/src/lib.rs` | config | N/A | self | exact |

---

## Pattern Assignments

### `crates/nosh-server/src/pty_io.rs` (NEW — utility, event-driven)

**Analog:** No existing module. Closest behavioral analog is the existing `spawn_blocking` reader loop in `server.rs:360-373` — but that is the code being replaced, not copied. The D-02 trait boundary is a new concept.

**The existing reader loop being replaced** (`server.rs:360-373`):
```rust
let output_reader = tokio::task::spawn_blocking(move || {
    let mut buf = [0u8; PTY_CHUNK];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break, // PTY EOF: shell closed.
            Ok(n) => {
                if out_tx.blocking_send(buf[..n].to_vec()).is_err() {
                    break; // receiver gone — stop reading.
                }
            }
            Err(_) => break,
        }
    }
});
```

**What replaces it (self-pipe pattern from RESEARCH.md — VERIFIED against nix 0.29.0 source):**

The new module exposes:
- A `PtyReaderHandle` struct holding the `JoinHandle` (for `await`) and the `shutdown_tx: OwnedFd` (write end of the self-pipe).
- A `start_interruptible_reader(master_raw_fd: i32, reader: PtyReader, out_tx: mpsc::Sender<Vec<u8>>) -> anyhow::Result<PtyReaderHandle>` function.
- The blocking loop uses `nix::poll::poll()` on `[master_fd, pipe_read_fd]` before each `Read::read()` call. Shutdown fires when async code writes one byte to `pipe_write_fd`.

**Imports pattern to follow** (from `server.rs:14-27` and `session.rs:14-19` for style):
```rust
// Style: explicit use items, no glob imports, crate:: prefix for local modules
use std::io::Read;
use std::os::fd::{AsRawFd, BorrowedFd, OwnedFd};
use anyhow::Context as _;
use nix::poll::{poll, PollFd, PollFlags, PollTimeout};
use nix::unistd::{pipe, write};
use tokio::sync::mpsc;
use crate::session::PtyReader;
```

**Module declaration pattern** (from `lib.rs:4-6`):
```rust
// lib.rs currently:
pub mod registry;
pub mod server;
pub mod session;
// Add:
pub mod pty_io;
```

**Cargo.toml feature addition** (from `nosh-server/Cargo.toml` line 31 — current state):
```toml
# Current:
nix = { version = "0.29", default-features = false, features = ["signal", "user"] }
# Change to:
nix = { version = "0.29", default-features = false, features = ["signal", "user", "poll"] }
```

---

### `crates/nosh-server/src/server.rs` (MODIFIED — service/pump, request-response)

**Analog:** The same file's W2 writer-handback await pattern (TransportLost arm) at lines 555-573 is the EXACT precedent for the new reader-await pattern.

**W2 writer-handback await pattern** (`server.rs:555-573`) — THE CANONICAL ANALOG:
```rust
SessionEnd::TransportLost => {
    tracing::info!("transport lost; orphaning session (PTY kept alive, no SIGHUP)");

    // W2 fix: the input task stores the writer back into the slot on
    // exit. The `drop(in_tx)` above unblocks it; AWAIT its completion so
    // the writer is guaranteed to be in the slot BEFORE we orphan — no
    // racy 200 ms timeout that could leave the orphan writer-less and
    // permanently un-reattachable. We bound the await generously in case
    // the task is blocked inside a PTY write; on the rare timeout the
    // slot may lack a writer, and a later reattach will cleanly reject
    // (take_pty_writer None → re-orphan) rather than wedge.
    let _ = tokio::time::timeout(Duration::from_secs(5), &mut input_writer).await;

    // Transition the slot to Orphaned; the registry enforces the cap.
    registry.orphan(&slot);
    ...
}
```

**Pattern to MIRROR for the reader (D-03):** After `signal_shutdown()`, add an analogous bounded await BEFORE `registry.orphan()`:
```rust
// NEW — mirrors the W2 writer await pattern exactly:
reader_handle.signal_shutdown();
let _ = tokio::time::timeout(Duration::from_secs(5), reader_handle.join).await;
// Now orphan — reader is guaranteed to have exited.
let _ = tokio::time::timeout(Duration::from_secs(5), &mut input_writer).await;
registry.orphan(&slot);
```

**Output pump spawn site** (`server.rs:358-373`) — site 1 of 2, to be replaced:
```rust
// OUTPUT pump: a blocking thread reads PTY output and forwards chunks; an
// async task drains them into PtyData frames on the stream.
let (out_tx, mut out_rx) = mpsc::channel::<Vec<u8>>(64);
let mut reader = reader;
let output_reader = tokio::task::spawn_blocking(move || {
    let mut buf = [0u8; PTY_CHUNK];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if out_tx.blocking_send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
            Err(_) => break,
        }
    }
});
```

**Output pump spawn site 2 of 2** (`run_reattach_session`, `server.rs:747-762`) — identical pattern, same replacement:
```rust
let output_reader = tokio::task::spawn_blocking(move || {
    let mut buf = [0u8; PTY_CHUNK];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if out_tx.blocking_send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
            Err(_) => break,
        }
    }
});
```

**Input writer loop** (`server.rs:385-396`) — LEFT UNTOUCHED (D-03a). Confirmed: `in_rx.blocking_recv()` returns `None` when `in_tx` is dropped, so this loop is already interruptible. Do not regress this:
```rust
let mut input_writer = tokio::task::spawn_blocking(move || {
    let mut writer = writer_for_task;
    while let Some(bytes) = in_rx.blocking_recv() {
        if writer.write_all(&bytes).is_err() || writer.flush().is_err() {
            break;
        }
    }
    // Hand the writer back to the slot unconditionally.
    slot_for_writer.return_pty_writer(writer);
});
```

**BUG to remove** (`server.rs:607`):
```rust
// REMOVE — abort() has no effect on an executing spawn_blocking task:
output_reader.abort();
input_writer.abort();
```

**BUG to remove** (`server.rs:879`):
```rust
// REMOVE — same bug in run_reattach_session:
output_reader.abort();
input_writer.abort();
```

**Master raw fd extraction pattern** (from RESEARCH.md §Pitfall 2 — lock briefly, release before spawn):
```rust
// Extract the raw fd value while holding the lock, then release before spawning.
// The raw fd number is an i32 — no lifetime constraint once copied.
let master_raw_fd: i32 = {
    let guard = slot.session.lock().unwrap();
    guard.master_raw_fd().expect("master fd must be available")
};
// Lock released here. Pass master_raw_fd (i32) to start_interruptible_reader.
```

**mpsc channel pattern** (`server.rs:358, 382, 747, 767`) — channel capacity 64, `mpsc::channel` (not `sync_channel`):
```rust
let (out_tx, mut out_rx) = mpsc::channel::<Vec<u8>>(64);
let (in_tx, mut in_rx) = mpsc::channel::<Vec<u8>>(64);
```

---

### `crates/nosh-server/src/session.rs` (MODIFIED — model, CRUD)

**Analog:** The existing `try_clone_reader` method at line 167 and the `sighup` method at line 173 are the pattern for the new `master_raw_fd()` accessor.

**Existing method pattern to follow for new accessor** (`session.rs:133-180`):
```rust
// Pattern: brief self method, delegates to master field, handles errors via anyhow or Option
pub fn child_pid(&self) -> Option<u32> {
    self.child_pid
}

pub fn resize(&self, cols: u16, rows: u16) -> anyhow::Result<()> {
    self.master
        .resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
        .context("resize pty")
}

pub fn try_clone_reader(&self) -> anyhow::Result<PtyReader> {
    self.master.try_clone_reader().context("clone pty reader for reattach")
}

pub fn sighup(&self) {
    if let Some(pid) = self.child_pid {
        let _ = nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(pid as i32),
            nix::sys::signal::Signal::SIGHUP,
        );
    }
}
```

**New method to add** (follows the same pattern — `#[cfg(unix)]` gating, `Option<i32>` return):
```rust
/// The PTY master fd as a raw integer, for `nix::poll` use in the interruptible
/// reader. Returns `None` if the MasterPty does not expose a raw fd (non-Unix).
/// CALLER: copy the value out while holding the session lock, then release the
/// lock BEFORE passing to spawn_blocking (never hold the lock across poll/await).
#[cfg(unix)]
pub fn master_raw_fd(&self) -> Option<i32> {
    use std::os::unix::io::AsRawFd as _;
    self.master.as_raw_fd()
}
```

**Private field** (`session.rs:123`): `master: Box<dyn MasterPty + Send>` is private — the new public `master_raw_fd()` method is the only way to expose it. Do not make `master` public.

**SessionSlot delegation pattern** (`registry.rs:287-292`) — the slot's `clone_pty_reader` delegates to `session.lock().try_clone_reader()`. Mirror this for `master_raw_fd`:
```rust
// registry.rs existing delegation:
pub fn clone_pty_reader(&self) -> anyhow::Result<crate::session::PtyReader> {
    self.session
        .lock()
        .unwrap()
        .try_clone_reader()
}

// Add to SessionSlot — same pattern:
#[cfg(unix)]
pub fn master_raw_fd(&self) -> Option<i32> {
    self.session.lock().unwrap().master_raw_fd()
}
```

---

### `crates/nosh-server/src/registry.rs` (MODIFIED — service, CRUD)

**Analog:** The existing `take_pty_writer` / `return_pty_writer` / `orphan` methods are the pattern for any slot state bookkeeping. No changes are needed to the registry's core logic — the reader fix is in `server.rs` and `pty_io.rs`. The only addition is the `master_raw_fd` delegation on `SessionSlot` (see above).

**Slot state machine pattern** (`registry.rs:272-319`) — brief Mutex locks, never across `.await`:
```rust
pub fn take_pty_writer(&self) -> Option<crate::session::PtyWriter> {
    self.pty_writer.lock().unwrap().take()
}

pub fn return_pty_writer(&self, w: crate::session::PtyWriter) {
    *self.pty_writer.lock().unwrap() = Some(w);
}

pub fn mark_orphaned(&self) {
    *self.last_active.lock().unwrap() = Instant::now();
    *self.state.lock().unwrap() = SlotState::Orphaned;
}
```

**Orphan call site** (`server.rs:572-573`) — the registry invariant: `registry.orphan(&slot)` is called AFTER the reader AND writer tasks have exited. The new reader-await must be inserted BEFORE this call (same position as the existing `input_writer` await at line 570).

---

## Shared Patterns

### Blocking-thread handback (W2 — the LOAD-BEARING ANALOG for D-03)

**Source:** `crates/nosh-server/src/server.rs` lines 555-573 (TransportLost arm, both `run_session` and `run_reattach_session`)

**Apply to:** Both `run_session` and `run_reattach_session` TransportLost teardown arms.

The pattern is: "signal shutdown → bounded await completion → orphan". The existing W2 writer-handback is exactly this pattern for the writer. The new reader shutdown follows it identically:

```rust
// Signal shutdown first (analogous to drop(in_tx)):
reader_handle.signal_shutdown();
// Bounded await (analogous to the existing input_writer await):
let _ = tokio::time::timeout(Duration::from_secs(5), reader_handle.join).await;
// Writer await (existing, unchanged):
let _ = tokio::time::timeout(Duration::from_secs(5), &mut input_writer).await;
// Only now orphan (slot guaranteed to have writer, reader guaranteed to have exited):
registry.orphan(&slot);
```

### Brief Mutex lock, never across `.await`

**Source:** `crates/nosh-server/src/registry.rs` (all Mutex methods), `session.rs` (all public methods)

**Apply to:** All new `Session::master_raw_fd()` and `SessionSlot::master_raw_fd()` calls — extract the `i32` value under the lock, release the lock, then pass the value into async or spawn_blocking contexts.

### `#[cfg(unix)]` gating for Unix-specific APIs

**Source:** `crates/nosh-server/src/session.rs` (implied by nix dependency); portable-pty's `as_raw_fd()` is `#[cfg(unix)]` in the upstream trait.

**Apply to:** `Session::master_raw_fd()`, `SessionSlot::master_raw_fd()`, the entire `pty_io.rs` module (or its Unix impl block), and any `PtyReaderHandle::signal_shutdown()` that calls `nix::unistd::write`.

### Test harness pattern for session-spanning assertions

**Source:** `crates/nosh-server/src/registry.rs` lines 1141-1215 (`exited_orphan_removed_via_real_taken_child_path`) and `crates/nosh-client/tests/persistence.rs` lines 136-271 (`transport_loss_orphans_without_sighup`)

**Apply to:** D-04 completion-barrier test.

The established pattern:
- `#[tokio::test]` attribute
- Guard with `if !std::path::Path::new("/bin/sh").exists() { return; }`
- Bounded polling loop: `let deadline = std::time::Instant::now() + Duration::from_secs(5); loop { if <condition> { break; } if Instant::now() > deadline { panic!("..."); } tokio::time::sleep(Duration::from_millis(20)).await; }`
- For unit tests (D-04): `Arc<AtomicUsize>` as the completion counter; `session::open` + `session::lookup_self` for real PTY (same as `open_sh_session` helper in registry tests at line 895)

```rust
// Helper pattern from registry.rs lines 889-900:
fn open_sh_session(key: NoshPublicKey) -> crate::session::Session {
    use crate::session;
    let passwd = session::lookup_self(Some("/bin/sh"));
    let (sess, _reader, _writer) =
        session::open(&passwd, "xterm", 80, 24, &[], key).expect("open /bin/sh");
    sess
}
```

---

## No Analog Found

| File | Role | Data Flow | Reason |
|------|------|-----------|--------|
| `crates/nosh-server/src/pty_io.rs` | utility/trait | event-driven (fd poll) | No module in the codebase encapsulates an interruptible blocking-thread reader. The closest is the spawn_blocking reader loop in server.rs — but that is exactly what is being replaced, not a pattern to copy. RESEARCH.md §Pattern 1 (self-pipe) and §InterruptiblePtyReader trait are the authoritative patterns to implement from. |

---

## Metadata

**Analog search scope:** `crates/nosh-server/src/` (all `.rs` files), `crates/nosh-client/tests/` (integration test patterns)
**Files scanned:** `server.rs` (full; 909 lines), `session.rs` (full; 347 lines), `registry.rs` (full; 1577 lines, read first 1310 + remainder), `lib.rs` (full; 8 lines), `persistence.rs` (full; 272 lines)
**Pattern extraction date:** 2026-06-01
