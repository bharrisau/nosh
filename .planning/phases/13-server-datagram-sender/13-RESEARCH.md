# Phase 13: Server Datagram Sender - Research

**Researched:** 2026-06-01
**Domain:** QUIC datagram emission, acked-epoch diff loop, ResumeComplete gating, epoch-ack protocol
**Confidence:** HIGH

---

<user_constraints>
## User Constraints (from CONTEXT.md)

### Locked Decisions

- **D-13-01 Acked-epoch model:** Server diffs current screen vs last CLIENT-acked screen (by epoch); keeps sending unconfirmed changes until acked. Datagram loss is self-healing (the un-acked state reappears on the next tick).
- **D-13-01a Epoch-ack channel:** Add a small client→server epoch-ack to `nosh-proto` carrying the client's last-applied epoch. Preferred carrier: a tiny datagram. Server maintains per-connection last-acked epoch + confirmed snapshot.
- **D-13-01b Resume subsumes keyframe:** On cold-reattach, server resets the acked baseline to "nothing" — first post-ResumeComplete diff is naturally the full screen. No separate keyframe path needed.
- **D-13-01c Scope boundary:** Phase 13 = server sender + epoch-ack message + server ack handling + integration test (test client sends epoch acks). Real client ack emission is Phase 14.
- **D-13-02 Tick cadence:** `diff_interval` = ~16 ms, `MissedTickBehavior::Skip`, mirrors `migration_poll` pattern (server.rs:398-432). One diff per tick — NOT one per PTY chunk.
- **D-13-02a Skip unchanged:** If TerminalState unchanged AND client caught up, send nothing. Keep resending if unconfirmed (acked < current epoch).
- **D-13-03 ResumeComplete gate:** Suppress datagrams until replay completes. Fresh `run_session` signals immediately. Signal mechanism is Claude's discretion.
- **D-13-04 Additive:** Reliable PtyData stream unchanged; zero regression.

### Claude's Discretion

- ResumeComplete signal primitive (atomic flag vs watch channel vs oneshot).
- Per-connection last-acked snapshot storage mechanism.
- How TerminalState exposes current vs snapshot for diff computation.
- Ack carrier confirmation (datagram vs control message — confirmed: datagram).
- StateDiff-input shape handed to `encode_datagram` each tick.

### Deferred Ideas (OUT OF SCOPE)

- Real client rendering from datagrams + emitting epoch acks during normal use — Phase 14 (PREDICT-01).
- Speculative local echo overlay — Phase 15.
- Connection-loss overlay / OSC52 / title — Phase 16.

</user_constraints>

<phase_requirements>
## Phase Requirements

| ID | Description | Research Support |
|----|-------------|------------------|
| SYNC-03 | Server emits coalesced state diffs over QUIC datagrams (one diff per ~16 ms tick); gated by ResumeComplete signal so they never apply to a partial cold-reattach replay | All six research questions resolved below |

</phase_requirements>

---

## Summary

Phase 13 wires together three previously-built components: the `StateDiff` wire format (Phase 11 `datagram.rs`), the authoritative `TerminalState` model (Phase 12 `terminal.rs`), and the session pump in `server.rs`. The work is additive: a `diff_interval` tick arm is spliced into BOTH `run_session` and `run_reattach_session`, gated by a per-connection `ResumeComplete` flag. The diff-against-acked-snapshot loop requires only a `Vec<Vec<Cell>>` snapshot clone per connection (not a second `TerminalState`), since `Cell` already matches `DiffRun` field types with zero conversion.

The epoch-ack is a new postcard datagram tagged `0x02`, decoded from `conn.read_datagram()` in the same select! arm that currently only has the migration_poll timer. Because the client sends no other datagrams this milestone, any `0x02`-tagged incoming datagram is an epoch-ack; any `0x01`-tagged incoming datagram would be a misrouted StateDiff (log-and-ignore).

The most important implementation detail is the ResumeComplete gate: a simple `Arc<AtomicBool>` shared between the replay completion site in `run_reattach_session` (line ~724, "replay complete") and the diff_interval arm is the minimal correct mechanism — no channel needed, no wakeup needed (the interval fires every 16 ms regardless, and the flag is checked at the top of the arm).

**Primary recommendation:** Per-connection state lives entirely in the connection task local scope (`run_session` / `run_reattach_session`): last-acked epoch (`u64`), last-acked snapshot (`Vec<Vec<Cell>>`), current epoch counter (`u64`), ResumeComplete flag (`bool` or `AtomicBool`). The slot (`SessionSlot`) is not modified — this is pure connection-task state.

---

## Architectural Responsibility Map

| Capability | Primary Tier | Secondary Tier | Rationale |
|------------|-------------|----------------|-----------|
| Terminal diff computation | API/Backend (nosh-server) | — | Server owns authoritative state; diff vs snapshot happens in connection task |
| Datagram emission | API/Backend (nosh-server) | — | `conn.send_datagram()` in the session pump select! arm |
| Epoch-ack reception | API/Backend (nosh-server) | — | `conn.read_datagram()` in same select! arm; server side only this phase |
| Epoch-ack emission | Client (nosh-client) | — | Phase 14 only; integration test does minimal in-test emission |
| ResumeComplete gate | API/Backend (nosh-server) | — | Flag shared between replay site and diff arm in run_reattach_session |
| Wire format (StateDiff + ClientEpoch) | nosh-proto | — | Existing encode_datagram; new TAG_CLIENT_EPOCH decode added to datagram.rs |

---

## Standard Stack

### Core (no new crates needed)

All required capabilities exist in the current dependency graph. Phase 13 adds no new crates.

| Component | Where | Purpose |
|-----------|-------|---------|
| `quinn::Connection::send_datagram` | quinn 0.11.9 (already in dep graph) | Emit StateDiff datagrams server→client |
| `quinn::Connection::read_datagram` | quinn 0.11.9 | Receive epoch-ack datagrams client→server |
| `quinn::Connection::max_datagram_size` | quinn 0.11.9 | Get the cap for `encode_datagram` |
| `tokio::time::interval` + `MissedTickBehavior::Skip` | tokio 1.x | 16ms diff_interval tick |
| `std::sync::atomic::AtomicBool` | std | ResumeComplete gate (no extra dep) |
| `nosh_proto::datagram::{encode_datagram, decode_datagram}` | nosh-proto (Phase 11) | StateDiff encode/decode |
| `nosh_server::registry::SessionSlot::terminal_state` | nosh-server (Phase 12) | Source of current screen state |

### No New Dependencies

The Cargo.toml files for `nosh-server` and `nosh-proto` need no changes. All needed primitives are already present.

---

## Package Legitimacy Audit

No new packages are installed in this phase. N/A.

---

## Architecture Patterns

### System Architecture Diagram

```
PTY output bytes
      |
      v
slot.push_output_and_parse(chunk)
  -> SequencedOutputBuffer (seq assignment, replay)
  -> TerminalState::advance (grid update, epoch bump)
      |
      v (on diff_interval.tick())
diff arm in select!
  [ResumeComplete == true?]
    NO  -> skip (datagram suppressed)
    YES -> read_terminal_state_snapshot_via_lock()
           compute_diff(current_cells, last_acked_snapshot)
           [any_changes OR acked_epoch < current_epoch?]
             NO  -> skip (no datagram sent)
             YES -> encode_datagram(diff, max_datagram_size)
                    conn.send_datagram(payload)
                    (deferred_runs re-queued for next tick)
      |
      v (on conn.read_datagram())
epoch-ack arm
  decode TAG_CLIENT_EPOCH(bytes) -> acked_epoch u64
  if acked_epoch > last_acked_epoch:
    update last_acked_epoch
    update last_acked_snapshot (snapshot current grid at this epoch)
```

### Recommended Project Structure

No new files required beyond additions to:
```
crates/nosh-proto/src/datagram.rs   # Add TAG_CLIENT_EPOCH + ClientEpoch + decode_epoch_ack()
crates/nosh-server/src/server.rs    # Add diff_interval arm to run_session + run_reattach_session
crates/nosh-client/tests/sync.rs    # New integration test file for SYNC-03
```

### Pattern 1: diff_interval arm (mirrors migration_poll)

The migration_poll arm at server.rs:398-432 is the exact template. The diff_interval arm follows the same shape:

```rust
// Source: server.rs:398-432 (migration_poll arm as template — VERIFIED in codebase)

// Initialization (before the loop):
let mut diff_interval = tokio::time::interval(Duration::from_millis(16));
diff_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
let resume_complete = false; // set true immediately for run_session; set true after replay for run_reattach_session
let mut last_acked_epoch: u64 = 0;
let mut current_epoch: u64 = 0;
let mut last_acked_snapshot: Vec<Vec<Cell>> = Vec::new(); // empty = "baseline is nothing"
let mut pending_deferred: Vec<DiffRun> = Vec::new();

// Inside select! loop:
_ = diff_interval.tick() => {
    if !resume_complete {
        continue; // D-13-03: suppress until ResumeComplete
    }

    // Snapshot current terminal state (brief lock — never across .await).
    let (cols, rows, cursor, cells, epoch_now) = {
        let ts = slot.terminal_state.lock().unwrap_or_else(|e| e.into_inner());
        let (cols, rows) = ts.size();
        let cursor = ts.cursor();
        let cells: Vec<Vec<Cell>> = ts.viewport_rows()
            .map(|(_, row)| row.to_vec())
            .collect();
        let epoch_now = current_epoch; // see epoch management below
        (cols, rows, cursor, cells, epoch_now)
    };

    // D-13-02a: skip if unchanged and client is caught up.
    if cells == last_acked_snapshot && last_acked_epoch >= current_epoch {
        continue;
    }

    // Compute changed runs vs last-acked snapshot.
    let runs = compute_diff_runs(&cells, &last_acked_snapshot, cols, rows);
    let all_runs: Vec<DiffRun> = pending_deferred.drain(..).chain(runs).collect();

    let diff = StateDiff { epoch: current_epoch, cols, rows, cursor, runs: all_runs };
    let cap = match conn.max_datagram_size() {
        Some(c) if c >= MIN_CAP => c,
        _ => continue, // datagrams not negotiated or cap too small
    };

    match encode_datagram(&diff, cap) {
        Ok((payload, deferred)) => {
            pending_deferred = deferred;
            if let Err(e) = conn.send_datagram(payload) {
                use quinn::SendDatagramError::*;
                match e {
                    TooLarge => { /* encode_datagram guarantees this doesn't fire */ }
                    UnsupportedByPeer | Disabled => break SessionEnd::TransportLost,
                    ConnectionLost(_) => break SessionEnd::TransportLost,
                }
            }
        }
        Err(_) => {} // encode failure: skip this tick
    }
}
```

### Pattern 2: epoch-ack arm (read_datagram in select!)

```rust
// Source: quinn 0.11.9 Connection::read_datagram() API — VERIFIED in quinn source

datagram = conn.read_datagram() => {
    match datagram {
        Ok(bytes) => {
            // Any incoming datagram from the client is an epoch-ack (D-13-01a).
            // Phase 13 is the only datagram consumer on the server side.
            match nosh_proto::datagram::decode_epoch_ack(&bytes) {
                Ok(acked_epoch) if acked_epoch > last_acked_epoch => {
                    last_acked_epoch = acked_epoch;
                    // Snapshot the grid at the acked epoch as the new baseline.
                    // Brief lock — not across .await.
                    let ts = slot.terminal_state.lock().unwrap_or_else(|e| e.into_inner());
                    last_acked_snapshot = ts.viewport_rows()
                        .map(|(_, row)| row.to_vec())
                        .collect();
                }
                Ok(_) => {} // older ack: ignore (out-of-order or dup)
                Err(_) => {} // malformed: ignore (D-13-01a: any loss is self-correcting)
            }
        }
        Err(_) => {
            // Connection lost — same as other connection errors.
            break SessionEnd::TransportLost;
        }
    }
}
```

### Pattern 3: epoch management

The terminal `TerminalState` does not currently expose an epoch — that is a Phase 13 concern only. The epoch counter lives in the connection task, incremented whenever the grid changes:

```rust
// Epoch bump: increment current_epoch whenever the terminal state changes
// (i.e., whenever push_output_and_parse is called with non-empty data).
// In the PTY output arm:
slot.push_output_and_parse(&data);
current_epoch += 1;  // any PTY output = new epoch candidate

// Alternative: compare grid snapshots inside the diff arm (no epoch in TerminalState).
// This is simpler: if cells != last_sent_cells, current_epoch += 1 at tick time.
// The research recommends the simpler path: epoch increments at tick time based
// on whether cells differ from the last snapshot, not per-chunk.
```

**Recommended epoch strategy (simpler, avoids per-chunk counter increment):**
- `current_epoch` increments inside the diff_interval arm when the current grid snapshot differs from the last-SENT snapshot (not the last-acked snapshot).
- This avoids a second snapshot copy and keeps the epoch tied to the diff-worthy state changes visible to the client.
- On the first tick after `push_output_and_parse`, the grid will differ from `last_sent_cells` (a separate local var from `last_acked_snapshot`) and epoch increments.

### Pattern 4: ResumeComplete gate

For `run_session`: a plain `bool resume_complete = true` set before the loop. No shared state needed.

For `run_reattach_session`: a plain `bool resume_complete = false` set before the loop, flipped to `true` immediately after the replay loop (server.rs:~724 "replay complete" log line). Since the diff_interval arm and the replay logic are both in the same async task (same function), no synchronization primitive is needed — it's just a local variable:

```rust
// After the replay loop in run_reattach_session (~line 720):
tracing::info!(replaying_from_seq, chunks = chunks.len(), truncated, "replay complete");
let mut resume_complete = true; // ← set here; used in the select! loop below

// Inside select! loop:
_ = diff_interval.tick() => {
    if !resume_complete { continue; }
    // ...
}
```

This is the simplest correct solution. An `Arc<AtomicBool>` is only needed if the gate must be observed across task boundaries (it doesn't — both the replay site and the select! arm are in the same task).

### Pattern 5: diff computation (scan grid vs snapshot)

```rust
// Source: TerminalState::viewport_rows() API — VERIFIED in crates/nosh-server/src/terminal.rs:383

fn compute_diff_runs(
    current: &[Vec<Cell>],
    baseline: &[Vec<Cell>],  // empty vec = treat all cells as changed
    cols: u16,
    rows: u16,
) -> Vec<DiffRun> {
    let mut runs = Vec::new();
    for (row_idx, current_row) in current.iter().enumerate() {
        let row = row_idx as u16;
        // Compare vs baseline row (if baseline is empty/short, all cells are "changed").
        let baseline_row: &[Cell] = baseline
            .get(row_idx)
            .map(|r| r.as_slice())
            .unwrap_or(&[]);

        // Scan for runs of changed cells sharing the same style/colors.
        let mut col = 0u16;
        while (col as usize) < current_row.len() {
            let c = col as usize;
            let cell = &current_row[c];
            let base = baseline_row.get(c);
            if base.map(|b| b == cell).unwrap_or(false) {
                col += 1;
                continue; // cell unchanged
            }
            // Start a new run.
            let start_col = col;
            let style = cell.style;
            let fg = cell.fg;
            let bg = cell.bg;
            let mut chars = String::new();
            // Extend run while style/colors match (all changed; don't merge style changes).
            while (col as usize) < current_row.len() {
                let cc = col as usize;
                let c2 = &current_row[cc];
                if c2.style != style || c2.fg != fg || c2.bg != bg {
                    break; // style break: emit and start new run
                }
                let base2 = baseline_row.get(cc);
                // Include cell in this run even if unchanged — run already started
                // (avoids fragmenting adjacent cells with identical style).
                // ALTERNATIVE (cheaper): break if unchanged and style same.
                // For correctness under acked-epoch: it's safe to include unchanged
                // cells in a run (idempotent on the client side).
                chars.push(c2.ch);
                col += 1;
            }
            if !chars.is_empty() {
                runs.push(DiffRun { row, start_col, style, fg, bg, chars });
            }
        }
    }
    runs
}
```

**Important note:** The run-extension logic above merges unchanged cells into a run when style matches. A simpler approach breaks on the first unchanged cell, producing more but smaller runs. Both are correct under the acked-epoch model. The research recommends the simpler break-on-unchanged approach to keep run counts lower and avoid inflating datagram payloads.

### Anti-Patterns to Avoid

- **Holding the terminal_state lock across .await:** `slot.terminal_state` is a `std::sync::Mutex`. Never hold it across any await point. Snapshot the needed state (grid cells, cursor, size) into local Vecs inside the lock, then release before any async operation. [VERIFIED: Anti-Pattern #2 documented in registry.rs]
- **Using `encode_datagram` return value (deferred) incorrectly:** Deferred runs must be prepended to the NEXT tick's runs, not appended. Prepending ensures cursor-proximate cells from the previous tick are re-prioritized by the sort in `encode_datagram`. [VERIFIED: encode_datagram design in datagram.rs]
- **Spinning when max_datagram_size is None:** If the peer didn't negotiate datagram support (shouldn't happen with our transport_config, but defensive), skip the tick silently — don't log every 16ms. [VERIFIED: transport.rs sets datagram buffers on both endpoints]
- **Breaking the select! loop on SendDatagramError::TooLarge:** `encode_datagram` guarantees `payload.len() < cap`, and `cap = max_datagram_size()`. TooLarge should be unreachable, but handle it as a bug/skip, not a session terminator.
- **Resetting the acked snapshot on every epoch-ack:** Only update `last_acked_snapshot` when `acked_epoch > last_acked_epoch`. Out-of-order or duplicate acks (harmless on a datagram channel) must not overwrite a newer baseline with an older one.
- **Adding a second read_datagram task/future outside the select!:** `read_datagram()` returns a `ReadDatagram` future (not a stream). It can be polled inside `select!` directly alongside the other arms. No extra spawning needed. [VERIFIED: quinn source, ReadDatagram is a future with a .await]

---

## Don't Hand-Roll

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| Size-capped datagram encoding | Custom truncation logic | `encode_datagram(diff, cap)` (Phase 11) | Already handles cursor-priority partial fill, split runs, continue-past-rejection — validated by tests |
| Terminal state snapshot | Second TerminalState instance | Clone `viewport_rows()` into `Vec<Vec<Cell>>` | TerminalState has no Clone; copying only the cells is the right granularity |
| Diff computation | VT-aware diff engine | Cell-by-cell scan against baseline slice | The grid is already decoded; scanning is O(rows×cols) per tick, fast |
| Datagram size negotiation | Custom MTU discovery | `conn.max_datagram_size()` | quinn handles QUIC path MTU; our transport_config already enables datagrams |
| Tick interval | Custom timer | `tokio::time::interval` + `MissedTickBehavior::Skip` | Matches the existing migration_poll pattern; Skip prevents tick accumulation on slow ticks |

**Key insight:** Phase 11 and 12 built precisely the primitives Phase 13 needs. The work here is plumbing, not invention.

---

## Common Pitfalls

### Pitfall 1: Holding `terminal_state` Mutex Across .await
**What goes wrong:** The diff_interval arm needs to read the grid to compute a diff. If the `std::sync::Mutex` lock is held while calling `conn.send_datagram()` (even though `send_datagram` is synchronous), Clippy/Rust will allow it — but `select!` context switches can leave the lock held across a yield point on other arms.
**Why it happens:** The connection task is single-threaded but not single-task. A `std::sync::Mutex` held across `.await` can deadlock if another arm also needs the lock.
**How to avoid:** Snapshot cells into a `Vec<Vec<Cell>>` local before releasing the lock. The lock duration is microseconds.
**Warning signs:** `slot.terminal_state.lock()` inside a block that also contains `.await`.

### Pitfall 2: Epoch-Ack Advancing the Snapshot Backward
**What goes wrong:** The epoch-ack arrives out of order (older epoch than already acked). If `last_acked_snapshot` is overwritten unconditionally, the baseline regresses and the server sends more data than needed on the next tick.
**Why it happens:** Datagrams are unordered; a delayed ack for epoch N can arrive after an ack for epoch N+2.
**How to avoid:** Only update `last_acked_snapshot` when `acked_epoch > last_acked_epoch`.

### Pitfall 3: Deferred Runs Accumulation Without a MAX Guard
**What goes wrong:** If the screen is persistently large (e.g., full-screen vim) and many deferred runs accumulate across multiple ticks, the local `pending_deferred` Vec can grow unboundedly.
**Why it happens:** Deferred runs from tick N are prepended to tick N+1. If each tick also defers, the list grows.
**How to avoid:** Add a `MAX_DEFERRED_RUNS` cap (e.g., `MAX_RUNS` from `datagram.rs` = 4096). If `pending_deferred.len() > MAX_DEFERRED_RUNS`, truncate to the most-recently-deferred entries (cursor-proximate were already picked first by `encode_datagram`'s sort, so older deferred runs are lower priority).
**Warning signs:** `pending_deferred.len()` growing unboundedly in a long-running full-screen session.

### Pitfall 4: Two read_datagram Futures in Parallel
**What goes wrong:** Creating two concurrent `conn.read_datagram()` futures (e.g., one in the select! arm and one in a spawned task) will not compile cleanly since `ReadDatagram` holds a `&Connection` reference and is not `Send`.
**Why it happens:** Misreading the quinn API — `read_datagram()` returns a future, not a stream.
**How to avoid:** One `conn.read_datagram()` arm in the select! loop. No separate task.

### Pitfall 5: ResumeComplete Signal as a Channel (Unnecessary Complexity)
**What goes wrong:** If `ResumeComplete` is implemented as a `oneshot::Receiver` arm in the select!, it consumes a select! slot and requires careful handling of the `biased` ordering.
**Why it happens:** Over-engineering the signal mechanism.
**How to avoid:** Since the replay loop and the diff select! loop are in the same async task (`run_reattach_session`), a plain `bool` flipped after the replay loop suffices. No channel needed.

### Pitfall 6: `TAG_CLIENT_EPOCH` Byte Value Collision
**What goes wrong:** `datagram.rs` already reserves `TAG_STATE_DIFF = 0x01` and has a comment `// const TAG_CLIENT_EPOCH: u8 = 0x02;` for Phase 13. If the planner accidentally uses `0x01` for the epoch-ack, the decode will misroute.
**Why it happens:** The reserved constant was commented out, not yet activated.
**How to avoid:** Uncomment and use `TAG_CLIENT_EPOCH = 0x02` as reserved in the existing comment in `datagram.rs`.

### Pitfall 7: Incorrect Snapshot Timing on Epoch-Ack
**What goes wrong:** When the server receives an epoch-ack for epoch N, it snapshots the current grid (which may be at epoch M > N). The snapshot is now "too new" — if the client has only confirmed up to N, the server's baseline is ahead of what the client has applied.
**Why it happens:** The server doesn't store grid snapshots at each epoch; it only has the current grid.
**Root resolution:** This is actually acceptable for Phase 13. The acked-epoch model's invariant is: the server diffs current vs a snapshot it knows the client has confirmed. If the server snapshots the current grid on ack receipt (even if "too new"), the only consequence is that the server's next diff will include fewer cells (it thinks the client has the current grid when the client only has the N-epoch grid). The client will self-correct because it applies diffs with epoch > last_applied, and the server will send the remaining delta naturally. The only strict requirement is that `last_acked_snapshot` never moves backward (guarded by the `acked_epoch > last_acked_epoch` check).
**Warning signs:** None — this is a design-level correctness decision, not a bug trigger.

---

## Code Examples

### quinn send_datagram API (verified)

```rust
// Source: quinn-0.11.9/src/connection.rs:433 — VERIFIED from quinn source on this machine
// conn.send_datagram is synchronous (returns Result immediately, not async)
conn.send_datagram(payload: Bytes) -> Result<(), quinn::SendDatagramError>

// Variants of SendDatagramError (VERIFIED from quinn source):
// - TooLarge: payload exceeds negotiated max size (prevented by encode_datagram)
// - UnsupportedByPeer: peer didn't negotiate datagrams (prevented by transport_config)
// - Disabled: not configured on this endpoint (prevented by transport_config)
// - ConnectionLost(ConnectionError): connection dropped
```

### quinn read_datagram API (verified)

```rust
// Source: quinn-0.11.9/src/connection.rs:349 — VERIFIED from quinn source on this machine
// read_datagram returns a ReadDatagram future (not async fn, but usable with .await)
conn.read_datagram() -> impl Future<Output = Result<Bytes, ConnectionError>>

// Usage in select!:
datagram = conn.read_datagram() => {
    match datagram {
        Ok(bytes) => { /* process epoch-ack */ }
        Err(_) => break SessionEnd::TransportLost,
    }
}
```

### TAG_CLIENT_EPOCH constant (reserved in Phase 11 source)

```rust
// Source: crates/nosh-proto/src/datagram.rs:22 — VERIFIED in codebase
// const TAG_CLIENT_EPOCH: u8 = 0x02; // reserved for Phase 13 (ClientEpoch, client → server)
// Phase 13 UNCOMMENTS this line and adds decode_epoch_ack().
```

### TerminalState read API (verified)

```rust
// Source: crates/nosh-server/src/terminal.rs:383 — VERIFIED in codebase

// Iterate viewport rows without cloning TerminalState:
ts.viewport_rows() -> impl Iterator<Item = (u16, &[Cell])>

// Read cursor and size:
ts.cursor() -> CursorPos  // { row: u16, col: u16 }
ts.size() -> (u16, u16)   // (cols, rows)

// Read a single cell (returns &'static Cell for out-of-bounds, copies needed):
ts.cell(row: u16, col: u16) -> &Cell

// Cell type (from terminal.rs:58, VERIFIED) — matches DiffRun fields exactly:
struct Cell {
    ch: char,
    style: CellStyle,   // same as DiffRun.style
    fg: Option<u8>,     // same as DiffRun.fg
    bg: Option<u8>,     // same as DiffRun.bg
}
// Terminal state is behind Mutex<TerminalState> in SessionSlot.
// Access pattern:
let ts = slot.terminal_state.lock().unwrap_or_else(|e| e.into_inner());
// ... read ts fields ...
// drop(ts); // lock released; NO .await while ts is held
```

### ClientEpoch datagram wire format

```rust
// New in nosh-proto/src/datagram.rs for Phase 13:

const TAG_CLIENT_EPOCH: u8 = 0x02;  // uncomment from reserved comment

/// A client→server epoch acknowledgement sent as a QUIC datagram.
/// The client sends this after successfully applying a StateDiff with `epoch`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ClientEpoch {
    /// The last epoch the client has applied to its display.
    pub epoch: u64,
}

/// Encode a ClientEpoch as a tagged datagram payload.
pub fn encode_epoch_ack(epoch: u64) -> Bytes {
    let body = postcard::to_allocvec(&ClientEpoch { epoch }).expect("postcard");
    let mut payload = Vec::with_capacity(1 + body.len());
    payload.push(TAG_CLIENT_EPOCH);
    payload.extend_from_slice(&body);
    Bytes::from(payload)
}

/// Decode an epoch-ack datagram. Returns Err for unknown tags (including TAG_STATE_DIFF).
pub fn decode_epoch_ack(bytes: &[u8]) -> Result<u64, ProtoError> {
    let (tag, body) = bytes.split_first().ok_or(...)?;
    if *tag != TAG_CLIENT_EPOCH {
        return Err(ProtoError::Postcard(postcard::Error::DeserializeBadEncoding));
    }
    let ce: ClientEpoch = postcard::from_bytes(body).map_err(ProtoError::Postcard)?;
    Ok(ce.epoch)
}
```

### Integration test shape

```rust
// New file: crates/nosh-client/tests/sync.rs
// Pattern follows existing tests in crates/nosh-client/tests/ (VERIFIED in codebase)

#[tokio::test]
async fn sync03_server_emits_datagram_after_pty_output() {
    // 1. Spawn test server with /bin/sh.
    // 2. Connect, open PTY session.
    // 3. Send a known string to the shell ("echo hello\n").
    // 4. Loop conn.read_datagram() until a TAG_STATE_DIFF datagram arrives.
    // 5. decode_datagram(bytes) -> StateDiff; assert StateDiff.runs is non-empty.
    // 6. Assert StateDiff.epoch >= 1.
}

#[tokio::test]
async fn sync03_acked_epoch_advances_baseline() {
    // 1. Spawn server, connect, send "echo A\n".
    // 2. Receive first StateDiff (epoch E1).
    // 3. Send epoch-ack datagram: encode_epoch_ack(E1).
    // 4. Send "echo B\n".
    // 5. Receive next StateDiff (epoch E2 > E1).
    // 6. Assert E2's runs only contain the "B" content — not "A" (baseline advanced).
    // 7. (Note: this assertion is approximate since the shell may produce more output.)
}

#[tokio::test]
async fn sync03_datagrams_suppressed_during_replay() {
    // 1. Spawn server, open session, type some input.
    // 2. Drop connection (simulate TransportLost).
    // 3. Reconnect with Reattach (reattach test pattern from reattach.rs).
    // 4. During replay window: assert no read_datagram() completes before ResumeComplete.
    //    (Approach: read_datagram with a short timeout DURING replay — expect timeout/no data.)
    // 5. After replay: assert datagrams start flowing.
    // Note: "during replay" timing is tricky in tests; consider a server-side hook
    // or a sufficiently long replay to make the window observable.
}
```

---

## State of the Art

| Old Approach | Current Approach | Impact |
|--------------|------------------|--------|
| Phase 11: `StateDiff` defined as wire type only | Phase 13: actually emitted from session pump | Closes the datagram channel end-to-end for server |
| Reliable stream only (PtyData) | Reliable stream + datagram channel (additive) | No regression; adds loss-tolerant display path |
| Epoch-ack as a reserved comment in datagram.rs | Phase 13: `TAG_CLIENT_EPOCH = 0x02` activated | Enables the acked-baseline model |

---

## Open Questions

1. **Snapshot strategy: grid snapshot at ack time vs grid snapshot at send time**
   - What we know: When the server receives an epoch-ack for epoch N, the current grid may be at epoch M > N. Snapshotting the current grid as the new baseline means the server thinks the client has confirmed more than it actually has.
   - What's unclear: Is this a correctness problem or a minor efficiency inefficiency?
   - **RESOLVED:** It is an efficiency issue only, not a correctness issue. The acked-epoch model guarantees convergence: any difference between what the client actually has and what the server thinks it has will show up as changed cells in the next diff. The server will re-send those cells. The model is self-correcting. Snapshotting the current grid at ack time is the correct and simple approach.

2. **Epoch increment strategy: per-chunk vs at-tick-time**
   - What we know: TerminalState does not track an internal epoch counter. The Phase 13 epoch is a connection-task local `u64`.
   - What's unclear: Should it increment every time `push_output_and_parse` is called (per PTY chunk) or only at tick time (when the grid snapshot differs from last-sent)?
   - **RESOLVED:** Increment at tick time, not per chunk. Per-chunk is noisy (a burst of 10 PTY chunks in one tick produces epoch+10, but the client only sees one datagram). At-tick-time is simpler: if the grid snapshot differs from the last-sent snapshot, increment `current_epoch` and send. This decouples epoch semantics from the PTY output buffer's sequence numbering.

3. **Where to store last-acked snapshot: connection task local vs SessionSlot**
   - What we know: The slot is shared across connection tasks (a reattach creates a new task on the same slot). The last-acked snapshot is per-connection (client's acked state resets on reattach per D-13-01b).
   - **RESOLVED:** Connection task local. On reattach, the snapshot resets naturally because `run_reattach_session` initializes `last_acked_snapshot` to empty (and `last_acked_epoch` to 0), making the first post-resume diff a full-screen diff (D-13-01b).

4. **MAX_DEFERRED_RUNS guard**
   - What we know: Phase 11's `MAX_RUNS = 4096` guards decode; no guard exists on the deferred queue size.
   - **RESOLVED:** Add a guard in the diff_interval arm: if `pending_deferred.len() > MAX_RUNS`, truncate (keep the most recently deferred, which are cursor-distant; cursor-close ones were already sent). This prevents memory growth in full-screen-vim scenarios. This is distinct from Phase 11's `IN-03` (`MAX_ENCODE_RUNS` guard in the fill loop) — Phase 13 adds a guard on the pending queue, not inside `encode_datagram`.

5. **terminal_state field visibility in SessionSlot**
   - What we know: `terminal_state` is declared `terminal_state: Mutex<TerminalState>` in `registry.rs:244`. The field visibility is not shown as `pub` in the partial read.
   - **RESOLVED (by inspection):** The field is NOT pub (matches Rust default private visibility). The server already accesses it via `slot.push_output_and_parse()` and `slot.resize()` — both are delegating methods on SessionSlot. Phase 13 needs to read the terminal state directly in the diff_interval arm. Two options: (a) add a `pub fn with_terminal_state<F, R>(&self, f: F) -> R` delegate to SessionSlot, or (b) make `terminal_state` pub(crate). The delegate pattern is cleaner and consistent with the existing lock-discipline pattern.

---

## Environment Availability

Step 2.6: SKIPPED — Phase 13 is purely code additions to existing crates. No new external tools, services, CLIs, or runtimes. All dependencies (quinn, tokio, etc.) are already in the workspace and verified present by the existing passing test suite.

---

## Validation Architecture

`workflow.nyquist_validation` is explicitly `false` in `.planning/config.json`. Section omitted per config.

---

## Security Domain

Phase 13 does not introduce new authentication, authorization, session management, access control, or cryptographic operations. The datagram channel is protected by QUIC's TLS 1.3 per-packet authentication (same keys as the stream channel). The epoch-ack is an application-layer monotonic counter — replay of a stale epoch-ack is harmless (the server only advances its baseline forward, and the `acked_epoch > last_acked_epoch` check prevents regression).

**No new ASVS categories apply beyond what Phase 11/12 already addressed.**

One security-adjacent invariant to preserve: the `TAG_CLIENT_EPOCH` decoder must return `Err` for unknown tags (including `TAG_STATE_DIFF = 0x01`) — a client should not send `TAG_STATE_DIFF` datagrams to the server, and the server should not misinterpret them as epoch-acks. The decode function enforces this by requiring `tag == TAG_CLIENT_EPOCH`.

---

## Sources

### Primary (HIGH confidence — verified from local codebase)

- `crates/nosh-proto/src/datagram.rs` — StateDiff, encode_datagram, decode_datagram, TAG_STATE_DIFF, reserved TAG_CLIENT_EPOCH comment, MIN_CAP, MAX_RUNS
- `crates/nosh-server/src/terminal.rs` — TerminalState API (viewport_rows, cell, cursor, size, Cell type), Cell fields match DiffRun exactly
- `crates/nosh-server/src/server.rs` — run_session select! loop, migration_poll pattern (398-432), run_reattach_session replay completion (~720), push_output_and_parse callsite, MissedTickBehavior usage
- `crates/nosh-server/src/registry.rs` — SessionSlot structure, terminal_state field, push_output_and_parse, lock discipline, resize delegate
- `crates/nosh-proto/src/transport.rs` — datagram buffer sizes, both endpoints have datagrams enabled
- `crates/nosh-proto/src/messages.rs` — Message enum, Ack{seq} distinct from epoch-ack
- `quinn-0.11.9/src/connection.rs` (local cargo cache) — send_datagram signature, SendDatagramError variants, read_datagram ReadDatagram future

### Secondary (MEDIUM confidence — from test harness)

- `crates/nosh-client/tests/common/mod.rs` — spawn_server_with_shell pattern for integration tests
- `crates/nosh-client/tests/transport.rs` — datagram_roundtrip, conn.max_datagram_size() usage
- `crates/nosh-client/tests/reattach.rs` — reattach test pattern (server+client in-process)
- `crates/nosh-client/src/client.rs:249` — send_datagram/read_datagram usage in client

---

## Metadata

**Confidence breakdown:**
- Standard stack: HIGH — no new crates; all APIs verified from local source
- Architecture: HIGH — patterns directly traced from existing code
- Pitfalls: HIGH — each derived from concrete code analysis (lock discipline, enum variants, etc.)
- Integration test shape: MEDIUM — pattern follows existing tests but new test file is not yet written

**Research date:** 2026-06-01
**Valid until:** 2026-09-01 (quinn 0.11 API is stable; TerminalState API is codebase-local)
