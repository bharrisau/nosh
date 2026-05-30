# Plan 05-01 Summary: Session Persistence Data Structures

**Status:** Complete
**Completed:** 2026-05-30
**Commit:** dc92287

## What Was Built

Created `crates/nosh-server/src/registry.rs` — the persistence substrate for Phase 5.

### SequencedOutputBuffer (D-10/D-11)
- Monotonic `u64` sequence numbers starting at 0, assigned at push time
- 64 KiB drop-oldest ring (`DEFAULT_OUTPUT_BUFFER_BYTES = 64 * 1024`)
- Truncation marker (`truncated: bool` + `lowest_retained_seq: u64`)
- Critical `ring.len() > 1` guard ensures newest chunk always survives even if a single chunk exceeds max_bytes

### SessionSlot (D-03/D-04)
- Wraps `Session` in `std::sync::Mutex` (not tokio, per Anti-Pattern #2)
- Single `last_active: Mutex<Instant>` drives both idle-timeout and LRU eviction (D-03)
- `Active` / `Orphaned` state machine (D-04)
- `touch()`, `mark_orphaned()`, `push_output()`, `resize()`, `try_wait()`, `sighup()` delegates

### SessionRegistry (D-05/D-06/D-07/D-08)
- `HashMap<[u8; 32], Vec<Arc<SessionSlot>>>` keyed by SSH identity raw bytes
- `DEFAULT_MAX_PER_IDENTITY = 5` (D-05)
- `orphan()` enforces cap: LRU eviction of oldest-`last_active` orphan with `tracing::warn` (D-06)
- Active slots never evicted, never count toward cap (D-07)
- `reap_once()` collects victims under the lock, reaps after release (Anti-Pattern #2)
- Idle-timeout reaping: `Duration::ZERO` = disabled (D-08)
- `spawn_reaper()`: background tokio task, 1s cadence

### Supporting changes
- `Session::child_mut()` added to `session.rs` for non-blocking `try_wait` via `SessionSlot`
- `pub mod registry` declared in `lib.rs`

## Tests
All 11 unit tests pass:
- 5 SequencedOutputBuffer tests (seq ordering, 64 KiB overflow, newest-survives, truncation marker)
- 3 SessionRegistry cap/LRU tests (cap evicts LRU, identities independent, Active never evicted)
- 3 reaper tests (idle=0 no-op, finite timeout reaps, exited-shell reap)

## Verification
- `cargo build -p nosh-server`: clean
- `cargo test -p nosh-server --lib registry`: 11/11 passed
- `cargo clippy -p nosh-server -- -D warnings`: clean
- No new Cargo.toml dependencies (bytes, uuid, tokio already present)
