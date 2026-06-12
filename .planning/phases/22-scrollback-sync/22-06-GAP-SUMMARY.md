# Phase 22 Gap-Closure 06: ScrollbackCredit End-to-End Routing

## Gap

The integration checker identified a blocking end-to-end wiring bug: scrollback
credit replenishment was silently discarded by the server, causing deep history
paging (SCROLL-01, SCROLL-02) and the scrollback channel (MUX-03) to stall and
close after the initial 256 KiB credit window was exhausted.

## Root Cause

The client drain task emits `Message::ScrollbackCredit { channel_id, bytes }` on
the control stream (client→server direction) to replenish the server sender's
credit window. The server's control-stream handler in both `run_session` and
`run_reattach_session` grouped `ScrollbackCredit` with `ScrollbackRequest` and
`ScrollbackPage` in a single log-and-ignore arm labelled "scrollback frames on
control stream = protocol error".

This was wrong. `ScrollbackCredit` is a client→server frame that legitimately
travels on the control stream — the same direction as `ChannelCredit`, which had
a correctly wired arm. `ScrollbackRequest` must travel on the channel's own
`RecvStream` (M-2 deadlock avoidance); `ScrollbackPage` is a server→client frame.
Only the latter two are genuine protocol errors on the control stream.

The consequence: `run_scrollback_sender_task` starts with
`INITIAL_CREDIT = 256 KiB`, deducts the encoded size of each `ScrollbackPage`, and
waits in the credit-wait loop (`events.recv()`) when the window is exhausted. With
credits silently dropped, the window could never be replenished. Once cumulative
`ScrollbackPage` wire bytes exceeded 256 KiB the sender blocked in the credit-wait
loop, hit the 30 s WR-S-02 bounded timeout, and closed the channel. The user
stopped receiving history.

## Fix

**File:** `crates/nosh-server/src/server.rs`

Split `ScrollbackCredit` out of the ignore group in **both** control-stream
handlers and wire it exactly like the existing `ChannelCredit` arm:

```rust
Ok(Message::ScrollbackCredit { channel_id, bytes }) => {
    if let Some(task_tx) = channel_map.get(&channel_id) {
        let _ = task_tx.try_send(ChannelEvent::Credit(bytes));
    } else {
        tracing::debug!(channel_id, "ScrollbackCredit for unknown channel; ignoring");
    }
}
```

Applied in:
- `run_session` control handler (~line 1254 before fix, now ~line 1254–1264)
- `run_reattach_session` control handler (~line 2023 before fix, now ~line 2023–2033)

The misleading comment claiming `ScrollbackCredit` is "server→client" was corrected
in both locations.

`ScrollbackRequest` and `ScrollbackPage` remain in the log-and-ignore arm (those
genuinely must not appear on the control stream).

## Regression Test

**File:** `crates/nosh-client/tests/channel_mux.rs`
**Test name:** `scrollback_deep_paging_replenishes_credit`

Uses the existing `produce_scrollback` helper (40 lines) to establish scrollback
history, then enters a tight loop:

1. Send `ScrollbackRequest { from_line: 0, count: 40 }` on the channel stream.
2. Read back the `ScrollbackPage`; measure its encoded wire size (same calculation
   the server uses to deduct from `remaining_credit`).
3. Send `ScrollbackCredit { bytes: encoded_len }` on the **control stream**.
4. Accumulate `cumulative_bytes` until it reaches `TARGET_BYTES = 512 KiB`
   (2 × `INITIAL_CREDIT`).

A 15 s per-page timeout ensures the test fails fast if the server blocks in the
credit-wait loop (the WR-S-02 timeout is 30 s; if credit never arrives the sender
stalls, and the 15 s test timeout fires first).

**Failure mode without the fix:** The first ~256 KiB of pages succeed (INITIAL_CREDIT
covers them). Once cumulative deductions exhaust the window, the sender blocks in
`events.recv()`. The 15 s page-read timeout fires with a clear message identifying
the bug.

**Success with the fix:** Each `ScrollbackCredit` reaches `ChannelEvent::Credit`,
replenishes `remaining_credit`, and pages keep flowing past 512 KiB.

## Files Changed

| File | Lines changed | Nature |
|------|--------------|--------|
| `crates/nosh-server/src/server.rs` | ~1246–1265 (run_session), ~2017–2036 (run_reattach_session) | Bug fix: split ScrollbackCredit arm, correct comment |
| `crates/nosh-client/tests/channel_mux.rs` | +130 lines at end of file | New regression test |

## Commit

`7eb0342` — fix(scrollback): route ScrollbackCredit to ChannelEvent::Credit in
run_session and run_reattach_session

## Verification

`cargo build --workspace` — clean.
`cargo test --workspace` — all tests pass (14/14 channel_mux tests green).
