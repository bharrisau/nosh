# Phase 10: PTY Reader Race Fix - Context

**Gathered:** 2026-06-01
**Status:** Ready for planning

<domain>
## Phase Boundary

Orphaned sessions cleanly terminate their PTY reader threads. A blocked `read()` on the
PTY master must be interruptible so that, when a session is orphaned (transport lost, no
SIGHUP — the `MasterPty` is intentionally kept open), the server's output-pump reader
exits promptly instead of parking forever in `spawn_blocking`. The blocking-thread count
(tokio blocking pool) must stay bounded under repeated session orphan/drop cycles.

This is hardening of the existing v1.1 session pump (`crates/nosh-server/src/server.rs`).
It is NOT new feature work — no datagrams, no prediction, no protocol changes. Those are
Phases 11+.

</domain>

<decisions>
## Implementation Decisions

### Interrupt mechanism (D-01) — REVISED after research (2026-06-01)
- **D-01 (LOCKED):** Unix implementation uses a **self-pipe + `nix::poll`**: the
  `spawn_blocking` reader thread `poll()`s `[pty_master_fd, shutdown_pipe_read_fd]`; async
  teardown code writes one byte to `shutdown_pipe_write_fd` to wake the blocked `read()` so
  the thread exits cleanly. This is the mechanism named in REQUIREMENTS.md HARDEN-01.
  - Cargo.toml: add the `"poll"` feature to `nix`. No `O_NONBLOCK` on the master fd.
  - The writer loop (`server.rs:385`) is **left untouched** — no EAGAIN handling needed.
  - One interruptible blocking thread per *live* session; on orphan it exits within one
    polling interval, so the blocking-pool count stays bounded (success criterion #2).
- **D-01-history (why not AsyncFd):** `AsyncFd` + `O_NONBLOCK` was the initial lean (to
  eliminate the blocking thread entirely). Research (10-RESEARCH.md) verified the master fd
  IS exposable via `MasterPty::as_raw_fd()`, BUT `O_NONBLOCK` is a *shared* open-file-
  description property — setting it forces a retry-on-`EAGAIN` rewrite of the currently-
  correct W2 writer loop (`UnixMasterWriter` dups the same fd). The user's original AsyncFd
  rationale was Windows compatibility, which is handled by the D-02 trait boundary (AsyncFd
  is Unix-only regardless). Given the moot rationale and the extra writer blast radius, the
  user chose self-pipe + poll. AsyncFd is NOT to be implemented this phase.

### Portability (D-02)
- **D-02:** Implement the interrupt behind a **small trait / abstraction boundary** (an
  "interruptible PTY reader") so the native-Windows/ConPTY server (Phase 17 / M6) can slot
  in its own implementation (e.g. `CancelIoEx` / overlapped I/O / a cancellation event with
  `WaitForMultipleObjects`) without reworking the session pump.
- **D-02a (clarification):** `AsyncFd` is a Unix-only Tokio primitive — it does NOT compile
  on Windows. Cross-platform support comes from D-02's trait boundary, not from `AsyncFd`
  itself. M4 ships only the Unix impl; no Windows code is written this phase.

### Fix scope (D-03)
- **D-03:** Fix the **output reader** interrupt AND the **reattach reader-clone path**
  (`session.rs:167` `try_clone_reader`): ensure a reattach gets a fresh reader while the
  orphaned session's prior reader has cleanly exited (no leaked fd / no second live reader
  on the same master). Closes the "PTY master fd not closed on reattach (old reader clone
  still live)" leak noted in PITFALLS §6.
- **D-03a:** The **input writer** (`server.rs:385`) needs no change — it is already
  interruptible (dropping `in_tx` ends `in_rx.blocking_recv()` and the task hands the writer
  back to the slot). Confirm this remains true; do not regress the W2 writer-handback fix.

### Thread-count verification (D-04)
- **D-04:** Prove success criterion #2 with a **completion-barrier test**: loop N
  create→orphan cycles and assert every output-reader exits within one polling interval via
  a shared counter / `oneshot` signal. Deterministic, no unstable build flags.
- **D-04a:** Do NOT depend on `tokio_unstable` `RuntimeMetrics` (build-config change) or a
  purely time-based "exits within 1s" probe (flaky under CI load) as the primary assertion.
  A time bound may be used as a secondary safety net only.

### Claude's Discretion
- Exact trait shape / module placement for the interruptible-reader boundary.
- Buffer sizes, channel capacities, and how the async read task integrates with the existing
  `out_tx`/`out_rx` plumbing and the `tokio::select!` session loop.
- Test harness mechanics (how N orphan cycles are driven; whether a dedicated test PTY/shell
  stub is used) — provided the assertion is the deterministic completion barrier of D-04.

</decisions>

<specifics>
## Specific Ideas

- The fix must preserve the v1.1 orphan semantics exactly: on `TransportLost`, NO SIGHUP, the
  `MasterPty` stays open so the shell keeps running for later reattach (D-01/D-02, Pitfall #7).
  The reader interrupt must NOT close the master fd held by the slot — only stop the reader.
- Eliminate the existing `output_reader.abort()` anti-pattern — `abort()` on a `spawn_blocking`
  task has no effect once the blocking read has started (PITFALLS §6, table line 314).

</specifics>

<canonical_refs>
## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### The bug and the prescribed fix
- `.planning/research/PITFALLS.md` §"Pitfall 6: PTY Reader Zombie Race" — root cause
  (`spawn_blocking` + `abort()` cannot interrupt a blocked `read()`), the three candidate
  fixes (signal-fd/poll, `AsyncFd`+`O_NONBLOCK`, close-fd stopgap), failure cascades, and the
  validation checklist ("reader exits within 1s of orphan; thread count does not grow").

### Requirement
- `.planning/REQUIREMENTS.md` — **HARDEN-01** (line 16): the acceptance statement for this
  phase. Note it names the self-pipe/`nix::poll` mechanism explicitly; D-01 upgrades the
  primary approach to `AsyncFd`+`O_NONBLOCK` with that mechanism as the documented fallback.

### Code to change
- `crates/nosh-server/src/server.rs` (~lines 356-373) — the OUTPUT pump `spawn_blocking`
  reader loop that is the locus of the zombie; (~385-398) the INPUT writer (already
  interruptible — do not regress); post-loop orphan/teardown split (~499-604).
- `crates/nosh-server/src/session.rs` — `try_clone_reader` (line 167), `open` (returns
  reader at line 266), orphan/SIGHUP semantics.
- `crates/nosh-server/src/registry.rs` — slot `orphan` / writer-handback (`take_pty_writer` /
  `return_pty_writer`) so the reader fix stays consistent with the W2 writer invariant.

### Async-cancellation / tokio semantics
- tokio `spawn_blocking` abort semantics (cannot abort once started):
  https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html
- tokio `AsyncFd` (Unix-only): https://docs.rs/tokio/latest/tokio/io/unix/struct.AsyncFd.html

</canonical_refs>

<code_context>
## Existing Code Insights

### Reusable Assets
- `Session` (`session.rs`) owns `master: Box<dyn MasterPty + Send>` and exposes
  `try_clone_reader()` / writer take — the reader fix builds on these handles.
- The `out_tx`/`out_rx` mpsc channel (`server.rs:357`) and the `tokio::select!` session loop
  (`server.rs:409`) already separate "read PTY bytes" from "frame them to the client"; the
  new interruptible reader plugs into the same channel contract.
- Registry slot writer-handback (W2 fix) is the precedent pattern for "always leave the slot
  in a reattachable state on task exit" — mirror it for the reader.

### Established Patterns
- Orphan-on-`TransportLost` keeps `MasterPty` open, NO SIGHUP (D-01/D-02, Pitfall #7). The
  reader interrupt must respect this: stop reading without closing the master fd.
- Blocking I/O bridged to async via `spawn_blocking` + mpsc; D-01 moves the *reader* off this
  pattern (to `AsyncFd`) while the writer stays on it.

### Integration Points
- Output reader creation (`server.rs:359-373`), reattach pump's reader acquisition
  (`try_clone_reader`), and the orphan teardown block (`server.rs:~560-604`) are the three
  sites that must agree on the new interrupt/cleanup contract.

</code_context>

<deferred>
## Deferred Ideas

- Native Windows/ConPTY interruptible-reader implementation — Phase 17 / M6. This phase only
  defines the trait boundary; the Windows impl is out of scope.
- Datagram/state-sync emission from the PTY output callsite — Phase 12/13. This phase touches
  the same callsite but adds no datagram behavior.

</deferred>

---

*Phase: 10-pty-reader-race-fix*
*Context gathered: 2026-06-01*
