//! Platform-specific terminal resize trigger and quit signal abstraction.
//!
//! All `#[cfg]` gates for platform behavior live here (plus `main.rs` auth
//! selection) — per the Phase 8 constraint: "ALL platform `#[cfg]` gates
//! confined to nosh-client."
//!
//! ## Resize triggers
//!
//! - **Unix:** `SIGWINCH` via `tokio::signal::unix::{signal, SignalKind}`.
//! - **Windows:** polls `crossterm::terminal::size()` on a ~300 ms interval and
//!   reports a resize when the dimensions change. We deliberately do NOT use
//!   `crossterm::event::EventStream` here: EventStream calls `ReadConsoleInput`,
//!   which DRAINS console input records — and the keystroke path
//!   (`tokio::io::stdin()` in the main pump loop) reads the SAME console input
//!   handle. Two concurrent readers split the input queue and corrupt multi-byte
//!   VT sequences: a cursor-position-report reply (`ESC [ row ; col R`) gets
//!   torn so the trailing `R` reaches the remote shell as a bare keystroke (vim
//!   → REPLACE mode), and arrow/function-key escapes (`ESC [ A` …) break.
//!   `terminal::size()` queries `GetConsoleScreenBufferInfo` and never touches
//!   the input queue, so `tokio::io::stdin()` stays the SOLE console reader and
//!   receives intact VT byte sequences.
//!
//! After `next_resize()` returns the CALLER re-reads the authoritative terminal
//! dimensions via `crossterm::terminal::size()`. **Do NOT trust the
//! width/height fields inside the `Event::Resize` event** (Pitfall 14: on
//! Windows the event may lag the real console size; re-reading is the safe
//! approach on both platforms).
//!
//! ## Quit signal
//!
//! `quit_signal()` is a cross-platform future that resolves when the user
//! explicitly requests exit (Ctrl-C / SIGINT). It is used by the reconnect
//! supervisor to break the retry loop when the user presses Ctrl-C while
//! a reconnect is in progress. During an active session Ctrl-C is forwarded
//! as the byte 0x03 to the remote shell (normal shell behavior) — this does
//! not conflict because `quit_signal()` is only polled during the reconnect
//! backoff wait.

/// Platform-abstracted terminal resize watcher.
///
/// Create with [`ResizeWatcher::new`] and await [`ResizeWatcher::next_resize`]
/// in the pump loop's `tokio::select!`. After `next_resize()` returns, re-read
/// `crossterm::terminal::size()` for the authoritative dimensions (Pitfall 14).
pub struct ResizeWatcher {
    #[cfg(unix)]
    signal: tokio::signal::unix::Signal,

    /// Interval timer that paces `terminal::size()` polling (Windows).
    #[cfg(windows)]
    poll: tokio::time::Interval,
    /// Last observed `(cols, rows)`; a resize is reported only when this changes.
    #[cfg(windows)]
    last_size: (u16, u16),
}

impl ResizeWatcher {
    /// Create a new `ResizeWatcher`.
    ///
    /// On Unix: installs a SIGWINCH signal handler via `tokio::signal::unix`.
    /// On Windows: starts a ~300 ms `terminal::size()` poll (NOT EventStream —
    /// see the module docs for why a second console-input reader is unsafe).
    pub fn new() -> anyhow::Result<Self> {
        #[cfg(unix)]
        {
            use anyhow::Context;
            use tokio::signal::unix::{signal, SignalKind};
            let signal = signal(SignalKind::window_change())
                .context("install SIGWINCH handler")?;
            Ok(Self { signal })
        }

        #[cfg(windows)]
        {
            // BUG-G (Windows ConPTY startup size-sync lag): do NOT seed last_size from
            // the startup `terminal::size()` reading. On Windows that first reading can
            // be a stale default (≈80×24) before ConPTY has synced the real window dims
            // (GetConsoleScreenBufferInfo lags the host until the first console event).
            // Seeding last_size with the stale value means the poll would only fire once
            // the OS reports a DIFFERENT size — which may never happen without a physical
            // resize, so the wrong initial size would persist (the reported symptom).
            //
            // Instead seed with a sentinel `(0, 0)` that can never equal a real terminal
            // size. The first poll that reads a plausible size therefore reports a resize,
            // and the pump's resize-debounce path re-reads the authoritative size and sends
            // a corrective `Resize`. Paired with the one-shot post-open re-measure in
            // run_pump, this self-heals the startup size without a user resize.
            let last_size = (0u16, 0u16);
            let mut poll =
                tokio::time::interval(std::time::Duration::from_millis(300));
            // Skip (don't burst) if we fall behind; first tick fires immediately
            // but next_resize() only reports an ACTUAL change vs last_size.
            poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            Ok(Self { poll, last_size })
        }

        // Compile-time check: if neither unix nor windows, this block is empty.
        // Unsupported platform — produce a compile error with a clear message.
        #[cfg(not(any(unix, windows)))]
        {
            compile_error!(
                "ResizeWatcher: unsupported platform — only unix and windows are supported"
            );
        }
    }

    /// Await the next terminal resize event.
    ///
    /// Returns `()` when a resize has been detected. The caller MUST then
    /// re-read `crossterm::terminal::size()` to obtain the authoritative
    /// terminal dimensions (Pitfall 14: do NOT use event fields directly).
    pub async fn next_resize(&mut self) {
        #[cfg(unix)]
        {
            // SIGWINCH fired — resize detected.
            let _ = self.signal.recv().await;
        }

        #[cfg(windows)]
        {
            // Poll terminal::size() (GetConsoleScreenBufferInfo) — never the
            // console INPUT queue — so we don't compete with tokio::io::stdin
            // for keystroke bytes. Report only on an actual dimension change.
            loop {
                self.poll.tick().await;
                let cur = crossterm::terminal::size().unwrap_or(self.last_size);
                if cur != self.last_size {
                    self.last_size = cur;
                    return;
                }
            }
        }
    }
}

/// A future that resolves when the user explicitly requests quit (Ctrl-C /
/// SIGINT). Cross-platform via `tokio::signal::ctrl_c`.
///
/// Used by the reconnect supervisor to break the retry loop. During an active
/// session Ctrl-C is forwarded as 0x03 to the shell — these do not conflict
/// because this future is only polled during the reconnect backoff wait, not
/// during session I/O.
pub async fn quit_signal() {
    let _ = tokio::signal::ctrl_c().await;
}
