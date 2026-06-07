# Phase 20: Repaint Pacing — Pattern Map

**Mapped:** 2026-06-07
**Files analysed:** 4 (2 modified, 2 test/guard files)
**Analogs found:** 4 / 4 (all are self-analogs — modifications to existing files)

---

## File Classification

| New/Modified File | Role | Data Flow | Closest Analog | Match Quality |
|---|---|---|---|---|
| `crates/nosh-server/src/server.rs` (tick arms ×2 + `DiffTickResult`) | service / event-driven loop | event-driven (tokio::select tick arm) | itself (lines 694–730 and 1186–1220) | exact — self-modification |
| `crates/nosh-proto/src/datagram.rs` (no change to `encode_datagram` itself) | utility | transform | itself | exact — called in burst loop, signature unchanged |
| `crates/nosh-client/src/screen.rs` (line 223 guard + two tests) | model | request-response | itself (line 223 and tests at lines 687–728) | exact — self-modification |
| `crates/nosh-client/tests/predict.rs` (confirm no `#[ignore]`) | integration test | event-driven | itself (line 715) | exact — CI gate confirmation |
| `crates/nosh-server/src/server.rs` (new `#[cfg(test)]` burst tests) | unit test | batch | `server.rs` tests at lines 1371–1461 | role-match |

---

## Pattern Assignments

### `crates/nosh-server/src/server.rs` — burst loop in both tick arms

**Analog:** the existing diff-tick arm in `run_session` (lines 694–730) and the identical arm in `run_reattach_session` (lines 1186–1220). Both are character-for-character copies today; both must be updated identically.

**Current tick arm shape** (lines 694–730 and 1186–1220 — same pattern):

```rust
// SYNC-03: diff-interval tick — emit one coalesced StateDiff datagram.
_ = diff_interval.tick() => {
    if !resume_complete {
        continue;
    }
    let cap = match conn.max_datagram_size() {
        Some(c) if c >= MIN_CAP => c,
        _ => continue, // datagrams not negotiated or cap too small — skip silently
    };
    let deferred = std::mem::take(&mut pending_deferred);
    if let Some(result) = build_state_diff(
        &slot,
        &mut current_epoch,
        last_acked_epoch,
        &last_acked_snapshot,
        &last_sent_snapshot,
        deferred,
        cap,
    ) {
        // CR-01 fix: store sent snapshot keyed by epoch BEFORE send_datagram.
        epoch_snapshots.push_back((result.epoch, result.sent_cells.clone()));
        if epoch_snapshots.len() > EPOCH_SNAPSHOT_CAP {
            epoch_snapshots.pop_front();
        }
        last_sent_snapshot = result.sent_cells;
        pending_deferred = result.deferred;
        if let Err(e) = conn.send_datagram(result.payload) {
            use quinn::SendDatagramError::*;
            match e {
                TooLarge => {} // encode_datagram guarantees this is unreachable
                UnsupportedByPeer | Disabled => break SessionEnd::TransportLost,
                ConnectionLost(_) => break SessionEnd::TransportLost,
            }
        }
    }
}
```

**Phase 20 change — what replaces the single-datagram block:**

The `build_state_diff` call and the `epoch_snapshots` / `last_sent_snapshot` housekeeping remain unchanged. The single `conn.send_datagram(result.payload)` and the `pending_deferred = result.deferred` assignment are wrapped in a burst loop. The burst loop:

1. Sends `result.payload` (the first datagram) exactly as today.
2. Loops `encode_datagram`-only (no second `build_state_diff` call — D-20-03 / R-1 fix) until `result.deferred` drains, the safety cap is hit, or `datagram_send_buffer_space()` drops below `cap`.
3. All burst datagrams use `result.epoch` (the single epoch from `build_state_diff` — D-20-04 / R-2 fix).
4. Sets `pending_deferred` to whatever deferred remains after the loop (D-20-05: leftovers ride the next tick's epoch).

**`DiffTickResult` struct extension** (lines 281–295 — Option A from RESEARCH.md Pitfall 4):

Current struct:
```rust
struct DiffTickResult {
    payload: Bytes,
    sent_cells: Vec<Vec<Cell>>,
    epoch: u64,
    deferred: Vec<DiffRun>,
}
```

The burst loop constructs `StateDiff` for iterations 2..N and needs `cols`, `rows`, `cursor`, `alt_screen` without re-locking `slot`. These are captured inside `build_state_diff` at line 323 (`slot.with_terminal_state(...)`) and currently only exposed indirectly via `sent_cells`. Add them to the result so the burst loop can construct `StateDiff { epoch: tick_epoch, cols, rows, cursor, alt_screen, runs: deferred }` without a re-lock:

```rust
struct DiffTickResult {
    payload: Bytes,
    sent_cells: Vec<Vec<Cell>>,
    epoch: u64,
    deferred: Vec<DiffRun>,
    // New fields (D-20 burst geometry — needed by burst iterations 2..N):
    cols: u16,
    rows: u16,
    cursor: CursorPos,
    alt_screen: bool,
}
```

Populate them from the `(cols, rows, cursor, alt_screen, cells)` tuple already destructured at line 323 of `build_state_diff`.

**`send_datagram` error handling pattern** (lines 723–729 — copy verbatim into burst loop):

```rust
if let Err(e) = conn.send_datagram(payload) {
    use quinn::SendDatagramError::*;
    match e {
        TooLarge => {} // unreachable if payload was produced by encode_datagram with the same cap
        UnsupportedByPeer | Disabled => break SessionEnd::TransportLost,
        ConnectionLost(_) => break SessionEnd::TransportLost,
    }
}
```

**Duplication flag — `send_burst()` helper:**

`run_session` tick arm (lines 694–730) and `run_reattach_session` tick arm (lines 1186–1220) are character-for-character copies. After Phase 20, both become identical burst blocks. The planner should extract a `send_burst()` free function (not a method — `quinn::Connection` is not easily mockable) called from both arms, or track both as parallel explicit edits. Either is acceptable; a helper avoids a third divergence in a future phase.

---

### `crates/nosh-proto/src/datagram.rs` — `encode_datagram` called in burst loop

**No change to `encode_datagram` itself.** The burst loop calls it with the carried `deferred` as `runs`. Pattern to copy for the burst loop call site:

**`encode_datagram` signature and return** (lines 261–264):

```rust
pub fn encode_datagram(
    diff: &StateDiff,
    cap: usize,
) -> Result<(Bytes, Vec<DiffRun>), ProtoError>
```

**Burst call pattern** — construct `StateDiff` from result fields and call `encode_datagram`:

```rust
let burst_diff = StateDiff {
    epoch: result.epoch,      // same epoch as build_state_diff returned — D-20-04
    cols: result.cols,
    rows: result.rows,
    cursor: result.cursor,
    alt_screen: result.alt_screen,
    runs: deferred,           // only the leftover runs from the previous encode_datagram
};
match encode_datagram(&burst_diff, cap) {
    Ok((payload, next_deferred)) => {
        // send payload, advance deferred = next_deferred, burst_count += 1
    }
    Err(_) => break, // CapTooSmall is unreachable at runtime (cap from max_datagram_size)
}
```

**`MIN_CAP` and import** (line 26 of server.rs imports `encode_datagram, decode_epoch_ack, StateDiff, DiffRun, MIN_CAP, MAX_RUNS` from `nosh_proto::datagram`). The burst loop also uses `StateDiff` to construct burst diffs — no new imports required.

---

### `crates/nosh-client/src/screen.rs` — `apply()` guard change (line 223)

**Analog:** the existing guard at line 223 (the line being changed) and the `apply_monotonic_same_epoch_is_noop` test at line 688.

**Current guard** (line 222–225):

```rust
// D-14-05: monotonic staleness check — discard stale or duplicate diffs.
if diff.epoch <= self.last_applied_epoch {
    return;
}
```

**After change** (D-20-07 — one character change):

```rust
// D-14-05 / D-20-07: discard strictly older diffs only.
// Same-epoch burst datagrams (all datagrams in one tick share one epoch, D-20-04)
// MUST all apply their runs to the confirmed grid.
if diff.epoch < self.last_applied_epoch {
    return;
}
```

**`last_applied_epoch` assignment** (line 290 — unchanged, the existing pattern):

```rust
self.confirmed_cursor = diff.cursor;
self.last_applied_epoch = diff.epoch;  // updated on every successful apply
```

For burst datagrams D1, D2, D3 with the same epoch E: D1 applies (E > 0 initial), sets `last_applied_epoch = E`. D2: `E < E` is false → applies. D3: same. All three write their runs. This is correct — each datagram carries a disjoint (deferred) subset of runs so there is no double-write risk.

**Existing test that must be revised** (lines 687–714):

```rust
#[test]
fn apply_monotonic_same_epoch_is_noop() {
    // ... applies epoch=1, then applies a DIFFERENT diff also at epoch=1
    // ... asserts second apply is a no-op (confirmed grid unchanged)
    assert_eq!(screen.confirmed_cell(0, 0).ch, 'h');
    assert_eq!(screen.last_applied_epoch(), 1);
}
```

After the `<=` → `<` change, same-epoch applies are no longer no-ops. This test must be:
1. Renamed to `apply_monotonic_older_epoch_is_noop`.
2. Revised to test `diff.epoch < last_applied_epoch` as the discard condition: apply epoch=2, then apply epoch=1 (strictly older), assert the older is discarded.
3. A new test `apply_same_epoch_burst_applies` added: apply epoch=1 diff with "hello", then apply epoch=1 diff with "XXXXX", assert confirmed grid contains "XXXXX" (the second same-epoch diff applied).

**Existing `apply_monotonic_lower_epoch_is_noop` test** (lines 716–728) is already correct for the new semantics (epoch=1 after epoch=2 was applied) — no change required.

---

### `crates/nosh-client/tests/predict.rs` — noecho CI gate (line 715)

**No code change.** Research confirmed no `#[ignore]` attribute is present. Pattern to verify:

```rust
// Line 714-715 — currently:
#[tokio::test]
async fn noecho_read_dash_s_zero_predicted_chars() {
```

This test must remain non-`#[ignore]` with burst active. The drain helpers at lines 960–1002 use `diff.epoch > screen.last_applied_epoch()` (not `>=`) as their own guard — these helpers are NOT affected by the `apply()` guard change because they gate the `screen.apply()` call externally. After the guard change, these helpers could simplify to calling `screen.apply()` directly and trusting its internal guard, but no change is required for correctness.

---

### `crates/nosh-server/src/server.rs` — new burst unit tests

**Analog:** the existing `#[cfg(test)] mod tests` block at lines 1371–1461. Pattern to copy for new burst tests:

**Module and import pattern** (lines 1372–1374):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use nosh_proto::datagram::CellStyle;
    // Add for burst tests:
    use nosh_proto::datagram::{CursorPos, StateDiff};
    use crate::terminal::Cell;
```

**Unit test shape for `build_state_diff`** — existing tests call `compute_diff_runs` directly with constructed `Vec<Vec<Cell>>` grids (no async, no quinn, no PTY). The burst tests follow the same shape: construct grids, call `build_state_diff`, then loop `encode_datagram`. No async, no `#[tokio::test]`.

**`burst_drains_when_grid_differs_from_acked_baseline` test** — must be written RED first (naive loop calling `build_state_diff` on every iteration spins; see RESEARCH.md Pitfall 1), then GREEN after D-20-03 fix. Uses an iteration bound (`max_iterations = 100`) to assert finite termination.

**`one_epoch_per_tick` test** — asserts `current_epoch` increments exactly 1 across N-datagram burst: call `build_state_diff` once (epoch increments from 0 to 1), verify epoch stays 1 after all subsequent `encode_datagram` iterations.

---

## Shared Patterns

### `datagram_send_buffer_space()` backpressure gate

**Source:** quinn 0.11.9 `Connection::datagram_send_buffer_space()` — verified in `~/.cargo/registry/.../quinn-.../src/connection.rs:493`.
**Apply to:** burst loop in both tick arms.

```rust
// Query once per burst iteration before calling encode_datagram + send_datagram.
// Returns bytes free in the application-layer outgoing queue.
// When space < cap, a new datagram would displace an older queued one — stop burst.
if conn.datagram_send_buffer_space() < cap {
    break;
}
```

### Safety cap constant

**Source:** decided in D-20-02 / RESEARCH.md Safety Cap Sizing.
**Apply to:** both tick arms (or the `send_burst()` helper if extracted).
**Value:** 64 datagrams/tick (~2.5–4× a full 80×24 repaint; never trips in normal TUI use).

```rust
const BURST_CAP: usize = 64;
let mut burst_count: usize = 1; // already sent the first datagram (from build_state_diff)
```

### `epoch_snapshots` push pattern (one push per tick, not per burst datagram)

**Source:** lines 716–719 (run_session) and 1205–1208 (run_reattach_session).
**Apply to:** both tick arms — push exactly once for the `DiffTickResult` epoch, before the burst loop.

```rust
// Push once for the tick's epoch (D-20-04: one epoch per tick).
// Do NOT push inside the burst loop — that would blow EPOCH_SNAPSHOT_CAP (Pitfall 5).
epoch_snapshots.push_back((result.epoch, result.sent_cells.clone()));
if epoch_snapshots.len() > EPOCH_SNAPSHOT_CAP {
    epoch_snapshots.pop_front();
}
last_sent_snapshot = result.sent_cells.clone(); // clone needed; original moves into snapshot
```

### `SendDatagramError` match pattern

**Source:** lines 723–729 (run_session) — copy verbatim into burst loop.

```rust
use quinn::SendDatagramError::*;
match e {
    TooLarge => {}
    UnsupportedByPeer | Disabled => break SessionEnd::TransportLost,
    ConnectionLost(_) => break SessionEnd::TransportLost,
}
```

Note: in a `send_burst()` helper the `break SessionEnd::TransportLost` becomes a `return` or an `Err(...)` — the calling arm then breaks. The planner must decide the helper's return type.

---

## No Analog Found

None. All four files are self-modifications of existing code. The `burst.rs` integration test file is new but follows the in-module `#[cfg(test)]` unit test pattern already present in `server.rs` (lines 1371–1461).

---

## Metadata

**Analog search scope:** `crates/nosh-server/src/server.rs`, `crates/nosh-proto/src/datagram.rs`, `crates/nosh-client/src/screen.rs`, `crates/nosh-client/tests/predict.rs`
**Files scanned:** 4 primary source files + quinn registry source for API verification
**Pattern extraction date:** 2026-06-07
