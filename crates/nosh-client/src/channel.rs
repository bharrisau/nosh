//! Per-logical-channel task for the client side (Phase 21 MUX-02/MUX-03).
//!
//! Each client channel runs as an independent `tokio::spawn` task so it cannot
//! block the main session pump. Reading channel data inline in the session loop
//! would cause head-of-line blocking and stall PTY output (Pitfall M-2/M-3).
//!
//! # Even-id allocator (MUX-04)
//!
//! Client-initiated channels use EVEN channel ids (2, 4, 6, …). Channel id 0
//! is reserved for the control stream. The parity rule is locked in CONTEXT.md /
//! ROADMAP SC#2 — server-initiated channels use ODD ids (1, 3, 5, …).
//!
//! # Credit flow (MUX-03)
//!
//! Each channel has a 256 KiB byte-credit window. The client task tracks how
//! many bytes it has drained from its receive buffer and advertises credit back
//! to the server via `Message::ChannelCredit` sent through the session pump's
//! `control_tx` mpsc. The pump holds the sole writer to the control stream (A4).
//!
//! # Single-writer invariant (A4)
//!
//! Channel tasks MUST NOT call `write_message` on the control stream `SendStream`
//! directly. All outbound control frames (`ChannelCredit`, `ChannelClose`) are
//! queued to the session pump via `control_tx`. A second concurrent writer would
//! corrupt control-stream framing (Pitfall M-6).

use std::time::Duration;

use tokio::sync::mpsc;

use nosh_proto::Message;
use nosh_proto::transport_trait::{NoshSendStream, NoshRecvStream};

/// Initial per-channel credit window advertised to the server (256 KiB, MUX-03).
pub const INITIAL_CREDIT: u64 = 256 * 1024;

/// Chunk size at which the drain task replenishes credit.
///
/// After draining this many bytes the task sends a `ChannelCredit` frame so the
/// server is not left waiting too long before it can send more. Using half the
/// window means at most two round trips are needed to fully drain a 256 KiB burst.
const CREDIT_REPLENISH_CHUNK: u64 = 128 * 1024;

// ── Even-id allocator ────────────────────────────────────────────────────────

/// Allocates even channel ids for client-initiated channels (MUX-04).
///
/// Channel id 0 is reserved for the control stream. Client-initiated channels
/// start at 2 and increment by 2. The allocator is NOT shared across tasks —
/// the session pump holds a single instance and allocates sequentially before
/// spawning or handing off channels.
pub struct EvenIdAllocator {
    next: u32,
}

impl EvenIdAllocator {
    /// Create a new allocator; the first id returned is 2.
    pub fn new() -> Self {
        Self { next: 2 }
    }

    /// Allocate the next even channel id. Wraps past `u32::MAX - 1` back to 2
    /// (channels at id 0 and 1 are always reserved; in practice connections end
    /// well before 2^31 channels are needed).
    pub fn next_id(&mut self) -> u32 {
        let id = self.next;
        self.next = if self.next >= u32::MAX - 1 { 2 } else { self.next + 2 };
        id
    }
}

impl Default for EvenIdAllocator {
    fn default() -> Self {
        Self::new()
    }
}

// ── Channel drain task ───────────────────────────────────────────────────────

/// Run the per-channel client drain task until the channel closes.
///
/// Reads bytes from `ch_recv` (server → client channel data), grants
/// `ChannelCredit` back to the server via `control_tx` as the buffer is drained,
/// and exits on RecvStream EOF or a read error.
///
/// On exit:
/// - Calls `ch_send.finish()` + bounded `ch_send.stopped()` to half-close the
///   send side (notifying the server that the client is done sending on this
///   channel).
/// - Sends `Message::ChannelClose { channel_id }` through `control_tx` so the
///   session pump can remove this channel from its map and notify the server.
///
/// The task is spawned by the caller with `tokio::spawn`; it holds both stream
/// halves and the `control_tx` handle exclusively — no shared state, no Mutex.
///
/// Unknown id / already-closed conditions: the task logs and returns; it never
/// panics (T-21-10 / MUX-04 no-panic rule).
pub async fn run_channel_task(
    channel_id: u32,
    mut ch_recv: quinn::RecvStream,
    mut ch_send: quinn::SendStream,
    control_tx: mpsc::Sender<Message>,
) {
    let mut buf = vec![0u8; 8192];
    let mut drained_since_replenish: u64 = 0;

    loop {
        match ch_recv.read(&mut buf).await {
            Ok(Some(n)) => {
                // Bytes drained from the receive buffer.
                drained_since_replenish += n as u64;

                // Replenish credit in chunks so the server is not left waiting.
                // Sends happen via the pump's mpsc, NEVER directly on the stream.
                if drained_since_replenish >= CREDIT_REPLENISH_CHUNK {
                    let bytes = drained_since_replenish;
                    drained_since_replenish = 0;
                    if control_tx
                        .send(Message::ChannelCredit { channel_id, bytes })
                        .await
                        .is_err()
                    {
                        // Session pump has gone away; nothing more to do.
                        tracing::debug!(channel_id, "control_tx closed during credit grant; exiting channel task");
                        break;
                    }
                }
            }
            Ok(None) => {
                // RecvStream EOF — server half-closed its send side.
                break;
            }
            Err(e) => {
                tracing::debug!(channel_id, err = %e, "channel RecvStream error; closing channel task");
                break;
            }
        }
    }

    // Flush any remaining unsent credit so the server's window stays accurate.
    if drained_since_replenish > 0 {
        let bytes = drained_since_replenish;
        let _ = control_tx
            .send(Message::ChannelCredit { channel_id, bytes })
            .await;
    }

    // Half-close: finish the client send side and wait briefly for the server to
    // acknowledge (mirrors the server-side half-close pattern in MUX-03).
    let _ = ch_send.finish();
    let _ = tokio::time::timeout(Duration::from_secs(2), ch_send.stopped()).await;

    // Notify the session pump to remove this channel from the map and send
    // ChannelClose to the server. Never log channel_id payload — log variant name.
    if control_tx
        .send(Message::ChannelClose { channel_id })
        .await
        .is_err()
    {
        tracing::debug!(
            channel_id,
            "control_tx closed when sending ChannelClose; pump already gone"
        );
    }
}

// ── Scrollback channel drain task ────────────────────────────────────────────

/// Run the client-side scrollback channel drain task.
///
/// Concurrently:
/// - Writes `ScrollbackRequest` frames to `ch_send` (client → server) as they
///   arrive from `req_rx` (sent by `run_pump` when the user pages up).
/// - Reads `ScrollbackPage` frames from `ch_recv` (server → client) via
///   [`nosh_proto::codec::read_message`] and forwards each to `run_pump`
///   through `page_tx`.
///
/// Flow-control (SCROLL-02 / MUX-03):
/// - Bytes consumed are counted; once `>= CREDIT_REPLENISH_CHUNK` the task
///   sends `Message::ScrollbackCredit { channel_id, bytes }` through
///   `control_tx` (NOT a direct stream write — A4 single-writer invariant).
///
/// Drop-on-full (RESEARCH Open Question 3):
/// - `page_tx.try_send` is used. If `page_tx` is full or closed (because
///   `run_pump` has snapped back to live mode), the page is DROPPED and
///   draining continues so the server's flow-control window is not stalled.
///
/// On `RecvStream` EOF / error:
/// - Flushes any remaining unsent credit via a final `ScrollbackCredit`.
/// - Half-closes `ch_send.finish()` + bounded `stopped()` (2 s timeout).
/// - Sends `Message::ChannelClose { channel_id }` through `control_tx` so
///   the session pump can remove the channel from its map.
///
/// # Single-writer invariant (A4)
///
/// This task MUST NOT call `write_message` on the control stream directly.
/// All outbound control frames go through `control_tx`.
///
/// # `ScrollbackRequest` routing (Open Question 1)
///
/// `ScrollbackRequest` travels on the channel's own `ch_send` stream (not the
/// control stream) to avoid the M-2 control/data flow-control deadlock.
/// `ch_send` and `ch_recv` are independent halves of the same QUIC bidi
/// stream; writing to `ch_send` cannot stall reading from `ch_recv`.
pub async fn run_scrollback_drain_task(
    channel_id: u32,
    mut ch_recv: Box<dyn NoshRecvStream>,
    mut ch_send: Box<dyn NoshSendStream>,
    control_tx: mpsc::Sender<Message>,
    page_tx: mpsc::Sender<Message>,
    mut req_rx: mpsc::Receiver<Message>,
) {
    let mut drained_since_replenish: u64 = 0;

    loop {
        tokio::select! {
            // Path 1: run_pump wants to send a ScrollbackRequest to the server.
            // Requests travel on ch_send (channel's own data stream) to avoid the
            // M-2 control/data flow-control deadlock (Open Question 1).
            req = req_rx.recv() => {
                match req {
                    Some(msg) => {
                        // Write the request to ch_send (client → server direction).
                        if nosh_proto::write_message_ns(&mut *ch_send, &msg).await.is_err() {
                            tracing::debug!(channel_id, "scrollback ch_send write error; exiting drain task");
                            break;
                        }
                    }
                    None => {
                        // run_pump has dropped req_tx; session is ending.
                        tracing::debug!(channel_id, "scrollback req_rx closed; exiting drain task");
                        break;
                    }
                }
            }
            // Path 2: server sent a ScrollbackPage frame.
            msg_result = nosh_proto::read_message_ns(&mut *ch_recv) => {
                match msg_result {
                    Ok(msg) => {
                        // Track bytes consumed for flow-control credit.
                        // The frame on the wire is: 4-byte length prefix + body.
                        // We use the postcard-encoded body length (from the codec).
                        // For credit-tracking we count the full wire bytes.
                        let wire_bytes = match nosh_proto::codec::encode(&msg) {
                            Ok(frame) => frame.len() as u64,
                            Err(_) => {
                                // Should never fail for a successfully decoded message.
                                // WR-C-01 fix: use a conservative non-zero byte count on
                                // the unlikely re-encode failure path. Returning 0 would
                                // permanently undercount consumed bytes, starving the
                                // server's flow-control window and stalling future pages.
                                // MAX_FRAME_LEN + 4 (the 4-byte length prefix) is the
                                // largest possible wire frame — an overcount here is safe
                                // (grants more credit than needed) versus the 0 undercount.
                                tracing::debug!(
                                    channel_id,
                                    "scrollback drain: re-encode failed for credit accounting; \
                                     using MAX_FRAME_LEN + 4 as conservative fallback"
                                );
                                nosh_proto::codec::MAX_FRAME_LEN as u64 + 4
                            }
                        };
                        drained_since_replenish += wire_bytes;

                        // Forward ScrollbackPage to run_pump via page_tx.
                        // Use try_send: if page_tx is full or closed (run_pump snapped
                        // back to Live), drop the page — do NOT buffer unboundedly and
                        // do NOT stop draining (RESEARCH Open Question 3 / T-22-14).
                        if matches!(msg, Message::ScrollbackPage { .. }) {
                            match page_tx.try_send(msg) {
                                Ok(()) => {}
                                Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                                    tracing::debug!(
                                        channel_id,
                                        "scrollback page_tx full — dropping page (run_pump in Live mode)"
                                    );
                                }
                                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                                    // run_pump has exited; nothing more to do.
                                    tracing::debug!(channel_id, "page_tx closed; scrollback drain exiting");
                                    break;
                                }
                            }
                        }
                        // Non-ScrollbackPage frames on this channel are unexpected;
                        // they are still drained (counted for credit) and discarded.

                        // Replenish credit in chunks so the server is not left waiting.
                        if drained_since_replenish >= CREDIT_REPLENISH_CHUNK {
                            let bytes = drained_since_replenish;
                            drained_since_replenish = 0;
                            if control_tx
                                .send(Message::ScrollbackCredit { channel_id, bytes })
                                .await
                                .is_err()
                            {
                                // Session pump has gone away; nothing more to do.
                                tracing::debug!(
                                    channel_id,
                                    "control_tx closed during scrollback credit grant; exiting"
                                );
                                break;
                            }
                        }
                    }
                    Err(e) => {
                        // EOF or framing error — exit the drain loop.
                        // EOF is the normal server half-close path.
                        tracing::debug!(
                            channel_id,
                            err = %e,
                            "scrollback channel RecvStream ended; closing drain task"
                        );
                        break;
                    }
                }
            }
        }
    }

    // Flush any remaining unsent credit so the server's window stays accurate.
    if drained_since_replenish > 0 {
        let bytes = drained_since_replenish;
        let _ = control_tx
            .send(Message::ScrollbackCredit { channel_id, bytes })
            .await;
    }

    // Half-close: finish the client send side and wait briefly for the server to
    // acknowledge (mirrors the server-side half-close pattern in MUX-03).
    let _ = ch_send.finish().await;
    let _ = tokio::time::timeout(Duration::from_secs(2), ch_send.stopped()).await;

    // Notify the session pump to remove this channel from the map and send
    // ChannelClose to the server (A4: via control_tx, never direct write).
    if control_tx
        .send(Message::ChannelClose { channel_id })
        .await
        .is_err()
    {
        tracing::debug!(
            channel_id,
            "control_tx closed when sending ChannelClose; pump already gone"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify EvenIdAllocator yields 2, 4, 6, 8 on successive calls.
    #[test]
    fn even_id_allocator_yields_even_ids() {
        let mut alloc = EvenIdAllocator::new();
        assert_eq!(alloc.next_id(), 2);
        assert_eq!(alloc.next_id(), 4);
        assert_eq!(alloc.next_id(), 6);
        assert_eq!(alloc.next_id(), 8);
    }

    /// Verify all allocated ids are even and non-zero.
    #[test]
    fn even_id_allocator_all_even_nonzero() {
        let mut alloc = EvenIdAllocator::new();
        for _ in 0..100 {
            let id = alloc.next_id();
            assert!(id % 2 == 0, "allocated id {id} is not even");
            assert!(id != 0, "id 0 is reserved for the control stream");
        }
    }

    /// Verify the INITIAL_CREDIT constant is 256 KiB (MUX-03).
    #[test]
    fn initial_credit_is_256_kib() {
        assert_eq!(INITIAL_CREDIT, 256 * 1024, "initial credit must be 256 KiB");
    }

    /// Verify CREDIT_REPLENISH_CHUNK is half the initial window.
    #[test]
    fn credit_replenish_chunk_is_half_window() {
        assert_eq!(
            CREDIT_REPLENISH_CHUNK,
            INITIAL_CREDIT / 2,
            "replenish chunk should be half the initial window"
        );
    }

    // ── run_scrollback_drain_task structural tests ───────────────────────────
    //
    // These tests verify:
    // (1) The function exists with the correct signature (compile-time check via
    //     function-pointer coercion).
    // (2) ScrollbackPage and ScrollbackCredit round-trip correctly through the
    //     codec used by the drain task.
    //
    // Full end-to-end delivery is tested in the channel_mux integration tests
    // where real QUIC connections are available.

    /// Verify run_scrollback_drain_task exports exist (compile-time check).
    ///
    /// References the function so the crate fails to compile if it is absent.
    ///
    /// We can't coerce an async fn to a plain fn pointer, so we use `std::mem::size_of_val`
    /// on the fn item reference to force name resolution.
    #[test]
    fn scrollback_drain_task_is_accessible() {
        // std::mem::size_of_val(&f) forces the compiler to resolve the name and
        // size the fn item, which requires the function to exist.  Works for
        // async fn and generic fn alike.
        let _ = std::mem::size_of_val(&run_scrollback_drain_task);
    }

    /// Verify ScrollbackPage encode/decode round-trip (the message type decoded
    /// by the drain task from ch_recv and forwarded on page_tx).
    #[tokio::test]
    async fn scrollback_page_round_trips_via_codec() {
        use nosh_proto::{Message, messages::ScrollbackLine};

        let original = Message::ScrollbackPage {
            channel_id: 4,
            from_line: 0,
            total_available: 100,
            epoch_at_snapshot: 42,
            lines: vec![ScrollbackLine { width: 80, cells: vec![] }],
        };
        let mut buf = std::io::Cursor::new(Vec::<u8>::new());
        nosh_proto::write_message(&mut buf, &original).await.expect("write");
        buf.set_position(0);
        let decoded = nosh_proto::read_message(&mut buf).await.expect("read");
        assert_eq!(original, decoded, "ScrollbackPage must round-trip via codec");
    }

    /// Verify ScrollbackCredit encode/decode (sent by the drain task via
    /// control_tx when drained_since_replenish >= CREDIT_REPLENISH_CHUNK).
    #[tokio::test]
    async fn scrollback_credit_round_trips_via_codec() {
        use nosh_proto::Message;

        let credit = Message::ScrollbackCredit { channel_id: 2, bytes: CREDIT_REPLENISH_CHUNK };
        let mut buf = std::io::Cursor::new(Vec::<u8>::new());
        nosh_proto::write_message(&mut buf, &credit).await.expect("write");
        buf.set_position(0);
        let decoded = nosh_proto::read_message(&mut buf).await.expect("read");
        assert_eq!(credit, decoded, "ScrollbackCredit must round-trip via codec");
    }
}
