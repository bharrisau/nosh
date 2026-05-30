# Phase 5 Verification: Session Persistence

**Phase:** 05-session-persistence
**Verified:** 2026-05-30
**Status:** PASSED

---

## Success Criteria Checklist

### PERSIST-01: Session survival on transport loss
- [x] `SequencedOutputBuffer`: monotonic u64 seq from 0, 64 KiB drop-oldest ring, truncation marker, newest always survives
- [x] `SessionRegistry`: per-identity orphan cap (default 5) with LRU eviction (logged, never silent), Active never evicted, identities independent
- [x] Reaper: removes exited orphans (no zombies) and idle-times-out orphans only when idle_timeout > 0
- [x] Transport loss → `registry.orphan()` (MasterPty open, no SIGHUP, Pitfall #7)
- [x] SessionClose + shell exit → immediate teardown + `registry.remove()`
- [x] Every sent PTY chunk buffered via `slot.push_output()` (D-10)

### PERSIST-02: Idle timeout configuration
- [x] `--idle-timeout-secs` default 0 = disabled (Mosh behavior, D-08)
- [x] `NOSH_IDLE_TIMEOUT_SECS` env fallback with CLI > env > default precedence (D-09)
- [x] `--max-sessions-per-identity` default 5 (D-05)
- [x] `nosh-server --help` lists both flags

### PERSIST-03: Per-identity cap and integration tests
- [x] `clean_session_close_does_not_orphan`: `total_orphans() == 0` after clean close
- [x] `shell_exit_does_not_orphan`: `total_orphans() == 0` after shell exit
- [x] `transport_loss_orphans_without_sighup`: `total_orphans() == 1`, no SIGHUP (no HUP-trap file)

---

## Test Results

```
cargo test --workspace

running 12 tests (nosh-auth)     → 11 passed, 1 ignored
running 7 tests (nosh-client)    → 6 passed, 1 ignored
running 3 tests (persistence)    → 3 passed
running 6 tests (session.rs)     → 6 passed
running 5 tests (transport.rs)   → 4 passed, 1 ignored
running 14 tests (nosh-server)   → 14 passed (11 registry + 1 server + 2 session)
running 1 test (main.rs)         → 1 passed (cli_env_precedence_idle_timeout)

TOTAL: 45 passed, 3 ignored, 0 failed
```

Previous baseline: 34 tests passing. Added 11 new tests.

## Quality Gates
- [x] `cargo build -p nosh-server` clean
- [x] `cargo clippy --workspace -- -D warnings` clean (no held-lock-across-await, no unused)
- [x] No new Cargo.toml dependencies (bytes/uuid/tokio/clap already present; clap "env" feature added)
- [x] `nosh-server --help` shows `--idle-timeout-secs` and `--max-sessions-per-identity`

## Locked Decisions Honored
- D-01: Explicit SessionClose + shell exit → immediate teardown (no orphan)
- D-02: Transport loss → orphan (subdivided SessionEnd enum)
- D-03: Single `last_active` timestamp drives both idle-timeout and LRU eviction
- D-04: Active/Orphaned state machine
- D-05: DEFAULT_MAX_PER_IDENTITY = 5
- D-06: LRU eviction of oldest orphan with tracing::warn
- D-07: Active sessions never evicted, never count toward cap
- D-08: Idle timeout default 0 = disabled
- D-09: NOSH_IDLE_TIMEOUT_SECS env fallback, CLI precedence
- D-10: Monotonic u64 sequence numbers in SequencedOutputBuffer
- D-11: Drop-oldest 64 KiB ring with truncation marker, newest always survives
- Pitfall #7: NO SIGHUP on transport loss

## Human Items
None. All acceptance criteria verified automatically.
