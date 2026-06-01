---
plan: 10-01
phase: 10-pty-reader-race-fix
status: complete
completed: 2026-06-01
commits:
  - 883ad58
  - 231d464
---

# Plan 10-01: Interruptible PTY Reader Foundation

## What Was Built

### pty_io API (exact signatures — Plan 02 wires against these)

```rust
// crates/nosh-server/src/pty_io.rs

pub struct PtyReaderHandle {
    pub join: JoinHandle<()>,               // await this to wait for reader exit
    #[cfg(unix)] shutdown_tx: OwnedFd,      // write-end of self-pipe (private)
}

impl PtyReaderHandle {
    #[cfg(unix)] pub fn signal_shutdown(&self);             // writes 1 byte via nix::unistd::write
    #[cfg(unix)] pub async fn shutdown_and_join(mut self);  // signal + .await join
}

#[cfg(unix)]
pub fn start_interruptible_reader(
    master_raw_fd: i32,
    reader: crate::session::PtyReader,
    out_tx: tokio::sync::mpsc::Sender<Vec<u8>>,
) -> anyhow::Result<PtyReaderHandle>
```

### Session/Registry accessors

```rust
// crates/nosh-server/src/session.rs (new method on Session):
#[cfg(unix)]
pub fn master_raw_fd(&self) -> Option<i32>   // delegates to self.master.as_raw_fd()

// crates/nosh-server/src/registry.rs (new method on SessionSlot):
#[cfg(unix)]
pub fn master_raw_fd(&self) -> Option<i32>   // acquires session Mutex briefly, calls Session::master_raw_fd()
```

### Teardown pattern for Plan 02 (D-03)

```rust
// In both TransportLost arms (BEFORE registry.orphan):
reader_handle.signal_shutdown();
let _ = tokio::time::timeout(Duration::from_secs(5), &mut reader_handle.join).await;
// Then the existing input_writer await:
let _ = tokio::time::timeout(Duration::from_secs(5), &mut input_writer).await;
registry.orphan(&slot);
```

### Master raw fd extraction pattern (Pitfall 2 — brief lock)

```rust
let master_raw_fd = slot.master_raw_fd().expect("Unix master fd available");
// slot.master_raw_fd() acquires and releases the session lock internally
```

## Key Files Created/Modified

- `crates/nosh-server/src/pty_io.rs` (new, 289 lines)
- `crates/nosh-server/src/session.rs` (master_raw_fd accessor added)
- `crates/nosh-server/src/registry.rs` (SessionSlot::master_raw_fd added)
- `crates/nosh-server/src/lib.rs` (pub mod pty_io declared)
- `crates/nosh-server/Cargo.toml` (nix "poll" feature enabled)

## Acceptance Criteria Verification

- [x] `cargo build -p nosh-server` exits 0
- [x] nix `"poll"` feature present in Cargo.toml
- [x] `Session::master_raw_fd(&self) -> Option<i32>` — `#[cfg(unix)]`, master field private (`grep -c 'pub master'` == 0)
- [x] `SessionSlot::master_raw_fd` exists, `#[cfg(unix)]`, brief lock, no .await inside
- [x] `pub mod pty_io;` in lib.rs
- [x] `pty_io.rs` min_lines: 289 (> 60 required)
- [x] `PtyReaderHandle` present with `signal_shutdown` + `shutdown_and_join` + `pub join`
- [x] No `O_NONBLOCK`/`F_SETFL` in pty_io.rs (code level)
- [x] No master fd close in pty_io.rs
- [x] nix::poll and nix::unistd::pipe both used
- [x] PollFds in same stack frame as poll() call (Pitfall 5)
- [x] D-04 test: AtomicUsize exit_count == N, /bin/sh guard, no RuntimeMetrics

## Deviations from Plan

None — plan executed as written.

One compile fix required (not a deviation): initial `signal_shutdown` called `nix::unistd::write(self.shutdown_tx.as_raw_fd(), b"x")` using `as_raw_fd()`, but `nix::unistd::write` expects `AsFd`. Fixed to `nix::unistd::write(&self.shutdown_tx, b"x")` which uses `OwnedFd: AsFd` directly.

## Self-Check: PASSED
