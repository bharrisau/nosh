# Plan 03-02 Summary: Server PTY session pump

**Status:** Complete

## What was built
- New `crates/nosh-server/src/session.rs` (the M3 reattach seam, D-08):
  - `PasswdEntry` + `lookup_self(shell_override)` via `nix::unistd::{geteuid, User::from_uid}`
    with env fallback; honors a `--shell` override.
  - **Deny-by-default env** (`build_child_env`, FOOTGUN-1/2, SESS-07): builds the child env from a
    server-owned baseline (`HOME/USER/LOGNAME/SHELL/PATH`) plus only whitelisted client vars
    (`TERM`, `LANG`, `TZ`, any `LC_*`). `LD_*`, `BASH_ENV`, `SSH_AUTH_SOCK`, `IFS`, `SHELLOPTS`,
    `PYTHONPATH`, `NODE_OPTIONS` etc. can never appear because they are never copied. Unit-tested.
  - `Session { session_id: Uuid, identity, username, master, child, child_pid, idle_since }`.
  - `open(..)` allocates a PTY via `portable_pty::native_pty_system()` and spawns the user's shell
    as a **login shell** (portable-pty `new_default_prog()` sets argv[0] = `-<basename>`), with
    `env_clear()` then the sanitized env and `cwd(home)`.
  - `Session::resize` (SESS-05); `wait_child`/`reap_child` run `Child::wait` on `spawn_blocking`
    (no zombie, SESS-10); `sighup()` sends SIGHUP to the shell pid.
- Rewrote `handle_connection`/`run_session` in `server.rs`: removed the Phase 2 echo loops; reads a
  `SessionOpen` first frame; pumps PTY output→`PtyData` and client `PtyData`→PTY (blocking I/O bridged
  via `spawn_blocking` + mpsc channels); applies `Resize`. On shell exit: drains remaining output,
  sends `SessionClose { exit_code, .. }`, waits for the client to read it (`send.stopped()`), then
  `conn.close(0, "shell exited")` (SESS-08/09). On client disconnect: SIGHUP + reap (SESS-10).
  Wrapped in a `tracing` span carrying `session_id`, `peer`, `username` (SESS-11).
- Added `--shell` flag to the server binary; `run_accept_loop` now threads `shell_override`.

## Files
- `crates/nosh-server/Cargo.toml` (+ portable-pty, uuid, nix)
- `crates/nosh-server/src/session.rs` (new)
- `crates/nosh-server/src/lib.rs`, `src/server.rs`, `src/main.rs`

## Verification
- `cargo build -p nosh-server`, `cargo clippy -p nosh-server --all-targets` clean.
- `session::tests::build_child_env_is_deny_by_default` passes (unit-level SESS-07).

## Requirements
SESS-01, SESS-04, SESS-05, SESS-06, SESS-07, SESS-08, SESS-09, SESS-10, SESS-11.

## Notes / limitations
- Single-account spike: no privilege drop / setuid (D-03). The shell runs as the account the server
  runs under. `Session.identity` is `None` for now — Phase 2 does not yet surface the peer cert key
  into the connection handler; noted as an M3 wiring seam.
</content>
