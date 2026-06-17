//! Adversarial repro for the Phase-28 UAT BLOCKER: reliable-stream framing
//! desync → reconnect/reattach storm.
//!
//! Field symptom (Windows client, native QUIC, vim/heavy-TUI):
//!   WARN nosh_client: reliable stream error, triggering reconnect:
//!   frame too large: 1718183741 bytes (max 16777216)
//! 1718183741 == 0x6669673D == ASCII "fig=" — i.e. terminal *payload* bytes were
//! read as a u32-BE frame-length prefix (codec.rs). The length-prefixed reliable
//! stream desynced, producing a bogus ~1.7 GiB length > MAX_FRAME_LEN (16 MiB),
//! which the production `run_pump` turns into a TransportDrop → reconnect →
//! reattach+replay → desync again → loop.
//!
//! The EXISTING coverage (`reattach.rs::reattach_replays_unacked_output_byte_exact`)
//! deliberately produces ALL output BEFORE the first disconnect and stays idle
//! across every orphan→reattach gap (see its lines 138-147), so it exercises only
//! small markers and a quiescent post-reattach stream. It cannot surface a desync
//! that needs (a) a LARGE replay and/or (b) HEAVY continuous output AFTER reattach
//! — which is exactly the operator's vim/htop scenario.
//!
//! These two tests close that gap:
//!   * `steady_state_heavy_output_no_reattach` — heavy OSC-laden continuous output
//!     on a FRESH session (no reattach). Isolates "is framing fragile under heavy
//!     output generally?" from "is the reattach/replay path the culprit?".
//!   * `reattach_then_heavy_steady_state_output` — large buffered replay across an
//!     orphan→reattach cycle, THEN heavy OSC-laden output AFTER reattach. This is
//!     the faithful repro of the operator's path.
//!
//! Pass condition for both: every reliable frame reads back cleanly (no codec
//! error) until a `DONEMARK` sentinel appears in the accumulated PtyData. A
//! framing desync surfaces as `read_message_ns` returning Err (the same Err the
//! production client logs as "frame too large") and fails the test with the
//! captured error + transcript tail.

use std::sync::Arc;
use std::time::{Duration, Instant};

use nosh_client::client::{self, ReattachOutcome};
use nosh_client::quinn_transport::{QuinnTransport, QuinnSendStream, QuinnRecvStream};
use nosh_proto::transport_trait::{NoshSendStream, NoshRecvStream};
use nosh_server::registry::SessionRegistry;

mod common;
use common::{spawn_server_with_registry, TestKey, HOST};

const SH: &str = "/bin/sh";

fn have_sh() -> bool {
    std::path::Path::new(SH).exists()
}

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

fn client_endpoint_for(key: &TestKey) -> (quinn::Endpoint, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let kh = dir.path().join("known_hosts");
    let ep = common::client_endpoint(key.client_identity(), kh).unwrap();
    (ep, dir)
}

/// Heavy, OSC-laden shell loop ending in `DONEMARK`.
///
/// Each iteration sets the window title (OSC 0, `ESC ] 0 ; … BEL`) and prints a
/// line that intentionally contains the literal bytes `fig=` (the exact bytes
/// that desynced in the field: 0x6669673D) plus padding so frames are non-trivial
/// in size. `printf` interprets the octal escapes (`\033` = ESC, `\007` = BEL), so
/// these go onto the wire as real control bytes — mimicking vim/htop title churn.
fn heavy_osc_script(iters: u32) -> String {
    format!(
        "i=0; while [ $i -lt {iters} ]; do \
           printf '\\033]0;ttl%d\\007MARK%06d fig=%d ::::::::::::::::::::::::::::::::::::::::::::::::::::::::::::\\n' $i $i $i; \
           i=$((i+1)); \
         done; printf 'DONEMARK\\n'\n"
    )
}

/// Search `hay` for the byte subsequence `needle`.
fn contains_subseq(hay: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || hay.len() < needle.len() {
        return needle.is_empty();
    }
    hay.windows(needle.len()).any(|w| w == needle)
}

/// Read reliable frames into `acc` until `DONEMARK` appears in accumulated
/// PtyData. ANY codec/transport Err before then is the repro signal and panics
/// with the captured error (the production "frame too large" desync). A stall
/// (no DONEMARK within the deadline) also panics, distinguishing "stalled" from
/// "desynced" so the failure mode is unambiguous.
async fn read_until_done(recv: &mut dyn NoshRecvStream, acc: &mut Vec<u8>, label: &str) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if Instant::now() > deadline {
            panic!(
                "[{label}] timed out before DONEMARK after {} bytes (output stalled, no desync Err). Tail: {:?}",
                acc.len(),
                String::from_utf8_lossy(&acc[acc.len().saturating_sub(200)..])
            );
        }
        match tokio::time::timeout(Duration::from_secs(5), nosh_proto::read_message_ns(recv)).await {
            Ok(Ok(nosh_proto::Message::PtyData { data })) => {
                acc.extend_from_slice(&data);
                if contains_subseq(acc, b"DONEMARK") {
                    return;
                }
            }
            Ok(Ok(_)) => { /* control frames (TerminalControl, etc.) are fine */ }
            Ok(Err(e)) => panic!(
                "[{label}] reliable-stream framing error BEFORE DONEMARK — REPRO of the \
                 FrameTooLarge desync blocker: {e}\nAccumulated {} bytes. Tail: {:?}",
                acc.len(),
                String::from_utf8_lossy(&acc[acc.len().saturating_sub(200)..])
            ),
            Err(_) => { /* idle window; loop and re-check deadline */ }
        }
    }
}

/// ISOLATION SCENARIO (no reattach): heavy OSC-laden continuous output on a fresh
/// session must read back frame-clean. If THIS fails, the desync is in the
/// general framing path, not reattach/replay.
#[tokio::test]
async fn steady_state_heavy_output_no_reattach() {
    if !have_sh() {
        eprintln!("skipping steady_state_heavy_output_no_reattach: /bin/sh unavailable");
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
    let (mut send, mut recv, _token) =
        client::open_session_with_token(&qt, "xterm-256color".to_string(), 270, 72, vec![])
            .await
            .expect("open_session_with_token");

    client::send_input(&mut send, heavy_osc_script(400).as_bytes())
        .await
        .expect("send heavy script");

    let mut acc = Vec::<u8>::new();
    read_until_done(&mut *recv, &mut acc, "no-reattach").await;

    assert!(
        contains_subseq(&acc, b"DONEMARK"),
        "expected DONEMARK in accumulated output"
    );
    assert!(
        contains_subseq(&acc, b"fig="),
        "expected the field byte-pattern 'fig=' to have traversed the reliable stream"
    );

    drop(send);
    drop(recv);
    conn.close(0u32.into(), b"done");
    ep.close(0u32.into(), b"done");
}

/// FAITHFUL REPRO: build up a LARGE un-acked replay buffer, drop → orphan →
/// reattach (forcing a heavy replay), THEN drive heavy OSC-laden output AFTER
/// reattach. Reads every reliable frame and asserts no desync until DONEMARK.
#[tokio::test]
async fn reattach_then_heavy_steady_state_output() {
    if !have_sh() {
        eprintln!("skipping reattach_then_heavy_steady_state_output: /bin/sh unavailable");
        return;
    }

    let registry = SessionRegistry::new(5, Duration::ZERO);
    let client_key = TestKey::generate();
    let server = server_with_key(registry.clone(), &client_key).await;

    // ── Fresh session: emit a heavy pre-disconnect burst, then block on read so
    //    the shell stays alive across the orphan gap. We deliberately do NOT
    //    drain it, so the whole burst is un-acked and must be REPLAYED on
    //    reattach (large replay = the stressed path).
    let (ep, _dir) = client_endpoint_for(&client_key);
    let conn = client::connect(&ep, server.addr, HOST, Duration::from_secs(30))
        .await
        .expect("connect");
    let qt = QuinnTransport(conn.clone());
    let (mut send, recv, mut token) =
        client::open_session_with_token(&qt, "xterm-256color".to_string(), 270, 72, vec![])
            .await
            .expect("open_session_with_token");

    // Pre-disconnect burst: 150 heavy OSC lines, then `read` keeps the shell alive
    // WITHOUT emitting a DONEMARK yet (so DONEMARK only appears post-reattach).
    let pre_burst = format!(
        "i=0; while [ $i -lt 150 ]; do \
           printf '\\033]0;pre%d\\007PRE%06d fig=%d ----------------------------------------------------------\\n' $i $i $i; \
           i=$((i+1)); \
         done; read _x\n"
    );
    client::send_input(&mut send, pre_burst.as_bytes())
        .await
        .expect("send pre-burst");

    // Let the shell produce the whole burst server-side (buffered, un-acked).
    tokio::time::sleep(Duration::from_millis(500)).await;

    // ── Abrupt drop (no Ack, no SessionClose) → orphan with a large un-applied tail.
    conn.close(1u32.into(), b"test transport loss");
    drop(send);
    drop(recv);
    drop(conn);
    ep.close(0u32.into(), b"done");
    drop(ep);

    let orphan_deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if registry.total_orphans() >= 1 {
            break;
        }
        if Instant::now() > orphan_deadline {
            panic!("server did not orphan within 5s");
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    // ── Reattach. last_acked_seq = 0 (we applied nothing), so the server replays
    //    the entire buffered burst over the reliable stream.
    let (ep2, _dir2) = client_endpoint_for(&client_key);
    let conn2 = client::connect(&ep2, server.addr, HOST, Duration::from_secs(30))
        .await
        .expect("reconnect");
    let (send2_quinn, recv2_quinn) = conn2.open_bi().await.expect("open bi");
    let mut send2: Box<dyn NoshSendStream> = Box::new(QuinnSendStream(send2_quinn));
    let mut recv2: Box<dyn NoshRecvStream> = Box::new(QuinnRecvStream(recv2_quinn));

    client::send_reattach(&mut *send2, token, 0)
        .await
        .expect("send reattach");
    let outcome = client::await_reattach_reply(&mut *recv2)
        .await
        .expect("await_reattach_reply");
    match outcome {
        ReattachOutcome::Ok { new_token, .. } => {
            token = new_token;
        }
        ReattachOutcome::Err => panic!("reattach must succeed"),
    }
    let _ = token; // not used past here

    // ── Drive HEAVY NEW OSC-laden output AFTER reattach. First unblock the
    //    pre-burst `read`, then run the heavy loop ending in DONEMARK. Reads
    //    flow over the SAME reattached recv stream that just carried the replay,
    //    so a replay→steady-state framing seam shows up here.
    client::send_input(&mut *send2, b"\n")
        .await
        .expect("unblock read");
    client::send_input(&mut *send2, heavy_osc_script(300).as_bytes())
        .await
        .expect("send post-reattach heavy script");

    let mut acc = Vec::<u8>::new();
    read_until_done(&mut *recv2, &mut acc, "post-reattach").await;

    assert!(
        contains_subseq(&acc, b"DONEMARK"),
        "expected DONEMARK after reattach + heavy output"
    );

    drop(send2);
    drop(recv2);
    conn2.close(0u32.into(), b"done");
    ep2.close(0u32.into(), b"done");
}
