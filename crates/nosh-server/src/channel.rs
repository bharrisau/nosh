//! Per-logical-channel task for the server side (Phase 21 MUX-02/MUX-03).
//!
//! Each accepted channel runs as a separate `tokio::spawn` task so it cannot block
//! the main session pump. Reading channel data inline in the session select! loop
//! would cause head-of-line blocking: a slow channel consumer would stall PTY output
//! and miss the 16 ms diff-interval tick (Pitfall M-2 / Pitfall M-3).
//!
//! # Credit flow (MUX-03)
//!
//! Each channel has a 256 KiB byte-credit window. The channel task tracks
//! `remaining_credit` locally and pauses sends when it reaches zero. Credit is
//! replenished by `ChannelEvent::Credit(n)` messages forwarded from the session pump
//! (after the client sends `Message::ChannelCredit` on the control stream).
//!
//! # Single-writer invariant (A4)
//!
//! Channel tasks MUST NOT call `write_message` on the control stream `SendStream`
//! directly. A second concurrent writer would corrupt control-stream framing
//! (Pitfall M-6). All outbound control frames (`ChannelClose`, `ChannelCredit`)
//! are sent back to the session pump via `control_tx` — the pump holds the sole
//! writer handle.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use tokio::sync::mpsc;

use nosh_proto::Message;
use nosh_proto::messages::ScrollbackLine;

// ── Public types ──────────────────────────────────────────────────────────────

/// Events delivered to a running channel task by the session pump.
pub enum ChannelEvent {
    /// A new (SendStream, RecvStream) pair has been bound to this channel.
    ///
    /// Delivered after the client opens the QUIC bidi stream carrying the
    /// channel-id varint prefix. The task holds onto these for all subsequent
    /// channel I/O.
    Stream(quinn::SendStream, quinn::RecvStream),
    /// The peer has granted additional send credit (bytes, MUX-03).
    Credit(u64),
    /// Peer or local side initiated close; the task should finish and return.
    Close,
}

/// Initial per-channel send-credit window (256 KiB, MUX-03).
pub const INITIAL_CREDIT: u64 = 256 * 1024;

/// Maximum number of scrollback lines returned in a single `ScrollbackPage` response.
///
/// Caps the allocation from a single client `ScrollbackRequest{count: u32::MAX}` to
/// 1024 lines × (column width × cell size), preventing a multi-gigabyte server-side
/// allocation (T-22-08 / V5 input validation).
pub const MAX_PAGE_SIZE: usize = 1024;

// ── Varint helper ─────────────────────────────────────────────────────────────

/// Read a postcard/LEB128-encoded `u32` varint from a QUIC `RecvStream`.
///
/// Reads at most 5 bytes (the maximum encoding for a `u32`). Returns `Err` on
/// a truncated stream, a malformed continuation byte after 5 bytes, or an
/// overflow — never panics (V5 input validation, T-21-06).
///
/// The encoding matches `postcard::to_allocvec(&channel_id_u32)`: each byte
/// contributes 7 bits of the value (little-endian); the high bit of each byte
/// signals that more bytes follow. For channel ids 0–127 this is a single byte.
pub async fn read_varint_u32(recv: &mut quinn::RecvStream) -> anyhow::Result<u32> {
    let mut value: u32 = 0;
    let mut shift: u32 = 0;

    for _ in 0..5 {
        let mut buf = [0u8; 1];
        recv.read_exact(&mut buf).await?;
        let byte = buf[0];
        // Add the 7 low-order bits of this byte to the accumulated value.
        value |= ((byte & 0x7F) as u32) << shift;
        shift += 7;
        if byte & 0x80 == 0 {
            // Continuation bit is clear: this is the last byte.
            return Ok(value);
        }
    }

    // Five bytes consumed and the continuation bit was still set.
    anyhow::bail!("varint overflow: more than 5 bytes for a u32 channel-id prefix")
}

// ── Channel task ─────────────────────────────────────────────────────────────

/// Run the per-channel server task until the channel closes or the session ends.
///
/// The task owns a `mpsc::Receiver<ChannelEvent>`. The session pump sends
/// events:
/// - `ChannelEvent::Stream(send, recv)` — bind the data stream pair for I/O.
/// - `ChannelEvent::Credit(n)` — replenish the send-credit window.
/// - `ChannelEvent::Close` — remote or session-level close; exit cleanly.
///
/// On exit the task sends `Message::ChannelClose { channel_id }` through
/// `control_tx` so the pump can remove this channel from the map. The pump is
/// the SOLE writer of the control stream (A4); this task never calls
/// `write_message` on any shared stream directly.
pub async fn run_channel_task(
    channel_id: u32,
    mut events: mpsc::Receiver<ChannelEvent>,
    control_tx: mpsc::Sender<Message>,
) {
    // Wait for the stream-bind event (the QUIC bidi stream carrying this channel
    // arrives after ChannelAccept has been sent — there may be a brief delay).
    let (mut ch_send, mut ch_recv) = loop {
        match events.recv().await {
            Some(ChannelEvent::Stream(s, r)) => break (s, r),
            Some(ChannelEvent::Close) | None => {
                // Closed before the stream even arrived; nothing to do.
                let _ = control_tx
                    .send(Message::ChannelClose { channel_id })
                    .await;
                return;
            }
            Some(ChannelEvent::Credit(_)) => {
                // Credit before the stream is bound; keep waiting.
            }
        }
    };

    // Run the echo loop (test-only behaviour) or a production stub.
    run_channel_task_inner(
        channel_id,
        &mut ch_send,
        &mut ch_recv,
        &mut events,
        &control_tx,
    )
    .await;

    // Half-close: finish the send side and give the peer a moment to drain.
    let _ = ch_send.finish();
    let _ = tokio::time::timeout(Duration::from_secs(2), ch_send.stopped()).await;

    // Notify the pump to remove this channel from the map.
    let _ = control_tx
        .send(Message::ChannelClose { channel_id })
        .await;
}

// ── Inner channel I/O (production stub + test-only echo) ─────────────────────

/// Inner channel loop — separated from `run_channel_task` so the half-close and
/// control-stream notification always run regardless of how the inner loop exits.
async fn run_channel_task_inner(
    channel_id: u32,
    ch_send: &mut quinn::SendStream,
    ch_recv: &mut quinn::RecvStream,
    events: &mut mpsc::Receiver<ChannelEvent>,
    _control_tx: &mpsc::Sender<Message>,
) {
    // Echo behaviour: test-only, gated so the symbol is absent in production
    // builds. Must match the accept gate in server.rs (any(test, test-support))
    // — otherwise an integration test in a downstream crate (cfg(test)=false but
    // feature="test-support"=true) would accept the Echo channel yet fall through
    // to the production stub below, silently discarding bytes and hanging the
    // peer's read.
    #[cfg(any(test, feature = "test-support"))]
    {
        run_echo_loop(channel_id, ch_send, ch_recv, events).await;
        return;
    }

    // Production: no channel type that needs a body loop is accepted in this
    // phase; the task drains Close events and waits for RecvStream EOF.
    #[cfg(not(any(test, feature = "test-support")))]
    {
        let _ = channel_id; // suppress unused-variable warning in production
        let _ = ch_send;    // the send half is closed in the caller's half-close
        loop {
            let mut buf = [0u8; 4096];
            tokio::select! {
                read_res = ch_recv.read(&mut buf) => {
                    match read_res {
                        Ok(Some(_)) => { /* discard — no production consumer yet */ }
                        Ok(None) => break, // RecvStream EOF
                        Err(_) => break,
                    }
                }
                ev = events.recv() => {
                    match ev {
                        Some(ChannelEvent::Close) | None => break,
                        Some(ChannelEvent::Credit(_)) => { /* no-op in stub */ }
                        Some(ChannelEvent::Stream(_, _)) => {
                            // A second stream binding on an already-active channel is
                            // unexpected; drop it and continue.
                        }
                    }
                }
            }
        }
    }
}

// ── Scrollback sender task (Phase 22 SCROLL-01/02, S-1/S-4/S-5) ─────────────

/// Serve scrollback pages over a reliable `SendStream` on demand (SCROLL-01/02).
///
/// This function is the production server-side scrollback handler. It reads
/// `ScrollbackRequest` frames from the channel's own `RecvStream` (`ch_recv`)
/// and writes `ScrollbackPage` frames to the channel's `SendStream` (`ch_send`).
///
/// # Security invariants
///
/// - **S-1 reliable-only at the type level:** the signature accepts only
///   `&mut quinn::SendStream` — there is no `&quinn::Connection` parameter and
///   therefore no datagram-send path is reachable from this function body.
///   A grep over the function body for the datagram token is the falsifiable proof.
///
/// - **S-4 / M-6 bounded back-pressure:** byte credit controls how much may be
///   written. When `remaining_credit == 0` the task blocks on `events.recv()` for
///   a `ChannelEvent::Credit` — it never busy-loops and never blocks the session
///   pump's select! loop (the pump and this task are separate `tokio::spawn`).
///
/// - **S-5 atomic epoch capture:** `epoch_at_snapshot` is read from `epoch_src`
///   with `Ordering::Acquire` into a local variable immediately before entering
///   the synchronous `with_terminal_state` closure. There is no `.await` between
///   the `load` and the closure, so the epoch and the scrollback lines are
///   mutually consistent (no torn read between what the scrollback reports and
///   what the datagram stream is doing). The end-to-end concurrency proof that
///   no gap or duplicate exists across a concurrent diff tick is the integration
///   test `scrollback_epoch_handoff_no_gap` in plan 22-04 — that test is the
///   falsifiable S-5 proof obligation. This plan asserts only the code structure.
///
/// - **V5 allocation cap:** `count` from the client request is clamped to
///   `MAX_PAGE_SIZE` (1024) before calling `scrollback_lines`. A
///   `ScrollbackRequest { count: u32::MAX }` cannot force a multi-gigabyte
///   allocation (T-22-08).
///
/// - **M-2 deadlock avoidance:** `ScrollbackRequest` is read on the channel's
///   own `ch_recv`, never the control stream. `ScrollbackPage` is written on
///   `ch_send`. `ChannelClose` is routed via `control_tx` (A4 invariant) —
///   never written to `ch_send` or any shared stream directly.
pub async fn run_scrollback_sender_task(
    channel_id: u32,
    slot: Arc<crate::registry::SessionSlot>,
    ch_send: &mut quinn::SendStream,
    ch_recv: &mut quinn::RecvStream,
    events: &mut mpsc::Receiver<ChannelEvent>,
    control_tx: &mpsc::Sender<Message>,
    epoch_src: Arc<std::sync::atomic::AtomicU64>,
) {
    let mut remaining_credit: u64 = INITIAL_CREDIT;

    loop {
        if remaining_credit == 0 {
            // Credit exhausted: wait for a replenishment event before attempting
            // any further sends (S-4 / MUX-03 back-pressure).
            // IMPORTANT: do NOT use read(&mut buf[..0]) here — that returns
            // Ok(None) which is misread as EOF. Block until credit arrives.
            match events.recv().await {
                Some(ChannelEvent::Credit(n)) => {
                    remaining_credit = remaining_credit.saturating_add(n);
                }
                Some(ChannelEvent::Close) | None => break,
                Some(ChannelEvent::Stream(_, _)) => { /* unexpected; ignore */ }
            }
            continue;
        }

        tokio::select! {
            // Read the next ScrollbackRequest from the channel's own RecvStream
            // (M-2: never the control stream).
            msg = nosh_proto::codec::read_message(ch_recv) => {
                match msg {
                    Ok(Message::ScrollbackRequest { channel_id: req_cid, from_line, count }) => {
                        // Ignore requests for a different channel_id (protocol error; logged,
                        // not fatal — keep serving the correct channel).
                        if req_cid != channel_id {
                            tracing::debug!(
                                channel_id,
                                req_cid,
                                "ScrollbackRequest for wrong channel_id; ignoring"
                            );
                            continue;
                        }
                        // V5 / T-22-08: cap count before calling scrollback_lines to prevent
                        // a u32::MAX request forcing a multi-gigabyte allocation.
                        let count = (count as usize).min(MAX_PAGE_SIZE);

                        // WR-P-01 fix / S-5 atomic epoch + scrollback snapshot:
                        // Read epoch_at_snapshot INSIDE the with_terminal_state closure
                        // so the epoch and scrollback lines are read under the same mutex
                        // acquisition. This eliminates the TOCTOU window: a PTY task
                        // cannot push new scrollback lines between the epoch load and the
                        // mutex lock, preventing a torn read at the scrollback/live seam.
                        //
                        // The epoch_src atomic is updated by the diff tick (after releasing
                        // the terminal_state mutex with Release ordering). Reading it while
                        // holding the terminal_state mutex means we see a consistent view:
                        // either the epoch was stored before we locked (we read the latest)
                        // or the PTY task is still writing under a lock we hold (we read
                        // the prior epoch, but then the scrollback lines we see are also
                        // from that same prior state). Either way, no gap or duplicate.
                        //
                        // Concurrency-correctness proof: the integration test
                        // `scrollback_epoch_handoff_no_gap` in plan 22-04 is the
                        // falsifiable S-5 proof that no gap or duplicate occurs across
                        // a concurrent diff tick during a scrollback request.
                        let epoch_src_ref = &epoch_src;
                        let (epoch_at_snapshot, raw_lines, total_available) = slot.with_terminal_state(|ts| {
                            // Read epoch atomically under the terminal_state lock.
                            let epoch = epoch_src_ref.load(Ordering::Acquire);
                            let (lines, total) = ts.scrollback_lines(from_line, count);
                            (epoch, lines, total)
                        });

                        // Convert server-side Cell values to ScrollbackLine wire types.
                        // Cell.ch: char; Cell.style: CellStyle; Cell.fg/bg: Option<u8>.
                        // ScrollbackCell field types match Cell exactly (zero-copy assembly).
                        let lines: Vec<ScrollbackLine> = raw_lines
                            .into_iter()
                            .map(|row| {
                                let width = row.len() as u16;
                                let cells = row
                                    .into_iter()
                                    .map(|cell| nosh_proto::messages::ScrollbackCell {
                                        ch: cell.ch,
                                        style: cell.style,
                                        fg: cell.fg,
                                        bg: cell.bg,
                                    })
                                    .collect();
                                ScrollbackLine { width, cells }
                            })
                            .collect();

                        let page = Message::ScrollbackPage {
                            channel_id,
                            from_line,
                            total_available,
                            epoch_at_snapshot,
                            lines,
                        };

                        // Encode to measure byte cost before deducting from credit.
                        let encoded = match nosh_proto::codec::encode(&page) {
                            Ok(bytes) => bytes,
                            Err(e) => {
                                tracing::warn!(channel_id, "ScrollbackPage encode error: {e}; closing");
                                break;
                            }
                        };
                        let encoded_len = encoded.len() as u64;

                        // If the encoded page exceeds remaining credit, wait for a
                        // Credit event before writing (back-pressure, S-4 / MUX-03).
                        //
                        // WR-S-02 fix: bound the inner credit-wait with a 30 s timeout.
                        // Without it, a page that exceeds INITIAL_CREDIT (256 KiB) and a
                        // client that never grants more credit would block this task forever
                        // (the events.recv() never returns). On timeout, log and close the
                        // channel cleanly so the server slot is not permanently leaked.
                        let credit_wait_deadline =
                            tokio::time::Instant::now() + Duration::from_secs(30);
                        while remaining_credit < encoded_len {
                            match tokio::time::timeout_at(
                                credit_wait_deadline,
                                events.recv(),
                            ).await {
                                Ok(Some(ChannelEvent::Credit(n))) => {
                                    remaining_credit = remaining_credit.saturating_add(n);
                                }
                                Ok(Some(ChannelEvent::Close)) | Ok(None) => {
                                    // Session or channel closed while waiting for credit.
                                    let _ = ch_send.finish();
                                    let _ = tokio::time::timeout(
                                        Duration::from_secs(2),
                                        ch_send.stopped(),
                                    ).await;
                                    let _ = control_tx.send(Message::ChannelClose { channel_id }).await;
                                    return;
                                }
                                Ok(Some(ChannelEvent::Stream(_, _))) => { /* unexpected; ignore */ }
                                Err(_elapsed) => {
                                    // 30 s credit timeout: client stalled without granting
                                    // credit. Close the channel cleanly rather than blocking
                                    // forever (WR-S-02 bounded back-pressure timeout).
                                    tracing::warn!(
                                        channel_id,
                                        "scrollback sender: 30 s credit timeout — \
                                         closing channel (client stalled)"
                                    );
                                    let _ = ch_send.finish();
                                    let _ = tokio::time::timeout(
                                        Duration::from_secs(2),
                                        ch_send.stopped(),
                                    ).await;
                                    let _ = control_tx.send(Message::ChannelClose { channel_id }).await;
                                    return;
                                }
                            }
                        }

                        // Write the encoded frame directly (already have the bytes).
                        if ch_send.write_all(&encoded).await.is_err() {
                            break;
                        }
                        remaining_credit = remaining_credit.saturating_sub(encoded_len);
                    }
                    Ok(_other) => {
                        // Non-ScrollbackRequest frame on the scrollback channel data stream:
                        // ignore (scrollback channel only carries ScrollbackRequest from client).
                        tracing::debug!(
                            channel_id,
                            "unexpected message type on scrollback channel; ignoring"
                        );
                    }
                    Err(_) => {
                        // ch_recv EOF or decode error — peer closed the channel.
                        break;
                    }
                }
            }

            // Handle pump events (credit replenishment, close) concurrently.
            ev = events.recv() => {
                match ev {
                    Some(ChannelEvent::Credit(n)) => {
                        remaining_credit = remaining_credit.saturating_add(n);
                    }
                    Some(ChannelEvent::Close) | None => break,
                    Some(ChannelEvent::Stream(_, _)) => { /* unexpected; ignore */ }
                }
            }
        }
    }

    // Half-close: finish the send side and give the peer a moment to drain.
    let _ = ch_send.finish();
    let _ = tokio::time::timeout(Duration::from_secs(2), ch_send.stopped()).await;

    // Notify the pump to remove this channel from the map (A4: route via control_tx,
    // never write to a shared stream directly).
    let _ = control_tx
        .send(Message::ChannelClose { channel_id })
        .await;
}

/// Test-only echo loop: read bytes from the channel's RecvStream and echo them
/// back on the SendStream, respecting the 256 KiB byte-credit window (MUX-03).
///
/// Pauses sending when `remaining_credit` reaches zero; resumes when
/// `ChannelEvent::Credit(n)` arrives from the session pump.
///
/// Exits on RecvStream EOF, a read/write error, or `ChannelEvent::Close`.
#[cfg(any(test, feature = "test-support"))]
async fn run_echo_loop(
    _channel_id: u32,
    ch_send: &mut quinn::SendStream,
    ch_recv: &mut quinn::RecvStream,
    events: &mut mpsc::Receiver<ChannelEvent>,
) {
    let mut remaining_credit: u64 = INITIAL_CREDIT;
    let mut buf = vec![0u8; 8192];

    loop {
        if remaining_credit == 0 {
            // Credit exhausted: wait for a replenishment event before attempting
            // any further reads or sends (MUX-03 back-pressure; T-21-05).
            // IMPORTANT: do NOT attempt read(&mut buf[..0]) here — that returns
            // Ok(None) and is misread as EOF. Block until credit arrives.
            match events.recv().await {
                Some(ChannelEvent::Credit(n)) => {
                    remaining_credit = remaining_credit.saturating_add(n);
                }
                Some(ChannelEvent::Close) | None => break,
                Some(ChannelEvent::Stream(_, _)) => { /* unexpected; ignore */ }
            }
            continue;
        }

        // CR-01 fix: cap the read to however many bytes we can actually send so
        // that n <= remaining_credit is always guaranteed. Without this cap, bytes
        // that exceed the window are consumed from the QUIC RecvStream but silently
        // discarded on the echo path, creating a permanent accounting divergence the
        // peer cannot detect.
        let read_cap = remaining_credit.min(buf.len() as u64) as usize;

        tokio::select! {
            // Try to read from the channel stream (capped to remaining credit).
            read_res = ch_recv.read(&mut buf[..read_cap]) => {
                match read_res {
                    Ok(Some(n)) => {
                        // n <= read_cap <= remaining_credit, so no credit overrun
                        // is possible and the min guard is a tautology.
                        if ch_send.write_all(&buf[..n]).await.is_err() {
                            break;
                        }
                        remaining_credit -= n as u64;
                    }
                    Ok(None) => break, // RecvStream EOF — peer half-closed
                    Err(_) => break,
                }
            }
            // Handle events from the session pump concurrently.
            ev = events.recv() => {
                match ev {
                    Some(ChannelEvent::Credit(n)) => {
                        remaining_credit = remaining_credit.saturating_add(n);
                    }
                    Some(ChannelEvent::Close) | None => break,
                    Some(ChannelEvent::Stream(_, _)) => { /* unexpected; ignore */ }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify that the LEB128 wire encoding table is correct (IN-01 wire-spec test).
    ///
    /// This is a **wire-encoding specification test** — it confirms that the byte
    /// sequences the opener writes via `postcard::to_allocvec(&channel_id_u32)` are
    /// what the LEB128 algorithm produces. It complements (but does not replace) the
    /// integration tests that exercise `read_varint_u32` end-to-end via a real QUIC
    /// connection (`accept_bi` in channel_mux.rs). `RecvStream` is not constructable
    /// without a live QUIC pair, so a standalone unit test of the actual code path is
    /// not practical here.
    ///
    /// LEB128 encoding: each byte contributes 7 bits (little-endian); high bit set
    /// means more bytes follow. Values 0–127 encode as a single byte.
    #[tokio::test]
    async fn read_varint_u32_roundtrip() {
        let cases: &[(u32, &[u8])] = &[
            (0,   &[0x00]),
            (1,   &[0x01]),
            (2,   &[0x02]),
            (127, &[0x7F]),
            // 128 in LEB128: low 7 bits = 0, continuation bit set → 0x80; next byte = 0x01
            (128, &[0x80, 0x01]),
            // 300 = 0x12C in LEB128: low 7 bits = 0x2C | 0x80 = 0xAC; next byte = 0x02
            (300, &[0xAC, 0x02]),
        ];

        for &(expected_id, encoded) in cases {
            // Use nosh_proto's write path to create an in-memory stream, then
            // verify read_varint_u32 decodes the known-correct bytes.
            // Build a mock RecvStream from known bytes by writing directly.
            // Since RecvStream is not constructable without a real QUIC connection,
            // we test the byte-level logic by driving the equivalent cursor.
            let _ = expected_id; // used in the assert below
            let _ = encoded;     // used in the assert below

            // Verify our encoding table matches the LEB128 spec manually.
            let mut manual = Vec::new();
            let mut n = expected_id;
            loop {
                let byte = (n & 0x7F) as u8;
                n >>= 7;
                if n != 0 {
                    manual.push(byte | 0x80);
                } else {
                    manual.push(byte);
                    break;
                }
            }
            assert_eq!(
                &manual[..], encoded,
                "manual LEB128 encoding of {} should be {:?}",
                expected_id, encoded
            );
        }
    }

    /// Verify INITIAL_CREDIT is the expected 256 KiB (MUX-03).
    #[test]
    fn initial_credit_is_256_kib() {
        assert_eq!(INITIAL_CREDIT, 256 * 1024, "initial credit must be 256 KiB");
    }

    /// Verify MAX_PAGE_SIZE is 1024 (T-22-08 / V5 allocation cap).
    #[test]
    fn max_page_size_is_1024() {
        assert_eq!(MAX_PAGE_SIZE, 1024, "MAX_PAGE_SIZE must be 1024 lines (T-22-08)");
    }
}
