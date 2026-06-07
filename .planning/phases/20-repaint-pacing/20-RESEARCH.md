# Phase 20: Repaint Pacing — Research

**Researched:** 2026-06-07
**Domain:** QUIC datagram burst loop, server tick/send, client apply() guard, noecho CI gate
**Confidence:** HIGH

---

<user_constraints>
## User Constraints (from CONTEXT.md)

### Locked Decisions

**D-20-01:** Burst datagrams within a tick until `datagram_send_buffer_space()` is exhausted OR the diff is fully sent, whichever first. `datagram_send_buffer_space()` is the primary backpressure gate.

**D-20-02:** Add a generous safety cap on datagrams-per-tick set well above a full 80×24 repaint worth of datagrams (e.g. ~2× a full repaint) — high enough it never trips in normal TUI use; its sole purpose is to bound a pathological diff from monopolising the send buffer in a single tick. Do NOT use a tight fixed cap.

**D-20-03:** Within a single tick, call `build_state_diff` exactly once. Then drain the resulting run list by looping `encode_datagram` only (no per-datagram recompute of `fresh_runs`). Each iteration sends one payload and carries its `deferred` remainder to the next `encode_datagram` call within the same tick's budget. This is the architectural fix for R-1.

**D-20-04:** One epoch per tick. All burst datagrams sent within a tick share that tick's single epoch value. `confirmed_epoch` must not advance during a `read -s` window.

**D-20-05:** When a repaint spills past one tick's budget, the leftover runs carry to the next tick as `deferred` and ride the next tick's (new) epoch. The existing epoch guard at server.rs ~line 346 bumps the epoch because `pending_deferred` is non-empty. Do NOT hold one epoch across multiple ticks.

**D-20-06:** Preserve deferred-first ordering — carried-over deferred runs go ahead of freshly computed runs.

**D-20-07:** Change the `apply()` monotonic guard in `ClientScreen` from `<=` to `<` so multiple same-epoch burst datagrams within a tick all apply their runs to the confirmed grid.

**D-20-08:** No pacing-specific predictor change. The existing tentative-epoch machinery plus Phase 19 alt-screen suppression already cover burst repaints. Add only test coverage; do NOT add burst-detection state to the predictor.

**D-20-09:** `noecho_read_dash_s_zero_predicted_chars` must pass as a required, non-`#[ignore]` CI gate with burst code active. `burst_drains_when_grid_differs_from_acked_baseline` must pass RED-before-fix / GREEN-after.

### Claude's Discretion

- Exact value of the safety cap (within the "generous, never-trips-normally" guidance).
- Precise structure of the per-tick burst loop and how `datagram_send_buffer_space()` is queried each iteration.
- Test harness specifics for the 150 ms-RTT vim-startup assertion (SC1) — simulated/loopback acceptable.

### Deferred Ideas (OUT OF SCOPE)

- Burst-aware predictor suppression (explicit burst-detection state in the client) — rejected as scope creep (D-20-08).
- Changing the 16 ms tick interval — out of scope; bursting within a tick is the fix, not changing tick cadence.
</user_constraints>

<phase_requirements>
## Phase Requirements

| ID | Description | Research Support |
|----|-------------|------------------|
| PACE-01 | Full-screen repaint lands in ~1 RTT, multiple datagrams burst per tick, bounded by `datagram_send_buffer_space()` | Build loop design, quinn API, safety cap sizing |
| PACE-02 | Burst preserves noecho invariant — one epoch per tick, `noecho_read_dash_s_zero_predicted_chars` CI gate | Epoch guard analysis, test status, apply() guard change |
| PACE-03 | Two 999.4 traps cannot recur — no infinite spin, `burst_drains_when_grid_differs_from_acked_baseline` RED/GREEN | build_state_diff refactor, encode_datagram-only drain loop |
</phase_requirements>

---

## Summary

Phase 20 makes the server burst multiple state-diff datagrams within a single 16 ms tick so that full-screen repaints (vim startup, multi-line paste) arrive at the client in roughly one RTT instead of dribbling one MTU per tick. The architecture was attempted once before (999.4) and reverted due to two bugs that are now fully diagnosed: R-1 (infinite spin from recomputing `fresh_runs` against a non-advancing `last_acked_snapshot` every burst iteration) and R-2 (per-datagram epoch increment advancing `confirmed_epoch` during a `read -s` noecho window). Both bugs are prevented architecturally by the locked decisions.

The implementation touches three call sites: (1) the diff-tick arm in `run_session` (server.rs ~line 694), (2) the identical diff-tick arm in `run_reattach_session` (server.rs ~line 1186), and (3) the `apply()` monotonic guard in `ClientScreen` (screen.rs line 223, `<=` → `<`). The `build_state_diff` function itself does NOT change — the epoch guard and deferred-first ordering it already implements are correct. The burst loop wraps `build_state_diff` once then calls only `encode_datagram` + `send_datagram` in a loop.

The noecho integration test (`noecho_read_dash_s_zero_predicted_chars` in `crates/nosh-client/tests/predict.rs`) is currently NOT `#[ignore]`-tagged and already passes against the single-datagram server. With burst active, it remains the mandatory security gate: one epoch per tick means the client's `confirmed_epoch` does not advance more frequently than before, so the noecho structural invariant is unaffected.

**Primary recommendation:** Refactor the single-datagram send in both tick arms into a `send_burst()` helper that calls `build_state_diff` once, then loops `encode_datagram` + `send_datagram` until `datagram_send_buffer_space() < cap` or `pending_deferred.is_empty()` or a safety cap of 64 datagrams/tick is hit. Change `apply()` guard to `<`. Write `burst_drains_when_grid_differs_from_acked_baseline` as a RED-before unit test first.

---

## Architectural Responsibility Map

| Capability | Primary Tier | Secondary Tier | Rationale |
|------------|-------------|----------------|-----------|
| Burst loop shape (R-1 fix) | nosh-server tick arm | — | `build_state_diff` + `encode_datagram` live in server.rs; client is unchanged |
| Epoch assignment (R-2 fix) | nosh-server `build_state_diff` | — | Already assigns one epoch per call; burst wraps this without calling it again |
| Backpressure gate | QUIC layer (`datagram_send_buffer_space`) | server tick arm | Quinn API exposes the application-layer send buffer space; server queries it each iteration |
| Client receive guard | nosh-client `ClientScreen::apply()` | — | Single-character change at screen.rs:223 |
| Noecho invariant | nosh-client predictor (structural) | CI gate in tests/predict.rs | Tentative-epoch machinery is unchanged; test proves invariant against real server |

---

## Standard Stack

No new crates are added in Phase 20. All changes are within the existing workspace.

### Version Verification

| Crate | Version in workspace | Role in Phase 20 |
|-------|---------------------|------------------|
| `quinn` | 0.11.9 | `Connection::datagram_send_buffer_space()`, `send_datagram()`, `max_datagram_size()` |
| `bytes` | 1.x (transitive) | `Bytes` payload type returned by `encode_datagram` |

**No new dependencies. No `Cargo.toml` changes.**

---

## Package Legitimacy Audit

> Not applicable — Phase 20 installs no external packages.

---

## Architecture Patterns

### System Architecture Diagram

```
Per-tick (16 ms) diff arm in run_session / run_reattach_session:

  diff_interval.tick()
        │
        ▼
  max_datagram_size() → cap
        │
        ▼
  build_state_diff() ──────────────────────────────────┐
  [called EXACTLY ONCE per tick]                       │
  - snapshots terminal under lock                      │
  - increments current_epoch by 1 (if grid changed    │
    or pending_deferred non-empty)                     │
  - computes fresh_runs ONCE vs last_acked_snapshot    │
  - prepends pending_deferred (deferred-first order)   │
  - calls encode_datagram → (payload₀, deferred₀)     │
  returns DiffTickResult { payload, epoch, deferred }  │
        │                                              │
        ▼                                              │
  ┌─────────────────────────────┐                     │
  │  burst loop (new):          │                     │
  │  while deferred not empty   │                     │
  │  AND burst_count < CAP      │                     │
  │  AND datagram_send_buffer_  │                     │
  │      space() >= cap:        │                     │
  │    encode_datagram(         │◄────────────────────┘
  │      diff with same epoch,  │  [NO build_state_diff
  │      deferred runs only)    │   call here — R-1 fix]
  │    → (payloadₙ, deferredₙ) │
  │    send_datagram(payloadₙ)  │
  │    deferred = deferredₙ     │
  │    burst_count += 1         │
  └─────────────────────────────┘
        │
        ▼
  remaining deferred → pending_deferred
  (carried to NEXT tick; next tick's build_state_diff
   bumps epoch again because pending_deferred non-empty)
```

```
Client receive path (screen.rs apply()):

  conn.read_datagram() → StateDiff { epoch: E, runs: [...] }
        │
        ▼
  apply() monotonic guard:
    if diff.epoch < self.last_applied_epoch { return }  [< not <=]
    ↑ CHANGED from <= to <
    Burst datagrams D1, D2, D3 all carry epoch=E.
    D1: E > 0 (initial) → applies, sets last_applied_epoch = E
    D2: E < E is false → applies (same epoch, different runs)
    D3: E < E is false → applies
    All three write their runs to confirmed grid.
```

### Recommended Project Structure (no new files needed)

```
crates/nosh-server/src/
└── server.rs         ← burst loop in diff_interval.tick() arm (×2: run_session + run_reattach_session)

crates/nosh-client/src/
└── screen.rs         ← apply() guard: line 223, <= → <

crates/nosh-client/tests/
└── predict.rs        ← noecho_read_dash_s_zero_predicted_chars (remove #[ignore] if present, confirm runs with burst)

crates/nosh-server/tests/ (or integration location TBD)
└── burst.rs          ← NEW: burst_drains_when_grid_differs_from_acked_baseline (RED-before/GREEN-after)
                         Optional: vim_startup_burst_sc1 (SC1 latency gate)
```

### Pattern 1: Burst Loop Inside the Tick Arm

**What:** After calling `build_state_diff` once, send the first datagram then keep draining `deferred` via `encode_datagram` only (no fresh diff recompute) until the budget or cap is exhausted.

**When to use:** In both `run_session` and `run_reattach_session` diff-tick arms, replacing the current single-datagram send.

```rust
// Source: derived from verified quinn API + existing server.rs patterns
// In the diff_interval.tick() arm:

if !resume_complete { continue; }
let cap = match conn.max_datagram_size() {
    Some(c) if c >= MIN_CAP => c,
    _ => continue,
};

// ── D-20-03: build_state_diff called EXACTLY ONCE per tick ──────────────
let first_deferred = std::mem::take(&mut pending_deferred);
let Some(first) = build_state_diff(
    &slot,
    &mut current_epoch,       // epoch incremented here — once per tick (D-20-04)
    last_acked_epoch,
    &last_acked_snapshot,
    &last_sent_snapshot,
    first_deferred,
    cap,
) else { continue };

// Store the per-epoch snapshot for CR-01 (snapshot-at-send-time).
epoch_snapshots.push_back((first.epoch, first.sent_cells.clone()));
if epoch_snapshots.len() > EPOCH_SNAPSHOT_CAP { epoch_snapshots.pop_front(); }
last_sent_snapshot = first.sent_cells.clone();

// Send the first datagram.
let send_result = conn.send_datagram(first.payload.clone());
// handle send_result errors as before (UnsupportedByPeer/Disabled/ConnectionLost → break)

// ── D-20-01/D-20-02: burst remaining deferred runs ───────────────────────
// Reuse the SAME epoch from build_state_diff (D-20-04: one epoch per tick).
let tick_epoch = first.epoch;
let mut deferred = first.deferred;
let mut burst_count: usize = 1; // already sent one datagram above
const BURST_CAP: usize = 64; // ~2× full 80×24 repaint at typical MTU (D-20-02)

while !deferred.is_empty()
    && burst_count < BURST_CAP
    && conn.datagram_send_buffer_space() >= cap
{
    // Construct the diff for the next burst datagram using the carried deferred
    // runs and the SAME epoch (D-20-04). No build_state_diff call here (D-20-03).
    let diff = StateDiff {
        epoch: tick_epoch,
        cols, rows, cursor, alt_screen,  // from first.sent_cells or snapshot
        runs: deferred,
    };
    let (payload, next_deferred) = match encode_datagram(&diff, cap) {
        Ok(pair) => pair,
        Err(_) => break,
    };
    if let Err(e) = conn.send_datagram(payload) {
        use quinn::SendDatagramError::*;
        match e {
            TooLarge => {}
            UnsupportedByPeer | Disabled | ConnectionLost(_) => {
                // bubble to outer loop — set a flag or restructure
                break;
            }
        }
    }
    deferred = next_deferred;
    burst_count += 1;
}

// Carry leftover to next tick (D-20-05: next tick's build_state_diff bumps epoch).
pending_deferred = deferred;
```

**Design note:** The `StateDiff` constructed for burst iterations 2..N needs the terminal geometry (`cols`, `rows`, `cursor`, `alt_screen`) from the snapshot taken in the first `build_state_diff` call. Extract these from `first.sent_cells` dimensions or capture them alongside the `DiffTickResult`. The planner should decide whether to add these fields to `DiffTickResult` or capture them locally before the first call.

### Pattern 2: apply() Guard Change

**What:** Change `<=` to `<` at `screen.rs:223` so same-epoch burst datagrams all apply.

**Current code (line 222-224):**
```rust
// D-14-05: monotonic staleness check — discard stale or duplicate diffs.
if diff.epoch <= self.last_applied_epoch {
    return;
}
```

**After change:**
```rust
// D-14-05 / D-20-07: discard strictly older diffs only.
// Same-epoch burst datagrams (all datagrams in one tick share one epoch) MUST apply.
if diff.epoch < self.last_applied_epoch {
    return;
}
```

**Impact on existing tests:** The existing test `apply_monotonic_same_epoch_is_noop` (screen.rs:688) explicitly asserts that a same-epoch diff is a no-op. This test will need to be updated — it was written for the single-datagram-per-epoch model. After the change, same-epoch diffs apply (they are the burst datagrams). The test must be revised to test `epoch < last_applied` for the no-op case.

### Anti-Patterns to Avoid

- **Calling `build_state_diff` inside the burst loop:** Recomputing `fresh_runs` on every iteration against a non-advancing `last_acked_snapshot` refills the deferred queue faster than it drains — the R-1 infinite spin. The burst loop calls `encode_datagram` only.
- **Incrementing `current_epoch` per datagram:** Each epoch increment eventually causes `confirmed_epoch` to advance on the client, which violates the noecho invariant when a `read -s` is active — the R-2 regression. One epoch per tick; all burst datagrams share it.
- **Using `send_datagram_wait()`:** This async variant waits for buffer space, serialising sends across the `select!` boundary. Use synchronous `send_datagram()` in the burst loop and query `datagram_send_buffer_space()` to gate.
- **Breaking out of the burst on `TooLarge`:** `encode_datagram` guarantees payload fits within `cap` (which equals `max_datagram_size()`). A `TooLarge` error is unreachable if the code is correct. Match it as a skip, not a fatal break.
- **Holding the burst loop across a `.await`:** The entire burst runs synchronously within the `diff_interval.tick()` select arm body. No `.await` inside the burst loop (this is already true of `send_datagram` which is synchronous).

---

## Don't Hand-Roll

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| Send-buffer backpressure | Custom datagram count heuristic | `conn.datagram_send_buffer_space()` | Quinn exposes the application-layer queue depth; a fixed count fails under a congested path (may under-burst) or an uncongested path (may over-burst unnecessarily) |
| MTU sizing | Manual QUIC overhead subtraction | `conn.max_datagram_size()` | Quinn computes the negotiated MTU minus QUIC overhead; already used at the call site |
| Datagram payload sizing | Manual postcard serialisation budget | `encode_datagram(&diff, cap)` | Already handles splitting, deferred, and the strict `< cap` guarantee |

**Key insight:** The hard part of burst pacing is already solved by `encode_datagram`'s deferred-run return value. The burst loop is a thin wrapper that calls `encode_datagram` repeatedly with the deferred remainder.

---

## Common Pitfalls

### Pitfall 1: R-1 Infinite Spin (the 999.4 revert cause)

**What goes wrong:** `build_state_diff` is called on every burst iteration. It recomputes `fresh_runs = compute_diff_runs(&cells, last_acked_snapshot)` each time. `last_acked_snapshot` does not advance during a burst (epoch-acks arrive in the `conn.read_datagram()` arm which does not run while the synchronous burst arm is executing). `fresh_runs` is non-empty on every call. The deferred queue never drains. The loop spins forever; the session task pegs a CPU core.

**Root cause:** The burst loop called `build_state_diff` instead of `encode_datagram` on iterations 2..N.

**Prevention:** D-20-03 is the architectural fix. The burst loop calls `encode_datagram` only for iterations 2..N. The `burst_drains_when_grid_differs_from_acked_baseline` test must be written first (RED against a naive loop, GREEN after the fix).

**Warning signs:** Integration test hangs; CPU pegged on the server task; `pending_deferred.len()` not decreasing in a simulated burst.

### Pitfall 2: R-2 Noecho Epoch Leak (the 999.4 revert cause)

**What goes wrong:** `current_epoch` is incremented once per burst datagram instead of once per tick. The client receives burst datagrams with epochs E, E+1, E+2, ... On the client, each new epoch may cause `cull()` to run and advance `confirmed_epoch` even when the server never echoed the typed character. The structural noecho invariant (`confirmed_epoch` never advances during `read -s`) breaks.

**Root cause:** The epoch increment was inside the burst loop instead of outside it.

**Prevention:** D-20-04. The epoch is assigned by `build_state_diff` (which increments `current_epoch` once, when the grid changed or deferred is non-empty). All burst datagrams use the same `tick_epoch` extracted from the first `DiffTickResult`. `build_state_diff` is called once; `current_epoch` is incremented once.

**Warning signs:** `noecho_read_dash_s_zero_predicted_chars` fails; `predictor.confirmed_epoch()` advances past the initial value during the `read -s` window.

### Pitfall 3: apply() Guard Breaking Existing Tests

**What goes wrong:** Changing `<=` to `<` at screen.rs:223 causes the existing `apply_monotonic_same_epoch_is_noop` test to fail — it asserts that a diff with the same epoch is discarded. This was the correct behaviour for single-datagram-per-epoch but is wrong for burst.

**Prevention:** The test must be updated to reflect the new semantics: same-epoch diffs apply (burst), only strictly-older-epoch diffs are discarded. The updated test should assert that `diff.epoch < last_applied_epoch` is the discard condition. The regression guard should instead test `diff.epoch < last_applied_epoch` (e.g., epoch=1 after epoch=2 was applied).

**Warning signs:** The `apply_monotonic_same_epoch_is_noop` test fails after the guard change.

### Pitfall 4: DiffTickResult Missing Terminal Geometry for Burst Iterations

**What goes wrong:** The burst loop needs to construct a `StateDiff` for iterations 2..N using the same `cols`, `rows`, `cursor`, and `alt_screen` from the snapshot taken in `build_state_diff`. If these are not captured before the burst loop, the code either borrows the slot (requiring a re-lock) or uses stale values.

**Prevention:** Capture `cols`, `rows`, `cursor`, and `alt_screen` from the `DiffTickResult` or from the slot snapshot before entering the burst loop. Option A: add `cols`, `rows`, `cursor`, `alt_screen` fields to `DiffTickResult`. Option B: reconstruct them from `result.sent_cells.len()` and a local capture inside the tick arm. Option A is cleaner.

**Warning signs:** Borrow checker error on `slot` inside the burst loop; or the burst datagrams carry wrong geometry.

### Pitfall 5: EPOCH_SNAPSHOT_CAP Overflow with Burst

**What goes wrong:** The epoch-snapshot store (`epoch_snapshots: VecDeque<(u64, Vec<Vec<Cell>>)>`) is capped at `EPOCH_SNAPSHOT_CAP = 16`. With one epoch per tick (D-20-04), burst does not change the epoch count — the cap is unchanged. If burst accidentally incremented the epoch per datagram, sending 64 burst datagrams would require 64 snapshot entries, far exceeding the cap.

**Prevention:** D-20-04 (one epoch per tick) keeps the snapshot count at one per tick regardless of burst size. Only one `epoch_snapshots.push_back(...)` call per tick arm execution. Verify by checking `epoch_snapshots.len()` after a burst tick — it must be `<= EPOCH_SNAPSHOT_CAP`, which is trivially true at one push per tick.

**Warning signs:** `epoch_snapshots.len()` growing proportionally to burst size; snapshots being evicted before the client can ack them.

### Pitfall 6: run_reattach_session Has an Identical Tick Arm

**What goes wrong:** The diff-tick arm at server.rs ~line 1186 in `run_reattach_session` is a near-copy of the arm at ~line 694 in `run_session`. If only `run_session` is updated, reattached sessions dribble single datagrams per tick.

**Prevention:** Both tick arms must be updated. The planner should structure this as a `send_burst()` helper function called from both arms, or as two explicit code changes tracked in the plan.

**Warning signs:** vim-startup burst works on fresh sessions but not on reattached sessions.

---

## Code Examples

### Quinn API: datagram_send_buffer_space and send_datagram

```rust
// Source: verified in ~/.cargo/registry, quinn 0.11.9 connection.rs line 493
// pub fn datagram_send_buffer_space(&self) -> usize
// Returns: datagram_send_buffer_size.saturating_sub(datagrams.outgoing_total)
// Semantics: bytes free in the application-layer send queue (Layer 1).
// When space >= cap, send_datagram will not displace an older queued datagram.

let space: usize = conn.datagram_send_buffer_space();
if space < cap {
    break; // Layer 1 queue full for our datagram size
}
```

```rust
// Source: existing usage in server.rs lines 722, 1211
// pub fn send_datagram(&self, data: Bytes) -> Result<(), SendDatagramError>
// Synchronous. Returns Err on: TooLarge, UnsupportedByPeer, Disabled, ConnectionLost.
// Does NOT block or park the task.

if let Err(e) = conn.send_datagram(payload) {
    use quinn::SendDatagramError::*;
    match e {
        TooLarge => {} // unreachable if payload < max_datagram_size
        UnsupportedByPeer | Disabled | ConnectionLost(_) => break SessionEnd::TransportLost,
    }
}
```

```rust
// Source: existing usage in server.rs line 698
// pub fn max_datagram_size(&self) -> Option<usize>
// Returns None if datagrams were not negotiated or disabled.
// Returns Some(mtu) where mtu is the QUIC negotiated MTU minus overhead.

let cap = match conn.max_datagram_size() {
    Some(c) if c >= MIN_CAP => c,
    _ => continue,
};
```

### encode_datagram Drain Pattern (R-1-safe)

```rust
// Source: existing crates/nosh-proto/src/datagram.rs encode_datagram signature
// pub fn encode_datagram(diff: &StateDiff, cap: usize) -> Result<(Bytes, Vec<DiffRun>), ProtoError>
// The deferred Vec<DiffRun> is fed back into the NEXT encode_datagram call
// within the same tick, NOT into a new build_state_diff call.

let diff_for_burst = StateDiff {
    epoch: tick_epoch,  // SAME epoch as first datagram (D-20-04)
    cols: tick_cols,
    rows: tick_rows,
    cursor: tick_cursor,
    alt_screen: tick_alt_screen,
    runs: deferred,     // only the leftover runs from the previous encode_datagram
};
match encode_datagram(&diff_for_burst, cap) {
    Ok((payload, next_deferred)) => {
        // send payload, advance deferred = next_deferred
    }
    Err(_) => break,
}
```

### apply() Guard: Before and After

```rust
// BEFORE (screen.rs:223 — current state):
if diff.epoch <= self.last_applied_epoch {
    return; // discards same-epoch datagrams — wrong for burst
}

// AFTER (D-20-07):
if diff.epoch < self.last_applied_epoch {
    return; // discards only strictly older datagrams — same-epoch burst datagrams apply
}
```

### burst_drains_when_grid_differs_from_acked_baseline Test Skeleton

```rust
// Source: pattern derived from existing server.rs build_state_diff tests
// This test MUST be written RED-before-fix: a naive loop calling build_state_diff
// each iteration never terminates (pending_deferred.len() stays > 0).
// After the D-20-03 fix it terminates in ceil(cells/mtu_runs) iterations.

#[test]
fn burst_drains_when_grid_differs_from_acked_baseline() {
    // Setup: a non-empty grid vs an empty last_acked_snapshot.
    // A full 80×24 grid of non-space chars guarantees compute_diff_runs returns ~1920 cells.
    // With a typical MTU cap (~120 cells/datagram), draining takes ~16 iterations.

    let mut current_epoch = 0u64;
    let last_acked_epoch = 0u64;
    let last_acked_snapshot: Vec<Vec<Cell>> = Vec::new(); // empty baseline
    let last_sent_snapshot: Vec<Vec<Cell>> = Vec::new();
    // ... populate slot with a non-empty grid ...

    let cap = 1200; // typical MTU
    let max_iterations = 100; // generous bound; actual should be ~16-20

    // FIRST: call build_state_diff once (D-20-03)
    let first_deferred = vec![];
    let first = build_state_diff(
        &slot, &mut current_epoch, last_acked_epoch,
        &last_acked_snapshot, &last_sent_snapshot, first_deferred, cap,
    ).expect("should produce a result for a non-empty grid");
    
    let tick_epoch = first.epoch;
    let mut deferred = first.deferred;
    let mut count = 1;

    // THEN: drain via encode_datagram only (not build_state_diff again)
    while !deferred.is_empty() {
        assert!(count < max_iterations,
            "burst must drain in finite iterations (R-1 guard): still {} deferred after {} iters",
            deferred.len(), count);
        let diff = StateDiff { epoch: tick_epoch, /* geometry */ runs: deferred };
        let (_, next_deferred) = encode_datagram(&diff, cap).unwrap();
        deferred = next_deferred;
        count += 1;
    }
    // deferred is empty → the grid was fully encoded in `count` iterations.
    assert!(deferred.is_empty(), "deferred must be empty after burst drain");
}
```

### Safety Cap Sizing Rationale

An 80×24 terminal has 1920 cells. Each `DiffRun` covers a run of same-style characters. In practice, a vim startup writes many single-char runs (box-drawing, syntax tokens) — roughly 8–15 bytes per run in postcard. With a 1200-byte MTU, each datagram carries approximately 80–120 cells worth of runs. A full repaint therefore needs approximately 16–24 datagrams.

A safety cap of **64 datagrams/tick** is ~2.5–4× a full 80×24 repaint — generous enough that it never triggers for any normal TUI app, but bounds a pathological case (e.g. a 400-column terminal or a 200-line paste) at 64 × 1200 = ~76 KB of datagram payload in one tick. At 150 ms RTT, the send buffer is 1 MiB by default; 76 KB is well within it.

---

## State of the Art

| Old Approach | Current Approach | When Changed | Impact |
|--------------|------------------|--------------|--------|
| One datagram per tick (Phase 13 SYNC-03) | Burst within a tick until buffer exhausted | Phase 20 (this phase) | Full-screen repaint in ~1 RTT instead of N ticks × 16 ms |
| `epoch <= last_applied_epoch` discard | `epoch < last_applied_epoch` discard | Phase 20 (D-20-07) | Same-epoch burst datagrams all apply instead of only the first |

---

## Assumptions Log

| # | Claim | Section | Risk if Wrong |
|---|-------|---------|---------------|
| A1 | `datagram_send_buffer_space()` is on the public `quinn::Connection` API in 0.11.9 (not only on internal `quinn_proto` types) | Standard Stack, Code Examples | Compile error; fallback is a fixed `BURST_CAP` count only |

**Verification status for A1:** Confirmed by cargo registry source — `~/.cargo/registry/.../quinn-.../src/connection.rs:493` shows `pub fn datagram_send_buffer_space(&self) -> usize` on the public `Connection` type. [VERIFIED: cargo registry]

**If this table is otherwise empty:** All other claims in this research were derived from direct code inspection of the live codebase or from cargo registry source — no additional user confirmation needed.

---

## Open Questions

1. **Where do `cols`/`rows`/`cursor`/`alt_screen` come from for burst iterations 2..N?**
   - What we know: `build_state_diff` captures these under the slot lock and stores them in `sent_cells`. `DiffTickResult` currently only exposes `sent_cells` (from which dimensions can be derived) but not the cursor or `alt_screen` separately.
   - What's unclear: Whether to add fields to `DiffTickResult` (clean but requires a struct change) or capture them locally inside the tick arm before calling `build_state_diff` (slightly awkward but no struct change).
   - Recommendation: Add `cols: u16`, `rows: u16`, `cursor: CursorPos`, `alt_screen: bool` to `DiffTickResult` — the planner should task this as part of the `DiffTickResult` extension sub-task.

2. **SC1 test harness: how to simulate 150 ms RTT for the vim-startup timing assertion?**
   - What we know: The existing sync.rs harness uses in-process loopback (effectively 0 RTT). Quinn's `TransportConfig` does not expose RTT injection. The existing transport.rs and sync.rs tests use `client::connect` + in-process server, which has sub-millisecond RTT.
   - What's unclear: Whether the SC1 assertion can be proven via a unit/integration test or requires a live measurement. At 0 RTT loopback, the timing criterion (≤2 RTT ≈ 300 ms) is trivially satisfied by any implementation; the meaningful assertion is "full 80×24 repaint arrives in one tick's burst" (i.e., `pending_deferred.is_empty()` after the first burst tick).
   - Recommendation: The planner should frame SC1 as a structural assertion: after one tick fires and the burst loop completes, `pending_deferred.is_empty()` is true for an 80×24 grid. A live RTT timing test is a manual/observational check, not automated — document it as such in the plan.

3. **`apply_monotonic_same_epoch_is_noop` test must be updated**
   - What we know: This test (screen.rs:688) explicitly asserts that a second `apply()` call with the same epoch is a no-op. With `<` instead of `<=`, same-epoch applies are allowed — the test assertion flips.
   - Recommendation: Rename the test to `apply_monotonic_older_epoch_is_noop` and change the assertion: apply epoch=2, then apply epoch=1 (strictly older), assert the older is discarded. Add a new test `apply_same_epoch_burst_applies` that applies two diffs with the same epoch and asserts both sets of runs are written to the confirmed grid.

---

## Environment Availability

Step 2.6: SKIPPED (no external dependencies — all changes are in the existing Rust workspace with no new crates).

---

## Validation Architecture

### Test Framework

| Property | Value |
|----------|-------|
| Framework | `cargo nextest` (CLAUDE.md recommended) / `cargo test` |
| Config file | `nextest.toml` or workspace default |
| Quick run command | `cargo test -p nosh-client -- noecho burst screen` |
| Full suite command | `cargo nextest run` or `cargo test --workspace` |

### Phase Requirements → Test Map

| Req ID | Behavior | Test Type | Automated Command | File Exists? |
|--------|----------|-----------|-------------------|-------------|
| PACE-01 | Full-screen repaint arrives in one burst tick | unit (structural) | `cargo test -p nosh-server -- burst_drains` | ❌ Wave 0 |
| PACE-01 | vim-startup full repaint ≤2 RTT at 150 ms | manual/observational | — (live test only) | N/A |
| PACE-02 | Zero predicted chars during `read -s` with burst active | integration | `cargo test -p nosh-client --test predict -- noecho_read_dash_s_zero_predicted_chars` | ✅ exists |
| PACE-02 | One epoch per tick regardless of burst size | unit | `cargo test -p nosh-server -- one_epoch_per_tick` | ❌ Wave 0 |
| PACE-03 | Burst drain terminates, `build_state_diff` called once | unit | `cargo test -p nosh-server -- burst_drains_when_grid_differs_from_acked_baseline` | ❌ Wave 0 |
| PACE-03 | `datagram_send_buffer_space()` is the budget gate | code review | — | — |
| SC4 | Same-epoch burst datagrams all apply to confirmed grid | unit | `cargo test -p nosh-client -- apply_same_epoch_burst_applies` | ❌ Wave 0 |

### Sampling Rate

- **Per task commit:** `cargo test -p nosh-client -p nosh-server` (unit tests only, fast)
- **Per wave merge:** `cargo nextest run --workspace`
- **Phase gate:** Full suite green, `noecho_read_dash_s_zero_predicted_chars` passing as non-ignored, before `/gsd:verify-work`

### Wave 0 Gaps

- [ ] `crates/nosh-server/tests/burst.rs` — covers PACE-01 (structural), PACE-03 (RED/GREEN)
- [ ] `burst_drains_when_grid_differs_from_acked_baseline` — RED before fix, GREEN after; must be the first test written
- [ ] `one_epoch_per_tick` unit test — asserts `current_epoch` increments exactly 1 for N-datagram burst
- [ ] `crates/nosh-client/tests/screen_burst.rs` or extend `screen.rs` tests — `apply_same_epoch_burst_applies`, updated `apply_monotonic_older_epoch_is_noop`
- [ ] Confirm `noecho_read_dash_s_zero_predicted_chars` runs in CI without `#[ignore]` (verified: no `#[ignore]` tag present in tests/predict.rs)

---

## Security Domain

### Applicable ASVS Categories

| ASVS Category | Applies | Standard Control |
|---------------|---------|-----------------|
| V2 Authentication | no | — |
| V3 Session Management | no | — |
| V4 Access Control | no | — |
| V5 Input Validation | yes (client apply() guard) | OOB row/col guards in `apply()` are unchanged; `<` change does not affect them |
| V6 Cryptography | no | — |

### Known Threat Patterns

| Pattern | STRIDE | Standard Mitigation |
|---------|--------|---------------------|
| Epoch replay / epoch confusion | Tampering | `apply()` guard (`<`); server epoch is monotonically increasing; client discards strictly older |
| Burst causing confirmed_epoch to advance during noecho | Info Disclosure | D-20-04 (one epoch per tick); `noecho_read_dash_s_zero_predicted_chars` CI gate |
| Pathological diff monopolising send buffer | Denial of Service | D-20-02 safety cap (64 datagrams/tick); `datagram_send_buffer_space()` gate |

**Security invariant:** The noecho suppression mechanism (`confirmed_epoch` never advances when server suppresses echo) is structural — it is not an explicit flag. The R-2 fix (one epoch per tick) preserves this invariant. The `noecho_read_dash_s_zero_predicted_chars` test is the mandatory proof.

---

## Sources

### Primary (HIGH confidence)

- `crates/nosh-server/src/server.rs` — `build_state_diff` (~line 308), `DiffTickResult` (~line 280), epoch guard (~line 346), `run_session` tick arm (~line 694), `run_reattach_session` tick arm (~line 1186), `EPOCH_SNAPSHOT_CAP = 16` (~line 171). Direct code inspection.
- `crates/nosh-proto/src/datagram.rs` — `encode_datagram` (~line 261), `MIN_CAP = 8` (~line 40), `MAX_RUNS = 4096` (~line 27). Direct code inspection.
- `crates/nosh-client/src/screen.rs` — `apply()` at line 221; `<=` guard at line 223; `last_applied_epoch` field. Direct code inspection.
- `crates/nosh-client/tests/predict.rs` — `noecho_read_dash_s_zero_predicted_chars` at line 715; no `#[ignore]` attribute. Direct code inspection.
- `~/.cargo/registry/.../quinn-.../src/connection.rs:493` — `pub fn datagram_send_buffer_space(&self) -> usize` confirmed on the public `Connection` type. [VERIFIED: cargo registry source]
- `.planning/phases/999.4-predictive-echo-repaint-pacing-live-fix-round-2/999.4-RESEARCH.md` — quinn API semantics for `send_datagram`, `datagram_send_buffer_space`, `max_datagram_size`; recommended burst strategy. Prior research from the reverted attempt. [HIGH — same codebase, same quinn version]
- `.planning/research/PITFALLS.md` — R-1 and R-2 pitfall descriptions, exact failure modes. [HIGH — first-party documented production failures]

### Secondary (MEDIUM confidence)

- `.planning/phases/20-repaint-pacing/20-CONTEXT.md` — locked decisions D-20-01 through D-20-09. User decisions, treated as ground truth.
- `.planning/REQUIREMENTS.md` — PACE-01, PACE-02, PACE-03 requirement text.
- `.planning/ROADMAP.md` — Phase 20 goal and success criteria.

### Tertiary (LOW confidence)

- Safety cap value of 64 datagrams/tick — derived from calculation (80×24 cells ÷ ~120 cells/datagram ≈ 16 datagrams/repaint, × 4 ≈ 64). [ASSUMED calculation — no authoritative source for the exact value]

---

## Metadata

**Confidence breakdown:**
- Quinn API (`datagram_send_buffer_space`, `send_datagram`, `max_datagram_size`): HIGH — verified in cargo registry source and existing codebase usage
- Burst loop architecture (R-1/R-2 fixes): HIGH — derived from direct code inspection and first-party pitfall documentation
- apply() guard change: HIGH — single-line change with confirmed current state (`<=` at line 223)
- Noecho test status (no `#[ignore]`): HIGH — direct code inspection confirms no `#[ignore]` attribute
- Safety cap value (64): MEDIUM — calculated estimate; any value clearly above 24 and clearly below 1000 satisfies the guidance

**Research date:** 2026-06-07
**Valid until:** 90 days (stable codebase; quinn 0.11.x API is stable)
