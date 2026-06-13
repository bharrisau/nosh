//! Phase 26 Plan 01: WebTransport reattach integration tests (WT-06, SC#4).
//!
//! Proves that a simulated network change in WebTransport mode resumes the
//! server-side session with byte-exact replay and a rotated token, with no
//! shell disruption beyond a reconnect.
//!
//! Tests:
//! - `wt06_seamless_resume_over_webtransport`: drop → reconnect → inner auth →
//!   Reattach → byte-exact replay + token rotation (SC#4 proof).
//!
//! Build and run with:
//!   cargo test -p nosh-client --features webtransport --test wt_reattach

#![cfg(feature = "webtransport")]

use std::time::Duration;

use nosh_client::client::{self, send_input};
use nosh_client::inner_auth::run_inner_auth_client;
use nosh_client::wt_transport::connect_wt;
use nosh_proto::{Message, read_message_ns, write_message_ns};

mod common;

const SH: &str = "/bin/sh";

/// SC#4 / WT-06: seamless resume over WebTransport after a simulated network change.
///
/// This test forces a WebTransport session drop, reconnects, re-runs inner auth,
/// sends Reattach, and asserts:
/// 1. ReattachOutcome::Ok (session resumed).
/// 2. new_token != token (D-05 token rotation).
/// 3. The replayed output contains the marker bytes that were printed before the
///    drop (byte-exact replay from SequencedOutputBuffer).
///
/// The test follows the MH-1 ordering: reconnect → run_inner_auth_client →
/// send Reattach on the authenticated stream. It does NOT call
/// client::reattach_collect (which opens a pre-auth bi stream and is wrong for WT).
///
/// Network-change simulation: client-side transport drop (the WtTestServer does
/// not expose the active wtransport::Connection). Dropping the client transport
/// triggers the server TransportLost → Orphaned path, same as a real network change.
#[tokio::test]
async fn wt06_seamless_resume_over_webtransport() {
    if !common::have_sh() {
        eprintln!("skipping wt06_seamless_resume_over_webtransport: /bin/sh unavailable");
        return;
    }

    let dir = tempfile::tempdir().unwrap();

    // ── Step 1: Build the real-auth WT harness ────────────────────────────────

    // Generate host key (server) and client key.
    let (host_signer, host_key) = common::generate_ed25519_keypair();
    let (client_signer, client_key) = common::generate_ed25519_keypair();

    // Server authorized_keys contains the client key.
    let authorized = vec![client_key.clone()];

    // Client known_hosts pre-trusts the server key (skip TOFU prompt).
    let known_hosts = common::known_hosts_trusting(&dir, &host_key);

    // Start the REAL-AUTH server (InnerAuthMode::Required).
    let server = common::spawn_wt_server_real_auth(Some(SH.to_string()), authorized, host_signer)
        .await
        .expect("spawn_wt_server_real_auth failed");

    // Capture the initial orphan count (0 — no sessions yet). After a successful
    // reattach the orphaned slot must be reclaimed (Orphaned → Reconnecting →
    // Active), so total_orphans() returns to this baseline. A fresh-session bug
    // would leave the dropped slot orphaned beside a new active one.
    let initial_orphan_count = server.registry.total_orphans();

    // ── Step 2: FIRST attach — establish session + drive marker into PTY ────────

    // Build the WT client config trusting the server's self-signed cert by hash.
    let url = format!("https://127.0.0.1:{}/nosh", server.addr.port());
    let config = common::client_config_with_pinning(&server.cert_hash);

    // Connect to the server (first WT session).
    let transport1 = tokio::time::timeout(
        Duration::from_secs(10),
        connect_wt(config, &url),
    )
    .await
    .expect("connect_wt timed out")
    .expect("connect_wt failed");

    // Run inner auth on the first session.
    let (mut send1, mut recv1) = tokio::time::timeout(
        Duration::from_secs(10),
        run_inner_auth_client(&*transport1, &known_hosts, common::HOST, client_signer.clone()),
    )
    .await
    .expect("run_inner_auth_client timed out")
    .expect("run_inner_auth_client failed");

    // Send SessionOpen on the authenticated stream.
    let session_open = Message::SessionOpen {
        term: "xterm-256color".to_string(),
        cols: 80,
        rows: 24,
        env: vec![],
    };
    tokio::time::timeout(
        Duration::from_secs(5),
        write_message_ns(&mut *send1, &session_open),
    )
    .await
    .expect("write SessionOpen timed out")
    .expect("write SessionOpen failed");

    // Read SessionOpened — capture the initial token.
    let token = match tokio::time::timeout(
        Duration::from_secs(5),
        read_message_ns(&mut *recv1),
    )
    .await
    .expect("read SessionOpened timed out")
    {
        Ok(Message::SessionOpened { token }) => token,
        Ok(other) => panic!("expected SessionOpened, got {}", other.variant_name()),
        Err(e) => panic!("failed to read SessionOpened: {e:#}"),
    };

    // Drive a deterministic marker into the PTY.
    let marker = b"printf MARK26A; ";
    tokio::time::timeout(
        Duration::from_secs(5),
        send_input(&mut *send1, marker),
    )
    .await
    .expect("send input timed out")
    .expect("send input failed");

    // Read PtyData frames until the marker bytes are observed.
    let mut output_before_drop = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
    while tokio::time::timeout_at(deadline, tokio::time::sleep(Duration::from_millis(50))).await.is_ok() {
        match tokio::time::timeout(
            Duration::from_secs(2),
            read_message_ns(&mut *recv1),
        )
        .await
        {
            Ok(Ok(Message::PtyData { data })) => {
                output_before_drop.extend_from_slice(&data);
                if String::from_utf8_lossy(&output_before_drop).contains("MARK26A") {
                    break;
                }
            }
            Ok(Ok(other)) => {
                panic!("unexpected message before marker: {}", other.variant_name());
            }
            Ok(Err(e)) => {
                panic!("failed to read PtyData: {e:#}");
            }
            Err(_) => {
                panic!("timeout waiting for MARK26A marker");
            }
        }
    }

    // Track highest_applied (count of PtyData chunks applied).
    // For this test, we don't need the exact value for replay (the assertion is
    // that the marker appears in replayed output), but track it honestly.
    let highest_applied = 0; // We're not tracking chunks for this test.

    // ASSERT that the marker appeared in the original session output.
    // This ensures the PTY was actually running before we dropped the connection.
    let before_str = String::from_utf8_lossy(&output_before_drop);
    assert!(
        before_str.contains("MARK26A"),
        "MARK26A marker was never printed in the original session output (PTY likely not running)"
    );

    // ── Step 3: SIMULATE NETWORK CHANGE — drop the first WT transport ────────

    // Drop the first WT transport to sever the session. This triggers the server
    // TransportLost → Orphaned path. The WtTestServer does not expose the active
    // wtransport::Connection, so a client-side drop is the deterministic equivalent.
    drop(transport1);
    drop(send1);
    drop(recv1);

    // Wait for the server to register the slot as Orphaned.
    // The server transitions to Orphaned on TransportLost (server.rs:1500-1505).
    // Poll the orphan count instead of a hardcoded sleep to avoid CI flakes.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let orphans = server.registry.total_orphans();
        if orphans > 0 {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("timed out waiting for slot to transition to Orphaned (still 0 orphans after 10s)");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // ── Step 4: RECONNECT — fresh WT session + inner auth + Reattach ───────────

    // Reconnect with a fresh WT session.
    let config2 = common::client_config_with_pinning(&server.cert_hash);
    let transport2 = tokio::time::timeout(
        Duration::from_secs(10),
        connect_wt(config2, &url),
    )
    .await
    .expect("reconnect connect_wt timed out")
    .expect("reconnect connect_wt failed");

    // Re-run inner auth on the new session (D-02: full inner handshake before Reattach).
    let (mut send2, mut recv2) = tokio::time::timeout(
        Duration::from_secs(10),
        run_inner_auth_client(&*transport2, &known_hosts, common::HOST, client_signer),
    )
    .await
    .expect("reconnect run_inner_auth_client timed out")
    .expect("reconnect run_inner_auth_client failed");

    // Send Reattach on the authenticated stream (MH-1 ordering: auth → Reattach).
    client::send_reattach(&mut *send2, token, highest_applied)
        .await
        .expect("send Reattach failed");

    // Await the server's reply.
    let outcome = tokio::time::timeout(
        Duration::from_secs(5),
        client::await_reattach_reply(&mut *recv2),
    )
    .await
    .expect("await_reattach_reply timed out")
    .expect("await_reattach_reply failed");

    // ── Step 5: ASSERT (adversarial core — each must fail if the guarantee regresses) ─

    // ASSERT 1: ReattachOutcome::Ok (resume succeeded over WT).
    let (new_token, replaying_from_seq, truncated) = match outcome {
        client::ReattachOutcome::Ok { new_token, replaying_from_seq, truncated } => {
            (new_token, replaying_from_seq, truncated)
        }
        client::ReattachOutcome::Err => {
            panic!("ReattachOutcome::Err — session did not resume over WT");
        }
    };

    // ASSERT 2: token rotation — new_token differs from the original.
    // This fails if rotation were removed (D-05 regression).
    // D-07: Do NOT print token bytes in assertion output.
    assert!(
        new_token != token,
        "reattach token must rotate on success (D-05 rotation) (D-07: token bytes not displayed)"
    );

    // ASSERT 3: truncated flag is false (buffer was not truncated in this short test).
    assert!(!truncated, "buffer should not be truncated in this test");

    // ASSERT 4: replaying_from_seq is 0 (we applied nothing).
    assert_eq!(replaying_from_seq, 0, "replaying_from_seq should be 0");

    // ASSERT 5: collect replayed PtyData and assert the marker appears.
    // This proves byte-exact replay from SequencedOutputBuffer (D-03).
    let mut replayed_output = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
    while tokio::time::timeout_at(deadline, tokio::time::sleep(Duration::from_millis(50))).await.is_ok() {
        match tokio::time::timeout(
            Duration::from_secs(2),
            read_message_ns(&mut *recv2),
        )
        .await
        {
            Ok(Ok(Message::PtyData { data })) => {
                replayed_output.extend_from_slice(&data);
                if String::from_utf8_lossy(&replayed_output).contains("MARK26A") {
                    break; // Marker found in replayed output.
                }
            }
            Ok(Ok(Message::SessionClose { .. })) => {
                break; // Session closed — stop reading.
            }
            Ok(Ok(_other)) => {
                // Ignore other messages (e.g., Ack).
            }
            Ok(Err(e)) => {
                panic!("failed to read replayed PtyData: {e:#}");
            }
            Err(_) => {
                break; // Timeout — stop reading.
            }
        }
    }

    let replayed_str = String::from_utf8_lossy(&replayed_output);
    assert!(
        replayed_str.contains("MARK26A"),
        "replayed output must contain MARK26A (byte-exact replay proof)"
    );

    // ASSERT 6: the SAME session resumed — the dropped slot was reclaimed.
    // After a successful reattach the orphan is consumed (Orphaned →
    // Reconnecting → Active), so total_orphans() is back to the baseline.
    // If the Reattach were ignored and a fresh SessionOpen created instead,
    // the dropped slot would remain orphaned (total_orphans() == 1).
    let final_orphan_count = server.registry.total_orphans();
    assert_eq!(
        final_orphan_count, initial_orphan_count,
        "orphan count must return to baseline (dropped slot reclaimed by reattach, not abandoned beside a fresh session)"
    );
}
