---
plan: 10-02
phase: 10-pty-reader-race-fix
status: complete
completed: 2026-06-01
commits:
  - 19f67e9
---

# Plan 10-02: Wire Interruptible Reader into server.rs

## What Was Built

### Two converted output-pump sites in server.rs

**Site 1 — run_session (formerly lines ~356-373):**
```rust
let master_raw_fd = slot.master_raw_fd().expect("Unix PTY master fd available");
let (out_tx, mut out_rx) = mpsc::channel::<Vec<u8>>(64);
let mut reader_handle = crate::pty_io::start_interruptible_reader(master_raw_fd, reader, out_tx)
    .expect("start interruptible PTY reader");
```

**Site 2 — run_reattach_session (formerly lines ~747-762):**
```rust
let master_raw_fd = slot.master_raw_fd().expect("Unix PTY master fd available for reattach");
let (out_tx, mut out_rx) = mpsc::channel::<Vec<u8>>(64);
let mut reader_handle = crate::pty_io::start_interruptible_reader(master_raw_fd, reader, out_tx)
    .expect("start interruptible PTY reader for reattach");
```

### Reader await in both TransportLost arms (D-03)

Pattern applied before `registry.orphan()` in **both** TransportLost arms:
```rust
// D-03: signal + await reader exit BEFORE orphan (guarantees no two live readers)
reader_handle.signal_shutdown();
let _ = tokio::time::timeout(Duration::from_secs(5), &mut reader_handle.join).await;
// W2 writer await preserved (D-03a):
let _ = tokio::time::timeout(Duration::from_secs(5), &mut input_writer).await;
registry.orphan(&slot);
```

### W2 writer handback: PRESERVED UNCHANGED

Both `in_rx.blocking_recv()` writer loops are byte-for-byte unchanged.
`return_pty_writer` call count: 4 (unchanged from baseline).
Writer timeout-await before `registry.orphan` in both TransportLost arms: ✓ confirmed.

### Removed anti-patterns

- `output_reader.abort()` removed from both sites (no-op on spawn_blocking — Pitfall 6)
- `input_writer.abort()` at post-match remains for the writer (this is harmless since the
  writer already exited on the TransportLost path; it's kept for ShellExited/ClientClosed
  paths where we don't await it)

### D-04 completion-barrier test

Implemented in `crates/nosh-server/src/pty_io.rs` (Plan 01 Task 2):
- Test name: `pty_io::tests::reader_exits_on_shutdown_barrier`
- N = 10 create→orphan cycles
- `Arc<AtomicUsize>` exit_count, PRIMARY assert: `exit_count == N`
- `/bin/sh` guard, no `RuntimeMetrics`, no unstable flags

## Key Files Modified

- `crates/nosh-server/src/server.rs` — both output pumps converted, both TransportLost arms
  fixed with reader signal+await before orphan, no-op abort() calls removed

## Acceptance Criteria Verification

- [x] `cargo build -p nosh-server` exits 0
- [x] `cargo test -p nosh-server` exits 0 — 25/25 tests pass (no regressions)
- [x] 2 `start_interruptible_reader` call sites (`grep -c start_interruptible_reader == 2`)
- [x] No `reader.read(&mut buf)` loop remaining (`grep -c reader.read(&mut buf) == 0`)
- [x] No `output_reader.abort()` (`grep -c output_reader.abort == 0`)
- [x] reader signal_shutdown + join await precedes input_writer await precedes registry.orphan in both arms
- [x] `in_rx.blocking_recv` count == 2 (writer loops unchanged)
- [x] `return_pty_writer` count unchanged (4 calls)
- [x] No O_NONBLOCK/F_SETFL in server.rs
- [x] D-04 test: AtomicUsize, assert_eq!(exit_count, N), /bin/sh guard, no RuntimeMetrics

## Deviations from Plan

None — plan executed as written.

Minor: `PTY_CHUNK` constant in server.rs became unused (both spawn_blocking loops were removed,
and pty_io.rs defines its own local constant). Added `#[allow(dead_code)]` with an updated doc
comment pointing to `crate::pty_io`. This preserves the documented design constant value without
a build warning. Not a functional deviation.

## Self-Check: PASSED
