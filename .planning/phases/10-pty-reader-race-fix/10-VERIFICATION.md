---
phase: 10-pty-reader-race-fix
status: passed
verified_at: 2026-06-01
verifier: gsd-verifier (inline, sonnet-4-6 orchestrator)
score: 7/7
must_haves_passed: 7
must_haves_failed: 0
must_haves_uncertain: 0
gaps: []
---

# Phase 10: PTY Reader Race Fix — Verification

## Goal

Orphaned sessions cleanly terminate their PTY reader threads — a blocked `read()` is
interruptible, so the server's blocking-thread count stays bounded under repeated
session orphan/drop cycles.

## Verification Method

Goal-backward verification: starting from what must be TRUE for the goal to be achieved,
verifying each level against the actual codebase (not SUMMARY.md claims).

---

## Must-Have Truths Verification

### Plan 10-01 Truths

**T1: A reusable interruptible PTY reader exists behind a trait boundary (D-02)**
- Status: ✓ VERIFIED
- Evidence:
  - `crates/nosh-server/src/pty_io.rs` exists (289 lines)
  - `pub struct PtyReaderHandle` with `pub join: JoinHandle<()>` + `shutdown_tx: OwnedFd`
  - `pub fn start_interruptible_reader(master_raw_fd: i32, reader: PtyReader, out_tx: Sender<Vec<u8>>) -> anyhow::Result<PtyReaderHandle>` — public API usable from server.rs
  - `#[cfg(not(unix))]` stub returns error (D-02a — Windows placeholder)
  - Wiring: called at 2 sites in server.rs (confirmed by grep)

**T2: An async caller can signal shutdown and await clean thread exit**
- Status: ✓ VERIFIED
- Evidence:
  - `PtyReaderHandle::signal_shutdown(&self)` — writes 1 byte via `nix::unistd::write(&self.shutdown_tx, b"x")`; ignores EBADF
  - `PtyReaderHandle::shutdown_and_join(mut self)` — signal + `.await join`
  - `pub join: JoinHandle<()>` — exposed for `tokio::time::timeout(&mut reader_handle.join)` in server.rs
  - Both TransportLost arms in server.rs verified: `reader_handle.signal_shutdown()` then bounded join await BEFORE `registry.orphan(&slot)`

**T3: PTY master raw fd reachable via SessionSlot::master_raw_fd (without exposing Session::master)**
- Status: ✓ VERIFIED
- Evidence:
  - `Session::master_raw_fd(&self) -> Option<i32>` in session.rs — `#[cfg(unix)]`, delegates to `self.master.as_raw_fd()`; `grep -c 'pub master'` == 0 (field stays private)
  - `SessionSlot::master_raw_fd(&self) -> Option<i32>` in registry.rs — `#[cfg(unix)]`, locks session mutex briefly, calls `Session::master_raw_fd()`, no `.await` inside lock
  - Used in server.rs at both pump sites: `slot.master_raw_fd().expect("...")`

### Plan 10-02 Truths

**T4: Orphaning a session stops the PTY reader within one polling interval**
- Status: ✓ VERIFIED
- Evidence:
  - `pty_io::tests::reader_exits_on_shutdown_barrier` test passes (observed in test run)
  - Mechanism: `nix::poll([master_fd, pipe_read_fd], NONE)` — shutdown pipe byte makes pipe_read_fd readable; loop exits on `fds[1].any()` → clean break within one poll interval
  - D-04 test: N=10 cycles, each signals shutdown, AtomicUsize exit_count PRIMARY assert == N

**T5: After repeated create→orphan cycles, every reader thread exits (bounded thread count)**
- Status: ✓ VERIFIED
- Evidence:
  - D-04 `reader_exits_on_shutdown_barrier` test: 10 create→orphan cycles, `Arc<AtomicUsize>` counter, PRIMARY `assert_eq!(exit_count.load(Acquire), N)` — passed
  - No `RuntimeMetrics` dependency (confirmed grep)
  - `tokio::spawn` wrapper increments counter after join, ensures exit is counted only on actual thread exit

**T6: On reattach, the prior orphaned reader has exited before a fresh reader is cloned**
- Status: ✓ VERIFIED
- Evidence:
  - Both TransportLost arms (run_session ~559-573, run_reattach_session ~865-872): `reader_handle.signal_shutdown()` + `tokio::time::timeout(5s, &mut reader_handle.join).await` appears BEFORE `registry.orphan(&slot)`
  - Registry.orphan puts slot in Orphaned state; only after orphan can a reattach call `slot.clone_pty_reader()` — the sequencing guarantee holds

**T7: W2 input-writer handback is preserved unchanged (orphaned slots always have a usable writer)**
- Status: ✓ VERIFIED
- Evidence:
  - `in_rx.blocking_recv()` appears 2 times in server.rs (unchanged from baseline)
  - `return_pty_writer` appears 4 times in server.rs (unchanged from baseline)
  - Writer timeout-await at both TransportLost arms preserved AFTER reader await, BEFORE orphan
  - `input_writer.abort()` retained in post-match (harmless on non-TransportLost paths; writer loop already exited when `in_tx` is dropped)

### Roadmap Success Criteria

**SC#1: Dropping/orphaning a session reliably stops the PTY reader within one polling interval**
- Status: ✓ VERIFIED (see T4)

**SC#2: Blocking-thread count stays bounded after repeated orphan cycles**
- Status: ✓ VERIFIED (see T5)

**SC#3: `cargo test` passes with no regressions**
- Status: ✓ VERIFIED
- Evidence: `cargo test -p nosh-server` output: `test result: ok. 25 passed; 0 failed; 0 ignored` (25 tests including new `pty_io::tests::reader_exits_on_shutdown_barrier`)

---

## Artifact Verification (SDK)

### Plan 10-01 artifacts: 4/4 PASSED
- `crates/nosh-server/src/pty_io.rs` — ✓ exists, substantive (289 lines), wired (called in server.rs)
- `crates/nosh-server/src/session.rs` — ✓ exists, contains `master_raw_fd`
- `crates/nosh-server/src/registry.rs` — ✓ exists, contains `master_raw_fd`
- `crates/nosh-server/Cargo.toml` — ✓ exists, contains `"poll"` in nix features

### Plan 10-02 artifacts: 2/2 PASSED
- `crates/nosh-server/src/server.rs` — ✓ contains `start_interruptible_reader` (2 sites)
- `crates/nosh-server/src/pty_io.rs` — ✓ contains `AtomicUsize` (D-04 test)

---

## Security Invariants

- **No O_NONBLOCK / F_SETFL** set in pty_io.rs or server.rs (D-01 / T-10-03) — CLEAN
- **No master fd close** in pty_io.rs (T-10-02 / Pitfall 7 — no SIGHUP on orphan) — CLEAN
- **Single OwnedFd ownership** per pipe end (T-10-01 / Pitfall 4) — read_fd in blocking closure, write_fd in handle; both drop-close automatically
- **PollFds in same stack frame** as poll() call (Pitfall 5) — verified in unix_reader_loop

---

## Build and Test Results

```
cargo build -p nosh-server: Finished (0 errors, 0 warnings)
cargo test -p nosh-server: ok. 25 passed; 0 failed; 0 ignored
  including: pty_io::tests::reader_exits_on_shutdown_barrier
```

---

## Verdict

**PASSED** — Phase goal achieved. All 7 must-have truths verified. 3/3 roadmap success
criteria met. `cargo test` green. No regressions. Phase 10 closure complete.
