//! Platform-specific terminal resize trigger and quit signal abstraction.
//!
//! All `#[cfg]` gates for platform behavior live here (plus `main.rs` auth
//! selection) — per the Phase 8 constraint: "ALL platform `#[cfg]` gates
//! confined to nosh-client."
//!
//! ## Resize triggers
//!
//! - **Unix:** `SIGWINCH` via `tokio::signal::unix::{signal, SignalKind}`.
//! - **Windows:** `crossterm::event::EventStream` watching for
//!   `crossterm::event::Event::Resize(_,_)` (Windows console resize events,
//!   not SIGWINCH which does not exist on Windows). The EventStream may also
//!   deliver key events — those are ignored here; keystroke input stays on
//!   `tokio::io::stdin()` in the main pump loop.
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

    #[cfg(windows)]
    stream: crossterm::event::EventStream,
    /// Set to `true` once `EventStream` is permanently exhausted (e.g. the
    /// console handle was closed). When `true`, `next_resize()` parks forever
    /// via `std::future::pending()` instead of returning immediately, which
    /// would cause a resize-message flood in the `tokio::select!` pump loop.
    #[cfg(windows)]
    stream_done: bool,
}

impl ResizeWatcher {
    /// Create a new `ResizeWatcher`.
    ///
    /// On Unix: installs a SIGWINCH signal handler via `tokio::signal::unix`.
    /// On Windows: creates a `crossterm::event::EventStream`.
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
            Ok(Self {
                stream: crossterm::event::EventStream::new(),
                stream_done: false,
            })
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
            // If the stream was previously exhausted, park forever. Returning
            // immediately would cause a resize-message flood in the pump loop's
            // tokio::select! (the arm fires on every iteration once exhausted).
            if self.stream_done {
                std::future::pending::<()>().await;
                return;
            }
            use futures::StreamExt;
            // Loop until we see a Resize event; ignore all other events
            // (key events etc. are handled via tokio::io::stdin in run_pump).
            while let Some(ev) = self.stream.next().await {
                if matches!(ev, Ok(crossterm::event::Event::Resize(_, _))) {
                    return;
                }
            }
            // Stream permanently exhausted (console handle closed or unrecoverable
            // error). Mark done and park so the pump loop does not spin.
            self.stream_done = true;
            std::future::pending::<()>().await;
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
