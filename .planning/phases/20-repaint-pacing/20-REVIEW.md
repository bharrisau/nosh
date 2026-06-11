---
phase: 20-repaint-pacing
reviewed: 2026-06-11T03:00:00Z
depth: deep
files_reviewed: 4
files_reviewed_list:
  - crates/nosh-server/src/server.rs
  - crates/nosh-client/src/screen.rs
  - crates/nosh-client/tests/predict.rs
  - crates/nosh-client/tests/sync.rs
findings:
  critical: 0
  warning: 3
  info: 2
  total: 5
status: issues_found
---

# Phase 20: Code Review Report

**Reviewed:** 2026-06-11T03:00:00Z
**Depth:** deep
**Files Reviewed:** 4
**Status:** issues_found

## Summary

Phase 20 adds `send_burst()` to the server tick arms and changes the `apply()` monotonic guard from `<=` to `<`. The two primary failure modes from the reverted 999.4 attempt are correctly prevented: `build_state_diff` is never called inside the burst loop (R-1 fix), and all burst datagrams within a tick share one epoch (R-2 fix). The loop structure is sound, `BURST_CAP=64` is correctly applied, `epoch_snapshots.push_back` fires exactly once per tick, and both `run_session` and `run_reattach_session` receive identical burst wiring.

Three warnings are worth fixing before Phase 21 ships. None are blockers, but two have correctness consequences under adversarial or degraded conditions.

---

## Warnings

### WR-01: epoch=0 datagram now passes `apply()` guard at initial state

**File:** `crates/nosh-client/src/screen.rs:224`

**Issue:** The old guard `diff.epoch <= self.last_applied_epoch` discarded any datagram with `epoch=0` unconditionally once initialised, because `last_applied_epoch` starts at 0 and `0 <= 0` is true. The new guard `diff.epoch < self.last_applied_epoch` passes epoch=0 through when `last_applied_epoch == 0` (since `0 < 0` is false). A spoofed or malformed `epoch=0` datagram arriving before any legitimate datagram will now be applied to the confirmed grid, including its `cols`/`rows` values triggering a potential resize and its `confirmed_cursor` overwriting the default.

The server always starts epochs at 1 (verified in `build_state_diff`: `*current_epoch += 1` on first tick), so this is not a reachable path from a legitimate server. However, the old guard provided defence-in-depth against a malformed or replayed epoch=0 from a man-in-the-middle (within the QUIC connection's encryption boundary). The new guard removes that protection.

**Severity:** WARNING — not a correctness bug in normal operation; becomes an issue only if a QUIC-level attacker can inject epoch=0 datagrams, which requires breaking QUIC encryption. Low risk in practice, but the defence was free and has been removed.

**Fix:** Guard against epoch=0 explicitly, or document the new invariant. The cleanest fix is to add a comment acknowledging the change and note that the server-enforced minimum epoch=1 is the real guard:

```rust
// D-14-05 / D-20-07: discard strictly older diffs only.
// Same-epoch burst datagrams (all datagrams in one tick share one epoch) MUST apply.
// Note: epoch=0 is not sent by any legitimate server (build_state_diff starts at 1),
// so the edge case where last_applied_epoch==0 and diff.epoch==0 would apply is
// unreachable from the normal server path. QUIC encryption provides transport-level
// protection against injected epoch=0 datagrams from outside the connection.
if diff.epoch < self.last_applied_epoch {
    return;
}
```

Alternatively, add a secondary guard:

```rust
if diff.epoch == 0 || diff.epoch < self.last_applied_epoch {
    return;
}
```

---

### WR-02: `send_burst` ignores `TooLarge` on first datagram send, does not carry `result.deferred` to next tick

**File:** `crates/nosh-server/src/server.rs:433-441`

**Issue:** When `conn.send_datagram(result.payload)` returns `Err(TooLarge)` at the very first send of the tick, the code falls through with a `{}` no-op and proceeds to the burst loop using `result.deferred` as the starting deferred state. At this point, the first payload has NOT been sent. The deferred runs from `result.deferred` are subsequently sent as burst datagrams — but these deferred runs are the overflow from `encode_datagram` on the first call; they assume the first payload's runs were already received by the client. They were not.

The comment says "`TooLarge` is unreachable: `build_state_diff` guarantees payload < cap". This is correct for normal operation — `encode_datagram` is called with `cap = max_datagram_size()`. However, `TooLarge` can occur if the QUIC path MTU decreases between the `max_datagram_size()` query at the top of the tick arm and the `send_datagram` call inside `send_burst`. This is unlikely but not impossible on a path with PMTUD failures or route changes.

If `TooLarge` fires and the code continues:
1. The first datagram (with the most cursor-proximate content) is silently dropped.
2. The deferred runs (assumed to follow the first datagram in order) are sent as datagrams 2..N with the old client grid state being incorrect.
3. The client will apply these partial runs to its confirmed grid against an incomplete baseline, potentially corrupting the display.

The old single-datagram code had the same `TooLarge` no-op at the same line — so this is a pre-existing behaviour that Phase 20 perpetuates. However, Phase 20's burst loop makes it strictly worse: the old code would simply not send anything when `TooLarge` fired; the new code sends the deferred runs anyway, which are stale without the first payload.

**Fix:** Return immediately on `TooLarge` at the first send, carrying the full deferred back:

```rust
// Send the first datagram (already encoded by build_state_diff).
if let Err(e) = conn.send_datagram(result.payload) {
    use quinn::SendDatagramError::*;
    match e {
        TooLarge => {
            // Path MTU shrank between max_datagram_size() and send_datagram().
            // The first payload is gone; do NOT send deferred runs (they are
            // meaningless without the first payload). Carry all deferred to
            // the next tick — next tick's build_state_diff will recompute.
            return (result.deferred, false);
        }
        UnsupportedByPeer | Disabled | ConnectionLost(_) => {
            return (result.deferred, true);
        }
    }
}
```

---

### WR-03: `noecho` test skips silently when `/bin/bash` is absent; not a hard CI failure

**File:** `crates/nosh-client/tests/predict.rs:746-749`

**Issue:** `noecho_read_dash_s_zero_predicted_chars` returns early (passes vacuously) when `/bin/bash` is absent. The D-20-09 requirement says this must be a "required, non-`#[ignore]` CI gate". Silently skipping is a weaker guarantee than failing hard — in a CI environment where bash is unexpectedly absent (e.g. a minimal Docker image), the test passes without doing anything.

The old code had the same `return;` pattern for `/bin/sh`. The new code copies the pattern but now the test is a security gate, not just a functional one. On the CI system defined for this project (Linux, `/bin/bash` path exists), this is not a reachable failure — but the gap exists.

**Fix:** Replace the `return;` with a `panic!` or use `eprintln!` + `std::process::exit(1)`. The least-invasive option that keeps the pattern consistent with the rest of the test suite:

```rust
if !have_bash() {
    panic!(
        "SECURITY GATE: noecho_read_dash_s_zero_predicted_chars requires /bin/bash \
         (D-20-09 mandatory CI gate). bash was not found at {BASH}. \
         Ensure bash is installed in this CI environment."
    );
}
```

---

## Info

### IN-01: Extra `Vec<Vec<Cell>>` clone per tick (both tick arms)

**File:** `crates/nosh-server/src/server.rs:838,842` (run_session); `1330,1334` (run_reattach_session)

**Issue:** Phase 20 changes `last_sent_snapshot = result.sent_cells;` (move) to `last_sent_snapshot = result.sent_cells.clone();` because `result` is now moved into `send_burst()`. This means `sent_cells` is cloned twice per successful tick: once for `epoch_snapshots.push_back(...)` (was already a clone in the old code) and once for `last_sent_snapshot` (was a move, is now a clone). For an 80×24 grid of `Cell` structs, this is two allocations of `~1920 * sizeof(Cell)` per tick instead of one. This is per-tick overhead with no functional downside, but is worth noting since `last_sent_snapshot` was specifically designed to take ownership (move semantics) in the prior code to avoid this cost.

A refactor to avoid the extra clone: clone `result.sent_cells` once into `last_sent_snapshot` before moving `result` into `send_burst`, and use `last_sent_snapshot.clone()` for the epoch_snapshots push. But the order matters (the snapshot push must happen before `send_burst` for CR-01 correctness). The simplest fix:

```rust
// Clone once into last_sent_snapshot (ownership).
last_sent_snapshot = result.sent_cells.clone();
// Push a clone of last_sent_snapshot (not result.sent_cells directly).
epoch_snapshots.push_back((result.epoch, last_sent_snapshot.clone()));
if epoch_snapshots.len() > EPOCH_SNAPSHOT_CAP {
    epoch_snapshots.pop_front();
}
// Now result.sent_cells is still live for send_burst (it still moves).
// Actually result.sent_cells must move into send_burst — so the above doesn't save a clone.
```

On reflection, the extra clone is structurally unavoidable given that `send_burst` takes `result` by value and must not be refactored to take fields individually (that would increase API surface). This is a minor and acknowledged cost of the by-value design. Document it rather than change it.

---

### IN-02: `drain_datagrams_with_cull` cull is deferred but not re-invoked if multi-epoch tick boundary occurs mid-drain

**File:** `crates/nosh-client/tests/predict.rs:1063-1068`

**Issue:** `drain_datagrams_with_cull` defers cull until after all datagrams in the drain window are applied, then calls `predictor.cull(screen, epoch_after, 5)` once with the final epoch. This is the correct pattern described in D-20-04. However, if two distinct epoch values arrive within the same 500ms drain window (e.g. E1 burst from one tick, then E2 burst from the next tick — plausible at 500ms window with 16ms tick interval), cull is only called once with E2. Predictions made against E1 that should have been culled by E1's arrival will instead be culled (or aged out) when cull(E2) runs.

In practice this is benign for the noecho test: no predictions are made during the `read -s` window, so there is nothing to cull incorrectly. But for the `drain_datagrams_until_quiet` helper (which does cull per distinct epoch), the same-epoch deduplication via `last_culled_epoch` is correct. The divergence between the two helpers — one culls once per drain, the other culls once per distinct epoch — could cause subtle differences in non-noecho tests that use `drain_datagrams_with_cull`.

This is a test harness concern, not a production code concern. The production `apply()` and predictor are unchanged (D-20-08).

**Fix:** Acceptable as-is for the noecho security gate's use case. If `drain_datagrams_with_cull` is ever reused for tests where predictions must be culled per-epoch, refactor it to track `last_culled_epoch` (like `drain_datagrams_until_quiet`). Add a comment to the function to document the one-cull-per-drain limitation:

```rust
// NOTE: cull fires once per drain call (on the highest epoch seen), not once
// per distinct epoch within the window. This is correct for the noecho gate
// (no predictions during read -s) but is NOT correct for tests where mid-burst
// prediction culling accuracy matters. Use drain_datagrams_until_quiet for those.
```

---

_Reviewed: 2026-06-11T03:00:00Z_
_Reviewer: Claude (gsd-code-reviewer)_
_Depth: deep_
