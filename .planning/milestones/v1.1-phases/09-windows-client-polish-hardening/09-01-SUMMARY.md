---
phase: 09-windows-client-polish-hardening
plan: "01"
subsystem: nosh-client
tags: [windows, console, vt-input, escape, ux]
dependency_graph:
  requires: []
  provides: [WIN-02-vt-input, escape-machine]
  affects: [crates/nosh-client]
tech_stack:
  added: [windows-sys 0.59]
  patterns: [RAII-guard, state-machine, cfg-windows]
key_files:
  created: []
  modified:
    - Cargo.toml
    - crates/nosh-client/Cargo.toml
    - crates/nosh-client/src/client.rs
    - crates/nosh-client/src/main.rs
decisions:
  - "windows-sys 0.59 added to workspace.dependencies (not inline); matches existing ssh-agent-client-rs target-dep pattern"
  - "EscapeState holds per-byte state across read() calls to maintain line-start correctly across partial reads"
  - "Server PtyData bypasses EscapeState entirely — written directly to stdout — per T-09-01 threat model"
  - "RawModeGuard struct carries #[cfg(windows)] fields; non-Windows builds compile to effectively unit-struct cost"
metrics:
  duration_minutes: 20
  completed_date: "2026-05-30T07:21:14Z"
  tasks_completed: 2
  files_changed: 4
---

# Phase 9 Plan 01: Windows VT Console-Mode + ~. Escape Summary

Windows console-input fix (ENABLE_VIRTUAL_TERMINAL_INPUT on stdin) and ssh-style `~.` local-quit escape for all platforms.

## Tasks Completed

### Task 1: Windows VT console-mode handling in RawModeGuard

Added `windows-sys 0.59` (workspace dep, nosh-client target.cfg(windows) dep) and extended `RawModeGuard`:

- Struct gains `#[cfg(windows)] orig_stdin_mode: u32` and `orig_stdout_mode: u32` fields.
- `enable()`: after crossterm `enable_raw_mode`, on Windows: `GetConsoleMode` + `SetConsoleMode` on stdin to add `ENABLE_VIRTUAL_TERMINAL_INPUT (0x0200)` and clear `ENABLE_PROCESSED_INPUT | ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT`; then `GetConsoleMode` + `SetConsoleMode` on stdout to add `ENABLE_VIRTUAL_TERMINAL_PROCESSING (0x0004)`.
- Error path: if any Win32 call fails, previously-set modes are restored before returning the error.
- `Drop`: restores both original modes via `SetConsoleMode` BEFORE calling `disable_raw_mode`, so panic-unwind also restores the console (T-09-02 mitigation).
- Non-Windows: struct is effectively empty, existing behavior unchanged.

Root cause addressed (STATE.md 2026-05-30): crossterm's `enable_raw_mode` on Windows clears line/echo/processed-input but does NOT set `ENABLE_VIRTUAL_TERMINAL_INPUT`, causing vim/less/arrows/PageUp-Down to break and Ctrl-C to terminate the process (exit 130) instead of forwarding as 0x03.

### Task 2: EscapeState machine in the stdin pump

Added `EscapeState` enum (LineStart/SeenTilde/MidLine) in main.rs implementing the OpenSSH client escape protocol:

- `~.` at line-start → `PumpOutcome::UserQuit`, zero bytes forwarded
- `~~` at line-start → one literal `~` forwarded, transitions to MidLine
- `~` followed by any other byte at line-start → `~` + byte forwarded, state updated per byte
- Mid-line `~` → forwarded literally with no escape semantics
- `\n` transitions to LineStart; session start is LineStart

State is persisted across `stdin.read()` calls so line-start is maintained correctly for multi-chunk reads. The machine is fed **only** local stdin bytes; server `PtyData` goes directly to stdout, never through `EscapeState` (T-09-01 security invariant: a malicious server cannot inject `~.` to force local disconnect).

Added `long_about` to the clap command documenting `~.` (disconnect) and `~~` (literal tilde).

**Unit tests**: 7 tests in `escape_tests` module — all pass on Linux.

## Verification

### Validated on Linux
- `cargo build --workspace`: passes (Windows-gated code compiles via `#[cfg]`)
- `cargo test --bin nosh-client escape`: 7/7 passed
- `grep -c ENABLE_VIRTUAL_TERMINAL_INPUT crates/nosh-client/src/client.rs`: 1 (inside `#[cfg(windows)]`)
- Server PtyData path provably separate: written directly to stdout in the `read_message` arm, not through EscapeState

### Requires Windows Host
- ENABLE_VIRTUAL_TERMINAL_INPUT active: vim/less/arrows/PageUp-Down work correctly
- Ctrl-C forwarded as 0x03, not exit 130
- ENABLE_VIRTUAL_TERMINAL_PROCESSING: server ANSI renders correctly
- Console restored on all exit paths (normal, error, panic-unwind)
- `~.` at line-start disconnects without sending bytes; `~~` sends one `~`

## Deviations from Plan

None — plan executed exactly as written. Named constants used throughout (ENABLE_VIRTUAL_TERMINAL_INPUT, ENABLE_PROCESSED_INPUT, etc.) per plan-checker advisory.

## Self-Check: PASSED
- `crates/nosh-client/src/client.rs`: FOUND
- `crates/nosh-client/src/main.rs`: FOUND (contains EscapeState)
- Commit eb2659b: FOUND
