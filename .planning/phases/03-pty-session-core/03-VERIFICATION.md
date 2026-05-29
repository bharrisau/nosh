# Phase 3 Verification: PTY Session Core

**Verified:** 2026-05-29
**Method:** Goal-backward — start from each SESS requirement and the phase goal, trace to the code and
the automated test (or the human-verification step) that proves it.
status: human_needed

## Goal

> An authenticated QUIC connection spawns a real PTY login shell; keystrokes and output flow over a
> single bidi stream; resize and signals propagate; the client terminal is raw-mode (RAII-restored);
> env is sanitized; the remote exit code propagates; the connection closes cleanly with a structured
> reason; a server-side session struct holds state for M3 reattach; events are traced.

All structural elements exist and the automatable behaviors are proven by `cargo test --workspace`.
Two requirements (SESS-03 SIGKILL restore, SESS-06 Ctrl-C interrupt) are correctly implemented but
require a human at a real terminal — they are listed below with exact steps rather than marked passed.

## Build / test gate

| Gate | Result |
|------|--------|
| `cargo build --workspace --all-targets` | PASS |
| `cargo test --workspace` | PASS — session 6/6, auth 6/6 (+1 ignored), transport 4/4 (+1 ignored), proto 4/4, server lib 1/1 |
| `cargo clippy --workspace --all-targets` | PASS (no warnings) |

## Requirement-by-requirement

| Req | Status | Evidence |
|-----|--------|----------|
| SESS-01 real PTY + login shell | PASS (auto) | `session.rs::open` uses `native_pty_system()` + `new_default_prog()` (login shell, argv0 `-sh`); test `sess01_02_04_real_tty_and_io` asserts `test -t 0` → `IS_TTY`. |
| SESS-02 keystroke/output flow over reliable stream | PASS (auto) | Server input/output pumps; test asserts `echo hello-nosh` output returns. |
| SESS-03 client raw mode + RAII restore | **HUMAN** | `client::RawModeGuard` (Drop → `disable_raw_mode`). Drop fires on normal/panic/error paths. SIGKILL cannot run Drop → see manual step 1. |
| SESS-04 TERM + initial size to PTY | PASS (auto) | `SessionOpen{term,cols,rows}`; `open` sets `PtySize`; test asserts `stty size` == `40 132`. |
| SESS-05 resize coalesced + propagates | PARTIAL/auto | Propagation proven: test `sess05_resize` sends `Resize{100,50}` → `stty size` == `50 100`; `Session::resize` calls `MasterPty::resize`. Coalescing (~40 ms debounce) is in `main.rs` `run_interactive` (signal arms a `sleep_until` deadline) — verified by inspection; see manual step 2 for the visual storm-free drag. |
| SESS-06 Ctrl-C reaches foreground group | **HUMAN** | Client forwards `0x03` as `PtyData` (no trap); PTY line discipline (ISIG) delivers SIGINT. Headless assertion is timing-flaky; see manual step 3. |
| SESS-07 env sanitization | PASS (auto, security) | `build_child_env` deny-by-default; unit test `build_child_env_is_deny_by_default` + integration `sess07_env_sanitization` assert TERM/LC_ALL/TZ present and LD_PRELOAD/BASH_ENV/SSH_AUTH_SOCK/IFS/SHELLOPTS/PYTHONPATH/NODE_OPTIONS absent. |
| SESS-08 exit code via SessionClose | PASS (auto) | Server `wait_child` → `SessionClose{exit_code}`; client exits with it; test `sess08_exit_code` asserts code 42. |
| SESS-09 clean structured close | PASS (auto) | Server sends `SessionClose` then `conn.close(0, "shell exited")`; test `sess09_clean_close` asserts `ApplicationClosed`. |
| SESS-10 server-side session struct + reap | PASS (auto) | `Session` struct (session_id/identity/master/child_pid/idle_since); `sighup`+`reap_child`; test `sess10_no_zombie_after_disconnect` asserts the shell pid is reaped (gone / non-zombie) after disconnect. |
| SESS-11 tracing spans | PASS (inspection) | `run_session` enters `info_span!("session", %session_id, %peer, username=%username)`; open/resize/exit/disconnect logged inside it. Non-asserting smoke; fields present in source. |

## Human-verification items

Run a real interactive session first:

```
# Terminal A — server (force a known shell for the demo)
cargo run -p nosh-server -- --addr 127.0.0.1 --port 4433 \
  --host-key <ed25519> --authorized-keys <authorized_keys> --shell /bin/bash
# Terminal B — client (ssh-agent must hold your key)
cargo run -p nosh-client -- --addr 127.0.0.1 --port 4433 --host localhost
```

1. **SESS-03 — terminal restored after SIGKILL.** In the client session, run `vim` (or just leave the
   raw-mode shell). From another terminal, `kill -9 <nosh-client pid>`. Expectation: the local terminal
   is still usable (cooked mode) — typing echoes, `Enter` works. (Drop cannot run on SIGKILL, so the
   shell's own `reset`/`stty sane` may be needed; confirm the documented behavior and note any gap.)
   Also confirm the *normal* exit path restores correctly: run `exit` and verify the terminal is sane.

2. **SESS-05 — resize storm-free.** Open `htop` or `vim` in the session. Drag the terminal window
   border to resize rapidly. Expectation: the remote program reflows to the final size; no flicker
   storm / lag from per-row resizes (the ~40 ms debounce coalesces the burst into one `Resize`).

3. **SESS-06 — Ctrl-C interrupts a foreground process.** In the session run `sleep 100`, then press
   Ctrl-C. Expectation: `sleep` is interrupted immediately and the shell prompt returns (SIGINT was
   delivered to the foreground process group via the PTY line discipline).

## Limitations (by design, recorded)
- Single-account server, no privilege drop / setuid (D-03). Shell runs as the account the server
  runs under; `Session.identity` is `None` (Phase 2 doesn't yet surface the peer key — M3 wiring seam).
- Datagrams carry no session traffic this milestone (D-02); only their enablement is asserted.
- Cold reattach is NOT implemented; `idle_since` + the `Session` struct are the M3 seam only.
</content>
