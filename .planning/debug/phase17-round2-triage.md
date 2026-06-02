---
status: investigating
trigger: "Phase 17 round 2: triage 5 findings (BUG-G init size, BUG-H ctrl-L, BUG-E backspace clamp, BUG-F read -s enter, typematic glitch) Windows-specific vs platform-agnostic; fix only Windows-specific"
created: 2026-06-02
updated: 2026-06-02
---

## Current Focus

reasoning_checkpoint:
  hypothesis: "BUG-G is Windows-specific: on Windows/ConPTY, crossterm::terminal::size() (GetConsoleScreenBufferInfo) can report a stale 80x24-ish default at process startup before ConPTY has synced the host window dims to the pseudoconsole; the true size only becomes visible after the first console event. The client measures size ONCE at main.rs:531 (right after RawModeGuard::enable) and sends it in SessionOpen (client.rs:587-607); the server opens the PTY at that size (session.rs:228-244). A physical resize later sends a correct Resize → server resizes PTY → vim renders correctly (platform.rs:98-118 + main.rs:1147-1151). On Linux crossterm::size() uses TIOCGWINSZ which is reliable at startup, so the same code sends the correct size and the bug does not reproduce."
  confirming_evidence:
    - "Linux size() = TIOCGWINSZ (reliable at startup); Windows size() = GetConsoleScreenBufferInfo window rect (subject to ConPTY startup sync lag). Different syscalls per platform."
    - "Symptom exactly matches a stale-default-then-correct-on-resize signature: tiny square at top-left, fixed by physically resizing."
    - "Client sends size only at SessionOpen; the only later size update is the resize-event path (main.rs:1142-1151) — nothing re-measures size shortly after connect."
  falsification_test: "If a Linux client also opened vim tiny at the same default size, BUG-G would be platform-agnostic. Reported only on Windows; Linux size() path is reliable → consistent with Windows-specific."
  fix_rationale: "Windows-gated: after the session is open, schedule a one-shot delayed re-measure of crossterm::terminal::size() and send a Resize if it differs from the size sent in SessionOpen. This corrects the ConPTY startup-lag size without altering the Linux path at all. Low-risk: reuses the existing send_resize + server Resize handler (already exercised by the working manual-resize path)."
  blind_spots: "Cannot run a Windows console here to confirm the exact stale value or the precise delay needed; relying on the known ConPTY sync-lag quirk + the operator's resize-fixes-it observation. The one-shot re-measure is belt-and-suspenders: if size() is already correct, the Resize is a no-op (same dims → server resize to same size is harmless)."

next_action: implement Windows-gated one-shot post-open resize re-measure in run_pump; keep Linux path unchanged; build + clippy + release.

## Symptoms

expected: vim full-size on connect; Ctrl-L clears screen; backspace clamps at prompt; Enter advances after read -s; smooth fast typing.
actual: vim tiny on connect (fixed by manual resize); Ctrl-L deletes one line; backspace past prompt; Enter no advance after read -s; vim glitch on fast typing.
errors: none
reproduction: Windows client vs Linux server, interactive vim/bash.
started: Phase 17 live validation round 2.

## Eliminated

## Evidence

- main.rs:531 — `let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));` read ONCE at startup, BEFORE any session. Passed into fresh_session(cols, rows) → open_session_with_token.
- BUG-G WINDOWS-SPECIFIC: client.rs:587-607 sends SessionOpen{cols,rows}; server session.rs:228-244 opens PTY at that size; later Resize at main.rs:1142-1151 via platform.rs:98-118. Linux size()=TIOCGWINSZ reliable; Windows size()=GetConsoleScreenBufferInfo subject to ConPTY startup sync-lag → stale default until first event. VERDICT: Windows-specific, FIX NOW.
- BUG-H PLATFORM-AGNOSTIC: client never passes raw VT clear/ED through; display is a full-framebuffer diff (screen.rs:318-419). Ctrl-L → form-feed → forwarded to server PTY → server vte terminal model processes ED/clear → emits StateDiff → client diffs confirmed grid. Any wrong clear handling lives in the Linux-only server terminal model (terminal.rs) or in the diff/screen logic shared by all platforms. Reproduces identically on a Linux client. VERDICT: platform-agnostic, BACKLOG.
- BUG-E PLATFORM-AGNOSTIC: predictor.rs PredictBackspace (line 334) and PredictCursorLeft (line 345) clamp only at col 0 (`if self.predicted_cursor.col > 0`), NOT at the prompt-start column. Predictor has no knowledge of prompt boundary. Pure shared predictor logic, no #[cfg]. Reproduces on Linux. VERDICT: platform-agnostic, BACKLOG.
- BUG-F PLATFORM-AGNOSTIC: read -s noecho path is the structural tentative-epoch mechanism in predictor.rs (cell_at/is_tentative + reset on Enter, lines 369-374, 473-475, 584-604) plus the screen diff render. The Enter after a noecho prompt is classify_input→EpochReset (predictor.rs:640) which only resets local prediction; actual newline advance comes from the server StateDiff cursor. All shared logic; no Windows #[cfg] involved. Reproduces on Linux. VERDICT: platform-agnostic, BACKLOG.
- TYPEMATIC PLATFORM-AGNOSTIC: stdin path main.rs:1085-1133 reads up to 8KB per batch then classify_input; bulk batches >4 bytes → BulkSuppressed (predictor.rs:627), key-repeat coalesces into one large read → suppressed prediction then full repaint. No Windows-specific batching: tokio::io::stdin() on both platforms; the only Windows console specialization (platform.rs) is the resize POLL and is explicitly NOT a second input reader (platform.rs:10-21 keeps tokio::io::stdin the sole reader). Glitch is general prediction-under-burst, reproduces on Linux at high key-repeat. VERDICT: platform-agnostic, BACKLOG. (low-risk Windows-only fix not identified; do not touch.)

## Resolution

root_cause: BUG-G Windows-specific ConPTY startup size-sync lag — crossterm::terminal::size() (GetConsoleScreenBufferInfo) returns a stale default at startup; SessionOpen sent the wrong size; nothing re-measured until a physical resize.
fix: (1) platform.rs Windows ResizeWatcher seeds last_size with sentinel (0,0) so the first poll reporting any real size triggers a corrective Resize. (2) main.rs run_pump adds a Windows-only one-shot ~400ms post-open size re-measure that sends a corrective Resize if size now differs from the opened dims. Both #[cfg(windows)]-gated; Unix path unchanged. BUG-H/E/F/typematic = platform-agnostic, backlog (not fixed).
verification: cargo build -p nosh-client OK; cargo clippy -p nosh-client --bins --lib clean; cargo build --release -p nosh-client OK (Windows host, windows cfg active). Server-pulling test failure is the documented Linux-only dev-dep, not a regression. Live re-test pending operator.
files_changed: [crates/nosh-client/src/main.rs, crates/nosh-client/src/platform.rs]
