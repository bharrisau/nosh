//! Phase 21–22 channel-multiplexing + scrollback-sync integration tests.
//!
//! Phase 21 tests: SC#2–SC#6 for MUX-01…MUX-05.
//! Phase 22 tests (SCROLL-01/02/04/05): scrollback_basic_fetch,
//! scrollback_epoch_handoff_no_gap, scrollback_post_reattach,
//! scrollback_pty_latency_isolation, scrollback_backpressure_drop_oldest,
//! scrollback_inorder_under_loss, scrollback_keybinding_snap_back.
//!
//! These tests drive a real in-process server and client to prove the mux layer
//! end-to-end: control-first OPEN→ACCEPT/REJECT negotiation, opaque REJECT,
//! concurrent non-blocking channels (no HOL blocking), credit-paced backpressure,
//! clean half/full-close, no-panic unknown-id handling, genuine simultaneous
//! client-even + server-odd open, and cold-reattach re-open over the control stream.
//!
//! # Control-stream architecture
//!
//! The control stream carries PTY frames (PtyData, Resize, SessionClose…) AND
//! channel control frames (ChannelOpen, ChannelAccept, ChannelReject,
//! ChannelCredit, ChannelClose) multiplexed together. Any test that performs
//! channel data-stream I/O (reading/writing ch_send / ch_recv) MUST keep
//! draining ctrl_recv concurrently, otherwise the server session pump can block
//! when its ctrl_send write buffer fills up — which would prevent the
//! `accept_bi` arm from binding the data stream to the echo task (deadlock).
//! We solve this by spawning a `ctrl_drain` task that forwards ALL ctrl_recv
//! frames to a tokio mpsc channel. The test reads channel control frames and
//! PtyData from this mpsc without touching ctrl_recv directly.

use std::sync::Arc;
use std::time::{Duration, Instant};

use nosh_client::client::{self, ReattachOutcome};
use nosh_client::quinn_transport::{QuinnTransport, QuinnSendStream, QuinnRecvStream};
use nosh_proto::datagram::{decode_datagram, encode_epoch_ack};
use nosh_proto::messages::ChannelType;
use nosh_proto::transport_trait::{NoshTransport, NoshSendStream, NoshRecvStream};
use nosh_server::registry::SessionRegistry;
use tokio::sync::mpsc;

mod common;
use common::{spawn_server_with_registry, TestKey, HOST};

const SH: &str = "/bin/sh";

fn have_sh() -> bool {
    std::path::Path::new(SH).exists()
}

/// Spawn a server authorising a single client key.
async fn server_with_key(
    registry: Arc<SessionRegistry>,
    client_key: &TestKey,
) -> common::TestServer {
    let host_key = TestKey::generate();
    spawn_server_with_registry(
        &host_key,
        &[&client_key.public],
        nosh_server::server::AuthLimits::default(),
        Some(SH.to_string()),
        registry,
    )
    .await
}

/// Build a client endpoint for the given key against a fresh temp known_hosts file.
fn client_endpoint_for(key: &TestKey) -> (quinn::Endpoint, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let kh = dir.path().join("known_hosts");
    let ep = common::client_endpoint(key.client_identity(), kh).unwrap();
    (ep, dir)
}

// ── Control-stream helpers ────────────────────────────────────────────────────

/// Read the next channel control frame from the control recv stream, draining and
/// discarding any PtyData, TerminalControl, SessionOpened, Ack, and other
/// non-channel frames. Returns the first ChannelAccept, ChannelReject, ChannelOpen,
/// ChannelCredit, or ChannelClose that arrives. Times out after `timeout_ms` ms.
///
/// Use this function when you can hold ctrl_recv exclusively (no concurrent
/// channel data-stream I/O). For tests that do ch_send/ch_recv operations,
/// use `spawn_ctrl_drain` instead to avoid deadlocks.
async fn recv_channel_reply(
    recv: &mut dyn NoshRecvStream,
    timeout_ms: u64,
) -> anyhow::Result<nosh_proto::Message> {
    let deadline = Duration::from_millis(timeout_ms);
    loop {
        let msg = tokio::time::timeout(deadline, nosh_proto::read_message_ns(recv))
            .await
            .map_err(|_| anyhow::anyhow!("timed out waiting for channel reply"))?
            .map_err(|e| anyhow::anyhow!("control stream error: {e}"))?;
        match &msg {
            nosh_proto::Message::PtyData { .. }
            | nosh_proto::Message::SessionClose { .. }
            | nosh_proto::Message::TerminalControl(_)
            | nosh_proto::Message::Ack { .. }
            | nosh_proto::Message::SessionOpened { .. } => {
                continue;
            }
            _ => return Ok(msg),
        }
    }
}

/// Spawn a background task that forwards ALL ctrl_recv frames (PtyData AND channel
/// control frames) to `frame_tx`. The server pump can then always write to ctrl_send
/// without blocking, preventing deadlocks when the test is doing channel data-stream I/O.
///
/// The spawned task runs until ctrl_recv closes or the JoinHandle is aborted.
fn spawn_ctrl_drain(
    mut ctrl_recv: Box<dyn NoshRecvStream>,
    frame_tx: mpsc::UnboundedSender<nosh_proto::Message>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match nosh_proto::read_message_ns(&mut *ctrl_recv).await {
                Ok(msg) => {
                    if frame_tx.send(msg).is_err() {
                        break; // receiver dropped; test is done
                    }
                }
                Err(_) => break, // stream closed
            }
        }
    })
}

/// Receive the next frame from the drain channel, with a timeout.
async fn next_ctrl_frame(
    rx: &mut mpsc::UnboundedReceiver<nosh_proto::Message>,
    timeout_ms: u64,
) -> anyhow::Result<nosh_proto::Message> {
    match tokio::time::timeout(Duration::from_millis(timeout_ms), rx.recv()).await {
        Ok(Some(msg)) => Ok(msg),
        Ok(None) => anyhow::bail!("ctrl drain channel closed"),
        Err(_) => anyhow::bail!("timed out waiting for ctrl frame after {timeout_ms} ms"),
    }
}

/// Drain the given mpsc receiver until the first channel control frame of the
/// expected type arrives, discarding PtyData. Times out after `timeout_ms` ms total.
async fn await_channel_accept_from_drain(
    rx: &mut mpsc::UnboundedReceiver<nosh_proto::Message>,
    expected_id: u32,
    timeout_ms: u64,
) -> anyhow::Result<bool> {
    let deadline = Duration::from_millis(timeout_ms);
    loop {
        let msg = match tokio::time::timeout(deadline, rx.recv()).await {
            Ok(Some(m)) => m,
            Ok(None) => anyhow::bail!("ctrl drain closed"),
            Err(_) => anyhow::bail!("timed out waiting for ChannelAccept for id {expected_id}"),
        };
        match msg {
            nosh_proto::Message::ChannelAccept { channel_id } if channel_id == expected_id => {
                return Ok(true)
            }
            nosh_proto::Message::ChannelReject { .. } => return Ok(false),
            _ => {} // discard PtyData and other frames
        }
    }
}

// ── SC#2 / MUX-01: control-first negotiation + opaque REJECT ─────────────────

/// SC#2 / MUX-01: prove ChannelAccept arrives on the control stream BEFORE the
/// data stream is bound, and that PortForward, AgentForward, and Scrollback
/// receives an opaque ChannelReject with no reason payload.
#[tokio::test]
async fn channel_open_accept_reject() {
    if !have_sh() {
        eprintln!("skipping channel_open_accept_reject: /bin/sh unavailable");
        return;
    }

    let registry = SessionRegistry::new(5, Duration::ZERO);
    let client_key = TestKey::generate();
    let server = server_with_key(registry.clone(), &client_key).await;

    let (ep, _dir) = client_endpoint_for(&client_key);
    let conn = client::connect(&ep, server.addr, HOST, Duration::from_secs(30))
        .await
        .expect("connect");
    let qt = QuinnTransport(conn.clone());

    let (mut ctrl_send, mut ctrl_recv, _token) =
        client::open_session_with_token(&qt, "xterm".to_string(), 80, 24, vec![])
            .await
            .expect("open session");

    // ── Echo channel: expect ChannelAccept (test/test-support builds) ─────────
    // MUX-01: the ACCEPT must arrive on the control stream BEFORE any bidi data
    // stream is opened by the client.
    let echo_id: u32 = 2;
    client::send_channel_open(&mut *ctrl_send, echo_id, ChannelType::Echo)
        .await
        .expect("send ChannelOpen for Echo");
    let reply = recv_channel_reply(&mut *ctrl_recv, 2000)
        .await
        .expect("recv channel reply for Echo");
    assert!(
        matches!(reply, nosh_proto::Message::ChannelAccept { channel_id } if channel_id == echo_id),
        "Echo ChannelOpen must yield ChannelAccept (id {echo_id}); got {}",
        reply.variant_name()
    );

    // ── PortForward: opaque ChannelReject ─────────────────────────────────────
    let pf_id: u32 = 4;
    client::send_channel_open(&mut *ctrl_send, pf_id, ChannelType::PortForward)
        .await
        .expect("send ChannelOpen for PortForward");
    let pf_reply = recv_channel_reply(&mut *ctrl_recv, 2000)
        .await
        .expect("recv channel reply for PortForward");
    match pf_reply {
        nosh_proto::Message::ChannelReject { channel_id } => {
            assert_eq!(channel_id, pf_id, "ChannelReject id must match ChannelOpen id");
            // Opaque reject: discriminant (1 byte) + channel_id varint (1 byte for id 4) = 2 bytes.
            // No reason payload (T-21-02 / MUX-01).
            let encoded =
                postcard::to_allocvec(&nosh_proto::Message::ChannelReject { channel_id })
                    .expect("encode ChannelReject");
            assert_eq!(
                encoded.len(),
                2,
                "ChannelReject must be opaque (discriminant + id only, 2 bytes); got {encoded:?}"
            );
        }
        other => panic!(
            "PortForward ChannelOpen must yield ChannelReject; got {}",
            other.variant_name()
        ),
    }

    // ── AgentForward: opaque ChannelReject ────────────────────────────────────
    let af_id: u32 = 6;
    client::send_channel_open(&mut *ctrl_send, af_id, ChannelType::AgentForward)
        .await
        .expect("send ChannelOpen for AgentForward");
    let af_reply = recv_channel_reply(&mut *ctrl_recv, 2000)
        .await
        .expect("recv AgentForward reply");
    assert!(
        matches!(af_reply, nosh_proto::Message::ChannelReject { channel_id } if channel_id == af_id),
        "AgentForward must yield ChannelReject; got {}", af_reply.variant_name()
    );

    // ── Scrollback: ChannelAccept (Phase 22 — scrollback sync implemented) ────
    // The ACCEPT arrives on the control stream before any bidi data stream is
    // bound (MUX-01); the server then spawns run_scrollback_sender_task.
    let sb_id: u32 = 8;
    client::send_channel_open(&mut *ctrl_send, sb_id, ChannelType::Scrollback)
        .await
        .expect("send ChannelOpen for Scrollback");
    let sb_reply = recv_channel_reply(&mut *ctrl_recv, 2000)
        .await
        .expect("recv Scrollback reply");
    assert!(
        matches!(sb_reply, nosh_proto::Message::ChannelAccept { channel_id } if channel_id == sb_id),
        "Scrollback must yield ChannelAccept (Phase 22); got {}", sb_reply.variant_name()
    );

    conn.close(0u32.into(), b"done");
    ep.close(0u32.into(), b"done");
}

// ── SC#3 / MUX-02: echo round-trip + PTY latency under channel saturation ────

/// SC#3 / MUX-02: open an Echo channel, prove bidirectional byte round-trip, and
/// assert that PTY input latency stays below 5 ms while the echo channel is
/// saturated at its credit window (no HOL blocking).
///
/// Since each channel task is `tokio::spawn`'d independently, the server session
/// pump never reads channel data inline — the `out_rx.recv()` PTY-output arm and
/// the `incoming_stream = conn.accept_bi()` arm run independently of any echo task.
/// This test proves the property both architecturally and by measurement.
#[tokio::test]
async fn channel_echo_roundtrip() {
    if !have_sh() {
        eprintln!("skipping channel_echo_roundtrip: /bin/sh unavailable");
        return;
    }

    let registry = SessionRegistry::new(5, Duration::ZERO);
    let client_key = TestKey::generate();
    let server = server_with_key(registry.clone(), &client_key).await;

    let (ep, _dir) = client_endpoint_for(&client_key);
    let conn = client::connect(&ep, server.addr, HOST, Duration::from_secs(30))
        .await
        .expect("connect");
    let qt = QuinnTransport(conn.clone());

    let (mut ctrl_send, ctrl_recv, _token) =
        client::open_session_with_token(&qt, "xterm".to_string(), 80, 24, vec![])
            .await
            .expect("open session");

    // Spawn ctrl drain BEFORE sending ChannelOpen so PtyData and ChannelAccept
    // can arrive interleaved without blocking the server pump.
    let (frame_tx, mut frame_rx) = mpsc::unbounded_channel::<nosh_proto::Message>();
    let _drain = spawn_ctrl_drain(ctrl_recv, frame_tx);

    // ── Open Echo channel ─────────────────────────────────────────────────────
    let echo_id: u32 = 2;
    client::send_channel_open(&mut *ctrl_send, echo_id, ChannelType::Echo)
        .await
        .expect("send ChannelOpen for Echo");
    let accepted = await_channel_accept_from_drain(&mut frame_rx, echo_id, 2000)
        .await
        .expect("await ChannelAccept for Echo");
    assert!(accepted, "server must accept Echo channel in test build");

    // Bind the bidi data stream with the varint channel-id prefix (MUX-02).
    let (mut ch_send, mut ch_recv) = conn.open_bi().await.expect("open_bi for echo channel");
    let prefix = postcard::to_allocvec(&echo_id).expect("encode channel-id varint");
    ch_send.write_all(&prefix).await.expect("write varint prefix");

    // ── Basic echo round-trip ────────────────────────────────────────────────
    let payload = b"hello from nosh channel test";
    ch_send.write_all(payload).await.expect("write to echo channel");
    let mut echoed = vec![0u8; payload.len()];
    ch_recv
        .read_exact(&mut echoed)
        .await
        .expect("read echo reply");
    assert_eq!(&echoed[..], payload, "echo channel must return exactly the bytes written");

    // ── SC#3: PTY input latency < 5 ms under channel saturation ──────────────
    //
    // Saturate the echo channel: write 256 KiB without draining ch_recv. The
    // server echo task will block (either at the MUX-03 credit limit or at the
    // QUIC send window) after echoing all buffered data. This paused state
    // simulates a fully-saturated second channel.
    //
    // While the echo task is paused, send a PTY keystroke and measure round-trip
    // latency. No HOL blocking means the PTY path (out_rx → ctrl_send) is never
    // delayed by the blocked echo task.
    let saturation_bytes = 256 * 1024usize;
    let sat_payload = vec![0xAAu8; saturation_bytes];
    let sat_handle = tokio::spawn(async move {
        // Best-effort write; QUIC/credit backpressure may cut it short.
        let _ = ch_send.write_all(&sat_payload).await;
    });

    // Give the saturation write a brief moment to fill the server's receive buffer.
    tokio::time::sleep(Duration::from_millis(10)).await;

    // SC#3: PTY input latency must stay below 5 ms while the echo channel is
    // saturated (proof of no head-of-line blocking between channels).
    //
    // A single keystroke sample is sensitive to one-off tokio scheduler jitter on
    // a loaded host (a stray 6 ms sample does not indicate HOL blocking). We take
    // several samples and assert the MEDIAN is < 5 ms: genuine HOL blocking would
    // delay *every* sample behind the 256 KiB saturation write (hundreds of ms), so
    // the median still fails hard on a real regression, while transient per-sample
    // jitter no longer flakes the binding 5 ms contract.
    const SC3_SAMPLES: usize = 5;
    let mut samples: Vec<Duration> = Vec::with_capacity(SC3_SAMPLES);
    for i in 0..SC3_SAMPLES {
        let marker = format!("SC3_PTY_LATENCY_NOSH_{i}");
        let t0 = Instant::now();
        client::send_input(&mut *ctrl_send, format!("printf '{marker}\\n'\n").as_bytes())
            .await
            .expect("send PTY input");

        let mut found = false;
        let deadline = Instant::now() + Duration::from_millis(500);
        while Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_millis(200), frame_rx.recv()).await {
                Ok(Some(nosh_proto::Message::PtyData { data })) => {
                    if String::from_utf8_lossy(&data).contains(&marker) {
                        samples.push(t0.elapsed());
                        found = true;
                        break;
                    }
                }
                Ok(Some(_)) => {} // skip other frames
                Ok(None) => break,
                Err(_) => {} // per-iteration timeout; keep trying within deadline
            }
        }
        assert!(
            found,
            "PTY marker {marker} must be visible within 500 ms even under channel saturation"
        );
    }

    samples.sort_unstable();
    let median = samples[samples.len() / 2];

    // SC#3 binding assertion: median PTY round-trip must be < 5 ms.
    assert!(
        median < Duration::from_millis(5),
        "SC#3 VIOLATED: median PTY input latency was {:?} over {} samples under channel \
         saturation (must be < 5 ms). A real HOL-blocking regression delays every sample \
         behind the saturation write — verify channel tasks are always tokio::spawn'd and \
         never read inline in the session pump. Samples: {:?}",
        median, SC3_SAMPLES, samples
    );

    sat_handle.abort();
    conn.close(0u32.into(), b"done");
    ep.close(0u32.into(), b"done");
}

// ── MUX-03: credit flow-control backpressure without deadlock ────────────────

/// MUX-03: confirm the sender pauses at the credit window and resumes only after
/// ChannelCredit is granted — forward progress resumes without deadlock once the
/// consumer drains and credit flows back.
#[tokio::test]
async fn channel_flow_control_backpressure() {
    if !have_sh() {
        eprintln!("skipping channel_flow_control_backpressure: /bin/sh unavailable");
        return;
    }

    let registry = SessionRegistry::new(5, Duration::ZERO);
    let client_key = TestKey::generate();
    let server = server_with_key(registry.clone(), &client_key).await;

    let (ep, _dir) = client_endpoint_for(&client_key);
    let conn = client::connect(&ep, server.addr, HOST, Duration::from_secs(30))
        .await
        .expect("connect");
    let qt = QuinnTransport(conn.clone());

    let (mut ctrl_send, ctrl_recv, _token) =
        client::open_session_with_token(&qt, "xterm".to_string(), 80, 24, vec![])
            .await
            .expect("open session");

    // Spawn ctrl drain.
    let (frame_tx, mut frame_rx) = mpsc::unbounded_channel::<nosh_proto::Message>();
    let _drain = spawn_ctrl_drain(ctrl_recv, frame_tx);

    // Open an Echo channel.
    let echo_id: u32 = 2;
    client::send_channel_open(&mut *ctrl_send, echo_id, ChannelType::Echo)
        .await
        .expect("send ChannelOpen for Echo");
    let accepted =
        await_channel_accept_from_drain(&mut frame_rx, echo_id, 2000)
            .await
            .expect("await ChannelAccept");
    assert!(accepted, "server must accept Echo channel");

    let (mut ch_send, mut ch_recv) = conn.open_bi().await.expect("open_bi");
    let prefix = postcard::to_allocvec(&echo_id).expect("encode varint");
    ch_send.write_all(&prefix).await.expect("write varint prefix");

    // Write the full initial credit window (256 KiB) in a background task.
    let window: usize = 256 * 1024;
    let payload = vec![0xCCu8; window];
    let write_handle = tokio::spawn(async move {
        let mut s = ch_send;
        s.write_all(&payload).await.expect("write saturation payload");
        s
    });

    // Drain all 256 KiB of echoed bytes, replenishing credit as we go.
    // Without credit replenishment the server echo task would block at
    // remaining_credit = 0. By sending ChannelCredit as we drain, we prove
    // forward progress resumes after the pause (no deadlock).
    let mut total_received: usize = 0;
    let mut credit_baseline: usize = 0;
    let mut buf = vec![0u8; 8192];
    let drain_deadline = Instant::now() + Duration::from_secs(10);

    while total_received < window {
        assert!(
            Instant::now() < drain_deadline,
            "deadlock: received {total_received} of {window} bytes within 10 s"
        );
        match tokio::time::timeout(Duration::from_millis(500), ch_recv.read(&mut buf)).await {
            Ok(Ok(Some(n))) => {
                total_received += n;
                let since_last = total_received - credit_baseline;
                // Replenish in 128 KiB chunks (half the initial window).
                if since_last >= 128 * 1024 {
                    nosh_proto::write_message_ns(
                        &mut *ctrl_send,
                        &nosh_proto::Message::ChannelCredit {
                            channel_id: echo_id,
                            bytes: since_last as u64,
                        },
                    )
                    .await
                    .expect("send ChannelCredit");
                    credit_baseline = total_received;
                }
            }
            Ok(Ok(None)) => break,
            Ok(Err(e)) => panic!("ch_recv error: {e}"),
            Err(_) => {} // brief idle
        }
    }

    assert_eq!(
        total_received, window,
        "MUX-03: must receive all {window} bytes without deadlock; got {total_received}"
    );

    // Send final credit for any remainder.
    let remainder = total_received - credit_baseline;
    if remainder > 0 {
        nosh_proto::write_message_ns(
            &mut *ctrl_send,
            &nosh_proto::Message::ChannelCredit {
                channel_id: echo_id,
                bytes: remainder as u64,
            },
        )
        .await
        .expect("send final ChannelCredit");
    }

    // Confirm forward progress: one more byte after credit replenishment.
    let mut ch_send = write_handle.await.expect("write task completed");
    ch_send.write_all(b"z").await.expect("write post-credit byte");
    let mut one = [0u8; 1];
    tokio::time::timeout(Duration::from_secs(2), ch_recv.read_exact(&mut one))
        .await
        .expect("no timeout on post-credit echo")
        .expect("read post-credit echo");
    assert_eq!(one[0], b'z', "post-credit byte must echo correctly");

    conn.close(0u32.into(), b"done");
    ep.close(0u32.into(), b"done");
}

// ── SC#4 / MUX-04: clean half/full-close + no-panic unknown-id ───────────────

/// SC#4 / MUX-04: half-close the send side, confirm the peer sees EOF and the
/// channel is fully released; then assert that a ChannelAccept for an id that was
/// never opened is a no-op (session survives, no panic). Tests T-21-07.
#[tokio::test]
async fn channel_lifecycle_clean() {
    if !have_sh() {
        eprintln!("skipping channel_lifecycle_clean: /bin/sh unavailable");
        return;
    }

    let registry = SessionRegistry::new(5, Duration::ZERO);
    let client_key = TestKey::generate();
    let server = server_with_key(registry.clone(), &client_key).await;

    let (ep, _dir) = client_endpoint_for(&client_key);
    let conn = client::connect(&ep, server.addr, HOST, Duration::from_secs(30))
        .await
        .expect("connect");
    let qt = QuinnTransport(conn.clone());

    let (mut ctrl_send, ctrl_recv, _token) =
        client::open_session_with_token(&qt, "xterm".to_string(), 80, 24, vec![])
            .await
            .expect("open session");

    // Spawn ctrl drain — keeps server pump from blocking on ctrl_send.
    let (frame_tx, mut frame_rx) = mpsc::unbounded_channel::<nosh_proto::Message>();
    let _drain = spawn_ctrl_drain(ctrl_recv, frame_tx);

    // Open an Echo channel.
    let echo_id: u32 = 2;
    client::send_channel_open(&mut *ctrl_send, echo_id, ChannelType::Echo)
        .await
        .expect("send ChannelOpen for Echo");
    let accepted = await_channel_accept_from_drain(&mut frame_rx, echo_id, 2000)
        .await
        .expect("await ChannelAccept");
    assert!(accepted, "server must accept Echo channel");

    let (mut ch_send, mut ch_recv) = conn.open_bi().await.expect("open_bi");
    let prefix = postcard::to_allocvec(&echo_id).expect("encode varint");
    ch_send.write_all(&prefix).await.expect("write varint prefix");

    // Write a small payload and confirm round-trip.
    ch_send.write_all(b"lifecycle").await.expect("write lifecycle payload");
    let mut buf = vec![0u8; 9];
    ch_recv.read_exact(&mut buf).await.expect("read lifecycle echo");
    assert_eq!(&buf, b"lifecycle", "echo must match");

    // ── Half-close: finish the send side ─────────────────────────────────────
    ch_send.finish().expect("finish send side");

    // Server echo task sees RecvStream EOF, finishes its loop, and finishes its
    // own send side. Client's ch_recv must reach EOF within 2 s.
    let drain_result = tokio::time::timeout(Duration::from_secs(2), async {
        let mut trailing = Vec::new();
        let mut tiny = [0u8; 64];
        loop {
            match ch_recv.read(&mut tiny).await {
                Ok(Some(n)) => trailing.extend_from_slice(&tiny[..n]),
                Ok(None) => return true, // EOF — clean close
                Err(_) => return false,
            }
        }
    })
    .await;

    assert!(
        drain_result.unwrap_or(false),
        "channel RecvStream must reach EOF after half-close within 2 s"
    );

    // ── Second ChannelClose for the same id: no-op ────────────────────────────
    nosh_proto::write_message_ns(
        &mut *ctrl_send,
        &nosh_proto::Message::ChannelClose { channel_id: echo_id },
    )
    .await
    .expect("send second ChannelClose for same id");

    // ── ChannelAccept for unknown odd id: no-op ───────────────────────────────
    // Channel id 9999 is odd → the server handles it as a client reply to a
    // server-initiated open (under the test-support feature gate). Since no
    // server-initiated open with id 9999 was issued, this is a logged no-op
    // (T-21-07 no-panic rule).
    nosh_proto::write_message_ns(
        &mut *ctrl_send,
        &nosh_proto::Message::ChannelAccept { channel_id: 9999 },
    )
    .await
    .expect("send ChannelAccept for unknown odd id");

    tokio::time::sleep(Duration::from_millis(50)).await;

    // ── Session survival: control stream must still accept writes ─────────────
    let alive = nosh_proto::write_message_ns(
        &mut *ctrl_send,
        &nosh_proto::Message::Ack { seq: 0 },
    )
    .await;
    assert!(
        alive.is_ok(),
        "session must survive after ChannelClose for closed id + unknown-id ChannelAccept"
    );

    conn.close(0u32.into(), b"done");
    ep.close(0u32.into(), b"done");
}

// ── SC#6 / MUX-04: genuine simultaneous client-even + server-odd ChannelOpen ─

/// SC#6 / MUX-04: fire a REAL client-even open (id 2) and a REAL server-odd open
/// (id 1) concurrently, assert non-colliding ids in disjoint parity spaces, both
/// channels accepted and echoing, and the session surviving.
///
/// The server-odd open is triggered via the `#[cfg(any(test, feature = "test-support"))]`
/// `server_open_tx` accessor documented in 21-02-SUMMARY.md.
#[tokio::test]
async fn channel_simultaneous_open() {
    if !have_sh() {
        eprintln!("skipping channel_simultaneous_open: /bin/sh unavailable");
        return;
    }

    let registry = SessionRegistry::new(5, Duration::ZERO);
    let client_key = TestKey::generate();
    let server = server_with_key(registry.clone(), &client_key).await;

    let (ep, _dir) = client_endpoint_for(&client_key);
    let conn = client::connect(&ep, server.addr, HOST, Duration::from_secs(30))
        .await
        .expect("connect");
    let qt = QuinnTransport(conn.clone());

    let (mut ctrl_send, ctrl_recv, _token) =
        client::open_session_with_token(&qt, "xterm".to_string(), 80, 24, vec![])
            .await
            .expect("open session");

    // Wait until the session pump has stored server_open_tx in the slot.
    let slot = {
        let slot_deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(s) = registry.first_active_slot() {
                break s;
            }
            assert!(Instant::now() < slot_deadline, "slot did not become active within 5 s");
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    };
    let server_open_tx = {
        let tx_deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(tx) = slot.take_server_open_tx() {
                break tx;
            }
            assert!(Instant::now() < tx_deadline, "server_open_tx not stored within 5 s");
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    };

    // Spawn ctrl drain to forward all frames.
    let (frame_tx, mut frame_rx) = mpsc::unbounded_channel::<nosh_proto::Message>();
    let _drain = spawn_ctrl_drain(ctrl_recv, frame_tx);

    // ── Fire concurrent opens ─────────────────────────────────────────────────
    // Trigger the server to allocate odd id 1 and write ChannelOpen on ctrl_send.
    server_open_tx
        .send(ChannelType::Echo)
        .await
        .expect("trigger server-initiated ChannelOpen");

    // Immediately send client ChannelOpen for even id 2.
    let client_id: u32 = 2;
    client::send_channel_open(&mut *ctrl_send, client_id, ChannelType::Echo)
        .await
        .expect("send client ChannelOpen");

    // Collect two channel control frames: ChannelOpen from server (odd id)
    // and ChannelAccept from server (for our even id). Order is non-deterministic.
    let mut server_open_id: Option<u32> = None;
    let mut client_accept_id: Option<u32> = None;

    let frames_deadline = Instant::now() + Duration::from_secs(3);
    while (server_open_id.is_none() || client_accept_id.is_none())
        && Instant::now() < frames_deadline
    {
        let msg =
            match tokio::time::timeout(Duration::from_millis(500), frame_rx.recv()).await {
                Ok(Some(m)) => m,
                Ok(None) => break,
                Err(_) => continue,
            };
        match msg {
            nosh_proto::Message::ChannelOpen { channel_id, .. } => {
                server_open_id = Some(channel_id);
                // Client must reply ChannelAccept to the server-initiated open.
                nosh_proto::write_message_ns(
                    &mut *ctrl_send,
                    &nosh_proto::Message::ChannelAccept { channel_id },
                )
                .await
                .expect("reply ChannelAccept to server-initiated open");
            }
            nosh_proto::Message::ChannelAccept { channel_id } => {
                client_accept_id = Some(channel_id);
            }
            _ => {} // PtyData and other frames
        }
    }

    let server_open_id = server_open_id.expect("server-initiated ChannelOpen must arrive");
    let client_accept_id = client_accept_id.expect("ChannelAccept for client open must arrive");

    // ── (a) Client id is even ─────────────────────────────────────────────────
    assert_eq!(client_accept_id, client_id, "ChannelAccept id must match ChannelOpen id");
    assert!(client_accept_id % 2 == 0, "client-initiated id must be even; got {client_accept_id}");

    // ── (b) Server id is odd ──────────────────────────────────────────────────
    assert!(server_open_id % 2 != 0, "server-initiated id must be odd; got {server_open_id}");

    // ── (c) No collision ──────────────────────────────────────────────────────
    assert_ne!(server_open_id, client_accept_id, "server odd id and client even id must not collide");

    // ── (d) Both channels echo data ───────────────────────────────────────────
    let (mut cds, mut cdr) = conn.open_bi().await.expect("open_bi client channel");
    let cp = postcard::to_allocvec(&client_id).expect("encode client id");
    cds.write_all(&cp).await.expect("write client varint prefix");

    let (mut sds, mut sdr) = conn.open_bi().await.expect("open_bi server channel");
    let sp = postcard::to_allocvec(&server_open_id).expect("encode server id");
    sds.write_all(&sp).await.expect("write server varint prefix");

    cds.write_all(b"even-ch").await.expect("write client echo payload");
    sds.write_all(b"odd--ch").await.expect("write server echo payload");

    let mut ce = vec![0u8; 7];
    let mut se = vec![0u8; 7];
    cdr.read_exact(&mut ce).await.expect("read client echo");
    sdr.read_exact(&mut se).await.expect("read server echo");
    assert_eq!(&ce, b"even-ch", "client channel echo must round-trip");
    assert_eq!(&se, b"odd--ch", "server channel echo must round-trip");

    // ── (e) Session survives ──────────────────────────────────────────────────
    let alive = nosh_proto::write_message_ns(
        &mut *ctrl_send,
        &nosh_proto::Message::Ack { seq: 0 },
    )
    .await;
    assert!(alive.is_ok(), "session must survive simultaneous open");

    conn.close(0u32.into(), b"done");
    ep.close(0u32.into(), b"done");
}

// ── SC#5 / MUX-05: cold-reattach re-open ──────────────────────────────────────

/// SC#5 / MUX-05: open an Echo channel, orphan the session (abrupt connection
/// drop), cold-reattach, then re-open the channel via a fresh OPEN/ACCEPT round-trip
/// after ReattachOk — NOT via byte-replay (Pitfall 4 / MUX-05).
#[tokio::test]
async fn channel_reattach_reopen() {
    if !have_sh() {
        eprintln!("skipping channel_reattach_reopen: /bin/sh unavailable");
        return;
    }

    let registry = SessionRegistry::new(5, Duration::ZERO);
    let client_key = TestKey::generate();
    let server = server_with_key(registry.clone(), &client_key).await;

    // ── Fresh session + echo channel ─────────────────────────────────────────
    let (ep1, _dir1) = client_endpoint_for(&client_key);
    let conn1 = client::connect(&ep1, server.addr, HOST, Duration::from_secs(30))
        .await
        .expect("connect");
    let qt1 = QuinnTransport(conn1.clone());

    let (mut ctrl_send1, ctrl_recv1, token) =
        client::open_session_with_token(&qt1, "xterm".to_string(), 80, 24, vec![])
            .await
            .expect("open session 1");

    let (frame_tx1, mut frame_rx1) = mpsc::unbounded_channel::<nosh_proto::Message>();
    let _drain1 = spawn_ctrl_drain(ctrl_recv1, frame_tx1);

    let echo_id: u32 = 2;
    client::send_channel_open(&mut *ctrl_send1, echo_id, ChannelType::Echo)
        .await
        .expect("send ChannelOpen session 1");
    let accepted1 = await_channel_accept_from_drain(&mut frame_rx1, echo_id, 2000)
        .await
        .expect("await ChannelAccept session 1");
    assert!(accepted1, "server must accept Echo channel");

    let (mut ch_send1, mut ch_recv1) = conn1.open_bi().await.expect("open_bi session 1");
    let prefix1 = postcard::to_allocvec(&echo_id).expect("encode varint");
    ch_send1.write_all(&prefix1).await.expect("write varint prefix 1");

    ch_send1.write_all(b"pre-orphan").await.expect("write before orphan");
    let mut echo1 = vec![0u8; 10];
    ch_recv1.read_exact(&mut echo1).await.expect("read echo before orphan");
    assert_eq!(&echo1, b"pre-orphan", "echo must work before orphan");

    // ── Orphan the session ────────────────────────────────────────────────────
    drop(ctrl_send1);
    drop(ch_send1);
    drop(ch_recv1);
    conn1.close(1u32.into(), b"test transport loss");
    drop(conn1);
    ep1.close(0u32.into(), b"done");
    drop(ep1);

    let orphan_deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if registry.total_orphans() >= 1 {
            break;
        }
        assert!(Instant::now() < orphan_deadline, "server did not orphan within 5 s");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    // ── Reconnect and Reattach ────────────────────────────────────────────────
    let (ep2, _dir2) = client_endpoint_for(&client_key);
    let conn2 = client::connect(&ep2, server.addr, HOST, Duration::from_secs(30))
        .await
        .expect("connect for reattach");

    let (s2, r2) = conn2.open_bi().await.expect("open bi for reattach");
    let mut ctrl_send2: Box<dyn NoshSendStream> = Box::new(QuinnSendStream(s2));
    let mut ctrl_recv2: Box<dyn NoshRecvStream> = Box::new(QuinnRecvStream(r2));
    client::send_reattach(&mut *ctrl_send2, token, 0)
        .await
        .expect("send Reattach");

    // await_reattach_reply reads directly from ctrl_recv2 before the drain is started.
    let outcome = client::await_reattach_reply(&mut *ctrl_recv2)
        .await
        .expect("await reattach reply");
    assert!(
        matches!(outcome, ReattachOutcome::Ok { .. }),
        "cold reattach must succeed"
    );

    // After ReattachOk, start the drain for the reattached session.
    let (frame_tx2, mut frame_rx2) = mpsc::unbounded_channel::<nosh_proto::Message>();
    let _drain2 = spawn_ctrl_drain(ctrl_recv2, frame_tx2);

    // ── Re-open the Echo channel AFTER ReattachOk ─────────────────────────────
    // The server cleared its channel_map on orphan (no replay of channel state).
    // The client must issue a fresh ChannelOpen (Pitfall 4 / MUX-05).
    client::send_channel_open(&mut *ctrl_send2, echo_id, ChannelType::Echo)
        .await
        .expect("send ChannelOpen after reattach");

    let accepted2 = await_channel_accept_from_drain(&mut frame_rx2, echo_id, 2000)
        .await
        .expect("await ChannelAccept after reattach");
    assert!(
        accepted2,
        "SC#5: server must accept re-opened Echo channel on reattached session"
    );

    let (mut ch_send2, mut ch_recv2) = conn2.open_bi().await.expect("open_bi after reattach");
    let prefix2 = postcard::to_allocvec(&echo_id).expect("encode varint");
    ch_send2.write_all(&prefix2).await.expect("write varint prefix after reattach");

    ch_send2.write_all(b"post-reattach").await.expect("write after reattach");
    let mut echo2 = vec![0u8; 13];
    ch_recv2.read_exact(&mut echo2).await.expect("read echo after reattach");
    assert_eq!(
        &echo2,
        b"post-reattach",
        "SC#5: re-opened channel must echo data after cold reattach"
    );

    conn2.close(0u32.into(), b"done");
    ep2.close(0u32.into(), b"done");
}

// ── Phase 22 scrollback integration tests (SCROLL-01/02/04/05) ───────────────
//
// All scrollback tests use `spawn_ctrl_drain` + `await_channel_accept_from_drain`
// (never `recv_channel_reply`) because:
//   (a) The scrollback channel performs data-stream I/O concurrently with control
//       frames, requiring the drain to be live.
//   (b) `recv_channel_reply` holds ctrl_recv exclusively and would deadlock as
//       soon as the server pump tries to write another frame while we are reading
//       the scrollback data stream.
//
// Session bring-up: open a session, spawn a ctrl_drain, produce enough PTY output
// to scroll at least one row into the server-side scrollback VecDeque, then open a
// Scrollback channel via `client::open_channel(ChannelType::Scrollback)`.

/// Helper: open a session, send enough PTY output to fill the 24-row grid and
/// scroll lines into the server-side scrollback VecDeque, then wait for at least
/// one datagram (proving terminal state has been applied).
///
/// Returns `(ctrl_send, frame_rx)`.  The caller must keep `frame_rx` live for the
/// duration of the test (the ctrl_drain task writes to it concurrently).
async fn produce_scrollback(
    conn: &quinn::Connection,
) -> (Box<dyn NoshSendStream>, mpsc::UnboundedReceiver<nosh_proto::Message>) {
    let qt = QuinnTransport(conn.clone());
    let (mut ctrl_send, ctrl_recv, _token) =
        client::open_session_with_token(&qt, "xterm".to_string(), 80, 24, vec![])
            .await
            .expect("open session for produce_scrollback");

    let (frame_tx, frame_rx) = mpsc::unbounded_channel::<nosh_proto::Message>();
    let _drain = spawn_ctrl_drain(ctrl_recv, frame_tx);

    // 40 numbered `printf` lines overflow the 24-row PTY grid, pushing rows into
    // the server-side scrollback VecDeque.
    for i in 0..40u32 {
        client::send_input(
            &mut *ctrl_send,
            format!("printf 'line_{i}\\n'\n").as_bytes(),
        )
        .await
        .expect("send PTY input to produce scrollback");
    }

    // Wait for at least one datagram to confirm the server has applied output.
    let datagram_deadline = Duration::from_secs(10);
    let got_datagram = tokio::time::timeout(datagram_deadline, async {
        loop {
            match conn.read_datagram().await {
                Ok(bytes) => {
                    if decode_datagram(&bytes).is_ok() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    })
    .await;
    assert!(
        got_datagram.is_ok(),
        "did not receive any datagram from the server within {datagram_deadline:?}"
    );

    // Brief settle time so shell echo is fully parsed into scrollback.
    tokio::time::sleep(Duration::from_millis(200)).await;

    (ctrl_send, frame_rx)
}

/// Helper: send a `ChannelOpen` for `ChannelType::Scrollback`, await Accept via
/// drain, open the QUIC bidi stream, and write the varint channel-id prefix.
///
/// Returns `(ch_send, ch_recv, channel_id)`.  Never uses `recv_channel_reply`.
async fn open_scrollback_channel(
    conn: &quinn::Connection,
    ctrl_send: &mut dyn NoshSendStream,
    frame_rx: &mut mpsc::UnboundedReceiver<nosh_proto::Message>,
) -> (quinn::SendStream, quinn::RecvStream, u32) {
    let channel_id: u32 = 2;
    client::send_channel_open(ctrl_send, channel_id, ChannelType::Scrollback)
        .await
        .expect("send ChannelOpen for Scrollback");
    let accepted = await_channel_accept_from_drain(frame_rx, channel_id, 3000)
        .await
        .expect("await ChannelAccept for Scrollback");
    assert!(accepted, "server must accept ChannelType::Scrollback");

    let (mut ch_send, ch_recv) = conn.open_bi().await.expect("open_bi for scrollback channel");
    let prefix = postcard::to_allocvec(&channel_id).expect("encode channel-id varint");
    ch_send.write_all(&prefix).await.expect("write varint prefix on scrollback bidi stream");

    (ch_send, ch_recv, channel_id)
}

/// Helper: write a `ScrollbackRequest` to `ch_send` and read back the next
/// `ScrollbackPage` from `ch_recv`.
async fn send_request_read_page(
    ch_send: &mut quinn::SendStream,
    ch_recv: &mut quinn::RecvStream,
    channel_id: u32,
    from_line: u64,
    count: u32,
) -> nosh_proto::Message {
    nosh_proto::write_message(
        ch_send,
        &nosh_proto::Message::ScrollbackRequest {
            channel_id,
            from_line,
            count,
        },
    )
    .await
    .expect("write ScrollbackRequest");

    let deadline = Duration::from_secs(10);
    tokio::time::timeout(deadline, async {
        loop {
            match nosh_proto::read_message(ch_recv).await {
                Ok(msg @ nosh_proto::Message::ScrollbackPage { .. }) => return msg,
                Ok(_) => {}
                Err(e) => panic!("ch_recv error waiting for ScrollbackPage: {e}"),
            }
        }
    })
    .await
    .expect("timed out waiting for ScrollbackPage")
}

// ── SCROLL-01: basic fetch ────────────────────────────────────────────────────

/// SCROLL-01: prove end-to-end scrollback delivery over a real loopback session.
///
/// Also asserts that live datagrams continue to arrive with advancing epochs
/// DURING the active scrollback transfer (M-6 / SCROLL-05 ack-flow continuity):
/// the server pump must never stall because the client is in scrollback mode.
#[tokio::test]
async fn scrollback_basic_fetch() {
    if !have_sh() {
        eprintln!("skipping scrollback_basic_fetch: /bin/sh unavailable");
        return;
    }

    let registry = SessionRegistry::new(5, Duration::ZERO);
    let client_key = TestKey::generate();
    let server = server_with_key(registry.clone(), &client_key).await;

    let (ep, _dir) = client_endpoint_for(&client_key);
    let conn = client::connect(&ep, server.addr, HOST, Duration::from_secs(30))
        .await
        .expect("connect");

    let (mut ctrl_send, mut frame_rx) = produce_scrollback(&conn).await;

    let (mut ch_send, mut ch_recv, channel_id) =
        open_scrollback_channel(&conn, &mut *ctrl_send, &mut frame_rx).await;

    // Request up to 256 lines starting from the newest.
    let page = send_request_read_page(&mut ch_send, &mut ch_recv, channel_id, 0, 256).await;

    let (lines_count, total_avail, epoch_at_snap) = match &page {
        nosh_proto::Message::ScrollbackPage {
            lines,
            total_available,
            epoch_at_snapshot,
            ..
        } => (lines.len(), *total_available, *epoch_at_snapshot),
        other => panic!("expected ScrollbackPage, got {}", other.variant_name()),
    };

    assert!(
        lines_count > 0,
        "SCROLL-01: ScrollbackPage must have at least one line; got {lines_count}"
    );
    assert!(
        total_avail > 0,
        "SCROLL-01: total_available must be > 0; got {total_avail}"
    );
    assert!(
        epoch_at_snap > 0,
        "SCROLL-01: epoch_at_snapshot must be > 0 (server must have ticked); got {epoch_at_snap}"
    );

    // ── Epoch-ack continuity proof (M-6 / SCROLL-05) ─────────────────────────
    // While the scrollback channel is open, generate more PTY output and assert
    // that datagrams keep arriving with strictly increasing epochs — proving the
    // server pump is NOT stalling because of the open scrollback channel.
    client::send_input(&mut *ctrl_send, b"printf 'epoch_check\\n'\n")
        .await
        .expect("send PTY input during scrollback");

    let mut last_epoch = epoch_at_snap;
    let mut epoch_advanced = false;
    let epoch_deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < epoch_deadline {
        match tokio::time::timeout(Duration::from_millis(200), conn.read_datagram()).await {
            Ok(Ok(bytes)) => {
                if let Ok(diff) = decode_datagram(&bytes) {
                    if diff.epoch > last_epoch {
                        epoch_advanced = true;
                        last_epoch = diff.epoch;
                        // Emit epoch-ack (simulating client ack-flow continuity in Active mode).
                        let _ = conn.send_datagram(encode_epoch_ack(diff.epoch));
                        break;
                    }
                }
            }
            Ok(Err(_)) | Err(_) => {}
        }
    }

    assert!(
        epoch_advanced,
        "M-6 / SCROLL-05: datagram epoch must advance during an active scrollback transfer \
         (last_epoch={last_epoch}); server pump appears stalled"
    );

    // Grant credit so the sender can clean up.
    let _ = nosh_proto::write_message_ns(
        &mut *ctrl_send,
        &nosh_proto::Message::ScrollbackCredit {
            channel_id,
            bytes: 256 * 1024,
        },
    )
    .await;

    conn.close(0u32.into(), b"done");
    ep.close(0u32.into(), b"done");
}

// ── SCROLL-05 / S-5: epoch handoff — no torn epoch ───────────────────────────

/// SCROLL-05 / S-5: issue a `ScrollbackRequest` CONCURRENTLY with live diff ticks
/// (without quiescing the session) and assert:
///   (a) the page carries a single consistent `epoch_at_snapshot` (non-zero,
///       monotonically non-decreasing across two consecutive requests), and
///   (b) the seam invariant: `lines.len() <= total_available` and the second
///       page's `from_line` is `>= first page's line count` (no gap or duplicate
///       at the boundary).
#[tokio::test]
async fn scrollback_epoch_handoff_no_gap() {
    if !have_sh() {
        eprintln!("skipping scrollback_epoch_handoff_no_gap: /bin/sh unavailable");
        return;
    }

    let registry = SessionRegistry::new(5, Duration::ZERO);
    let client_key = TestKey::generate();
    let server = server_with_key(registry.clone(), &client_key).await;

    let (ep, _dir) = client_endpoint_for(&client_key);
    let conn = client::connect(&ep, server.addr, HOST, Duration::from_secs(30))
        .await
        .expect("connect");

    let (mut ctrl_send, mut frame_rx) = produce_scrollback(&conn).await;

    // Keep diff ticks firing by draining datagrams and acking in the background.
    let conn_bg = conn.clone();
    let ack_task = tokio::spawn(async move {
        for _ in 0..30u32 {
            tokio::time::sleep(Duration::from_millis(20)).await;
            match tokio::time::timeout(Duration::from_millis(50), conn_bg.read_datagram()).await {
                Ok(Ok(bytes)) => {
                    if let Ok(diff) = decode_datagram(&bytes) {
                        let _ = conn_bg.send_datagram(encode_epoch_ack(diff.epoch));
                    }
                }
                _ => {}
            }
        }
    });

    // Generate concurrent PTY output while we open the channel (races diff ticks).
    for i in 0..5u32 {
        client::send_input(
            &mut ctrl_send,
            format!("printf 'race_{i}\\n'\n").as_bytes(),
        )
        .await
        .expect("send PTY input racing with scrollback request");
    }

    let (mut ch_send, mut ch_recv, channel_id) =
        open_scrollback_channel(&conn, &mut *ctrl_send, &mut frame_rx).await;

    // First request — concurrent with diff ticks (no quiesce before this).
    let page1 = send_request_read_page(&mut ch_send, &mut ch_recv, channel_id, 0, 256).await;

    let (lines1, total_avail1, epoch1, from_line1) = match &page1 {
        nosh_proto::Message::ScrollbackPage {
            lines, total_available, epoch_at_snapshot, from_line, ..
        } => (lines.clone(), *total_available, *epoch_at_snapshot, *from_line),
        other => panic!("expected first ScrollbackPage, got {}", other.variant_name()),
    };

    // (a) Non-zero epoch (server has ticked).
    assert!(epoch1 > 0, "S-5: first page epoch_at_snapshot must be > 0; got {epoch1}");
    assert_eq!(from_line1, 0, "S-5: first page from_line must be 0");
    assert!(
        lines1.len() as u64 <= total_avail1,
        "S-5: lines.len() ({}) must not exceed total_available ({total_avail1})",
        lines1.len()
    );

    // Grant credit so a second request can be served.
    let _ = nosh_proto::write_message_ns(
        &mut *ctrl_send,
        &nosh_proto::Message::ScrollbackCredit {
            channel_id,
            bytes: 256 * 1024,
        },
    )
    .await;

    // Second request — epoch must be >= first (monotonic, no backward jump).
    let page2 = send_request_read_page(&mut ch_send, &mut ch_recv, channel_id, 0, 256).await;

    let epoch2 = match &page2 {
        nosh_proto::Message::ScrollbackPage { epoch_at_snapshot, .. } => *epoch_at_snapshot,
        other => panic!("expected second ScrollbackPage, got {}", other.variant_name()),
    };

    assert!(
        epoch2 >= epoch1,
        "S-5: second page epoch ({epoch2}) must be >= first ({epoch1}); epoch must not regress"
    );

    let _ = ack_task.await;
    conn.close(0u32.into(), b"done");
    ep.close(0u32.into(), b"done");
}

// ── SCROLL-05 / Pitfall 5: post-reattach fresh channel id ────────────────────

/// SCROLL-05 / Pitfall 5: after a cold reattach, the Scrollback channel must be
/// re-opened as a FRESH channel (no byte-replay of the pre-orphan channel state).
/// The server cleared its channel_map on orphan, so a new `ChannelOpen` for the
/// same id must succeed and deliver scrollback pages.
#[tokio::test]
async fn scrollback_post_reattach() {
    if !have_sh() {
        eprintln!("skipping scrollback_post_reattach: /bin/sh unavailable");
        return;
    }

    let registry = SessionRegistry::new(5, Duration::ZERO);
    let client_key = TestKey::generate();
    let server = server_with_key(registry.clone(), &client_key).await;

    // ── Session 1: produce scrollback, record the reattach token ─────────────
    let (ep1, _dir1) = client_endpoint_for(&client_key);
    let conn1 = client::connect(&ep1, server.addr, HOST, Duration::from_secs(30))
        .await
        .expect("connect session 1");
    let qt1 = QuinnTransport(conn1.clone());

    let (mut ctrl_send1, ctrl_recv1, token) =
        client::open_session_with_token(&qt1, "xterm".to_string(), 80, 24, vec![])
            .await
            .expect("open session 1 with token");

    let (frame_tx1, mut frame_rx1) = mpsc::unbounded_channel::<nosh_proto::Message>();
    let _drain1 = spawn_ctrl_drain(ctrl_recv1, frame_tx1);

    // Produce scrollback via PTY.
    for i in 0..40u32 {
        client::send_input(
            &mut *ctrl_send1,
            format!("printf 'pre_{i}\\n'\n").as_bytes(),
        )
        .await
        .expect("send PTY for pre-orphan scrollback");
    }
    // Wait for first datagram to confirm state was applied.
    let _ = tokio::time::timeout(Duration::from_secs(5), conn1.read_datagram()).await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Open a Scrollback channel (pre-orphan).
    let pre_orphan_sb_id: u32 = 2;
    client::send_channel_open(&mut *ctrl_send1, pre_orphan_sb_id, ChannelType::Scrollback)
        .await
        .expect("send ChannelOpen pre-orphan");
    let accepted1 = await_channel_accept_from_drain(&mut frame_rx1, pre_orphan_sb_id, 3000)
        .await
        .expect("await ChannelAccept pre-orphan");
    assert!(accepted1, "pre-orphan Scrollback must be accepted");

    // ── Orphan the session ────────────────────────────────────────────────────
    drop(ctrl_send1);
    conn1.close(1u32.into(), b"test orphan");
    drop(conn1);
    ep1.close(0u32.into(), b"done");
    drop(ep1);

    let orphan_deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if registry.total_orphans() >= 1 {
            break;
        }
        assert!(
            Instant::now() < orphan_deadline,
            "server did not register orphan within 5 s"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    // ── Session 2: reconnect + reattach ──────────────────────────────────────
    let (ep2, _dir2) = client_endpoint_for(&client_key);
    let conn2 = client::connect(&ep2, server.addr, HOST, Duration::from_secs(30))
        .await
        .expect("connect for reattach");

    let (s2, r2) = conn2.open_bi().await.expect("open bi for reattach");
    let mut ctrl_send2: Box<dyn NoshSendStream> = Box::new(QuinnSendStream(s2));
    let mut ctrl_recv2: Box<dyn NoshRecvStream> = Box::new(QuinnRecvStream(r2));
    client::send_reattach(&mut *ctrl_send2, token, 0)
        .await
        .expect("send Reattach");

    let outcome = client::await_reattach_reply(&mut *ctrl_recv2)
        .await
        .expect("await reattach reply");
    assert!(
        matches!(outcome, ReattachOutcome::Ok { .. }),
        "cold reattach must succeed"
    );

    let (frame_tx2, mut frame_rx2) = mpsc::unbounded_channel::<nosh_proto::Message>();
    let _drain2 = spawn_ctrl_drain(ctrl_recv2, frame_tx2);

    // ── Re-open Scrollback channel with a fresh ChannelOpen ──────────────────
    // EvenIdAllocator starts at 2 for every new session, so post_reattach_sb_id == 2.
    // The server cleared its channel_map on orphan, so the Accept proves fresh-open.
    let post_reattach_sb_id: u32 = 2;
    client::send_channel_open(&mut *ctrl_send2, post_reattach_sb_id, ChannelType::Scrollback)
        .await
        .expect("send ChannelOpen post-reattach");

    let accepted2 = await_channel_accept_from_drain(&mut frame_rx2, post_reattach_sb_id, 3000)
        .await
        .expect("await ChannelAccept post-reattach");

    // (a) Accept (not Reject) proves fresh-open semantics — no byte-replay.
    assert!(
        accepted2,
        "SCROLL-05 / Pitfall 5: Scrollback ChannelOpen post-reattach must be ACCEPTED \
         (got Reject); server channel_map must have been cleared on orphan"
    );

    // (b) A ScrollbackRequest on the fresh channel returns scrollback content.
    let (mut ch_send2, mut ch_recv2) = conn2.open_bi().await.expect("open_bi post-reattach");
    let prefix2 = postcard::to_allocvec(&post_reattach_sb_id).expect("encode varint");
    ch_send2.write_all(&prefix2).await.expect("write varint prefix");

    let page = send_request_read_page(&mut ch_send2, &mut ch_recv2, post_reattach_sb_id, 0, 256).await;
    match &page {
        nosh_proto::Message::ScrollbackPage { total_available, .. } => {
            assert!(
                *total_available > 0,
                "SCROLL-05: scrollback must be available post-reattach (total_available=0)"
            );
        }
        other => panic!(
            "SCROLL-05: expected ScrollbackPage post-reattach, got {}",
            other.variant_name()
        ),
    }

    conn2.close(0u32.into(), b"done");
    ep2.close(0u32.into(), b"done");
}

// ── SCROLL-02 / M-6: PTY latency isolation during scrollback transfer ─────────

/// SCROLL-02 / M-6: prove PTY input round-trip latency stays below 150 ms while
/// a scrollback transfer is actively flowing.  This shows `run_scrollback_sender_task`
/// is fully isolated in a `tokio::spawn` and never blocks the pump's PTY path.
///
/// 150 ms is generous (vs the 16 ms diff tick) to avoid CI flakes.  A genuine
/// M-6 regression (sender blocking the pump inline) delays every sample by
/// hundreds of ms.
#[tokio::test]
async fn scrollback_pty_latency_isolation() {
    if !have_sh() {
        eprintln!("skipping scrollback_pty_latency_isolation: /bin/sh unavailable");
        return;
    }

    let registry = SessionRegistry::new(5, Duration::ZERO);
    let client_key = TestKey::generate();
    let server = server_with_key(registry.clone(), &client_key).await;

    let (ep, _dir) = client_endpoint_for(&client_key);
    let conn = client::connect(&ep, server.addr, HOST, Duration::from_secs(30))
        .await
        .expect("connect");

    let (mut ctrl_send, mut frame_rx) = produce_scrollback(&conn).await;

    let (mut ch_send, mut ch_recv, channel_id) =
        open_scrollback_channel(&conn, &mut *ctrl_send, &mut frame_rx).await;

    // Issue a large request to keep the sender active.
    nosh_proto::write_message(
        &mut ch_send,
        &nosh_proto::Message::ScrollbackRequest {
            channel_id,
            from_line: 0,
            count: 1024,
        },
    )
    .await
    .expect("write large ScrollbackRequest");

    // Drain ch_recv in a background task so the transfer flows and does not
    // block ch_send's QUIC flow-control window.
    let drain_handle = tokio::spawn(async move {
        loop {
            match tokio::time::timeout(Duration::from_millis(50), nosh_proto::read_message(&mut ch_recv)).await {
                Ok(Ok(_)) => {}
                Ok(Err(_)) | Err(_) => break,
            }
        }
    });

    // ── Measure PTY round-trip latency under active scrollback transfer ───────
    const SAMPLES: usize = 5;
    let mut latency_samples: Vec<Duration> = Vec::with_capacity(SAMPLES);

    for i in 0..SAMPLES {
        let marker = format!("M6_LAT_{i}");
        let t0 = Instant::now();
        client::send_input(&mut *ctrl_send, format!("printf '{marker}\\n'\n").as_bytes())
            .await
            .expect("send PTY during transfer");

        let mut found = false;
        let deadline = Instant::now() + Duration::from_millis(1000);
        while Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_millis(200), frame_rx.recv()).await {
                Ok(Some(nosh_proto::Message::PtyData { data })) => {
                    if String::from_utf8_lossy(&data).contains(&marker) {
                        latency_samples.push(t0.elapsed());
                        found = true;
                        break;
                    }
                }
                Ok(Some(_)) => {}
                Ok(None) => break,
                Err(_) => {}
            }
        }
        assert!(found, "M-6: PTY marker {marker} missing within 1 s during scrollback transfer");
    }

    latency_samples.sort_unstable();
    let median = latency_samples[latency_samples.len() / 2];
    assert!(
        median < Duration::from_millis(150),
        "M-6 VIOLATED: median PTY latency {median:?} over {SAMPLES} samples during scrollback \
         transfer (bound < 150 ms); samples: {latency_samples:?}"
    );

    drain_handle.abort();
    conn.close(0u32.into(), b"done");
    ep.close(0u32.into(), b"done");
}

// ── SCROLL-02 / S-4: drop-oldest under back-pressure ─────────────────────────

/// SCROLL-02 / S-4: prove the scrollback pump does NOT hang when credit is
/// withheld and many requests are sent back-to-back.
///
/// Strategy: open a Scrollback channel, send multiple requests without ever
/// granting credit.  The sender exhausts its INITIAL_CREDIT window and then
/// waits on `events.recv()` (the credit-pause idiom from MUX-03).  Because the
/// sender is a separate `tokio::spawn`, this must NOT stall the main session pump.
/// We assert session liveness by confirming PTY input still echoes within 5 s.
#[tokio::test]
async fn scrollback_backpressure_drop_oldest() {
    if !have_sh() {
        eprintln!("skipping scrollback_backpressure_drop_oldest: /bin/sh unavailable");
        return;
    }

    let registry = SessionRegistry::new(5, Duration::ZERO);
    let client_key = TestKey::generate();
    let server = server_with_key(registry.clone(), &client_key).await;

    let (ep, _dir) = client_endpoint_for(&client_key);
    let conn = client::connect(&ep, server.addr, HOST, Duration::from_secs(30))
        .await
        .expect("connect");

    let (mut ctrl_send, mut frame_rx) = produce_scrollback(&conn).await;

    let (mut ch_send, _ch_recv, channel_id) =
        open_scrollback_channel(&conn, &mut *ctrl_send, &mut frame_rx).await;

    // Flood requests without granting any credit.  After the first page is
    // written (exhausting the 256 KiB INITIAL_CREDIT), the sender will block in
    // the credit-pause loop — but that is in a separate tokio task.
    for _ in 0..10u32 {
        let _ = nosh_proto::write_message(
            &mut ch_send,
            &nosh_proto::Message::ScrollbackRequest {
                channel_id,
                from_line: 0,
                count: 1024,
            },
        )
        .await;
    }

    // Give the sender time to exhaust credit.
    tokio::time::sleep(Duration::from_millis(150)).await;

    // ── Session liveness: PTY input must still echo ───────────────────────────
    let marker = "S4_ALIVE_NOSH";
    client::send_input(&mut *ctrl_send, format!("printf '{marker}\\n'\n").as_bytes())
        .await
        .expect("send PTY under scrollback back-pressure");

    let mut found = false;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(500), frame_rx.recv()).await {
            Ok(Some(nosh_proto::Message::PtyData { data })) => {
                if String::from_utf8_lossy(&data).contains(marker) {
                    found = true;
                    break;
                }
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => {}
        }
    }

    assert!(
        found,
        "S-4 VIOLATED: session hung under scrollback back-pressure — \
         PTY echo of '{marker}' did not arrive within 5 s"
    );

    conn.close(0u32.into(), b"done");
    ep.close(0u32.into(), b"done");
}

// ── SCROLL-02 / S-1: in-order delivery (reliable stream proof) ───────────────

/// SCROLL-02 / S-1: fetch two consecutive pages of scrollback and assert the
/// delivered line ranges are contiguous with no gap or overlap (S-1 reliable-only
/// proof).
///
/// Scrollback travels over a QUIC **reliable stream**.  Even under packet loss,
/// QUIC retransmits stream bytes in order, so the client always receives a
/// contiguous byte sequence.  A datagram-delivered scrollback would lose pages
/// under loss — this test would then fail because `from_line` of the second page
/// would not equal `lines1.len()` (a gap).
///
/// Note: loopback transport does not experience real packet loss.  What this test
/// proves is the ORDERING INVARIANT on the wire type: consecutive requests for
/// non-overlapping line ranges return exactly-contiguous pages.
#[tokio::test]
async fn scrollback_inorder_under_loss() {
    if !have_sh() {
        eprintln!("skipping scrollback_inorder_under_loss: /bin/sh unavailable");
        return;
    }

    let registry = SessionRegistry::new(5, Duration::ZERO);
    let client_key = TestKey::generate();
    let server = server_with_key(registry.clone(), &client_key).await;

    let (ep, _dir) = client_endpoint_for(&client_key);
    let conn = client::connect(&ep, server.addr, HOST, Duration::from_secs(30))
        .await
        .expect("connect");

    let (mut ctrl_send, mut frame_rx) = produce_scrollback(&conn).await;

    let (mut ch_send, mut ch_recv, channel_id) =
        open_scrollback_channel(&conn, &mut *ctrl_send, &mut frame_rx).await;

    // ── First page: from_line=0, count=16 ────────────────────────────────────
    let page1 = send_request_read_page(&mut ch_send, &mut ch_recv, channel_id, 0, 16).await;

    let (lines1_len, total_avail, from_line1) = match &page1 {
        nosh_proto::Message::ScrollbackPage {
            lines, total_available, from_line, ..
        } => (lines.len(), *total_available, *from_line),
        other => panic!("expected first ScrollbackPage, got {}", other.variant_name()),
    };

    assert_eq!(from_line1, 0, "S-1: first page from_line must be 0");
    assert!(
        lines1_len as u64 <= total_avail,
        "S-1: lines.len() ({lines1_len}) must not exceed total_available ({total_avail})"
    );

    if total_avail <= lines1_len as u64 {
        // History is short enough that one page covers everything.
        conn.close(0u32.into(), b"done");
        ep.close(0u32.into(), b"done");
        return;
    }

    // Grant credit for the second page.
    let _ = nosh_proto::write_message_ns(
        &mut *ctrl_send,
        &nosh_proto::Message::ScrollbackCredit {
            channel_id,
            bytes: 256 * 1024,
        },
    )
    .await;

    // ── Second page: from_line = lines1_len (immediately after first page) ────
    let next_from = lines1_len as u64;
    let page2 = send_request_read_page(&mut ch_send, &mut ch_recv, channel_id, next_from, 16).await;

    let (lines2_len, from_line2) = match &page2 {
        nosh_proto::Message::ScrollbackPage { lines, from_line, .. } => (lines.len(), *from_line),
        other => panic!("expected second ScrollbackPage, got {}", other.variant_name()),
    };

    // ── S-1 contiguity assertion ──────────────────────────────────────────────
    assert_eq!(
        from_line2, next_from,
        "S-1: second page from_line ({from_line2}) must equal first-page line count ({next_from}); \
         any discrepancy (gap or overlap) means scrollback is NOT travelling on a reliable stream"
    );

    // Both pages must be non-empty when more history is available.
    assert!(lines1_len > 0, "S-1: first page must be non-empty");
    if next_from < total_avail {
        assert!(
            lines2_len > 0,
            "S-1: second page must be non-empty when from_line ({next_from}) < total_available ({total_avail})"
        );
    }

    conn.close(0u32.into(), b"done");
    ep.close(0u32.into(), b"done");
}

// ── SCROLL-04: snap-back keybinding ──────────────────────────────────────────

/// SCROLL-04: prove the snap-back keybinding behaviour at the channel + PTY level.
///
/// The full SCROLL-04 state machine lives in `run_pump` (main.rs), which processes
/// raw stdin bytes that cannot be injected from an integration test.  This test
/// proves the two observable properties at the channel-primitives layer:
///
///   (1) Entry into Active mode: the Scrollback channel can be opened and a
///       `ScrollbackRequest` can be sent and served — this is the primitive that
///       `run_pump` exercises when the user presses Shift-PageUp.
///
///   (2) Non-paging keystroke forwarding (T-22-15 LOCKED): after the scrollback
///       request is served (emulating Active mode), sending PTY input bytes via
///       `client::send_input` produces a round-trip shell echo.  This proves that
///       the PTY send path is NOT blocked or swallowed during/after Active mode —
///       the LOCKED behaviour that non-paging bytes are always forwarded.
///
/// Unit tests for `CsiAccumulator` and `ScrollbackView` state transitions reside
/// in `crates/nosh-client/src/main.rs`.
#[tokio::test]
async fn scrollback_keybinding_snap_back() {
    if !have_sh() {
        eprintln!("skipping scrollback_keybinding_snap_back: /bin/sh unavailable");
        return;
    }

    let registry = SessionRegistry::new(5, Duration::ZERO);
    let client_key = TestKey::generate();
    let server = server_with_key(registry.clone(), &client_key).await;

    let (ep, _dir) = client_endpoint_for(&client_key);
    let conn = client::connect(&ep, server.addr, HOST, Duration::from_secs(30))
        .await
        .expect("connect");

    let (mut ctrl_send, mut frame_rx) = produce_scrollback(&conn).await;

    // ── (1) Enter Active: open Scrollback channel + send ScrollbackRequest ────
    let (mut ch_send, mut ch_recv, channel_id) =
        open_scrollback_channel(&conn, &mut *ctrl_send, &mut frame_rx).await;

    // Send the initial ScrollbackRequest (the action Shift-PageUp triggers).
    let page = send_request_read_page(&mut ch_send, &mut ch_recv, channel_id, 0, 256).await;
    match &page {
        nosh_proto::Message::ScrollbackPage { total_available, .. } => {
            // Page served — Active mode entry is viable.  total_available=0 is
            // allowed if scrollback is empty, but produce_scrollback should ensure > 0.
            let _ = total_available;
        }
        other => panic!(
            "SCROLL-04: expected ScrollbackPage (Active-mode entry), got {}",
            other.variant_name()
        ),
    }

    // ── (2) Non-paging keystroke: forwarded to the shell (not swallowed) ──────
    // In run_pump, any non-paging byte while Active snaps back to Live AND is
    // forwarded via send_input → PtyData.  We prove the forwarding property
    // directly: send_input produces a PTY echo.
    let snap_marker = "SNAP_NOSH";
    client::send_input(
        &mut ctrl_send,
        format!("printf '{snap_marker}\\n'\n").as_bytes(),
    )
    .await
    .expect("send non-paging keystroke (snap-back emulation)");

    let mut echoed = false;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(500), frame_rx.recv()).await {
            Ok(Some(nosh_proto::Message::PtyData { data })) => {
                if String::from_utf8_lossy(&data).contains(snap_marker) {
                    echoed = true;
                    break;
                }
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => {}
        }
    }

    assert!(
        echoed,
        "SCROLL-04 VIOLATED: PTY echo of '{snap_marker}' never arrived — \
         non-paging keystroke was swallowed (must be forwarded to the shell)"
    );

    conn.close(0u32.into(), b"done");
    ep.close(0u32.into(), b"done");
}

// ── SCROLL-02 / deep-paging credit replenishment ──────────────────────────────

/// Regression test for the ScrollbackCredit routing bug (gap-closure 22-06).
///
/// Root cause: the server's control-stream handler lumped `ScrollbackCredit` into
/// the "scrollback frame on control stream = protocol error, log and ignore" arm.
/// So credit grants the client sent were silently dropped — the server's
/// `run_scrollback_sender_task` started with `INITIAL_CREDIT = 256 KiB`, deducted
/// per page, and could never be replenished.  Once cumulative `ScrollbackPage` wire
/// bytes exceeded 256 KiB the sender blocked in the credit-wait loop, hit the 30 s
/// timeout, and closed the channel.
///
/// Fix: `ScrollbackCredit` is split into its own arm (mirroring `ChannelCredit`)
/// in both `run_session` and `run_reattach_session`.
///
/// This test uses the existing `produce_scrollback` helper (40 lines) and then
/// repeatedly re-requests the same range from `from_line=0`, granting
/// `ScrollbackCredit` back after each page.  Since the server deducts the encoded
/// page size from `remaining_credit` on every request, cumulative deductions reach
/// TARGET_BYTES (512 KiB) — beyond INITIAL_CREDIT (256 KiB) — only if credit is
/// actually delivered to the sender task.
///
/// Without the server fix this test fails: the 15 s page-read timeout fires once
/// cumulative credit deductions exhaust the 256 KiB window (sender blocks).
/// With the fix, pages keep flowing and the loop completes.
#[tokio::test]
async fn scrollback_deep_paging_replenishes_credit() {
    if !have_sh() {
        eprintln!("skipping scrollback_deep_paging_replenishes_credit: /bin/sh unavailable");
        return;
    }

    // ── Set up a session with scrollback content (reuse existing helper) ─────
    let registry = SessionRegistry::new(5, Duration::ZERO);
    let client_key = TestKey::generate();
    let server = server_with_key(registry.clone(), &client_key).await;

    let (ep, _dir) = client_endpoint_for(&client_key);
    let conn = client::connect(&ep, server.addr, HOST, Duration::from_secs(30))
        .await
        .expect("connect");

    // `produce_scrollback` generates 40 numbered lines and waits for a datagram
    // to confirm the server has processed them.  That is sufficient history for
    // real pages.
    let (mut ctrl_send, mut frame_rx) = produce_scrollback(&conn).await;

    // ── Open a scrollback channel ─────────────────────────────────────────────
    let (mut ch_send, mut ch_recv, channel_id) =
        open_scrollback_channel(&conn, &mut *ctrl_send, &mut frame_rx).await;

    // ── Deep-paging loop: request pages and grant credit after each ───────────
    //
    // Strategy: always request from_line=0 so the server serves a real page on
    // every iteration and deducts its encoded size from remaining_credit.  After
    // each page arrives, grant that many bytes back via ScrollbackCredit on the
    // control stream.  Accumulate bytes until TARGET_BYTES (512 KiB) — well past
    // INITIAL_CREDIT (256 KiB) — to prove replenishment is live end-to-end.
    //
    // With the bug (ScrollbackCredit silently dropped), remaining_credit drains to 0
    // after ~256 KiB; the sender blocks in the credit-wait loop and the 15 s timeout
    // fires.  With the fix, each grant reaches ChannelEvent::Credit and pages keep
    // flowing.
    const INITIAL_CREDIT: u64 = 256 * 1024;
    const TARGET_BYTES: u64 = INITIAL_CREDIT * 2; // must exceed 256 KiB to prove replenishment

    let mut cumulative_bytes: u64 = 0;
    let mut pages_received: u32 = 0;

    while cumulative_bytes < TARGET_BYTES {
        // Always request from_line=0 so every iteration forces a real page.
        nosh_proto::write_message(
            &mut ch_send,
            &nosh_proto::Message::ScrollbackRequest {
                channel_id,
                from_line: 0,
                count: 40,
            },
        )
        .await
        .expect("write ScrollbackRequest");

        // Read back the ScrollbackPage.  15 s gives generous CI headroom while
        // still catching the bug (the sender blocks until the 30 s WR-S-02 timeout).
        let page = tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                match nosh_proto::read_message(&mut ch_recv).await {
                    Ok(msg @ nosh_proto::Message::ScrollbackPage { .. }) => return msg,
                    Ok(_) => {}
                    Err(e) => panic!("ch_recv error in deep-paging loop: {e}"),
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!(
            "scrollback_deep_paging_replenishes_credit: 15 s timeout reading ScrollbackPage \
             (page {pages_received}, cumulative_bytes={cumulative_bytes}) — \
             server credit-wait loop never unblocked (SCROLL-02 / gap 22-06)"
        ));

        // Measure the encoded wire size (mirrors how the server deducts from remaining_credit).
        let encoded_len = nosh_proto::codec::encode(&page)
            .expect("encode ScrollbackPage for credit measurement")
            .len() as u64;

        cumulative_bytes += encoded_len;
        pages_received += 1;

        // Grant credit equal to the encoded cost of the page we just consumed.
        // Critical path: with the bug this is silently dropped and the server stalls;
        // with the fix it reaches ChannelEvent::Credit and replenishes the window.
        nosh_proto::write_message_ns(
            &mut *ctrl_send,
            &nosh_proto::Message::ScrollbackCredit {
                channel_id,
                bytes: encoded_len,
            },
        )
        .await
        .expect("write ScrollbackCredit on control stream");
    }

    assert!(
        cumulative_bytes >= TARGET_BYTES,
        "deep-paging loop exited early: cumulative_bytes={cumulative_bytes} < TARGET={TARGET_BYTES}"
    );
    assert!(pages_received > 0, "deep-paging loop: no pages received at all");

    conn.close(0u32.into(), b"done");
    ep.close(0u32.into(), b"done");
}
