//! Phase 21 channel-multiplexing integration tests — Roadmap success criteria
//! SC#2–SC#6 for MUX-01…MUX-05.
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
use nosh_proto::messages::ChannelType;
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
    recv: &mut quinn::RecvStream,
    timeout_ms: u64,
) -> anyhow::Result<nosh_proto::Message> {
    let deadline = Duration::from_millis(timeout_ms);
    loop {
        let msg = tokio::time::timeout(deadline, nosh_proto::read_message(recv))
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
    mut ctrl_recv: quinn::RecvStream,
    frame_tx: mpsc::UnboundedSender<nosh_proto::Message>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match nosh_proto::read_message(&mut ctrl_recv).await {
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

    let (mut ctrl_send, mut ctrl_recv, _token) =
        client::open_session_with_token(&conn, "xterm".to_string(), 80, 24, vec![])
            .await
            .expect("open session");

    // ── Echo channel: expect ChannelAccept (test/test-support builds) ─────────
    // MUX-01: the ACCEPT must arrive on the control stream BEFORE any bidi data
    // stream is opened by the client.
    let echo_id: u32 = 2;
    client::send_channel_open(&mut ctrl_send, echo_id, ChannelType::Echo)
        .await
        .expect("send ChannelOpen for Echo");
    let reply = recv_channel_reply(&mut ctrl_recv, 2000)
        .await
        .expect("recv channel reply for Echo");
    assert!(
        matches!(reply, nosh_proto::Message::ChannelAccept { channel_id } if channel_id == echo_id),
        "Echo ChannelOpen must yield ChannelAccept (id {echo_id}); got {}",
        reply.variant_name()
    );

    // ── PortForward: opaque ChannelReject ─────────────────────────────────────
    let pf_id: u32 = 4;
    client::send_channel_open(&mut ctrl_send, pf_id, ChannelType::PortForward)
        .await
        .expect("send ChannelOpen for PortForward");
    let pf_reply = recv_channel_reply(&mut ctrl_recv, 2000)
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
    client::send_channel_open(&mut ctrl_send, af_id, ChannelType::AgentForward)
        .await
        .expect("send ChannelOpen for AgentForward");
    let af_reply = recv_channel_reply(&mut ctrl_recv, 2000)
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
    client::send_channel_open(&mut ctrl_send, sb_id, ChannelType::Scrollback)
        .await
        .expect("send ChannelOpen for Scrollback");
    let sb_reply = recv_channel_reply(&mut ctrl_recv, 2000)
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

    let (mut ctrl_send, ctrl_recv, _token) =
        client::open_session_with_token(&conn, "xterm".to_string(), 80, 24, vec![])
            .await
            .expect("open session");

    // Spawn ctrl drain BEFORE sending ChannelOpen so PtyData and ChannelAccept
    // can arrive interleaved without blocking the server pump.
    let (frame_tx, mut frame_rx) = mpsc::unbounded_channel::<nosh_proto::Message>();
    let _drain = spawn_ctrl_drain(ctrl_recv, frame_tx);

    // ── Open Echo channel ─────────────────────────────────────────────────────
    let echo_id: u32 = 2;
    client::send_channel_open(&mut ctrl_send, echo_id, ChannelType::Echo)
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
        client::send_input(&mut ctrl_send, format!("printf '{marker}\\n'\n").as_bytes())
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

    let (mut ctrl_send, ctrl_recv, _token) =
        client::open_session_with_token(&conn, "xterm".to_string(), 80, 24, vec![])
            .await
            .expect("open session");

    // Spawn ctrl drain.
    let (frame_tx, mut frame_rx) = mpsc::unbounded_channel::<nosh_proto::Message>();
    let _drain = spawn_ctrl_drain(ctrl_recv, frame_tx);

    // Open an Echo channel.
    let echo_id: u32 = 2;
    client::send_channel_open(&mut ctrl_send, echo_id, ChannelType::Echo)
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
                    nosh_proto::write_message(
                        &mut ctrl_send,
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
        nosh_proto::write_message(
            &mut ctrl_send,
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

    let (mut ctrl_send, ctrl_recv, _token) =
        client::open_session_with_token(&conn, "xterm".to_string(), 80, 24, vec![])
            .await
            .expect("open session");

    // Spawn ctrl drain — keeps server pump from blocking on ctrl_send.
    let (frame_tx, mut frame_rx) = mpsc::unbounded_channel::<nosh_proto::Message>();
    let _drain = spawn_ctrl_drain(ctrl_recv, frame_tx);

    // Open an Echo channel.
    let echo_id: u32 = 2;
    client::send_channel_open(&mut ctrl_send, echo_id, ChannelType::Echo)
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
    nosh_proto::write_message(
        &mut ctrl_send,
        &nosh_proto::Message::ChannelClose { channel_id: echo_id },
    )
    .await
    .expect("send second ChannelClose for same id");

    // ── ChannelAccept for unknown odd id: no-op ───────────────────────────────
    // Channel id 9999 is odd → the server handles it as a client reply to a
    // server-initiated open (under the test-support feature gate). Since no
    // server-initiated open with id 9999 was issued, this is a logged no-op
    // (T-21-07 no-panic rule).
    nosh_proto::write_message(
        &mut ctrl_send,
        &nosh_proto::Message::ChannelAccept { channel_id: 9999 },
    )
    .await
    .expect("send ChannelAccept for unknown odd id");

    tokio::time::sleep(Duration::from_millis(50)).await;

    // ── Session survival: control stream must still accept writes ─────────────
    let alive = nosh_proto::write_message(
        &mut ctrl_send,
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

    let (mut ctrl_send, ctrl_recv, _token) =
        client::open_session_with_token(&conn, "xterm".to_string(), 80, 24, vec![])
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
    client::send_channel_open(&mut ctrl_send, client_id, ChannelType::Echo)
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
                nosh_proto::write_message(
                    &mut ctrl_send,
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
    let alive = nosh_proto::write_message(
        &mut ctrl_send,
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

    let (mut ctrl_send1, ctrl_recv1, token) =
        client::open_session_with_token(&conn1, "xterm".to_string(), 80, 24, vec![])
            .await
            .expect("open session 1");

    let (frame_tx1, mut frame_rx1) = mpsc::unbounded_channel::<nosh_proto::Message>();
    let _drain1 = spawn_ctrl_drain(ctrl_recv1, frame_tx1);

    let echo_id: u32 = 2;
    client::send_channel_open(&mut ctrl_send1, echo_id, ChannelType::Echo)
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

    let (mut ctrl_send2, mut ctrl_recv2) = conn2.open_bi().await.expect("open bi for reattach");
    client::send_reattach(&mut ctrl_send2, token, 0)
        .await
        .expect("send Reattach");

    // await_reattach_reply reads directly from ctrl_recv2 before the drain is started.
    let outcome = client::await_reattach_reply(&mut ctrl_recv2)
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
    client::send_channel_open(&mut ctrl_send2, echo_id, ChannelType::Echo)
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
