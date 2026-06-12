---
phase: 22-scrollback-sync
reviewed: 2026-06-12T00:00:00Z
depth: standard
files_reviewed: 2
files_reviewed_list:
  - crates/nosh-server/src/channel.rs
  - crates/nosh-server/src/server.rs
findings:
  critical: 0
  warning: 2
  info: 2
  total: 4
status: issues_found
---

# Phase 22: Code Review Report — Server-side Scrollback Sync

**Reviewed:** 2026-06-12
**Depth:** standard (git-diff hunks + targeted context reads)
**Files Reviewed:** 2
**Status:** issues_found

## Summary

The Phase 22 changes introduce `run_scrollback_sender_task` in `channel.rs` and wire it into both `run_session` and `run_reattach_session` in `server.rs`. All five mandatory security invariants (S-1, S-4, S-5, MAX_PAGE_SIZE, ChannelClose routing) are structurally satisfied; no critical-severity bugs were found.

Two warnings were found: a `ChannelClose` signal that can be silently dropped (channel full), leaving the channel task alive indefinitely until the connection closes; and an edge case where a single scrollback page larger than `INITIAL_CREDIT` can permanently stall the task if the client never grants additional credit. Two info items address a stale comment and an asymmetric debug-log gap between the two session paths.

### Invariant verification summary

- **S-1** (SendStream only, no datagram path): confirmed — `run_scrollback_sender_task` signature is `&mut quinn::SendStream`; no `&quinn::Connection` parameter; no `send_datagram` call site is reachable.
- **S-5** (no await between epoch load and scrollback read): confirmed — `epoch_src.load(Ordering::Acquire)` at channel.rs:297 is immediately followed by `slot.with_terminal_state(...)` at channel.rs:298 with no intervening `.await`.
- **M-6 / S-4** (back-pressure, no busy-loop): confirmed — `remaining_credit == 0` path blocks on `events.recv().await`; inner credit-wait loop also blocks on `events.recv().await`; both use `saturating_add`/`saturating_sub`.
- **MAX_PAGE_SIZE** (client count clamped): confirmed — `(count as usize).min(MAX_PAGE_SIZE)` at channel.rs:285.
- **Accept-gate flip** (both session paths): confirmed — `ChannelType::Scrollback => true` in both `run_session` (server.rs:1072) and `run_reattach_session` (server.rs:1894). PortForward and AgentForward remain `false` in both paths.
- **ChannelClose via control_tx**: confirmed — all exit paths in `run_scrollback_sender_task` route `ChannelClose` through `control_tx`, never written to `ch_send` directly.
- **epoch_src.store placement**: confirmed — `epoch_src.store(current_epoch, Ordering::Release)` is inside the `if let Some(result) = build_state_diff(...)` block in both session paths; only called after `current_epoch` has been incremented by `build_state_diff`.

---

## Warnings

### WR-S-01: `try_send(ChannelEvent::Close)` can silently drop the close signal; channel task stays alive until connection tear-down

**File:** `crates/nosh-server/src/server.rs:1187` (also line 1229, 1989)
**Issue:** When the pump receives a client `ChannelClose` (or rejects a `ChannelAccept`) it calls `task_tx.try_send(ChannelEvent::Close)`. The scrollback channel task's `task_rx` has capacity 64. If 64 unprocessed `Credit` events are queued (e.g. a burst of credit grants while the task is blocked in the inner credit-wait loop on `write_all`), `try_send(Close)` returns `Err(Full)` and the result is ignored (`let _ = ...`). The channel entry is removed from `channel_map` immediately (line 1186 removes it before the try_send), but the spawned task never receives a close signal. The task will not exit until either:
- it returns from `run_scrollback_sender_task` naturally (ch_recv EOF or write error), or
- the entire connection is torn down (all `task_tx` senders are dropped, causing `events.recv()` to return `None`).

In practice the window is very narrow (64 credit events queued simultaneously), but unlike the echo test task, the scrollback task is a production path that can be open for the life of a session. A hung task holds `slot_clone` (an `Arc<SessionSlot>`) open, delaying registry cleanup.

**Fix:** Use `send` (async, backpressure) rather than `try_send` for `Close` events, or drain the event queue down to capacity-1 before sending Close. The existing precedent for high-priority events is already established in the Stream delivery path (see the WR-01 fix comment at line 1279: "use send (not try_send) for Stream events"). Apply the same logic to Close:

```rust
// Replace at lines 1186-1187 and equivalent reattach path:
if let Some(task_tx) = channel_map.remove(&channel_id) {
    // try_send can silently drop Close if queue is full; use send to guarantee delivery.
    // The pump is in a select! arm; awaiting here cannot deadlock because the task
    // processes events from the *same* task_rx, not from control_rx.
    let _ = task_tx.send(ChannelEvent::Close).await;
}
```

Note: this changes the pump arm from sync to async. If that is undesirable, an alternative is to reserve one slot in the channel (capacity 65, always leave 1 slot for Close) by issuing `try_reserve()` on channel creation.

### WR-S-02: A scrollback page larger than `INITIAL_CREDIT` permanently stalls the task if the client never grants additional credit

**File:** `crates/nosh-server/src/channel.rs:343`
**Issue:** `INITIAL_CREDIT` is 256 KiB. A `ScrollbackPage` for 1024 lines of a 220-column terminal with non-ASCII content can exceed 256 KiB after postcard encoding. In that case `encoded_len > remaining_credit` on the very first page response, and the inner credit-wait loop (lines 343–360) blocks indefinitely waiting for `ChannelEvent::Credit`. The pump only delivers `Credit` events when the client sends `ChannelCredit` messages. If the client never sends additional credit (e.g. a misbehaving or slow client, or a client that assumes the server will send before credit is extended), the scrollback task is permanently blocked in the inner wait loop. The task cannot be cancelled by a connection-level timeout because it is not polling `ch_recv` or the connection while waiting.

A well-behaved client should grant credit before receiving data (per MUX-03), so this is a protocol-compliance issue on the client side. However the server should defend against it.

**Fix:** Add a bounded timeout to the inner credit-wait loop, or impose a hard cap on `encoded_len` before entering the loop. The simplest server-side defence is to limit the encoded output to `MAX_PAGE_SIZE` lines (already done) AND also verify that `encoded_len <= INITIAL_CREDIT` before attempting the write; if the encoded page exceeds the window, trim the page to fewer lines. An alternative is to add a per-channel idle timeout:

```rust
// Inside the inner credit-wait loop, add a connection-level idle guard:
while remaining_credit < encoded_len {
    match tokio::time::timeout(
        Duration::from_secs(30),
        events.recv(),
    ).await {
        Ok(Some(ChannelEvent::Credit(n))) => {
            remaining_credit = remaining_credit.saturating_add(n);
        }
        Ok(Some(ChannelEvent::Close)) | Ok(None) | Err(_) => {
            // Err(_) = timeout expired; treat as close.
            let _ = ch_send.finish();
            let _ = tokio::time::timeout(Duration::from_secs(2), ch_send.stopped()).await;
            let _ = control_tx.send(Message::ChannelClose { channel_id }).await;
            return;
        }
        Ok(Some(ChannelEvent::Stream(_, _))) => {}
    }
}
```

---

## Info

### IN-S-01: Stale comment claims Phase 22-02 has not yet wired the scrollback handler

**File:** `crates/nosh-server/src/server.rs:1246–1253`
**Issue:** The comment at line 1246 reads "Until Phase 22-02 wires the scrollback channel task, treat any of these on the control stream as a logged no-op". The `tracing::debug!` at line 1253 likewise says "(Phase 22-02 wires handler)". Phase 22 is now implemented; the handler is wired. These comments describe a past-tense design decision (scrollback frames on the control stream are a protocol error) but frame it as a future deferral, which is misleading when reading the code after Phase 22.

**Fix:** Update the comment and log message to describe the invariant, not the deferral:

```rust
// ScrollbackRequest belongs on the scrollback channel's own RecvStream, not
// the control stream. Any of these on the control stream is a client protocol
// error; log and ignore (do not close the session — treat as a no-op).
Ok(Message::ScrollbackRequest { channel_id, .. })
| Ok(Message::ScrollbackPage { channel_id, .. })
| Ok(Message::ScrollbackCredit { channel_id, .. }) => {
    tracing::debug!(
        channel_id,
        "scrollback frame on control stream (protocol error); ignoring"
    );
}
```

### IN-S-02: `run_reattach_session` silently drops scrollback-on-control-stream frames; no debug log

**File:** `crates/nosh-server/src/server.rs:2010`
**Issue:** `run_session` has an explicit arm for `ScrollbackRequest | ScrollbackPage | ScrollbackCredit` arriving on the control stream (lines 1248–1255) with a `tracing::debug!` log. `run_reattach_session` has no such arm; these frames fall into the `Ok(_) => {}` catch-all at line 2010 and are silently discarded with no log output. This is not a bug (the session does not crash), but the asymmetry means a client sending scrollback frames on the wrong stream in a reattach session produces no diagnostic output, making protocol-level debugging harder.

**Fix:** Add an explicit arm in `run_reattach_session`'s control-stream match that mirrors the one in `run_session`:

```rust
Ok(Message::ScrollbackRequest { channel_id, .. })
| Ok(Message::ScrollbackPage { channel_id, .. })
| Ok(Message::ScrollbackCredit { channel_id, .. }) => {
    tracing::debug!(
        channel_id,
        "scrollback frame on control stream during reattach (protocol error); ignoring"
    );
}
```

Place this arm before the `Ok(_) => {}` catch-all.

---

_Reviewed: 2026-06-12_
_Reviewer: Claude (gsd-code-reviewer)_
_Depth: standard_
