# Plan 03-03 Summary: Client interactive session + headless tests

**Status:** Complete

## What was built
- `nosh-client::client` session API:
  - `RawModeGuard` (RAII): `enable()` → raw mode; `Drop` → `disable_raw_mode()` (restores on normal
    return, panic/unwind, and the abrupt-network-loss error path — SESS-03). Documented that SIGKILL
    cannot run Drop (human-verify).
  - `collect_client_env()` — SendEnv-style whitelist (`TERM`, `LANG`, `TZ`, `LC_*`); never offers
    `SSH_AUTH_SOCK`/`LD_*` (D-05).
  - `open_session`, `send_input`, `send_resize`, and the headless `run_session_collect` /
    `collect_until_close` test drivers.
- Rewrote the client binary `main.rs`: connects, enters raw mode (guard), sends `SessionOpen`
  (TERM + terminal size + whitelisted env), pumps stdin→`PtyData` (Ctrl-C passes through as `0x03`,
  SESS-06) and stream→stdout, coalesces SIGWINCH ~40 ms into one `Resize` (SESS-05), and
  `std::process::exit(exit_code)` from the `SessionClose` (SESS-08). Logs go to stderr so they don't
  corrupt the PTY stdout stream. Dropped the old echo round-trips and `--idle-hold-secs`.
- Added `crossterm` and tokio `signal`/`io-std` features to nosh-client.
- Integration tests `crates/nosh-client/tests/session.rs` (reusing the Phase 2 ephemeral-Ed25519
  harness; force `/bin/sh`, skip if absent):
  - SESS-01/02/04 (real tty via `test -t 0`, `echo` round-trip, `stty size` == initial 40x132),
  - SESS-05 (Resize → `stty size` == 50x100),
  - SESS-07 (env sanitization — explicit presence of TERM/LC_ALL/TZ and absence of
    LD_PRELOAD/BASH_ENV/SSH_AUTH_SOCK/SHELLOPTS/PYTHONPATH/NODE_OPTIONS),
  - SESS-08 (`exit 42` → client exit code 42),
  - SESS-09 (clean `ApplicationClosed`),
  - SESS-10 (disconnect → shell pid reaped, not a zombie, via `/proc/<pid>/stat`).
- Updated the Phase 1/2 `transport.rs` and `auth.rs` tests: the server no longer echoes, so
  usability is now proven by running a real session over the authenticated link
  (`common::session_marker_usable`). All AUTH-01..05 and TRANS-01..05 assertions preserved.

## Files
- `crates/nosh-client/Cargo.toml`, `src/client.rs`, `src/main.rs`
- `crates/nosh-client/tests/session.rs` (new), `tests/common/mod.rs`, `tests/transport.rs`, `tests/auth.rs`

## Verification
- `cargo build --workspace --all-targets`, `cargo clippy --workspace --all-targets` clean.
- `cargo test --workspace`: all green — session 6/6, auth 6/6 (+1 ignored live-agent),
  transport 4/4 (+1 ignored 60s), proto 4/4, server lib 1/1.

## Requirements
SESS-02, SESS-03, SESS-04, SESS-05, SESS-06, SESS-08, SESS-09.

## Notes
- SESS-03 (raw-mode restore on SIGKILL) and SESS-06 (Ctrl-C interrupts a foreground sleep) are
  implemented correctly but listed as human-verification in VERIFICATION.md — they need a real tty
  and (for SIGKILL) a process Drop cannot run, so they can't be asserted headlessly with confidence.
</content>
