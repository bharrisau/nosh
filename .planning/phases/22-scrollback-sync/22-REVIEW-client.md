---
phase: 22-scrollback-sync
reviewed: 2026-06-12T00:00:00Z
depth: standard
files_reviewed: 2
files_reviewed_list:
  - crates/nosh-client/src/channel.rs
  - crates/nosh-client/src/main.rs
findings:
  critical: 1
  warning: 2
  info: 2
  total: 5
status: issues_found
---

# Phase 22: Code Review Report (Client)

**Reviewed:** 2026-06-12
**Depth:** standard
**Files Reviewed:** 2 (diff hunks: bf67b1c..HEAD)
**Status:** issues_found

## Summary

Reviewed the Phase 22 scrollback-sync client additions: `run_scrollback_drain_task` in
`channel.rs` and the `ScrollbackView` state machine, `CsiAccumulator`, and associated
`run_pump` wiring in `main.rs`.

The drain task architecture is sound: requests travel on the channel's own `ch_send`
(avoiding the M-2 deadlock), `tokio::select!` decouples reads and writes, and
`try_send` drop-on-full is implemented correctly. The CSI accumulator handles split
reads correctly and does not swallow non-paging bytes. Snap-back on non-paging
keystrokes works correctly per T-22-15. Cold reattach starts `scrollback_view` as
`Live` on every `run_pump` entry (T-22-13). The A4 single-writer invariant is
respected throughout.

One critical correctness bug was found in the epoch gate that makes scrollback
non-functional on any live connection. Two warnings and two info-level findings are
also noted.

---

## Critical Issues

### CR-C-01: `epoch_at_snapshot` initialised to 0 — scrollback exits immediately on first datagram

**File:** `crates/nosh-client/src/main.rs:1790-1794`

**Issue:**
When entering `ScrollbackView::Active` on a Shift-PageUp keypress, `epoch_at_snapshot`
is initialised to `0`:

```rust
scrollback_view = ScrollbackView::Active {
    lines: vec![],
    offset: 0,
    pending_request: true,
    epoch_at_snapshot: 0, // updated on first page
    total_available: 0,
};
```

The SCROLL-05 epoch gate at line 1507 evaluates:

```rust
if diff.epoch < epoch_at_snapshot {
    // suppress display, emit ack, continue
} else {
    // exit scrollback → Live
}
```

With `epoch_at_snapshot = 0`, the condition `diff.epoch < 0` is always `false` for
`u64` (epochs are always >= 1 on any live connection). The `else` branch fires on the
**very next datagram** — before the first `ScrollbackPage` has had any chance to arrive
and update `epoch_at_snapshot`. The client enters Active mode and snaps back to Live on
the next datagram tick, making scrollback effectively non-functional.

The tests in `scrollback_view_epoch_tests` do not catch this because they use
`active_view(10, 100)` (`epoch_at_snapshot = 10`), never the actual initial value of 0.

**Fix:**
Capture the current screen epoch when entering Active mode, so the gate is bounded from
the moment of entry:

```rust
// In the Shift-PageUp handler, Live → Active transition:
let current_epoch = screen.last_applied_epoch();
scrollback_view = ScrollbackView::Active {
    lines: vec![],
    offset: 0,
    pending_request: true,
    epoch_at_snapshot: current_epoch, // actual epoch at time of entry
    total_available: 0,
};
```

The server will set `epoch_at_snapshot` in the first `ScrollbackPage` response to its
own atomic load of the epoch — which may be >= `current_epoch`. The client should take
the maximum of its own value and the server's value when the first page arrives, or
simply trust the server's value (it will be >= current_epoch since it was snapshotted
after the request was sent). Either way, the initial `0` must be replaced with
`screen.last_applied_epoch()`.

---

## Warnings

### WR-C-01: Credit fallback of `0` on re-encode failure understates consumed bytes

**File:** `crates/nosh-client/src/channel.rs:243-253`

**Issue:**
When `nosh_proto::codec::encode(&msg)` fails for credit accounting, the fallback
value is `0`:

```rust
let wire_bytes = match nosh_proto::codec::encode(&msg) {
    Ok(frame) => frame.len() as u64,
    Err(_) => {
        // Should never fail for a successfully decoded message.
        // If it does, use a conservative byte count.
        0   // ← NOT conservative; zero credits never replenish the server window
    }
};
```

The comment says "use a conservative byte count" but `0` is the opposite of
conservative — it means zero credit is ever counted for that message. If this
fallback fires repeatedly (e.g. due to an unforeseen postcard regression), the
server's flow-control window is never replenished and the server pump stalls
indefinitely. The comment is also misleading.

This is a theoretical failure path (re-encoding a successfully decoded message
cannot fail with the current postcard codec), but the fallback value is wrong if it
ever fires.

**Fix:**
Use the maximum expected `ScrollbackPage` frame size as the conservative fallback,
or at minimum document that `0` is intentional and explain the rationale. A simple
upper bound:

```rust
Err(_) => {
    // Should never fail. Use a generous upper bound (64 KiB) so the
    // server window is over-credited rather than stalled.
    tracing::warn!(channel_id, "re-encode failed; crediting max frame size");
    nosh_proto::codec::MAX_FRAME_LEN as u64 + 4
}
```

### WR-C-02: No guard against entering `Active` mode when scrollback channel is unavailable

**File:** `crates/nosh-client/src/main.rs:1787-1808`

**Issue:**
The Shift-PageUp handler unconditionally enters `ScrollbackView::Active` and sends a
`ScrollbackRequest`, regardless of whether the scrollback channel was accepted by the
server. If `open_channel` returned `None` (server rejected) or `Err` (open failed),
no drain task is spawned — `scrollback_req_tx` writes fill the 16-slot mpsc buffer and
are then silently dropped, and no page ever arrives.

Combined with CR-C-01 (epoch gate exits immediately), the user experience is a brief
flash or invisible no-op, but the state machine transitions through Active states
unnecessarily and `scrollback_req_tx.try_send` silently fails 16 times before dropping
requests on the floor.

**Fix:**
Track whether the scrollback channel is available and skip Active transitions when it
is not:

```rust
// After open_channel attempt, before the main loop:
let scrollback_available = scrollback_streams.is_some();
// (scroll_streams is consumed by tokio::spawn; track availability separately)

// In the Shift-PageUp handler:
if csi.shift_pageup && scrollback_available {
    // ... existing Active entry logic
}
```

---

## Info

### IN-C-01: `scrollback_ctrl_tx.clone()` — original sender is never used by `run_pump`

**File:** `crates/nosh-client/src/main.rs:1375`

**Issue:**
The drain task receives `scrollback_ctrl_tx.clone()` but the original
`scrollback_ctrl_tx` held by `run_pump` is never used to send anything. The clone
keeps a second sender alive until `run_pump` returns, which is correct semantics
(the receiver is kept open), but the pattern obscures intent. Moving the sender
instead of cloning is simpler:

```rust
tokio::spawn(run_scrollback_drain_task(
    scrollback_channel_id,
    ch_recv,
    ch_send,
    scrollback_ctrl_tx, // move, not clone — run_pump never sends on this
    page_tx,
    scrollback_req_rx,
));
```

If `run_pump` never needs the sender, the move makes the exclusivity clear and
avoids the extra `Arc` ref-count bump.

### IN-C-02: `pending` `Vec` has no enforced capacity bound despite "At most 8 bytes" doc claim

**File:** `crates/nosh-client/src/main.rs:228-234`

**Issue:**
The doc comment says "At most 8 bytes are ever held" but the implementation uses an
unbounded `Vec<u8>` with `Vec::with_capacity(8)` as a hint only. There is no assertion
or truncation to enforce the claim. In practice the claim holds (the maximum valid
prefix is 5 bytes), but a logic error in a future modification could silently allow
unbounded growth without any compile-time or run-time guard.

This is low risk as-is, but worth a `debug_assert` for confidence:

```rust
// At the end of CsiAccumulator::process, before returning:
debug_assert!(
    self.pending.len() < 6,
    "CsiAccumulator pending buffer exceeded 5 bytes: {} (split-read invariant violated)",
    self.pending.len()
);
```

---

_Reviewed: 2026-06-12_
_Reviewer: Claude (gsd-code-reviewer)_
_Depth: standard_
