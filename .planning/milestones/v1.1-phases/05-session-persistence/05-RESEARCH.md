# Phase 5: Session Persistence - Research

**Researched:** 2026-05-30
**Question answered:** "What do I need to know to PLAN this phase well?"

> Scope note: Phase 5 introduces the orphan lifecycle, the `SessionRegistry`/`SessionSlot`,
> the `SequencedOutputBuffer`, the per-identity cap with LRU eviction, the idle timeout, and
> the zombie reaper. The `Reattach` protocol, token issuance, output replay, and connection
> migration are **explicitly deferred** to Phase 6/7 (CONTEXT.md `<deferred>`). The
> ARCHITECTURE.md sketch is written forward-looking (it keys slots by `SessionToken` and
> includes the reattach messages) — for Phase 5 the registry is keyed by the **SSH identity
> fingerprint** per CONTEXT D-03 and the canonical refs, and the token/reattach surface is NOT
> built here.

---

## Architectural Responsibility Map

| Tier | Component | File | Phase 5 responsibility |
|------|-----------|------|------------------------|
| Transport | `run_accept_loop` | `nosh-server/src/server.rs:102` | Construct one `Arc<SessionRegistry>`; clone into each connection task |
| Connection | `handle_connection` / `run_session` | `nosh-server/src/server.rs:149,203` | On transport-loss outcome, orphan the slot (no SIGHUP); on clean close / shell exit, tear down + decrement |
| Registry | `SessionRegistry` | `nosh-server/src/registry.rs` (NEW) | Map identity→slots; per-identity cap; LRU eviction; orphan/remove transitions |
| Slot | `SessionSlot` | `nosh-server/src/registry.rs` (NEW) | Own `Session` (+ MasterPty kept open) + `SequencedOutputBuffer` + `last_active` + state |
| Buffer | `SequencedOutputBuffer` | `nosh-server/src/registry.rs` (NEW) | u64-sequenced 64 KiB ring, drop-oldest, truncation marker |
| Reaper | background task | `nosh-server/src/registry.rs` + spawned in `run_accept_loop` | Periodic `child.try_wait()` over orphans; idle-timeout reaping |
| CLI | `Args` | `nosh-server/src/main.rs:16` | `--idle-timeout-secs` + `NOSH_IDLE_TIMEOUT_SECS` env fallback; thread into registry |

---

## What must be TRUE (goal-backward, from ROADMAP success criteria)

1. On QUIC drop, the server's `MasterPty` stays open → shell is NOT SIGHUP'd → still interactive. (PERSIST-01, Pitfall #7)
2. Outgoing PTY chunks get monotonic u64 seq numbers from session open, held in a 64 KiB ring. (PERSIST-01/replay-prep, D-10/D-11)
3. Idle timeout (default `0` = disabled) governs orphan lifetime; tested at `0` and a finite value. (PERSIST-02, D-08/D-09)
4. Per-identity cap (default 5) enforced before the first orphan is stored; exceed → deterministic eviction, not silent drop. (PERSIST-03, D-05/D-06/D-07, Pitfall #5)
5. Background reaper `try_wait()`s orphans; no zombies after shell exit. (PERSIST-01, Pitfall #6)

---

## Existing code: the seams Phase 5 reroutes (verified this session)

### The disconnect/teardown split — `server.rs:341-387`
Today `run_session` ends in a `match session_outcome`:
- `Some(exit_code)` → shell exited: drain output, send `SessionClose`, `conn.close(CLOSE_OK)`. **Keep as-is (D-01).**
- `None` → covers BOTH client `SessionClose`/clean-end AND transport error (read error / connection lost). Today both unconditionally `sess.sighup()` + reap. **Phase 5 must SUBDIVIDE this `None` branch (D-02):**
  - `SessionClose` / unexpected `SessionOpen` reopen → clean teardown (current SIGHUP + reap behavior). Typing `exit` or quitting the client cleanly must NOT linger.
  - `read_message` error (`Err(_)` arm at server.rs:332) / connection lost → **orphan**: move the slot to Orphaned, do NOT SIGHUP, keep `MasterPty` open.

The `None` outcome is produced at four points (server.rs:303 send-fail, :320 in_tx send-fail, :329 SessionClose/reopen, :332 recv error). To subdivide, replace the boolean `None` with a small outcome enum so the post-loop match can distinguish `ClientClosed` (clean) from `TransportLost` (orphan).

### The orphan seam already exists — `session.rs:128`
`Session.idle_since: Option<Instant>` is the v1.0 reattach seam. D-03 generalizes the orphan-marker concept into a single `last_active: Instant` that lives on the `SessionSlot` (updated while attached, frozen at orphaning). The `Session` itself need not change much; the lifecycle state lives in the slot.

### MasterPty ownership — `session.rs:123`
`master: Box<dyn MasterPty + Send>` is held inside `Session`. As long as the `Session` (hence `master`) is not dropped, the master fd stays open and the kernel does not SIGHUP the shell. **The whole trick of Pitfall #7 is: on transport loss, do not drop the `Session`; move it into the registry's orphan slot.** Note `MasterPty` is `Send` but not `Sync` → wrap the `Session` in a `Mutex` inside `SessionSlot` (ARCHITECTURE Anti-Pattern #3).

### Child reaping primitives already exist — `session.rs:153,159,172,188`
- `Session::take_child()` → `Option<Box<dyn Child + Send + Sync>>`
- `Session::sighup()` → best-effort SIGHUP via `nix::sys::signal::kill`
- `wait_child(child).await -> i32` (blocking wait on a thread)
- `reap_child(child).await` (blocking wait, discard code)
- `portable_pty::Child::try_wait()` is available on the trait — the reaper uses this (non-blocking) per Pitfall #6. The `Child` is currently `take()`n into `wait_task` in `run_session`; for an orphan that should keep waiting, the reaper needs access. Discretion: either keep the `wait_child` JoinHandle alive and have orphaning detach it into the slot, OR put the `Child` back into the slot and let the reaper `try_wait()` it. Simpler: keep the existing `wait_task` JoinHandle; on orphan, store the slot and let a registry-level reaper poll for shell-exit via a shared exit signal. **Recommended:** store the `Child` (untaken, or re-homed) in the slot's `Session` so the reaper can `try_wait()` uniformly across all orphans.

### CLI flag pattern — `main.rs:16-47`
clap derive `Args` with `#[arg(long, default_value_t = …)]`. Add:
```
#[arg(long, env = "NOSH_IDLE_TIMEOUT_SECS", default_value_t = 0)]
idle_timeout_secs: u64,
```
clap's `env = "…"` attribute gives the CLI-precedence-over-env behavior for free (D-09): an explicit `--idle-timeout-secs` overrides the env var, and the env var overrides the `0` default. Also add `--max-sessions-per-identity` (default 5, D-05) following the same pattern. Thread both into `run_accept_loop` → `SessionRegistry::new(max_per_identity, idle_timeout)`.

---

## SequencedOutputBuffer design (D-10/D-11)

```rust
use bytes::Bytes;                 // already a workspace dep (server.rs uses bytes)
use std::collections::VecDeque;

pub struct SequencedOutputBuffer {
    next_seq: u64,                 // monotonic, never resets — assigned per chunk
    ring: VecDeque<(u64, Bytes)>,  // (seq, chunk) retained
    total_bytes: usize,            // sum of chunk lengths in ring
    max_bytes: usize,              // 64 * 1024
    lowest_retained_seq: u64,      // = seq of ring.front(); truncation marker
    truncated: bool,               // set true once any chunk is dropped (D-11)
}
```
- `push(chunk: &[u8]) -> u64`: assign `seq = next_seq`, `next_seq += 1`, push `(seq, Bytes::copy_from_slice(chunk))`, add len to `total_bytes`; while `total_bytes > max_bytes && ring.len() > 1`, pop_front, subtract, set `truncated = true`, update `lowest_retained_seq`. Returns the assigned seq. Latest output always survives (keep at least the most recent chunk even if a single chunk > 64 KiB — drop everything older, keep newest).
- Edge case: a single chunk larger than `max_bytes`. PTY_CHUNK is 8 KiB so chunks ≤ 8 KiB in practice; still, guard `ring.len() > 1` so the newest chunk is never evicted (latest output always survives, D-11). Optionally truncate an over-large single chunk to its tail — discretion; keeping the whole newest chunk is acceptable and simplest.
- Wired in the output pump (server.rs:298-307): every `out_rx.recv()` chunk → `slot.output_buf.lock().push(&data)` BEFORE/with `write_message(PtyData)`. "buffered == sent-with-seq" (CONTEXT code_context).
- Phase 6 will add `.since(seq)` for replay; Phase 5 only writes + records truncation. Do NOT build replay now (deferred).

**Pitfall #9 note (for Phase 5):** use u64 (never 32-bit), never reset. Phase 5 only needs the monotonic counter + ring; the cross-connection replay correctness is Phase 6.

---

## SessionRegistry / SessionSlot design (keyed by SSH identity, D-03/D-05)

```rust
pub struct SessionRegistry {
    // identity fingerprint (or [u8;32] raw key) → that identity's slots
    inner: Mutex<HashMap<[u8; 32], Vec<Arc<SessionSlot>>>>,
    max_per_identity: usize,   // D-05 default 5
    idle_timeout: Duration,    // D-08 default 0 == disabled
}

pub struct SessionSlot {
    pub identity: NoshPublicKey,
    pub session_id: Uuid,
    session: Mutex<Session>,                 // owns MasterPty + Child; Send-not-Sync → Mutex
    output_buf: Mutex<SequencedOutputBuffer>,
    state: Mutex<SlotState>,                  // Active | Orphaned
    last_active: Mutex<Instant>,             // D-03 single timestamp: idle-timeout AND LRU
}

enum SlotState { Active, Orphaned }
```
- Key choice (discretion, D-03 says "registry key"): the canonical refs call the key the SSH-identity fingerprint; the raw 32-byte Ed25519 key (`NoshPublicKey::from_raw` round-trips, and `[u8;32]` is `Hash`/`Eq` and cheaper than the base64 string) is the natural map key. Either is acceptable; the **string `fingerprint()` is the human-facing identity in logs**. Use the raw key bytes as the HashMap key, log the `fingerprint()`.
- `register_active(slot)`: insert into the identity's Vec on session open (Active; not yet counted against the orphan cap — D-07: only orphans count).
- `orphan(slot)`: transition Active→Orphaned, freeze `last_active`. THEN enforce the cap over that identity's **orphaned** slots: if orphan count now > `max_per_identity`, pick the orphan with the **oldest `last_active`** (LRU, longest-orphaned), `sighup()`+reap it, remove it, and **log a warning** (D-06; Pitfall #5 "do not silently evict"). The just-orphaned slot (most-recently-active) is retained. NEVER evict an Active slot (D-07).
- `remove(identity, session_id)`: shell exited or clean close → drop the slot (Session drops → MasterPty closes), decrement.
- Locking discipline (ARCHITECTURE Anti-Pattern #2): hold the registry `inner` Mutex only for the O(1)/O(n-small) map op; do I/O (sighup/reap) with the `Arc<SessionSlot>` in hand after releasing the registry lock, or collect the victims under the lock and reap after.

**Cap is "before the first orphan is stored" (PERSIST-03 / success criterion 4):** the cap is evaluated at the orphan transition (when a slot would be *persisted*), not at session open. An Active session does not consume cap (D-07). "Deterministic error, not silent drop" (criterion 4) is satisfied by deterministic LRU eviction with a logged warning — the new orphan is always retained; the deterministic outcome is "oldest orphan evicted + warning logged."

---

## Background zombie reaper (Pitfall #6, PERSIST-01)

A single `tokio::spawn`ed task owned by the registry, started in `run_accept_loop`:
```
loop {
    sleep(reap_interval);                  // e.g. 1s — cadence is discretion
    for each orphaned slot across all identities:
        if slot.session.lock().try_wait_child() == Some(exited):
            registry.remove(slot)          // reaps + drops Session (closes MasterPty)
        else if idle_timeout > 0 && now - *slot.last_active.lock() >= idle_timeout:
            slot.sighup(); reap; registry.remove(slot)   // D-08 idle reaping
}
```
- `try_wait()` is non-blocking (Pitfall #6). Add a thin `Session::try_wait_child() -> Option<ExitStatus>` that calls `Child::try_wait()` on the held child (requires the orphan to retain its `Child`; see "Child reaping" above).
- Active sessions are NOT reaped by idle timeout (D-08: an attached session never times out) — the reaper only scans Orphaned slots.
- Reaper must update `last_active` ONLY for orphans (it's frozen at orphan time); attached sessions update `last_active` in the pump (D-03; coarse update is fine — discretion).

---

## Testability strategy (Nyquist: every behavior has an automated check)

Most Phase 5 logic is pure/deterministic and unit-testable **in-crate** (no full QUIC needed):

- **`SequencedOutputBuffer`** — unit tests in `registry.rs`: monotonic seq starts at 0 and increments; ring stays ≤ 64 KiB; drop-oldest keeps newest; `truncated`+`lowest_retained_seq` set correctly after overflow; small buffers never truncate. (D-10/D-11)
- **Cap + LRU eviction** — unit tests: orphan N+1 slots for one identity with default cap 5 → exactly the oldest-`last_active` orphan is evicted, newest retained; a different identity's slots are unaffected; an Active slot is never evicted. (D-05/D-06/D-07) Use a fake/lightweight Session or a real `/bin/sh` PTY guarded by `have_sh()`.
- **Idle timeout** — unit tests over the registry's reap logic with `idle_timeout = 0` (never reaps on idle) and a finite small duration (reaps an orphan whose `last_active` is older than the timeout). (D-08, criterion 3 — both 0 and finite)
- **No-zombie** — integration-ish test: open a real `/bin/sh` session, orphan it, let the shell `exit`, assert the reaper removes it within a few seconds and no zombie remains (`try_wait` returns the status). Guard with `have_sh()`. (Pitfall #6, criterion 5)
- **No-SIGHUP-on-orphan** — the highest-value correctness test (Pitfall #7, criterion 1): spawn server+client over the existing harness; run a shell that `trap 'echo GOTHUP' HUP`; drop the client connection abruptly (transport loss, not `SessionClose`); reconnect-or-inspect to assert the shell is still alive and "GOTHUP" did NOT fire. Phase 5 has no reattach, so verify liveness server-side: e.g. assert the orphan slot still exists and `try_wait` reports the child running, and that the orphan count incremented (rather than the session being torn down). This needs a small server-side test seam (expose registry state for tests) OR drive it through the existing in-process harness in `nosh-client/tests/`.
- **Clean close does NOT orphan** (D-01/D-02): client sends `SessionClose` (or the shell `exit`s) → assert the registry has zero orphans afterward (no lingering session).

**Test harness reuse:** `crates/nosh-client/tests/common/mod.rs` already builds an in-process authed server+client (`spawn_server_with_shell`, `TestKey`, `client_endpoint`). New integration tests for orphan behavior live alongside it (`nosh-client/tests/persistence.rs`) OR as a new `nosh-server/tests/` if a server-only seam is exposed. For pure registry/buffer unit tests, put them in `#[cfg(test)] mod tests` inside `registry.rs` (matches the existing `session.rs` pattern).

---

## Standard stack (no new external deps required)

| Need | Use | Already present? |
|------|-----|------------------|
| Ring buffer | `std::collections::VecDeque` | std |
| Zero-copy chunk | `bytes::Bytes` | yes (server Cargo.toml:26) |
| Identity key | `nosh_auth::NoshPublicKey` (`from_raw`, `fingerprint`) | yes |
| Session id | `uuid::Uuid` | yes (server Cargo.toml:30) |
| Signals/wait | `nix` signal + `portable_pty::Child::try_wait` | yes |
| Shared state | `std::sync::{Arc, Mutex}` + `tokio::spawn` | std + tokio |
| CLI env fallback | clap `#[arg(env = "…")]` | clap already used |
| Time | `std::time::{Instant, Duration}` | std |

**Do not hand-roll:** a custom ring (use `VecDeque`), a custom env-precedence parser (use clap `env=`), a custom child-wait loop (use `try_wait`). No `tokio::sync::Mutex` needed across awaits if locks are held only for the brief map/buffer ops (use `std::sync::Mutex`; ARCHITECTURE Anti-Pattern #2 — never hold across I/O).

---

## Common pitfalls (Phase-5-relevant, from PITFALLS.md)

- **#7 SIGHUP on disconnect (CORE):** never drop the `Session`/`MasterPty` on transport loss; move it into the orphan slot. The single most important correctness invariant of this phase.
- **#5 Unbounded orphan memory:** the per-identity cap must exist *before* the first orphan is stored, log a warning on eviction, never silently leak.
- **#6 Zombies:** reaper `try_wait()`s orphans; without it, exited shells become defunct entries.
- **#11 (already handled Phase 4):** `Session.identity` is non-optional `NoshPublicKey` captured post-handshake — reuse it as the registry key; do not re-extract.
- **Anti-Pattern #2 (locking):** hold the registry mutex only for the map op; reap/sighup after releasing it.
- **Anti-Pattern #3 (Send-not-Sync):** `MasterPty` is not `Sync` → `Mutex<Session>` in the slot.

## Out of scope for Phase 5 (deferred — do NOT build)

- `Reattach` / `SessionOpened` / `ReattachOk` / `ReattachErr` messages, session tokens, output replay (`.since()`), the Reconnecting state, reattach-race state machine (Pitfall #10), cross-connection seq resync correctness (Pitfall #9) — **Phase 6**.
- Connection migration (`transport.migration(true)`, `endpoint.rebind`) — **Phase 7** (Pitfall #1-4).
- Surfacing the truncation marker to the user — Phase 6 (Phase 5 only *records* it).
- Named/numbered session selection for multiple orphans per identity — M5+.

## RESEARCH COMPLETE
