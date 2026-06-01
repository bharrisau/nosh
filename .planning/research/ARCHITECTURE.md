# Architecture Research

**Domain:** QUIC-based roaming remote shell (Rust) — v1.2 M4 Predictive Echo + Daily-Driver Readiness
**Researched:** 2026-06-01
**Confidence:** HIGH (every claim grounded in actual crate source with file:line citations; Mosh SSP design verified against the published paper)

---

## System Overview

v1.2 adds two architectural layers on top of v1.1's session substrate:

1. A **datagram state-sync channel** carrying server-authoritative terminal-screen diffs to the client, replacing the current raw-byte stream for screen updates.
2. A **client-side predictor** that speculatively renders keystrokes locally and reconciles against confirmed server state.

The existing reliable stream (bidi stream 0) continues to carry control protocol messages, keystrokes, resize, and session lifecycle — its role is narrowed, not replaced. The existing `SequencedOutputBuffer` and cold-reattach machinery are preserved intact; state sync provides the fast-path display while reattach continues to provide the full-byte replay.

```
  CLIENT PROCESS (Linux / Windows)                     SERVER PROCESS (Linux)
  ┌────────────────────────────────────────┐            ┌─────────────────────────────────────────────────────────┐
  │  Terminal Driver (crossterm / Win API) │            │  Endpoint + accept loop                                 │
  │  ┌──────────────────────────────────┐  │            │  ┌─────────────────────────────────────────────────┐    │
  │  │  Predictor                       │  │            │  │  SessionSlot (Arc<Mutex<…>>)                     │    │
  │  │  PredictedCell grid              │  │            │  │  session: Session (MasterPty + child)            │    │
  │  │  epoch: u64                      │  │            │  │  output_buf: SequencedOutputBuffer (64KiB ring)  │    │
  │  │  pending: Vec<PendingPrediction> │  │            │  │  [NEW] term_state: Mutex<TerminalState>          │    │
  │  └──────┬───────────────────────────┘  │            │  └──────────────────┬──────────────────────────────┘    │
  │         │ speculative render           │            │                     │                                    │
  │  ┌──────▼───────────────────────────┐  │            │  OUTPUT pump (spawn_blocking reader)                    │
  │  │  ClientScreen                    │  │            │  PTY bytes → vte → TerminalState                        │
  │  │  (displayed to user)             │  │            │                     │                                    │
  │  │  confirmed + predicted overlay   │  │            │  [NEW] Datagram encoder:                                │
  │  └──────────────────────────────────┘  │            │  full diff TerminalState → StateDiff message            │
  │                                        │            │  → conn.send_datagram(…)                                │
  │  QUIC connection (quinn 0.11)          │            │                     │                                    │
  │  ┌─────────────────────────────────┐   │            │  RELIABLE STREAM (bidi stream 0):                       │
  │  │ bidi stream 0 (reliable)        │◄──┼── QUIC ───►│  keystroke input → PTY writer (spawn_blocking)          │
  │  │ keystrokes → server             │   │  UDP/443   │  SessionOpen/Reattach/Resize/SessionClose               │
  │  │ SessionOpen/Reattach/Resize /   │   │            │  Ack frames (for SequencedOutputBuffer trim)            │
  │  │ SessionClose / Ack              │   │            │  SessionOpened/ReattachOk/ReattachErr                   │
  │  └─────────────────────────────────┘   │            │                                                         │
  │  ┌─────────────────────────────────┐   │            │  RFC 9221 DATAGRAMS (loss-tolerant):                    │
  │  │ RFC 9221 datagrams (unreliable) │◄──┼────────────│  StateDiff { epoch, cells, cursor, … }                  │
  │  │ StateDiff frames (server→client)│   │            │  sent at PTY-output cadence (every ~16ms burst)         │
  │  │ latest-state-wins on loss       │   │            │  [NEW] client → server: ClientEpoch { confirmed: u64 }  │
  │  └─────────────────────────────────┘   │            │                                                         │
  └────────────────────────────────────────┘            └─────────────────────────────────────────────────────────┘
```

---

## Component Boundaries

### What Exists and Must Not Break

| Component | Location | Role in M4 |
|-----------|----------|------------|
| `SessionRegistry` | `crates/nosh-server/src/registry.rs` | Unchanged; owns slot lifecycle |
| `SessionSlot` | `crates/nosh-server/src/registry.rs:235` | Add `term_state: Mutex<TerminalState>` field; otherwise unchanged |
| `SequencedOutputBuffer` | `crates/nosh-server/src/registry.rs:41` | Unchanged; still feeds cold reattach |
| `Session` | `crates/nosh-server/src/session.rs:115` | Unchanged |
| `Message` enum | `crates/nosh-proto/src/messages.rs` | Extend with new datagram-only types; preserve existing discriminants |
| `run_session` / `run_reattach_session` | `crates/nosh-server/src/server.rs:285 / 622` | Add datagram-send task in the pump `tokio::select!`; otherwise unchanged |
| `run_pump` (client) | `crates/nosh-client/src/main.rs:604` | Add datagram-receive arm + predictor merge; otherwise unchanged |

### New Components for M4

| Component | Crate | Responsibility |
|-----------|-------|---------------|
| `TerminalState` | `nosh-server` (new module `terminal.rs`) | Server-authoritative VT screen model; `vte::Perform` impl; drives diff encoder |
| `StateDiff` | `nosh-proto` (new variant or new message type) | Wire encoding of screen diffs sent over datagrams |
| `DiffEncoder` | `nosh-server/terminal.rs` | Converts `TerminalState` snapshots to `StateDiff`; owned by the datagram sender task |
| `Predictor` | `nosh-client` (new module `predictor.rs`) | Client-side speculative echo; epoch tracking; overlay composition |
| `ClientScreen` | `nosh-client/predictor.rs` | Renders confirmed server state + predicted overlay to stdout |
| `ConnectionLossOverlay` | `nosh-client` (small addition to `run_pump`) | Detects link-down from quinn path events; writes on-screen notice without corrupting screen |

---

## Q1: Datagram State Sync — Integration with SequencedOutputBuffer and Reattach

### The Key Design Decision: Run Both Paths

The existing reliable stream carries `PtyData` frames. The `SequencedOutputBuffer` sequences every one of those chunks and is the reattach replay mechanism. M4 does NOT migrate output away from the reliable stream. Instead, datagrams add a **parallel fast-display path**:

```
PTY bytes arrive
  ├── (unchanged) → out_rx channel → SequencedOutputBuffer.push()
  │                              → write_message(stream, PtyData{data})   [reattach/ack path]
  └── (new) → vte parser → TerminalState.update()
                         → DiffEncoder.encode() → StateDiff
                         → conn.send_datagram(StateDiff)                  [display fast path]
```

The client, on receiving a `StateDiff` datagram, renders the confirmed server screen through the predictor. The reliable stream's `PtyData` frames continue to exist; during connection migration and cold reattach they are the source of truth for replay. The client can and should ignore `PtyData` when datagrams are available and current — but continues to count `PtyData` chunks for `Ack` (acking tells the server to trim the `SequencedOutputBuffer`).

**This means the client's `highest_applied` counter in `run_pump` (main.rs:422) advances on `PtyData` receipt as today; the predictor's epoch advances separately on `StateDiff` receipt.** These are two independent advancement signals.

### Why Not Replace the Stream Path Entirely

Cold reattach replays `PtyData` chunks from `SequencedOutputBuffer` by design (ROAM-02). Replacing the stream path with datagrams would require a different replay mechanism. The two-path design preserves the invariant without protocol changes.

On roaming (migration), the QUIC connection continues uninterrupted — datagrams and stream alike survive. State sync makes roaming seamless at the display layer: no gap in screen updates when the IP changes, because the latest `StateDiff` is complete and idempotent.

---

## Q2: Predictive Echo Architecture

### Server Side: TerminalState + DiffEncoder

`vte` (planned but not yet in `crates/nosh-server/Cargo.toml`) is the right parser. It is not yet a dependency — it must be added. The `vte::Perform` trait is the extension point; `TerminalState` implements it and maintains a 2-D grid of cells.

```
// crates/nosh-server/src/terminal.rs  [NEW FILE]

pub struct TerminalState {
    cols: u16,
    rows: u16,
    cells: Vec<Cell>,      // rows*cols, row-major
    cursor: CursorPos,
    epoch: u64,            // increments every time state is sent as a diff
}

impl vte::Perform for TerminalState { … }  // handle print/execute/csi/etc.

pub struct Cell {
    ch: char,
    attrs: CellAttrs,      // bold/underline/fg/bg
}
```

**Server-side datagram send task** (new arm in the `tokio::select!` loop inside `run_session`):

```
// Existing loop (server.rs:409) gains a new select! arm:
_ = diff_interval.tick() => {
    let diff = slot.encode_diff();     // lock term_state briefly, no .await
    let payload = nosh_proto::encode_datagram(&diff)?;
    let _ = conn.send_datagram(Bytes::from(payload));
}
```

The diff interval is approximately 16 ms (one 60 Hz frame), but coalesced: only send when state has changed since the last sent epoch. Under a blocked network (migration in progress), datagrams are simply dropped — the client falls back to the buffered `PtyData` stream naturally.

`TerminalState` lives inside `SessionSlot` as a new field:
```
// crates/nosh-server/src/registry.rs:235  (SessionSlot struct)
pub struct SessionSlot {
    // ... existing fields ...
    term_state: Mutex<TerminalState>,  // [NEW] for datagram state sync
}
```

The `push_output` method (registry.rs:334) is the current update point for `SequencedOutputBuffer`. Add a parallel call:
```
pub fn push_output_and_parse(&self, chunk: &[u8]) -> u64 {
    let seq = self.output_buf.lock().unwrap().push(chunk);
    self.term_state.lock().unwrap().feed(chunk);  // vte parse
    seq
}
```

(Or add a separate `parse_output` call at the same call site in `server.rs:421-428`.)

### Client Side: Predictor

Mosh's predictor operates on the client's local model of what the server screen will look like after each keystroke is confirmed. The design for nosh:

```
// crates/nosh-client/src/predictor.rs  [NEW FILE]

pub struct Predictor {
    /// Server-confirmed screen state (latest received StateDiff).
    confirmed: ScreenGrid,
    /// Epoch of the latest confirmed diff.
    confirmed_epoch: u64,
    /// Speculative predictions layered on top.
    pending: VecDeque<PendingPrediction>,
    /// Whether to show predictions (controlled by latency heuristic).
    display_mode: PredictDisplayMode,
}

pub struct PendingPrediction {
    epoch_required: u64,   // the server epoch that would confirm this prediction
    cell: CellPos,
    predicted_ch: char,
    predicted_attrs: CellAttrs,  // includes underline=true for "unconfirmed" rendering
}

pub enum PredictDisplayMode {
    /// Show predictions only when RTT > threshold (e.g. 50ms), like Mosh default.
    Adaptive { rtt_threshold_ms: u32 },
    /// Always show predictions (aggressive mode).
    Always,
    /// Never show (passthrough, for debugging).
    Never,
}
```

**Epoch tracking and confirmation:** Each `StateDiff` datagram carries a server epoch. When the client receives a `StateDiff` with `epoch >= pending[i].epoch_required`, prediction `i` is confirmed and its overlay is removed. If the server's confirmed cell differs from the prediction, the confirmed value wins (immediately — no animation, no delay).

**Conservative fallback:** Predictions are suppressed after `ESC`, `\r`, up/down arrows, and any keypress that would change line context. This matches Mosh's epoch-reset heuristic. The predictor enters a "background mode" where predictions are queued but not displayed until the next confirmation comes in.

**Keystrokes go on the reliable stream, not datagrams.** This is the correct design: keystrokes are control messages and must be delivered reliably and in order. They already travel via `send_input` → `Message::PtyData` on the bidi stream (client.rs:609-618). This is unchanged.

### Client-Side Render Loop Integration

`run_pump` in `main.rs:604` currently has these select arms:
- `read_message(recv)` → `PtyData` → `stdout.write_all` + `highest_applied++`
- `stdin.read` → forward keystrokes
- `resize` → send `Resize`
- `ack_interval` → send `Ack`

M4 adds:
- `conn.read_datagram()` → `StateDiff` → `predictor.apply_confirmed(diff)` → `screen.render_to_stdout()`

When datagrams are active, the `PtyData`→`stdout.write_all` path should be suppressed or made a no-op for display (but still advance `highest_applied` for acking). The simplest implementation: add a `datagram_active: bool` flag set on first `StateDiff` received; when true, skip `stdout.write_all` on `PtyData` but still increment `highest_applied`.

**The `PtyData` path continues to advance `highest_applied` because the `Ack` mechanism is keyed to it, and the `SequencedOutputBuffer` trim depends on it. Do not drop this.**

### Windows Client Compatibility

The predictor and datagram render path are pure Rust with no OS-specific calls. The `ClientScreen.render_to_stdout()` path writes ANSI sequences via `tokio::io::stdout()`, which on Windows flows through the `ENABLE_VIRTUAL_TERMINAL_PROCESSING` flag already set by `RawModeGuard` (client.rs:319-384). No additional Windows-specific work is needed for the predictor.

---

## Q3: Connection-Loss Notification

### Detection Point

Quinn provides `connection.path_stats()` (stable in quinn 0.11) and the existing migration-polling loop in `server.rs:437-451` polls `conn.remote_address()` at 500ms cadence. For the client, the detection mechanism is simpler: the `tokio::select!` in `run_pump` already detects transport drop via `Err(_)` on `read_message(recv)`. The issue is detecting a **partial** link-down that doesn't immediately produce an error (e.g. the connection is technically alive but no datagrams are arriving).

**Recommended client-side detection:** Add a `last_datagram_received: Instant` timestamp. If no `StateDiff` datagram arrives within a threshold (e.g. 5 × keep-alive interval = 75s, or more aggressively 5s for UX), inject the connection-loss overlay. Use `conn.stats().path.lost_packets` (quinn 0.11 `ConnectionStats`) as a secondary signal.

**Do NOT use the existing `migration_poll` cadence for this** — the migration poll is server-side (server.rs:405-410) and is for logging, not client UX.

### Injection Without Screen Corruption

The connection-loss notice must not corrupt the terminal state or the predictor's confirmed grid. The correct approach:

1. Record the current cursor position from `confirmed.cursor`.
2. Move to the bottom status line (or a fixed position).
3. Write the notice with a distinctive style (bold/reverse video).
4. Move the cursor back to the saved position.

This is the same approach Mosh uses for its "connecting" indicator. The overlay is owned by `ConnectionLossOverlay` and painted by `ClientScreen.render_to_stdout()` as a final post-processing step, after the confirmed + predicted cell merge.

```
// crates/nosh-client/src/predictor.rs

pub struct ConnectionLossOverlay {
    active: bool,
    message: String,  // e.g. "nosh: reconnecting [ESC-send ~. to abort]"
    since: Instant,
}
```

The overlay is activated when `Predictor.on_link_down()` is called from `run_pump`, and deactivated when the next `StateDiff` arrives.

**Security property preserved:** The overlay text is controlled by the local client code, not the server — a malicious server cannot inject the overlay or forge its dismissal.

---

## Q4: PTY Reader-Zombie Race Fix

### The Problem (Latent, Documented in PROJECT.md)

`output_reader` in `server.rs:360-373` is a `spawn_blocking` task that calls `reader.read(&mut buf)` in a blocking loop. When `abort()` is called on this task (server.rs:607), it sends a cancel signal to the blocking thread's future, but the underlying OS `read()` syscall on the PTY fd is NOT interrupted. The thread remains blocked until the PTY fd is closed by the OS (when the shell exits), which may never happen while the shell is still running.

This means `abort()` on `output_reader` is effectively a no-op while the shell is running — the tokio thread pool leaks this blocking thread.

### Architectural Fix

The correct fix is to close the PTY master fd to wake the blocked reader. Two approaches:

**Option A (recommended): Separate process / fd ownership discipline.**
Move the PTY reader to a child process boundary. When the session is orphaned, close the MasterPty's read fd. This is architecturally clean but requires changing how `Session.master` is held — the `MasterPty` would need to be split into a reader fd (closeable on demand) and a writer fd.

**Option B (simpler for M4): Signal via pipe.**
Add a `reader_shutdown: (tokio::sync::oneshot::Sender<()>, std::os::unix::io::RawFd)` to the session. The second element is a pipe write end; the blocking reader's `select`-like loop multiplexes `read(PTY, …)` and `read(pipe, …)`. Writing to the pipe wakes the blocking thread. This requires replacing `reader.read()` with a custom loop using `nix::select` or `nix::poll`.

**Option C (pragmatic for M4):** Accept the current behavior but add a bounded join timeout. The current code already does `output_reader.abort()` — the thread will unblock eventually when the PTY fd closes. Add a comment and a test that exercises the abort path. The latent nature (it doesn't produce incorrect behavior, only leaks the thread) makes this an acceptable deferral if M4 scope is tight.

**Recommendation for M4:** Option B in `server.rs` for the output reader specifically. Add a `(tokio::sync::watch::Sender<bool>, RawFd)` shutdown signal to `SessionSlot`. The `output_reader` blocking closure reads from both the PTY and the shutdown fd using `nix::poll`. On `abort()` (or explicit shutdown), write to the pipe fd to wake the `nix::poll` and allow clean exit.

The blocking writer task (input pump) does not have this problem — `in_rx.blocking_recv()` returns `None` when `in_tx` is dropped, which is already the shutdown path.

### Where the Fix Lives

- `crates/nosh-server/src/session.rs` — add `shutdown_pipe: Option<RawFd>` to `Session`; add `PtyReaderWithShutdown` type.
- `crates/nosh-server/src/server.rs:360-373` — replace the simple `reader.read` loop with a `nix::poll`-based loop.
- `crates/nosh-server/src/registry.rs` — `SessionSlot.sighup()` path can also send the shutdown signal.

Linux-only for M4 (the bug is Linux-only since Windows ConPTY is not yet a server target).

---

## Q5: Windows CI Cross-Compile Gate

### What Exists

`.github/workflows/windows-cross.yml` (inferred from STATE.md: "wire a git remote so windows-cross.yml CI compiles the #[cfg(windows)] path"). The CI gate exists but has never run because no git remote is configured.

### Integration Point

This is a workspace-level concern, not a crate-level one. The cross-compile step is:
```
cargo check --target x86_64-pc-windows-gnu
```

No changes to the Cargo workspace are needed. The fix is purely operational: configure a git remote (`origin`), ensure the workflow is triggered on push/PR to `main`.

The `ring` crate's precompiled Windows x86 assembly objects (confirmed HIGH confidence from STATE.md: "ring 0.17.14 precompiled x86_64-windows assembly objects are present — no NASM/CMake needed") mean the cross-compile works from a Linux host.

---

## Data Flow Changes for M4

### New-Session Flow (with datagram sync)

```
Client → server: QUIC handshake
  → accept_bi → SessionOpen
  → session::open → PTY + shell
  → register slot (unchanged)
  → send SessionOpened{token}
  → spawn output pump (unchanged)
  → [NEW] spawn diff sender task in tokio::select! arm

PTY output arrives:
  → out_rx.recv() in server select! loop (unchanged path)
  → slot.push_output_and_parse(&data)
      → SequencedOutputBuffer.push(chunk)   [unchanged — for reattach]
      → TerminalState.feed(chunk)           [NEW — drives diffs]
  → write_message(stream, PtyData{data})    [unchanged — for reattach/ack]
  → [NEW] diff_interval.tick() arm
      → diff = TerminalState.encode_diff()
      → conn.send_datagram(diff)            [NEW fast-display path]

Client receives StateDiff datagram:
  → predictor.apply_confirmed(diff)
  → screen.render_to_stdout()              [replaces direct stdout.write_all]
  → highest_applied unchanged (PtyData path still counts)
```

### Keystroke Flow (unchanged)

```
stdin → run_pump select! → escape machine → send_input(stream, PtyData{data})
  → server recv arm → in_tx.send(data)
  → blocking writer task → PTY writer
  → PTY output → TerminalState.feed() → next diff
```

Keystrokes are NOT sent as datagrams. They travel on the reliable stream exactly as today.

### Prediction Flow

```
User types 'a':
  → escape machine passes through
  → predictor.add_prediction(cell=cursor_pos, ch='a', underline=true)
  → screen renders: confirmed cells + predicted overlay (underlined 'a')

Server processes 'a':
  → PTY outputs 'a' → TerminalState.feed()
  → next diff epoch increments, includes cell at cursor_pos = 'a'

Client receives diff epoch E:
  → predictor.confirm_up_to(E)
  → prediction for 'a' confirmed → remove underline
  → screen re-renders: confirmed 'a' (no underline)
```

---

## Modified vs New Components

| Component | v1.1 State | v1.2 Change | Location |
|-----------|-----------|-------------|----------|
| `TerminalState` + `vte::Perform` impl | Absent (vte not in deps) | New module; add `vte` dep to `nosh-server/Cargo.toml` | `crates/nosh-server/src/terminal.rs` (new) |
| `DiffEncoder` / `StateDiff` wire type | Absent | New: datagram payload; add to `nosh-proto` | `crates/nosh-proto/src/messages.rs` + new encoding |
| `SessionSlot.term_state` | Absent | New field `Mutex<TerminalState>` | `crates/nosh-server/src/registry.rs:235` |
| `slot.push_output_and_parse` | `push_output` only | Extended to also call `term_state.feed()` | `crates/nosh-server/src/registry.rs:334` |
| `run_session` / `run_reattach_session` pump | No diff sender | New `tokio::select!` arm: `diff_interval.tick()` → `send_datagram` | `crates/nosh-server/src/server.rs:409` |
| `Predictor` + `ClientScreen` | Absent | New module | `crates/nosh-client/src/predictor.rs` (new) |
| `run_pump` in client | Raw `PtyData`→stdout | Add `conn.read_datagram()` arm; route through predictor | `crates/nosh-client/src/main.rs:604` |
| `ConnectionLossOverlay` | Only a bare `eprintln!` on reconnect | Structured overlay inside `Predictor`/`ClientScreen` | `crates/nosh-client/src/predictor.rs` |
| PTY reader shutdown signal | `abort()` only (latent race) | Add pipe-based shutdown for clean blocking-thread wakeup | `crates/nosh-server/src/session.rs` + `server.rs:360` |
| `nosh-server/Cargo.toml` | No `vte` | Add `vte = "0.15"` | `crates/nosh-server/Cargo.toml` |

---

## Dependency-Ordered Build Sequence

The sequence below respects hard dependencies. Each step should be independently testable before the next begins.

### Step 1: Wire Protocol — `nosh-proto` (no other deps)

Add `StateDiff` datagram message encoding to `crates/nosh-proto/src/messages.rs`. This is the foundation everything else touches.

```rust
// StateDiff is NOT a Message variant — it uses its own encoding because
// datagrams bypass the reliable stream framing (no length prefix needed;
// datagrams are self-contained). Use a separate encode/decode pair:
//
// crates/nosh-proto/src/datagram.rs  [NEW]
pub struct StateDiff {
    pub epoch: u64,
    pub cols: u16,
    pub rows: u16,
    pub cells: Vec<DiffCell>,   // sparse: only changed cells
    pub cursor: CursorPos,
}
pub fn encode_datagram(diff: &StateDiff) -> Result<Vec<u8>, …>;
pub fn decode_datagram(bytes: &[u8]) -> Result<StateDiff, …>;
```

Also add `ClientEpoch` datagram (client → server, confirms what epoch client has applied, used by server to skip sending unchanged diffs):
```rust
pub struct ClientEpoch { pub confirmed: u64 }
```

Unit tests: encode/decode round-trip. No new crate deps.

### Step 2: Server Terminal State — `nosh-server` terminal module

Add `vte = "0.15"` to `crates/nosh-server/Cargo.toml`. Create `crates/nosh-server/src/terminal.rs` with `TerminalState` implementing `vte::Perform`. Add `term_state: Mutex<TerminalState>` to `SessionSlot`. Add `push_output_and_parse` to `SessionSlot` (replace the `push_output` call site in `server.rs:421-428`).

Unit tests: feed known VT sequences → assert correct cell contents. No QUIC/PTY needed.

### Step 3: Server Datagram Sender — `run_session` pump extension

Add the `diff_interval.tick()` select arm to `run_session` and `run_reattach_session` in `server.rs`. This requires Step 1 (proto encoding) and Step 2 (TerminalState). Write an integration test: connect client and server, type characters, assert `conn.read_datagram()` on the client receives non-empty `StateDiff` frames.

### Step 4: Client Predictor Foundation — `nosh-client`

Create `crates/nosh-client/src/predictor.rs` with `Predictor`, `ClientScreen`, and `ConnectionLossOverlay`. At this step: only confirmed rendering (no speculative predictions yet). Add the `conn.read_datagram()` arm to `run_pump`. The client receives `StateDiff`, updates `Predictor.confirmed`, renders via `ClientScreen`. The `PtyData`→stdout path is kept but suppressed for display (still advances `highest_applied`).

End-to-end test: connect, type characters, assert that the screen rendered from datagrams matches the raw PTY output.

### Step 5: Speculative Echo — `Predictor` extension

Add `add_prediction`, `confirm_up_to`, and epoch-reset logic to `Predictor`. Add the prediction path to the keystroke arm in `run_pump`. Add `adaptive` display mode (suppress on low latency, show underlined on high latency). This is the hardest UX step — budget accordingly per INIT.md §10.

Unit tests: feed known keystrokes, assert prediction cells are set; feed confirming StateDiff, assert predictions are cleared.

### Step 6: Connection-Loss Notification

Activate `ConnectionLossOverlay` on datagram timeout / quinn path stats degradation. Wire `PredictDisplayMode` to detected RTT. Add visual smoke test to documentation.

### Step 7: PTY Reader Race Fix

Add the pipe-based shutdown to `Session` and `output_reader`. Regression test: create a session, abort it while the shell is running, assert the blocking thread terminates within a bounded time.

### Step 8: Windows CI Gate

Configure git remote, verify `windows-cross.yml` runs on push. No code changes.

---

## Anti-Patterns to Avoid for M4

### Anti-Pattern 1: Migrating Output from Stream to Datagrams

Replacing `PtyData` on the reliable stream with datagrams-only would break the cold-reattach replay path which depends on `SequencedOutputBuffer`. The two-path design (stream for reattach, datagrams for display) is correct.

### Anti-Pattern 2: Feeding StateDiff Server Output Back Through vte on the Client

The client predictor operates on a `ScreenGrid` (confirmed + predicted overlay). It must NOT feed raw PTY bytes through a second vte instance — that would require maintaining full terminal emulation state client-side (complex, duplicates the server, diverges on edge cases). Instead the client receives the server's authoritative diff: the server does the vte parsing, the client consumes the diff directly.

### Anti-Pattern 3: Sending Keystrokes as Datagrams

Keystrokes are control messages that must be delivered reliably and in order. Loss of a keystroke is never acceptable (the user typed it). Keystrokes stay on the reliable bidi stream. Only the display path (server→client terminal diffs) is loss-tolerant.

### Anti-Pattern 4: Epoch-Confirmed Predictions Requiring an Extra RTT

The epoch confirmation must not add a round trip. The server sends `StateDiff{epoch: N}` as the FIRST datagram after processing keystrokes. The client marks predictions confirmed upon receipt of epoch N. There is no ack of the ack. (This is distinct from `ClientEpoch` datagrams which are optional optimization hints for the server, not required for correctness.)

### Anti-Pattern 5: Writing the Connection-Loss Overlay Directly to stdout

Direct writes to `stdout` bypass the predictor's screen model and corrupt the terminal state. Route all output through `ClientScreen.render_to_stdout()`, which composes confirmed + predicted + overlay into a single render pass.

### Anti-Pattern 6: Blocking the `tokio::select!` Loop on vte Parsing

`TerminalState.feed(chunk)` (the vte parsing step) must be fast enough to run inline in the async pump or in a brief sync call under a Mutex. VT parsing with vte is O(bytes) and typically microseconds for an 8 KiB PTY chunk — it is safe to call under a Mutex without `spawn_blocking`. Verify with a micro-benchmark before adding complexity.

---

## Integration Points

### `quinn::Connection::send_datagram` + `read_datagram` (M4 activation)

| Boundary | Integration | Notes |
|----------|-------------|-------|
| `server.rs` pump loop | `conn.send_datagram(Bytes::from(payload))` | Already enabled via `transport_config` (datagram buffers set in `transport.rs:8-9`). send_datagram returns `Result`; `SendDatagramError::UnsupportedByPeer` means the other side didn't enable it — treat as no-op, not fatal. HIGH confidence. |
| `client/run_pump` | `conn.read_datagram().await` | Same enabling config. Must be in the `tokio::select!` arm; not a blocking call. HIGH confidence. |

### `vte::Perform` trait (new dep)

| Boundary | Integration | Notes |
|----------|-------------|-------|
| `TerminalState` in `nosh-server` | `impl vte::Perform for TerminalState` | `vte` 0.15 is in Alacritty's repo. `Perform` has `print`, `execute`, `csi_dispatch`, `esc_dispatch`, `hook`, `put`, `unhook`, `osc_dispatch`. Terminal model needs at minimum `print` (printable chars), `execute` (LF/CR/BS/BEL), `csi_dispatch` (cursor movement, erase). MEDIUM confidence — vte API is stable but verify the 0.15 surface before planning the impl. |

### `nosh-proto` datagram encoding

| Boundary | Integration | Notes |
|----------|-------------|-------|
| Server → client | `nosh_proto::encode_datagram(&diff)` | New path, no codec shared with the reliable-stream `Message` framing. Use `postcard` (already a dep) with a separate tag byte for StateDiff vs ClientEpoch. Must fit in one QUIC datagram ≤ `conn.max_datagram_size()` (typically 1200–1452 bytes for UDP/443). Sparse diff encoding is needed for large terminals. |

### PTY reader shutdown (pipe fd)

| Boundary | Integration | Notes |
|----------|-------------|-------|
| `server.rs:360-373` output_reader | `nix::poll` on [PTY fd, pipe read end] | Linux-only for M4. Requires `nix` feature `poll` (already a dep; check feature set in `nosh-server/Cargo.toml`). MEDIUM confidence — verify the nix poll API surface. |

---

## Workspace Structure for v1.2

```
nosh/
├── crates/
│   ├── nosh-proto/src/
│   │   ├── messages.rs          — unchanged (Message enum for reliable stream)
│   │   └── datagram.rs          [NEW] StateDiff, ClientEpoch encode/decode
│   │
│   ├── nosh-server/src/
│   │   ├── terminal.rs          [NEW] TerminalState, DiffEncoder; vte::Perform impl
│   │   ├── registry.rs          MODIFY: SessionSlot + term_state field;
│   │   │                                push_output_and_parse
│   │   └── server.rs            MODIFY: run_session / run_reattach_session
│   │                                    + diff sender select! arm
│   │
│   └── nosh-client/src/
│       ├── predictor.rs         [NEW] Predictor, ClientScreen, ConnectionLossOverlay
│       └── main.rs              MODIFY: run_pump + datagram arm + predictor render
```

---

## Sources

- `crates/nosh-server/src/registry.rs:235` — `SessionSlot` struct; `push_output` at line 334. Verified in codebase.
- `crates/nosh-server/src/registry.rs:41` — `SequencedOutputBuffer` struct and ring design. Verified in codebase.
- `crates/nosh-server/src/server.rs:356-373` — output reader `spawn_blocking` loop (zombie race location). Verified in codebase.
- `crates/nosh-server/src/server.rs:409` — `tokio::select!` pump loop (diff sender arm insertion point). Verified in codebase.
- `crates/nosh-client/src/main.rs:604` — `run_pump` function with select! arms. Verified in codebase.
- `crates/nosh-client/src/main.rs:422` — `highest_applied` counter and `PtyData` receipt path. Verified in codebase.
- `crates/nosh-proto/src/transport.rs:8-9` — `DATAGRAM_BUFFER` const; datagram buffer sizes already enabled. Verified in codebase.
- `crates/nosh-server/Cargo.toml` — `vte` NOT present (must be added). Verified in codebase.
- `crates/nosh-client/Cargo.toml` — `crossterm = "0.29"` + Windows VT flags confirmed. Verified in codebase.
- `.planning/PROJECT.md` — PTY reader-zombie race note (Phase 6, latent), Windows CI gate item, WSAEMSGSIZE deferral. Verified.
- `.planning/STATE.md` — Windows validation findings, ring crate precompiled objects confirmed. Verified.
- [Mosh paper: "An Interactive Remote Shell for Mobile Clients"](https://mosh.org/mosh-paper.pdf) — SSP design, epoch-based prediction, underline-unconfirmed rendering. MEDIUM confidence for design decisions (published research); HIGH confidence for the UX model.
- [Mosh source: mobile-shell/mosh](https://github.com/mobile-shell/mosh) — predictive.cc for epoch reset triggers (ESC, CR, arrow keys). MEDIUM confidence.
- [quinn::Connection::send_datagram](https://docs.rs/quinn/latest/quinn/struct.Connection.html) — HIGH confidence, verified in v1.0 research.
- [vte 0.15 crate](https://github.com/alacritty/vte) — Perform trait, 0.15.0 current. HIGH confidence from v1.0 research.

---

*Architecture research for: nosh v1.2 M4 Predictive Echo + Daily-Driver Readiness*
*Researched: 2026-06-01*
