---
phase: 21-channel-multiplexing-foundation
fixed_at: 2026-06-12T00:00:00Z
review_path: .planning/phases/21-channel-multiplexing-foundation/21-REVIEW.md
iteration: 1
findings_in_scope: 7
fixed: 7
skipped: 0
status: all_fixed
---

# Phase 21: Code Review Fix Report

**Fixed at:** 2026-06-12
**Source review:** `.planning/phases/21-channel-multiplexing-foundation/21-REVIEW.md`
**Iteration:** 1

**Summary:**
- Findings in scope: 7
- Fixed: 7
- Skipped: 0

## Fixed Issues

### CR-01: Echo loop silently discards bytes when `remaining_credit < n`

**Files modified:** `crates/nosh-server/src/channel.rs`
**Commit:** 91fc056
**Applied fix:** Moved the read-cap calculation (`read_cap = remaining_credit.min(buf.len() as u64) as usize`) before the `tokio::select!` and changed the `ch_recv.read` call to `ch_recv.read(&mut buf[..read_cap])`. With the cap in place, `n <= remaining_credit` is always guaranteed, so the `min` guard and the `to_send` intermediate were replaced with a direct `remaining_credit -= n as u64` decrement. The existing `remaining_credit == 0` guard already blocks on a `Credit` event rather than issuing `read(&mut buf[..0])`, so no additional guard was needed. Also confirmed that `channel_flow_control_backpressure` (the test that straddles the credit boundary) passes cleanly after the fix.

Note: this fix is logic-level. The `channel_flow_control_backpressure` integration test exercises the credit boundary end-to-end and passed without modification, confirming the accounting invariant holds.

### CR-02: Dropped stream without reset leaks the peer's `read_exact` until idle timeout

**Files modified:** `crates/nosh-server/src/server.rs`
**Commit:** e0256d1
**Applied fix:** In both the `run_session` accept_bi arm (~line 1209) and the `run_reattach_session` accept_bi arm (~line 1868), the unknown-channel-id else branch now calls `ch_send.reset(0u32.into())` and `ch_recv.stop(0u32.into()).ok()` before the streams are dropped, giving the peer a clean signal rather than letting it hang until the QUIC idle timeout.

### WR-01: `try_send` for `ChannelEvent::Stream` silently drops the stream if the task is busy

**Files modified:** `crates/nosh-server/src/server.rs`
**Commit:** e0256d1
**Applied fix:** Changed both accept_bi arms (run_session and run_reattach_session) from `task_tx.try_send(ChannelEvent::Stream(...))` to `task_tx.send(ChannelEvent::Stream(...)).await`, with a logged-but-non-fatal check on the returned `is_err()` for the case where the task exited before the stream arrived. Credit and Close events remain `try_send` (recoverable).

### WR-02: `recv_or_pending` busy-loops when `server_open_tx` is dropped early

**Files modified:** `crates/nosh-server/src/server.rs`
**Commit:** e0256d1
**Applied fix:** Replaced the `if let Some(ch_type) = server_open_req` guard with an explicit `match` that handles the `None` arm by setting `server_open_rx_opt = None`. This causes `recv_or_pending` to return `std::future::pending()` on all subsequent iterations, preventing the busy-loop that would starve the other select! arms. The misleading "treat as session-pump signal to stop" comment was replaced with an accurate explanation that the loop continues and only this arm is silenced.

### WR-03: `await_channel_accept` blocks the control stream reader during an interleaved `PtyData` storm

**Files modified:** `crates/nosh-client/src/client.rs`
**Commit:** 35865fc
**Applied fix:** Wrapped the single `match nosh_proto::read_message(...)` call in an outer `loop`. Pass-through frames (`PtyData`, `SessionOpened`, `TerminalControl`, `Ack`) now `continue` the loop; `ChannelAccept` (matching id), `ChannelReject`, unexpected frames, and errors all return immediately. This mirrors the `recv_channel_reply` helper already used in the integration tests.

### IN-01: `read_varint_u32_roundtrip` is a wire-spec test with no coverage of the actual code path

**Files modified:** `crates/nosh-server/src/channel.rs`
**Commit:** 91fc056
**Applied fix:** Expanded the doc-comment on `read_varint_u32_roundtrip` to explicitly state that it is a wire-encoding specification test complementing (but not replacing) the integration coverage in `channel_mux.rs`, and to explain why a standalone unit test of the actual `read_varint_u32` function is not practical (`RecvStream` is not constructable without a live QUIC pair).

### IN-02: `ChannelAccept`/`ChannelReject` blanket close in `run_reattach_session` lacks maintenance note

**Files modified:** `crates/nosh-server/src/server.rs`
**Commit:** e0256d1
**Applied fix:** Added a comment to the `ChannelAccept`/`ChannelReject` arm in `run_reattach_session` explaining that the blanket `ClientClosed` break is correct today (reattach has no server-open infrastructure, so both even and odd ids are protocol errors), and noting that this arm must be updated to mirror `run_session`'s cfg-gated logic if `run_reattach_session` ever gains a server-open path.

---

## Verification results

All builds and tests passed with zero failures:

- `cargo build --workspace` — clean
- `cargo build --release -p nosh-server` — clean (test-only echo seam absent in release)
- `cargo test --workspace` — all test suites passed (21 suites, 0 failures)
- `cargo test -p nosh-client --test channel_mux` — all 6 integration tests passed, no hangs

Coverage note for CR-01: the `channel_flow_control_backpressure` test writes data in 256 KiB chunks against a 256 KiB window, which straddles the credit boundary as the reviewer identified. That test passed after the read-cap fix. No additional assertion was needed.

---

_Fixed: 2026-06-12_
_Fixer: Claude (gsd-code-fixer)_
_Iteration: 1_
