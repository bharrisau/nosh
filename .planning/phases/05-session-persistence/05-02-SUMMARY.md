# Plan 05-02 Summary: Registry Wiring into Connection Pump

**Status:** Complete
**Completed:** 2026-05-30
**Commit:** 8a477d1 (bundled with 05-03)

## What Was Built

### Task 1: Thread Arc<SessionRegistry> + build slot + feed output buffer

- `run_accept_loop` signature changed to accept `registry: Arc<SessionRegistry>`
- Reaper spawned once: `let _reaper = registry.spawn_reaper();`
- Registry cloned into each connection task
- `handle_connection` and `run_session` both take `Arc<SessionRegistry>`
- After `session::open`, the `Session` is moved into `SessionSlot::new(sess)` and registered Active
- Every outgoing PTY chunk fed via `slot.push_output(&data)` (D-10)
- Drain loop in `ShellExited` path also calls `slot.push_output()` for final bytes
- `slot.touch()` called on client input (D-03 coarse tick)
- Resize routed through `slot.resize()` delegate (no direct `sess.resize()` on moved Session)

### Task 2: SessionEnd enum — transport loss vs clean close (D-02)

- `enum SessionEnd { ShellExited(i32), ClientClosed, TransportLost }`
- Shell-exit arms break `ShellExited(code)`
- `SessionClose`/unexpected-reopen arm breaks `ClientClosed`  
- Output send-fail breaks `TransportLost`
- Input send-fail breaks `TransportLost`
- `recv` `Err(_)` breaks `TransportLost`
- Post-loop match:
  - `ShellExited` → drain + send SessionClose + close conn + `registry.remove()`
  - `ClientClosed` → sighup + bounded reap + close conn + `registry.remove()`
  - `TransportLost` → `registry.orphan(&slot)`, detach wait_task (no abort), abort I/O bridges only

### Task 3: Integration tests in persistence.rs

- `clean_session_close_does_not_orphan`: shell exits → `total_orphans() == 0`
- `shell_exit_does_not_orphan`: shell exits normally → `total_orphans() == 0`
- `transport_loss_orphans_without_sighup`: abrupt `conn.close(1, ...)` → `total_orphans() == 1`, shell NOT SIGHUP'd (no HUP-trap file)

### Supporting changes

- `common/mod.rs`: `TestServer` now exposes `pub registry: Arc<SessionRegistry>`; `spawn_server_with_registry` helper added; `spawn_server_with_shell` delegates to it with a default registry
- `session.rs`: `SESS-10` updated — now asserts shell is NOT a zombie (orphaned alive) rather than reaped immediately
- `Cargo.toml`: clap `"env"` feature added for `NOSH_IDLE_TIMEOUT_SECS` env attribute

## Tests
- All persistence integration tests: 3/3 passed
- All existing session integration tests: 6/6 passed (SESS-10 updated)
- `cargo build -p nosh-server`: clean
- `cargo clippy -p nosh-server -- -D warnings`: clean
