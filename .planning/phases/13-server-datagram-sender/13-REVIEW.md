---
phase: 13-server-datagram-sender
reviewed: 2026-06-01T19:01:31Z
depth: standard
files_reviewed: 8
files_reviewed_list:
  - crates/nosh-proto/src/datagram.rs
  - crates/nosh-proto/src/lib.rs
  - crates/nosh-server/src/registry.rs
  - crates/nosh-server/src/server.rs
  - crates/nosh-client/tests/sync.rs
  - crates/nosh-client/src/client.rs
  - crates/nosh-client/src/main.rs
  - crates/nosh-client/tests/migration.rs
findings:
  critical: 2
  warning: 4
  info: 3
  total: 9
status: issues_found
---

# Phase 13: Code Review Report

**Reviewed:** 2026-06-01T19:01:31Z
**Depth:** standard
**Files Reviewed:** 8
**Status:** issues_found

## Summary

Phase 13 adds the server-side acked-epoch StateDiff datagram loop, epoch-ack wire
format (`encode_epoch_ack` / `decode_epoch_ack`), the `with_terminal_state`
Mutex-delegate, `compute_diff_runs`, and `build_state_diff`. The implementation is
generally sound but contains two blockers and four warnings.

The two critical issues are: (1) stale snapshot snapped at ack-receipt time instead
of at epoch-sent time, which means the baseline can regress under interleaved ticks,
and (2) the epoch-increment gate in `build_state_diff` silently skips incrementing
the epoch when the grid is unchanged but deferred runs remain, which starves those
runs indefinitely. Four warnings cover edge-case correctness gaps in
`compute_diff_runs`, `run_reattach_session`, and the sync test.

---

## Critical Issues

### CR-01: Acked-snapshot snapped at receive time, not at epoch-sent time — baseline can regress

**File:** `crates/nosh-server/src/server.rs:650-656` (and mirror at `1086-1092`)

**Issue:** When the server receives an epoch-ack for epoch E, it immediately
snapshots the *current* terminal state as the new `last_acked_snapshot`:

```rust
Ok(acked) if acked > last_acked_epoch => {
    last_acked_epoch = acked;
    last_acked_snapshot = slot.with_terminal_state(|ts| {
        ts.viewport_rows().map(|(_, row)| row.to_vec()).collect()
    });
}
```

The terminal state at ack-receipt time is NOT the state the client actually confirmed
seeing. Between when the server sent the datagram for epoch E and when it received the
ack, the PTY may have produced more output, advancing the terminal grid by N further
rows of changes. The new "baseline" now includes those N changes — changes the client
has not seen. On the next tick, `compute_diff_runs` diffs the current grid against
this artificially-advanced baseline and misses those N changes entirely. They are
simply never re-sent unless the client acks again — which it cannot do, because the
server never sends a diff for them.

The correct approach is to snapshot the terminal state at the time the datagram for
epoch E is built, and store that per-epoch snapshot for later use as the baseline when
epoch E is acked. The current code instead takes a snapshot at an arbitrary later
point, which can skip changes that the client never received.

This is the "snapshot-at-ack-time" design described in the research as
"self-correcting" for the WEAKER assertion, but it does NOT self-correct when
the skipped change corresponds to cells that subsequently match the new state (i.e.
text was written and then overwritten before the next ack cycle).

**Fix:** Store the snapshot of `cells` from the tick that produced epoch E, keyed by
epoch. On ack receipt, look up and apply the stored snapshot for that epoch:

```rust
// In build_state_diff, store sent snapshot in a per-epoch map:
let sent_snapshot = cells.clone(); // already computed
// ...
Some(DiffTickResult {
    payload,
    sent_cells: cells,
    epoch: *current_epoch,
    deferred,
})

// In the epoch-ack arm, use the stored per-epoch snapshot:
Ok(acked) if acked > last_acked_epoch => {
    last_acked_epoch = acked;
    if let Some(snap) = epoch_snapshots.remove(&acked) {
        last_acked_snapshot = snap;
    }
    // if not found (e.g. ack for an epoch older than the map), keep current baseline
}
```

A bounded `VecDeque<(u64, Vec<Vec<Cell>>)>` capped at (e.g.) 8 entries is sufficient;
older entries are pruned when new epochs are emitted.

---

### CR-02: Epoch not incremented when grid is unchanged but deferred runs remain — deferred runs starve

**File:** `crates/nosh-server/src/server.rs:299-308`

**Issue:** The epoch-increment gate in `build_state_diff` is:

```rust
// D-13-02a: skip if grid unchanged AND client is caught up.
if cells == last_acked_snapshot && last_acked_epoch >= *current_epoch {
    return None;
}

// Epoch management: increment at tick time when the grid changed since the last
// *sent* snapshot (not per-chunk).
if cells != last_sent_snapshot {
    *current_epoch += 1;
}
```

Consider this scenario: the grid was active, a datagram was sent at epoch E1, some
runs were deferred. The PTY then goes quiet and the grid does not change. On the next
tick:

- `cells == last_acked_snapshot` is FALSE (the client has not acked E1 yet), so the
  function does NOT return `None` — correct.
- `cells != last_sent_snapshot` is FALSE (the grid has not changed since E1 was sent).
- Therefore `*current_epoch` stays at E1; no increment.

The function then calls `compute_diff_runs` again, prepends the deferred runs, and
calls `encode_datagram` with `epoch = E1` — the same epoch as the previous datagram.
The client receives this second datagram with `epoch == E1`, compares it against
`last_applied_epoch` (also E1 after applying the first one), and discards it as a
duplicate (`epoch > last_applied_epoch` is false).

The deferred runs are never delivered until the grid changes again. On a large static
screen (e.g. full-screen `less` or `vim` at idle) this could starve the deferred
runs indefinitely.

**Fix:** Increment the epoch whenever there are deferred runs to send, even if the
grid itself has not changed:

```rust
if cells != last_sent_snapshot || !pending_deferred.is_empty() {
    *current_epoch += 1;
}
```

The `pending_deferred` check is correct here because `pending_deferred` is the
argument passed in and reflects the actual backlog before the fresh_runs are merged.

---

## Warnings

### WR-01: `compute_diff_runs` inner run-extension loop does not skip unchanged cells — oversized runs

**File:** `crates/nosh-server/src/server.rs:233-241`

**Issue:** The outer loop correctly skips changed cells to find a run start. But the
inner run-extension loop extends the run as long as style/fg/bg are consistent,
without checking whether each subsequent cell is actually changed vs. the baseline:

```rust
// Extend run while style/fg/bg are consistent.
while (col as usize) < current_row.len() {
    let cc = col as usize;
    let c2 = &current_row[cc];
    if c2.style != style || c2.fg != fg || c2.bg != bg {
        break; // style change: end run here
    }
    chars.push(c2.ch);
    col += 1;
}
```

This means that once a changed cell is found at `start_col`, the entire contiguous
region of cells with matching style is included in the run — even if most of them are
unchanged. For a 80-column row where only column 5 changed and columns 6-79 are
identical to the baseline, the run will include all 75 unchanged trailing cells.

The comment in the code acknowledges this: "The scanner breaks a run when the cell's
style/fg/bg changes (not on the first unchanged cell), so adjacent cells with identical
attributes are merged into one run even if some are unchanged." The rationale is
"trades slightly larger runs for fewer fragments." This is a design choice but it has
a concrete correctness consequence: the datagram cap is consumed by unchanged cells,
evicting changed cells from other rows into the deferred queue (compounding CR-02).

**Fix:** Break the inner extension loop on the first unchanged cell (or add a flag to
stop at the first cell that matches the baseline exactly):

```rust
while (col as usize) < current_row.len() {
    let cc = col as usize;
    let c2 = &current_row[cc];
    if c2.style != style || c2.fg != fg || c2.bg != bg {
        break;
    }
    // Stop extending if this cell is unchanged — don't include gratuitous
    // unchanged cells in the run (avoids wasting datagram cap, CR-02 amplifier).
    let base2 = baseline_row.get(cc);
    if base2.map(|b| b == c2).unwrap_or(false) {
        break;
    }
    chars.push(c2.ch);
    col += 1;
}
```

---

### WR-02: `run_reattach_session` ShellExited arm removes the slot from registry even though the original exit watcher already does so — double removal is idempotent but the wrong code path removes with the wrong key

**File:** `crates/nosh-server/src/server.rs:1148-1149`

**Issue:** In the `ShellExited` arm of `run_reattach_session`:

```rust
// Remove the slot (the original watcher may also do this; remove is idempotent).
registry.remove(&identity_raw, session_id);
```

`registry.remove` removes by `session_id` using `retain`. Meanwhile the original exit
watcher from `run_session` calls `registry.remove_slot` which removes by `Arc::ptr_eq`.
Both paths are exercised when the shell exits during a reattach session. The `remove`
call on the reattach path removes by `session_id`, which is correct in the common case.
However, if the original watcher has already called `remove_slot` — and the reattach
loop then opens a new session (same identity, same session_id, different `Arc`) — the
`remove` call would inadvertently remove the NEW session's slot, not just the dead one.

In the current code this cannot happen because `session_id` is a UUID unique per
`SessionSlot::new`. But the invariant is fragile: the comment says "remove is
idempotent" which is only true while the session_id is unique. The correct idiom for
the reattach path is `remove_slot` (pointer-equality) to match the original exit
watcher.

**Fix:** Replace `registry.remove(&identity_raw, session_id)` in the reattach
ShellExited arm with `registry.remove_slot(&slot)`.

---

### WR-03: `build_state_diff` drops the OLDEST deferred runs when the queue exceeds MAX_RUNS — cursor-proximate priority is destroyed

**File:** `crates/nosh-server/src/server.rs:318-322`

**Issue:** The deferred-queue cap is:

```rust
if all_runs.len() > MAX_RUNS {
    let drop = all_runs.len() - MAX_RUNS;
    all_runs.drain(..drop);
}
```

`all_runs` is assembled with `pending_deferred` first (the deferred backlog), then
`fresh_runs` appended. Draining from the front removes the deferred runs first — i.e.
the runs that were previously sorted cursor-proximate by `encode_datagram`. The fresh
runs (appended at the end) are retained even though they have not been prioritized yet.

This inverts the priority: content that was already cursor-sorted and waiting for
retransmission is evicted in favour of unsorted fresh content. A subsequent
`encode_datagram` call then re-sorts everything (including the fresh runs), which may
or may not correct the priority, but the dropped deferred runs are simply lost.

The correct behaviour when the queue must be capped is to truncate from the END (least
cursor-proximate runs, since `pending_deferred` entries were already sorted) or, more
robustly, to sort the merged list and then cap:

**Fix:** Drain from the END instead of the front:

```rust
if all_runs.len() > MAX_RUNS {
    all_runs.truncate(MAX_RUNS);
}
```

Or, if cursor-priority on the combined list is desired, sort first, then truncate.

---

### WR-04: `sync03_datagrams_flow_after_resume` test uses a timing-based idle-drain heuristic that is not cancellation-safe

**File:** `crates/nosh-client/tests/sync.rs:319-340`

**Issue:** The test drains the replay PtyData burst with a "3 consecutive idle
windows of 200ms each" heuristic:

```rust
let mut idle_strikes = 0u32;
loop {
    match tokio::time::timeout(
        Duration::from_millis(200),
        nosh_proto::read_message(&mut recv2),
    ).await {
        ...
        Err(_) => {
            idle_strikes += 1;
            if idle_strikes >= 3 {
                break; // replay burst is exhausted
            }
        }
    }
}
```

This is not cancellation-safe: if the `nosh_proto::read_message` future is cancelled
inside `timeout()` while mid-message-read (i.e. it read the length prefix but not the
body), the next call to `read_message` will try to parse a body where there is a new
message header. The `RecvStream` is a sequential byte stream and cancellation of a
partially-completed read leaves the stream in an inconsistent parse position. This can
cause the subsequent `send_input` call to appear to work but the test will incorrectly
read a corrupted frame as the first post-resume diff.

`nosh_proto::read_message` is the length-prefixed framing reader. If `timeout`
cancels it mid-read, the next call reads into the middle of the previously-started
frame.

**Fix:** Use a separate mechanism to determine replay completion — for example, check
that `conn2.read_datagram()` returns a StateDiff with a non-zero epoch (the server
only fires datagrams after `resume_complete = true`), or restructure the test to not
cancel `read_message` in flight. The simplest safe fix is to not cancel the future but
instead track wall-clock time:

```rust
let drain_deadline = tokio::time::Instant::now() + Duration::from_millis(600);
loop {
    let remaining = drain_deadline.saturating_duration_since(tokio::time::Instant::now());
    if remaining.is_zero() { break; }
    match tokio::time::timeout(remaining, nosh_proto::read_message(&mut recv2)).await {
        Ok(Ok(nosh_proto::Message::PtyData { .. })) => { /* keep draining */ }
        Ok(Ok(_)) => {}
        Ok(Err(_)) => break,
        Err(_) => break, // deadline expired without cancellation mid-message
    }
}
```

This still has the cancellation hazard if `timeout` fires while `read_message` holds
partial state. The fully correct fix is to make the drain loop non-cancellable, or to
restructure so the datagram arm drives progress instead of the stream arm.

---

## Info

### IN-01: `decode_epoch_ack` error type does not distinguish "wrong tag" from "bad body" — diagnostic opacity

**File:** `crates/nosh-proto/src/datagram.rs:187-190`

**Issue:** Both "wrong tag byte" and "truncated/corrupt postcard body" errors are
mapped to the same `ProtoError::Postcard(postcard::Error::DeserializeBadEncoding)`
value. This is correct for the no-oracle security property (callers should not
distinguish them), but it makes unit-test diagnosis and log-level triage harder. The
doc-comment correctly describes the distinction but the runtime value is opaque.

This is an info-level concern; the security invariant is correct.

**Fix:** No change required for correctness. If future diagnostic tooling needs to
distinguish the two paths, introduce a `ProtoError::UnknownTag` variant.

---

### IN-02: `compute_diff_runs` row index cast `row_idx as u16` will silently wrap for terminals > 65535 rows

**File:** `crates/nosh-server/src/server.rs:210`

**Issue:**

```rust
let row = row_idx as u16;
```

`TerminalState` is clamped to 1000 rows by `SessionSlot::resize`, so `row_idx` will
never reach 65535 in production. However if the clamp is ever relaxed or if a direct
`TerminalState::resize` call bypasses `SessionSlot`, the cast silently wraps. The cast
should be documented as safe-by-invariant or replaced with a debug assertion.

**Fix:** Add a debug assertion or a comment:

```rust
debug_assert!(row_idx <= u16::MAX as usize, "row index exceeds u16 range; resize cap must prevent this");
let row = row_idx as u16;
```

---

### IN-03: `run_session` and `run_reattach_session` duplicate the entire diff-tick select! arm — maintenance risk

**File:** `crates/nosh-server/src/server.rs:610-663` and `1051-1097`

**Issue:** The `diff_interval.tick()` arm and the `datagram = conn.read_datagram()`
arm in `run_session` are copy-pasted verbatim into `run_reattach_session`. Any fix to
one (e.g. the CR-01 snapshot-at-send-time fix) must be applied to both. The migration
test (`migration.rs`) and the sync tests (`sync.rs`) both exercise these paths
independently, so a divergence is unlikely to be caught in testing until the paths
develop separately.

**Fix:** Extract both arms into a shared async fn or a macro. For example, `build_and_send_diff` could replace the tick arm inline. The epoch-ack arm could be a small helper
`handle_epoch_ack(bytes, &mut last_acked_epoch, &mut last_acked_snapshot, &slot)`.

---

_Reviewed: 2026-06-01T19:01:31Z_
_Reviewer: Claude (gsd-code-reviewer)_
_Depth: standard_
