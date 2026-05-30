---
phase: 09-windows-client-polish-hardening
fixed: 2026-05-30
status: all_fixed
findings_in_scope: 4
fixed: 4
skipped: 0
iteration: 1
---

# Phase 9 Code Review Fix Report

Fixed the Critical + Warning findings from `09-REVIEW.md`, plus IN-03 (trivial security-relevant log hygiene). IN-01 and IN-02 skipped (informational; behavior matches OpenSSH / acceptable).

## Fixes Applied

### CR-01 (BLOCKER) — `~.` escape unreachable after first Enter — commit `2d4db1e`
`crates/nosh-client/src/main.rs`. The `EscapeState` machine treated only `b'\n'` as line-start, but raw mode (ICRNL disabled) delivers `b'\r'` on Enter — so after the first command the machine was stuck in `MidLine` and `~.` never fired. Changed all three transition sites (lines ~120, ~144, ~154) to `matches!(byte, b'\n' | b'\r')`, mirroring OpenSSH's last-was-CR tracking. Added two regression tests: `carriage_return_resets_to_line_start_enabling_escape` (the missed case) and `carriage_return_mid_line_tilde_dot_is_literal`. **9 escape tests pass** (was 7).

### WR-01 — unvalidated console handles in `RawModeGuard::drop` — commit `edb77b8`
`crates/nosh-client/src/client.rs`. `GetStdHandle` results are now checked against `INVALID_HANDLE_VALUE` and `0` (HANDLE is `isize` in windows-sys, so NULL is `== 0`) before `SetConsoleMode`, so console-mode restoration is skipped cleanly if the console detached rather than calling into an invalid handle.

### WR-02 — ambiguous `0x0004` flag comment — commit `edb77b8`
`crates/nosh-client/src/client.rs`. Flag comments split into labelled "Stdin handle flags" / "Stdout handle flags" sections noting that `0x0004` is a different named constant on each handle (`ENABLE_ECHO_INPUT` vs `ENABLE_VIRTUAL_TERMINAL_PROCESSING`). Code uses the named windows-sys constants throughout.

### IN-03 — warn log could disclose full line — commit `83c6186`
`crates/nosh-auth/src/keys.rs`. A no-whitespace malformed `authorized_keys` line could be logged in full as the `key_type` field. Now caps the token: tokens longer than 64 chars are replaced with `"<malformed-no-whitespace>"` before logging. Preserves the D-07 no-secret-logging invariant.

## Test Results
- `cargo test --bin nosh-client escape`: 9 passed (2 new `\r` tests)
- `cargo test -p nosh-auth authorized_keys`: 3 passed
- `cargo test --workspace`: 82 passed, 0 failed, 3 ignored

## Cross-platform note
WR-01's Windows console-handle guard cannot be compiled on this Linux host (no mingw); verified by inspection against windows-sys type definitions (`HANDLE = isize`). Full Windows confirmation remains a human-host item, consistent with the rest of the phase's Windows-runtime acceptance criteria.
