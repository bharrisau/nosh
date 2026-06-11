---
phase: 21-channel-multiplexing-foundation
reviewed: 2026-06-12T00:00:00Z
depth: deep
files_reviewed: 8
files_reviewed_list:
  - crates/nosh-proto/src/messages.rs
  - crates/nosh-proto/src/codec.rs
  - crates/nosh-server/src/channel.rs
  - crates/nosh-server/src/server.rs
  - crates/nosh-server/src/registry.rs
  - crates/nosh-client/src/channel.rs
  - crates/nosh-client/src/client.rs
  - crates/nosh-client/tests/channel_mux.rs
findings:
  critical: 2
  warning: 3
  info: 2
  total: 7
status: issues_found
---

# Phase 21: Code Review Report

**Reviewed:** 2026-06-12
**Depth:** deep
**Files Reviewed:** 8
**Status:** issues_found

## Summary

Phase 21 adds the channel-multiplexing foundation: five new `Message` variants with a discriminant-stability test, server and client channel task modules, control-stream dispatch in `run_session`/`run_reattach_session`, and a six-test integration suite. The overall architecture is sound: the security gates (auth-gated accept_bi, unconditional PortForward/AgentForward rejection, opaque REJECT, MAX_OPEN_CHANNELS cap) are correctly placed. The discriminant-stability test is complete and correct.

Two BLOCKER-class defects were found. The first is a silent data-loss bug in the echo loop's credit accounting: bytes that arrive beyond the current credit window are received from the QUIC stream but silently dropped on the echo path, yet credit is never charged for them, creating an accounting divergence the peer cannot detect. The second is a stale-stream resource leak: when `accept_bi` resolves a stream whose `channel_id` has no entry in `channel_map`, the stream is dropped without resetting it, leaving the peer's `read_exact` hanging until the QUIC idle timeout fires.

---

## Critical Issues

### CR-01: Echo loop silently discards bytes when `remaining_credit < n` — credit accounting diverges

**File:** `crates/nosh-server/src/channel.rs:228–234`

**Issue:** When `n` bytes are read from `ch_recv` but `remaining_credit < n`, the echo loop sends only `to_send = min(n, remaining_credit)` bytes, then decrements `remaining_credit` by `to_send`. The remaining `n - to_send` bytes are **discarded** — they are consumed from the QUIC RecvStream (the `read` call advanced the stream position) but never written to `ch_send`. Because those bytes are silently dropped, the peer's read of the full payload never completes; it hangs waiting for bytes that will never arrive.

At the same time, `remaining_credit` reaches 0 after the partial send. The task then blocks waiting for `ChannelEvent::Credit`. The credit replenishment arrives because the client has been draining `ch_recv`, but the server never sent the missing `n - to_send` bytes. The session is now in an unrecoverable state for that channel: the client's drain loop reports `drained_since_replenish` that is inconsistent with what the server actually echoed.

This is not reachable in the standard round-trip tests (which write small payloads well within the 256 KiB window), but is reachable in `channel_flow_control_backpressure` when the 256 KiB write chunks straddle the credit boundary, or in any production channel type where data arrives in chunks larger than the remaining window.

The correct fix is to **never** read more bytes from the stream than `remaining_credit` permits. If the task cannot send all bytes it has already read, it must either buffer them or stop reading until credit is replenished. The simplest sound fix is to cap the read buffer to `remaining_credit` bytes on each iteration, so `n <= remaining_credit` is always guaranteed:

```rust
// Limit the read to however many bytes we can actually send.
let read_cap = remaining_credit.min(buf.len() as u64) as usize;
let read_res = ch_recv.read(&mut buf[..read_cap]).await;
```

With this change `to_send == n` always holds and the `min` guard becomes a tautology (can be removed). `remaining_credit` is decremented by the full `n` and the invariant is preserved.

### CR-02: Dropped stream without reset leaks the peer's `read_exact` until idle timeout

**File:** `crates/nosh-server/src/server.rs:1200–1216` (same pattern at `1860–1886`)

**Issue:** When `accept_bi` resolves a stream and the channel-id varint prefix identifies an unknown or already-closed channel, the `(ch_send, ch_recv)` pair is simply dropped:

```rust
Ok(channel_id) => {
    if let Some(task_tx) = channel_map.get(&channel_id) {
        let _ = task_tx.try_send(ChannelEvent::Stream(ch_send, ch_recv));
    } else {
        // ch_send and ch_recv are silently dropped here
        tracing::debug!(...);
    }
}
```

When `ch_send` is dropped without calling `finish()` or `reset()`, quinn closes the stream with a `RESET_STREAM` frame at the QUIC layer — but only when it flushes. Crucially, `ch_recv` is also dropped without reading it to EOF, which means the peer (the client) issued `open_bi()` and is currently blocked inside `write_all(&prefix)` or a subsequent read. That call will eventually return an error when quinn propagates the reset, but the timing depends on quinn's internal flush, not on the application. In the worst case (under the QUIC idle timeout, which may be tens of seconds), the client's `read_exact` hangs.

More importantly, the dropped `ch_send` does NOT send a `STREAM_FIN` — it sends a reset — which is an error-level signal, not a clean EOF. Any client code doing `read_to_end` or `read_exact` after successfully writing the varint prefix will receive a `ConnectionError` or `ReadError::Reset` instead of a clean `Ok(None)`, making the error harder to diagnose.

The correct fix is to explicitly reset the send side (signalling a clean abandonment) and drain or stop the recv side before dropping:

```rust
} else {
    tracing::debug!(channel_id, "accept_bi: no channel task for id; dropping stream");
    // Reset the send side explicitly so the peer gets a clean signal.
    let _ = ch_send.reset(0u32.into());
    // Optionally stop the recv side to free QUIC flow-control window.
    ch_recv.stop(0u32.into()).ok();
}
```

The same pattern applies at `run_reattach_session`'s accept_bi arm (line ~1868).

---

## Warnings

### WR-01: `try_send` for `ChannelEvent::Stream` silently drops the stream if the task is busy

**File:** `crates/nosh-server/src/server.rs:1206–1209` (and `1865–1868` in `run_reattach_session`)

**Issue:** The stream-bind event uses `try_send`, which returns `Err(TrySendError::Full)` silently when the channel task's mpsc buffer (capacity 64) is full. In that case `ch_send` and `ch_recv` are dropped — producing the same reset-without-finish issue described in CR-02, and silently failing to bind the data stream to the channel task. The channel task will then block forever in its `loop { match events.recv().await { Some(ChannelEvent::Stream(s,r)) => break (s,r), ... } }` loop, waiting for a stream that will never arrive. The channel is in a live-but-permanently-blocked state; neither side makes progress; no error is surfaced; the map entry and task persist until the session closes.

The `Credit` and `Close` events correctly use `try_send` because losing a credit grant is recoverable (backpressure) and a lost Close just means a delayed teardown. Losing the `Stream` event is not recoverable without re-opening the channel.

Fix: use `task_tx.send(ChannelEvent::Stream(ch_send, ch_recv)).await` at the stream-bind site. Since this is inside a `select!` arm that already awaits, the `.await` is valid. A full channel (capacity 64) while waiting to bind a stream is pathological; the backpressure is acceptable.

```rust
// Use send (not try_send) for Stream so a full channel does not silently
// discard the stream-bind event.
if task_tx.send(ChannelEvent::Stream(ch_send, ch_recv)).await.is_err() {
    // Task already exited; log and continue.
    tracing::debug!(channel_id, "accept_bi: channel task gone before stream arrived");
}
```

### WR-02: `recv_or_pending` keeps `server_open_tx` alive in production build even when `server_open_rx_opt` is `None`

**File:** `crates/nosh-server/src/server.rs:795–813`

**Issue:** In the production build (`cfg(not(any(test, feature = "test-support")))`), both `server_open_tx` and `server_open_rx_inner` are immediately dropped via `let _ = (next_server_channel_id, server_open_tx, server_open_rx_inner)`, and `server_open_rx_opt` is set to `None`. This is correct from a security standpoint: the server-open path cannot be triggered in production.

However, there is a latent but provable correctness issue in the test path. In a test build, `server_open_tx` is stored in the slot (`slot.store_server_open_tx(server_open_tx)`) and `server_open_rx_opt` is `Some(server_open_rx_inner)`. The `recv_or_pending` arm in the select loop fires when the integration test sends a `ChannelType` via `server_open_tx`. However, if the test drops `server_open_tx` without sending, the `recv_or_pending` arm resolves with `None` (channel closed). The session pump at line 1296 treats `None` as "sender was dropped; treat as session-pump signal to stop" — but then falls through the `if let Some(ch_type) = server_open_req` check without breaking or logging. The `None` path is silently swallowed, which is correct, but the comment "Treat as session-pump signal to stop" (line 1296) is misleading: the loop continues, it does NOT stop. This is a comment/documentation correctness issue, not a functional bug, but it creates a false expectation for future maintainers.

More concretely: if the test drops `server_open_tx` early (before the session loop runs), the `recv_or_pending` arm wakes immediately with `None` on every select! iteration (a closed receiver always returns `None` immediately). This creates a **busy-loop** where the session pump's select! is dominated by the always-ready `None` arm, starving the other arms (PTY output, incoming_stream, msg). This is a DoS-in-tests that could manifest as flaky test timeouts when `server_open_tx` is dropped before the session ends.

Fix: set `server_open_rx_opt` to `None` when the receiver delivers `None` (indicating the sender was dropped), so `recv_or_pending` returns `pending()` from that point onward:

```rust
server_open_req = recv_or_pending(&mut server_open_rx_opt) => {
    match server_open_req {
        Some(ch_type) => { /* ... existing logic ... */ }
        None => {
            // Sender dropped; disable this arm permanently to avoid busy-loop.
            server_open_rx_opt = None;
        }
    }
}
```

### WR-03: `await_channel_accept` blocks the control stream reader during an interleaved `PtyData` storm

**File:** `crates/nosh-client/src/client.rs:705–727`

**Issue:** `await_channel_accept` calls `read_message` once and expects the reply to be either `ChannelAccept` or `ChannelReject`. If the server sends one or more `PtyData` frames before the `ChannelAccept` (which is normal — the PTY and the control pump run concurrently), the function returns `bail!("unexpected reply to ChannelOpen: PtyData")`. This makes `open_channel` unreliable in any real session context where PTY output is flowing.

The `recv_channel_reply` helper in the integration tests (`channel_mux.rs:74–95`) already works around this by looping to skip `PtyData`, `SessionClose`, `TerminalControl`, `Ack`, and `SessionOpened` frames. The production `await_channel_accept` function does not do this, meaning `open_channel` would spuriously fail in a live session with an active shell.

This is a WARNING rather than a BLOCKER because the only current callers are within the integration tests, which call `send_channel_open`/`recv_channel_reply` directly (bypassing `open_channel`/`await_channel_accept`). However, `open_channel` is a public API (`pub async fn`) and any future caller relying on it in a live session will hit this bug.

Fix: `await_channel_accept` should loop past non-channel-control frames, mirroring `recv_channel_reply`:

```rust
pub async fn await_channel_accept(
    control_recv: &mut quinn::RecvStream,
    expected_id: u32,
) -> anyhow::Result<ChannelAcceptOutcome> {
    loop {
        match nosh_proto::read_message(control_recv).await {
            Ok(Message::ChannelAccept { channel_id }) if channel_id == expected_id => {
                return Ok(ChannelAcceptOutcome::Accepted);
            }
            Ok(Message::ChannelAccept { channel_id }) => {
                anyhow::bail!("ChannelAccept for unexpected id {channel_id} (expected {expected_id})");
            }
            Ok(Message::ChannelReject { .. }) => return Ok(ChannelAcceptOutcome::Rejected),
            // Pass-through frames: PtyData, SessionOpened, TerminalControl, Ack
            Ok(Message::PtyData { .. })
            | Ok(Message::SessionOpened { .. })
            | Ok(Message::TerminalControl(_))
            | Ok(Message::Ack { .. }) => { continue; }
            Ok(other) => anyhow::bail!(
                "unexpected reply to ChannelOpen: {}", other.variant_name()
            ),
            Err(e) => anyhow::bail!("failed to read ChannelAccept/ChannelReject: {e}"),
        }
    }
}
```

---

## Info

### IN-01: `read_varint_u32` test does not exercise the actual `quinn::RecvStream` path

**File:** `crates/nosh-server/src/channel.rs:264–305`

**Issue:** The `read_varint_u32_roundtrip` test verifies the LEB128 encoding table manually but never calls `read_varint_u32` itself. The comment explains this honestly ("Since RecvStream is not constructable without a real QUIC connection, we test the byte-level logic by driving the equivalent cursor") but the consequence is that the actual code under test (`read_varint_u32`, which calls `recv.read_exact`) is never exercised by this unit test. Only the integration tests (via accept_bi) exercise it end-to-end.

For a function whose correctness is security-relevant (a malformed varint causes the stream to be dropped; an incorrect implementation could map the wrong channel-id, delivering data to the wrong channel task), a unit test that exercises the actual function path is valuable. The test as written has zero coverage of the real code path.

The test should either be replaced with an integration-style test that exercises `read_varint_u32` via a real pair, or be documented as a "wire encoding specification" test that complements (but does not replace) integration coverage.

### IN-02: `ChannelAccept`/`ChannelReject` for even ids treated as protocol errors in `run_reattach_session` even in test builds

**File:** `crates/nosh-server/src/server.rs:1845–1848`

**Issue:** In `run_reattach_session`, the `ChannelAccept`/`ChannelReject` arms collapse both even and odd ids into a single `ClientClosed` protocol-error branch:

```rust
Ok(Message::ChannelAccept { .. }) | Ok(Message::ChannelReject { .. }) => {
    tracing::warn!("client sent ChannelAccept/ChannelReject on reattach session; closing");
    break SessionEnd::ClientClosed;
}
```

In `run_session`, the equivalent block has separate `#[cfg(any(test, feature = "test-support"))]` guards that handle odd-id ChannelAccept/ChannelReject as valid replies to a server-initiated open (the SC#6 simultaneous-open test path). The reattach session does not have the `server_open_rx_opt` infrastructure and the SC#6 test does not exercise reattach, so this inconsistency has no functional impact today.

However, the code comment says "Server→client direction; receiving from the client is a protocol error" — which is only true for even ids (client-initiated ACCEPT/REJECT from the client is always wrong). An odd-id ChannelAccept from the client on a reattached session would be a valid reply to a server-initiated open (if the server ever opens channels on reattach, which it does not yet). The current blanket `ClientClosed` is over-aggressive: if the server-open path is ever added to `run_reattach_session`, this arm will silently kill the session when the client replies.

A low-risk clarification: add a comment explicitly noting that the reattach path has no server-open infrastructure and the blanket close is therefore currently correct. If `run_reattach_session` gains a server-open arm in a later phase, this arm must be updated to match `run_session`.

---

_Reviewed: 2026-06-12_
_Reviewer: Claude (gsd-code-reviewer)_
_Depth: deep_
