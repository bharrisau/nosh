//! Interruptible PTY reader — D-02 trait boundary + Unix self-pipe implementation.
//!
//! Replaces the un-interruptible `spawn_blocking` read loop (Pitfall 6: `abort()`
//! on an executing `spawn_blocking` task is a no-op). The blocking read thread now
//! polls `[master_fd, shutdown_pipe_read_fd]` via `nix::poll`; async teardown code
//! writes one byte to the pipe write-end to wake the thread and cause a clean exit.
//!
//! # Safety contract
//! - The reader receives the PTY master fd **as a copied `i32`** only — it never
//!   gains ownership and never closes it. Closing the master fd would SIGHUP the
//!   orphaned shell (Pitfall 7 / T-10-02).
//! - No `O_NONBLOCK` / `F_SETFL` is set anywhere here (D-01 / T-10-03).
//! - Each pipe end is owned by exactly one `OwnedFd` (Pitfall 4 / T-10-01); both
//!   close automatically on drop via the OS.
//! - `PollFd`s are constructed in the **same stack frame** as the `poll()` call so
//!   the `BorrowedFd` lifetimes are always valid (Pitfall 5).

use crate::session::PtyReader;
use anyhow::Context as _;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// Buffer size for PTY reads — matches `server::PTY_CHUNK` (8 KiB).
const PTY_CHUNK: usize = 8 * 1024;

// ─── Public handle ────────────────────────────────────────────────────────────

/// A handle to the running interruptible PTY reader task.
///
/// Returned by [`start_interruptible_reader`]. Holds the task's `JoinHandle` and
/// the write-end of the shutdown self-pipe.
///
/// # Teardown protocol (D-03)
/// Mirror the W2 writer-handback pattern from `server.rs`:
/// 1. Call [`PtyReaderHandle::signal_shutdown`] to wake the blocked `poll()`.
/// 2. Await the join handle via [`PtyReaderHandle::shutdown_and_join`] (or with
///    `tokio::time::timeout`) so the reader thread is **guaranteed to have exited**
///    before the session slot is orphaned — prevents a second live reader on the
///    same master fd after reattach (Pitfall 3).
pub struct PtyReaderHandle {
    /// The reader thread's join handle.
    ///
    /// Exposed as a public field so the caller can apply
    /// `tokio::time::timeout(.., &mut handle.join)` inline, mirroring the
    /// `&mut input_writer` pattern in the W2 writer-handback await.
    pub join: JoinHandle<()>,
    /// Write end of the self-pipe. Writing one byte wakes the blocked `poll()`.
    #[cfg(unix)]
    shutdown_tx: std::os::fd::OwnedFd,
}

impl PtyReaderHandle {
    /// Write one byte to the shutdown self-pipe.
    ///
    /// The blocking reader thread polls the pipe read-end alongside the PTY master
    /// fd; this byte makes the pipe readable, causing the thread to break its loop
    /// and exit cleanly within one `poll()` interval.
    ///
    /// Errors (e.g. `EBADF` if the reader already exited) are silently ignored —
    /// the purpose is best-effort wakeup.
    #[cfg(unix)]
    pub fn signal_shutdown(&self) {
        let _ = nix::unistd::write(&self.shutdown_tx, b"x");
    }

    /// Signal shutdown and await the reader thread's clean exit.
    ///
    /// Equivalent to `signal_shutdown()` followed by `join.await`. For a bounded
    /// wait, use `tokio::time::timeout` around this or around `&mut self.join`
    /// directly (the `join` field is public).
    #[cfg(unix)]
    pub async fn shutdown_and_join(mut self) {
        self.signal_shutdown();
        let _ = (&mut self.join).await;
    }
}

// ─── Unix implementation ──────────────────────────────────────────────────────

/// Start an interruptible PTY reader thread.
///
/// Spawns a `tokio::task::spawn_blocking` thread that:
/// 1. Polls `[master_raw_fd, shutdown_pipe_read_fd]` via `nix::poll`.
/// 2. On PTY readable: reads a chunk and forwards it via `out_tx.blocking_send`.
/// 3. On PTY EOF, send error, read error, or shutdown pipe readable: exits cleanly.
///
/// Returns a [`PtyReaderHandle`] whose `signal_shutdown` + `join` enable
/// deterministic teardown (D-03).
///
/// # Arguments
/// * `master_raw_fd` — The PTY master fd **as a copied `i32`**. The caller MUST
///   have copied this value out from under the session lock and released the lock
///   before calling this function (Pitfall 2). This function never closes the fd.
/// * `reader` — A `Box<dyn Read + Send>` cloned from the PTY master via
///   `session.try_clone_reader()`. Owned by the spawned blocking thread.
/// * `out_tx` — The async channel sender that the session pump's `out_rx` drains.
#[cfg(unix)]
pub fn start_interruptible_reader(
    master_raw_fd: i32,
    reader: PtyReader,
    out_tx: mpsc::Sender<Vec<u8>>,
) -> anyhow::Result<PtyReaderHandle> {
    use std::os::fd::AsRawFd as _;
    use std::os::fd::OwnedFd;

    // Create the self-pipe: read_fd goes to the blocking thread, write_fd stays
    // in the handle. Both are `OwnedFd` — single owner each side, drop-closes
    // automatically, no manual `libc::close` (T-10-01 / Pitfall 4).
    let (read_fd, write_fd): (OwnedFd, OwnedFd) =
        nix::unistd::pipe().context("create shutdown self-pipe")?;

    let pipe_raw = read_fd.as_raw_fd();

    // Move `read_fd` and `reader` into the blocking closure; `write_fd` stays in the handle.
    let join = tokio::task::spawn_blocking(move || {
        unix_reader_loop(master_raw_fd, pipe_raw, read_fd, reader, out_tx);
    });

    Ok(PtyReaderHandle { join, shutdown_tx: write_fd })
}

/// The core blocking reader loop for Unix.
///
/// Polls `[master_raw_fd, pipe_raw_fd]` before each `Read::read` call.
/// Exits cleanly on: shutdown pipe readable, PTY EOF, send error, or read error.
///
/// **Pitfall 5:** `PollFd`s are constructed *inside* the loop in the same stack
/// frame as `nix::poll::poll()`. The `BorrowedFd` lifetimes are tied to that
/// stack frame — they never outlive the `OwnedFd`s that back them.
#[cfg(unix)]
fn unix_reader_loop(
    master_raw_fd: i32,
    pipe_raw_fd: i32,
    _pipe_read_fd: std::os::fd::OwnedFd, // owned to keep the fd open; dropped on exit
    mut reader: PtyReader,
    out_tx: mpsc::Sender<Vec<u8>>,
) {
    use std::io::Read as _;
    use std::os::fd::BorrowedFd;
    use nix::poll::{poll, PollFd, PollFlags, PollTimeout};

    let mut buf = [0u8; PTY_CHUNK];

    loop {
        // Build PollFds in this stack frame — lifetimes tied here (Pitfall 5).
        // SAFETY: the raw fds are valid for the lifetime of this call:
        //   - master_raw_fd: owned by the session slot (lives until slot is dropped,
        //     which only happens AFTER this thread exits per D-03 teardown ordering).
        //   - pipe_raw_fd: backed by `_pipe_read_fd: OwnedFd` in this scope.
        let master_pfd = unsafe {
            PollFd::new(BorrowedFd::borrow_raw(master_raw_fd), PollFlags::POLLIN)
        };
        let pipe_pfd = unsafe {
            PollFd::new(BorrowedFd::borrow_raw(pipe_raw_fd), PollFlags::POLLIN)
        };
        let mut fds = [master_pfd, pipe_pfd];

        match poll(&mut fds, PollTimeout::NONE) {
            Err(nix::errno::Errno::EINTR) => continue, // signal interrupted poll(); retry
            Err(_) => break,  // genuine poll error (EBADF, EFAULT, ENOMEM) → exit
            Ok(0) => continue, // spurious wakeup (impossible with NONE, but safe to skip)
            Ok(_) => {}
        }

        // Check shutdown pipe first — priority over PTY data.
        if fds[1].any().unwrap_or(false) {
            break; // shutdown signalled
        }

        // PTY master readable — attempt a read.
        if fds[0].any().unwrap_or(false) {
            match reader.read(&mut buf) {
                Ok(0) => break, // PTY EOF: shell closed
                Ok(n) => {
                    if out_tx.blocking_send(buf[..n].to_vec()).is_err() {
                        break; // receiver gone — stop reading
                    }
                }
                Err(_) => break, // read error
            }
        }
    }
    // `reader` and `_pipe_read_fd` drop here — no fd leak.
}

// ─── Non-Unix stub ────────────────────────────────────────────────────────────

/// Non-Unix stub — the trait boundary (D-02) is expressed even where the Unix
/// implementation is absent. A Windows/ConPTY implementation will replace this
/// placeholder at Phase 17 / M6.
#[cfg(not(unix))]
pub fn start_interruptible_reader(
    _master_raw_fd: i32,
    _reader: PtyReader,
    _out_tx: mpsc::Sender<Vec<u8>>,
) -> anyhow::Result<PtyReaderHandle> {
    anyhow::bail!("interruptible PTY reader is not yet implemented on this platform")
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(all(unix, test))]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    /// D-04 completion-barrier test: N create→orphan cycles must each exit the
    /// reader thread deterministically.
    ///
    /// PRIMARY assertion: `exit_count == N` — every reader thread exited.
    /// SECONDARY (safety net only, not the pass criterion): the barrier is reached
    /// within a generous 5 s wall-clock bound; on timeout the test panics with
    /// the partial count (D-04a: no `RuntimeMetrics`, no purely time-based pass).
    #[tokio::test]
    async fn reader_exits_on_shutdown_barrier() {
        if !std::path::Path::new("/bin/sh").exists() {
            eprintln!("skipping reader_exits_on_shutdown_barrier: /bin/sh not available");
            return;
        }

        const N: usize = 10;
        let exit_count = Arc::new(AtomicUsize::new(0));

        for _ in 0..N {
            // Open a real /bin/sh PTY session.
            let passwd = crate::session::lookup_self(Some("/bin/sh"));
            let identity = nosh_auth::NoshPublicKey::from_raw([0x42u8; 32]);
            let (sess, reader, _writer) =
                crate::session::open(&passwd, "xterm", 80, 24, &[], identity)
                    .expect("session::open /bin/sh");

            // Extract the master raw fd while holding the session struct (no Mutex
            // here — we own sess directly in the test).
            let master_raw_fd = sess.master_raw_fd()
                .expect("Unix master fd must be available");

            // Channel: use a small buffer; drain not required (reader may block_send
            // briefly before shutdown, but buffer absorbs that).
            let (out_tx, mut out_rx) = mpsc::channel::<Vec<u8>>(64);

            // Start the interruptible reader.
            let handle = start_interruptible_reader(master_raw_fd, reader, out_tx)
                .expect("start_interruptible_reader");

            // Drop the session and writer so the PTY slave-side fd is closed (this
            // also avoids a PTY EOF race interfering with the shutdown test).
            drop(sess);

            // Spawn a task that awaits the join and increments the counter.
            let counter = exit_count.clone();
            let join = handle.join;
            let write_fd = handle.shutdown_tx;
            tokio::spawn(async move {
                // Signal shutdown, then await the reader thread.
                let _ = nix::unistd::write(&write_fd, b"x");
                let _ = join.await;
                counter.fetch_add(1, Ordering::Release);
            });

            // Drain out_rx to unblock any blocking_send in the reader.
            tokio::spawn(async move {
                while out_rx.recv().await.is_some() {}
            });
        }

        // Secondary safety net: wait up to 5 s for all reader threads to exit.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if exit_count.load(Ordering::Acquire) == N {
                break;
            }
            if Instant::now() > deadline {
                panic!(
                    "reader_exits_on_shutdown_barrier: only {}/{N} readers exited within 5 s",
                    exit_count.load(Ordering::Acquire)
                );
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        // PRIMARY assertion (D-04): every reader thread exited.
        assert_eq!(
            exit_count.load(Ordering::Acquire),
            N,
            "all {N} reader threads must have exited after signal_shutdown"
        );
    }
}
