---
phase: "03"
phase_name: pty-session-core
depth: standard
reviewer: gsd-code-review (inline; Task subagent unavailable)
date: 2026-05-29
files_reviewed: 6
status: findings
findings:
  critical: 0
  warning: 1
  info: 3
  total: 4
---

# Phase 03 — PTY Session Core: Code Review

## Scope

Reviewed the Phase 3 changes against the security/correctness checklist:

- `crates/nosh-proto/src/messages.rs`
- `crates/nosh-proto/src/codec.rs`
- `crates/nosh-server/src/server.rs`
- `crates/nosh-server/src/session.rs`
- `crates/nosh-client/src/client.rs`
- `crates/nosh-client/tests/session.rs`

## Verdict

The four security-critical axes are correctly implemented:

- **Env sanitization is deny-by-default and sound.** `session::open` calls
  `CommandBuilder::env_clear()` and applies only `build_child_env`, which starts
  from a server-owned baseline (`HOME/USER/LOGNAME/SHELL/PATH`) and copies a
  client var *only* if `env_key_allowed` returns true (`TERM`/`LANG`/`TZ` exact,
  or `LC_*` prefix). It is a structural whitelist, not a blacklist, so
  `LD_*`/`DYLD_*`/`BASH_ENV`/`ENV`/`IFS`/`SHELLOPTS`/`PYTHONPATH`/`NODE_OPTIONS`/
  `SSH_AUTH_SOCK` cannot reach the shell — they are simply never copied. A client
  also cannot smuggle a baseline override: `HOME`/`PATH`/`SHELL` are not in the
  whitelist, so a client-supplied `PATH=...` is dropped. `TERM` from the client
  is later force-overwritten by the negotiated `term` arg. Verified by the
  `build_child_env_is_deny_by_default` unit test and the `sess07_env_sanitization`
  integration test (asserts presence of `LC_ALL/TZ/TERM` and absence of every
  dangerous var, including the `/agent.sock` path).
- **No PTY/child leak.** The child is moved into a `tokio::spawn(wait_child(..))`
  task; `wait_child` runs the blocking `Child::wait()` on `spawn_blocking` so the
  executor is never stalled, and the wait reaps the child. On every disconnect
  path (`None` outcome) the server `sighup()`s then awaits the wait task (bounded
  5s) so the SIGHUP'd shell is reaped — no zombie. The `sess10_no_zombie_after_disconnect`
  test exercises abrupt client loss and asserts the pid is gone/non-zombie.
- **Exit-code path is correct.** `Child::wait()` → `status.exit_code() as i32` →
  `SessionClose{exit_code, reason}`; the client surfaces it (`sess08_exit_code`
  asserts 42). Signal-terminated children are handled sanely (see INFO-1).
- **Single-bidi framing cannot desync / cannot panic on a hostile peer.**
  `read_message` reads a 4-byte length prefix, rejects `len > MAX_FRAME_LEN`
  (16 MiB) with `FrameTooLarge` before allocating, and uses `read_exact` for both
  prefix and body (partial reads handled). A malformed postcard body returns
  `Err`, which the server maps to a clean close — no `unwrap`/`panic` on
  peer-controlled bytes.

Build, tests, clippy, and (Phase-3-file) fmt are green. One pre-existing fmt
drift in Phase 2 files is normalized in this review's fixup commit.

---

## Findings

### WR-1 (warning) — Abrupt transport loss is reported to the client as exit code 0
**File:** `crates/nosh-client/src/client.rs:315` (`collect_until_close`)

```rust
Err(_) => return Ok((output, 0)), // stream closed without an explicit close
```

If the connection drops abnormally (network loss, server crash, RST) *before* a
`SessionClose` frame arrives, the client driver returns exit code `0` — i.e. a
transport failure is surfaced as a successful shell exit. SESS-08 wants the real
remote exit code; on an error path a non-zero/sentinel code (or an `Err`) is more
honest and avoids a script mistaking a dropped session for `exit 0`. Low blast
radius today (headless test helper; the real interactive client run-loop is not
yet in this file), so classified warning rather than blocker. **Recommended:**
return a sentinel non-zero (e.g. `255`) or propagate the error on the `Err(_)`
arm. Left unfixed (behavior change with no current consumer; flag for the
interactive client phase).

### INFO-1 (info) — Signal-terminated child collapses to exit code 1; signal name discarded
**File:** `crates/nosh-server/src/session.rs:173` (`wait_child`)

`portable_pty::ExitStatus::from(std::process::ExitStatus)` sets `code = 1` for a
signal-killed child and stores the signal name separately; `wait_child` reads only
`exit_code()`, so a shell killed by e.g. SIGTERM reports `1`, not the conventional
`128 + signum`, and the signal name in `reason` is lost. This is "sane" per the
checklist (deterministic, non-panicking) and acceptable for the spike, but worth a
follow-up if exact POSIX-style codes matter. No fix applied.

### INFO-2 (info) — Disconnect reap is best-effort if the shell ignores SIGHUP
**File:** `crates/nosh-server/src/server.rs:357-359`

On disconnect the server `sighup()`s and awaits the wait task with a 5s timeout. A
shell that traps/ignores SIGHUP will outlive the timeout; the wait task is not
aborted afterward, so the blocking reaper thread lingers until the child actually
dies (no zombie, but a lingering blocking thread + orphaned process). Matches the
documented best-effort teardown intent for this milestone; reattach/forced-kill is
explicitly out of scope. No fix applied.

### INFO-3 (info) — `RawModeGuard` is defined but not yet exercised by a run-loop
**File:** `crates/nosh-client/src/client.rs:216-230`

The RAII guard is correct (Drop restores cooked mode, fires on normal return,
panic-unwind, and the held-scope error path; SIGKILL is the accepted human-verify
gap). However, no interactive client run-loop in the reviewed files actually holds
it, so its restore-on-abrupt-loss guarantee is currently unverified by code/tests
in scope. Confirm it is held across the real session pump when that lands. No fix
applied.

---

## Notes
- `cargo build --workspace --all-targets`: pass
- `cargo test --workspace`: pass (session.rs 6/6; auth, transport, proto green)
- `cargo clippy --workspace --all-targets`: no warnings
- No `unwrap`/`expect` on peer-controlled input anywhere in the session path.
- Pre-existing rustfmt drift in Phase 2 (`nosh-auth/src/{keys,signer,verifier}.rs`)
  normalized via `cargo fmt --all` in a separate `style:` fixup commit.
