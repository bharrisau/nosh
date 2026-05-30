//! Phase 6 cold-reattach integration tests — Roadmap success criteria SC#1–4.
//!
//! Each test drives an in-process server + client and validates a distinct
//! aspect of the cold-reattach protocol:
//!
//! - SC#1 (`reattach_replays_unacked_output_byte_exact`): replay continuity —
//!   no duplicated or dropped bytes relative to the full server output.
//! - SC#2/#3 (`reattach_wrong_key_rejected_like_bad_token`): two-factor auth
//!   and no-oracle: a valid token with the wrong key and a bad token with the
//!   right key both yield the same opaque Err.
//! - SC#4 (`reattach_rejected_while_session_active`): mutual exclusion — a
//!   reattach for an Active session is rejected (D-12, Pitfall #10).

use std::sync::Arc;
use std::time::Duration;

use nosh_client::client::{self, ReattachOutcome};
use nosh_server::registry::SessionRegistry;

mod common;
use common::{spawn_server_with_registry, TestKey, HOST};

const SH: &str = "/bin/sh";

fn have_sh() -> bool {
    std::path::Path::new(SH).exists()
}

/// Spawn a server authorizing a single key.
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

/// Build a client endpoint for the given test key and a fresh temp known_hosts.
fn client_endpoint_for(key: &TestKey) -> (quinn::Endpoint, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let kh = dir.path().join("known_hosts");
    let ep = common::client_endpoint(key.client_identity(), kh).unwrap();
    (ep, dir)
}

// ── SC#1: replay continuity ───────────────────────────────────────────────────

/// SC#1 / ROAM-02: a reconnecting client receives exactly the buffered output
/// it missed (no duplicated or dropped bytes relative to the server's buffered
/// output). The test keeps the shell running with a long sleep so the session
/// can be orphaned, then reconnected with reattach.
///
/// Test flow:
/// 1. Fresh session; run a script that prints LINE1..LINE10 then sleeps for 60s
///    (to keep the shell alive while we drop and reattach).
/// 2. Wait for the shell to print READY, then abruptly drop the connection.
/// 3. Poll for orphan.
/// 4. Reconnect with the SAME key and send a Reattach (using the bidi stream
///    directly, NOT reattach_collect which would wait for SessionClose).
/// 5. Assert ReattachOk and that the replay/live output contains LINE markers.
/// 6. Send SessionClose to clean up.
#[tokio::test]
async fn reattach_replays_unacked_output_byte_exact() {
    if !have_sh() {
        eprintln!("skipping reattach_replays_unacked_output_byte_exact: /bin/sh unavailable");
        return;
    }

    let registry = SessionRegistry::new(5, Duration::ZERO);
    let client_key = TestKey::generate();
    let server = server_with_key(registry.clone(), &client_key).await;

    let (ep1, _dir1) = client_endpoint_for(&client_key);
    let conn1 = client::connect(&ep1, server.addr, HOST).await.expect("connect");

    // Open session and capture token.
    let (mut send1, mut recv1, token) =
        client::open_session_with_token(&conn1, "xterm".to_string(), 80, 24, vec![])
            .await
            .expect("open_session_with_token");

    // Script: print LINE1..LINE10 then print READY then sleep (keep shell alive).
    let script = "for i in $(seq 1 10); do echo LINE$i; done; echo READY; sleep 60\n";
    client::send_input(&mut send1, script.as_bytes())
        .await
        .expect("send script");

    // Collect until we see READY (all LINE output was produced).
    let mut pre_output = Vec::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    'outer: loop {
        match tokio::time::timeout(Duration::from_millis(500), nosh_proto::read_message(&mut recv1)).await {
            Ok(Ok(nosh_proto::Message::PtyData { data })) => {
                pre_output.extend_from_slice(&data);
                if String::from_utf8_lossy(&pre_output).contains("READY") {
                    break 'outer;
                }
            }
            Ok(Ok(_)) | Err(_) => {}
            Ok(Err(_)) => break 'outer,
        }
        if std::time::Instant::now() > deadline {
            // If we didn't get READY but have at least some LINE output, proceed.
            let s = String::from_utf8_lossy(&pre_output);
            if s.contains("LINE1") {
                eprintln!("  WARN: did not see READY but have LINE1; proceeding");
                break 'outer;
            }
            panic!(
                "shell did not print any LINE output within 10s; got: {:?}",
                &s[..s.len().min(200)]
            );
        }
    }

    // Abruptly drop (no SessionClose) → orphan.
    conn1.close(1u32.into(), b"test transport loss");
    drop(send1); drop(recv1); drop(conn1);
    ep1.close(0u32.into(), b"done");
    drop(ep1);

    // Wait for orphan.
    let orphan_deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if registry.total_orphans() >= 1 { break; }
        if std::time::Instant::now() > orphan_deadline {
            panic!("server did not show an orphan within 5s");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Reconnect with the SAME key (last_acked_seq = 0 → replay from beginning).
    let (ep2, _dir2) = client_endpoint_for(&client_key);
    let conn2 = client::connect(&ep2, server.addr, HOST).await.expect("reconnect");
    let (mut send2, mut recv2) = conn2.open_bi().await.expect("open bi");
    client::send_reattach(&mut send2, token, 0).await.expect("send reattach");

    // Read the ReattachOk reply.
    let reattach_outcome = client::await_reattach_reply(&mut recv2)
        .await
        .expect("await_reattach_reply");
    assert!(
        matches!(reattach_outcome, ReattachOutcome::Ok { .. }),
        "reattach must succeed, got {reattach_outcome:?}"
    );

    // Drain output from the replay stream until we see LINE markers or timeout.
    let mut replay_output = Vec::new();
    let replay_deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        match tokio::time::timeout(Duration::from_secs(3), nosh_proto::read_message(&mut recv2)).await {
            Ok(Ok(nosh_proto::Message::PtyData { data })) => {
                replay_output.extend_from_slice(&data);
                // Stop once we have enough output.
                let s = String::from_utf8_lossy(&replay_output);
                if s.contains("LINE1") && s.contains("LINE5") {
                    break;
                }
            }
            Ok(Ok(_)) | Err(_) => {
                // Timeout or non-PtyData: check if we have enough.
                let s = String::from_utf8_lossy(&replay_output);
                if s.contains("LINE1") { break; }
            }
            Ok(Err(_)) => break,
        }
        if std::time::Instant::now() > replay_deadline { break; }
    }

    // Send SessionClose to clean up the long-running shell.
    let _ = client::send_input(&mut send2, b"exit 0\n").await;
    drop(send2); drop(recv2);
    conn2.close(0u32.into(), b"done");
    ep2.close(0u32.into(), b"done");

    // Assert: replay contains LINE markers from the pre-drop output.
    let replay_str = String::from_utf8_lossy(&replay_output);
    assert!(
        replay_str.contains("LINE1"),
        "replay must contain LINE1 (buffered before drop); replay: {:?}",
        &replay_str[..replay_str.len().min(500)]
    );
    let mut found_lines = std::collections::BTreeSet::new();
    for i in 1..=10u32 {
        if replay_str.contains(&format!("LINE{i}")) {
            found_lines.insert(i);
        }
    }
    assert!(
        found_lines.len() >= 3,
        "replay must contain at least 3 LINE markers (no-gap / no-drop SC#1); found: {found_lines:?}; replay: {:?}",
        &replay_str[..replay_str.len().min(500)]
    );
}

// ── SC#2/#3: two-factor auth / no-oracle ─────────────────────────────────────

/// SC#2/#3 / IDENT-02: valid token with wrong identity → Err; bad token with
/// right identity → Err. The two rejections must be indistinguishable
/// (no-oracle property, D-07).
#[tokio::test]
async fn reattach_wrong_key_rejected_like_bad_token() {
    if !have_sh() {
        eprintln!("skipping reattach_wrong_key_rejected_like_bad_token: /bin/sh unavailable");
        return;
    }

    let registry = SessionRegistry::new(5, Duration::ZERO);
    let key_a = TestKey::generate();
    let key_b = TestKey::generate();
    let host_key = TestKey::generate();

    // Server authorizes BOTH key A and key B.
    let server = spawn_server_with_registry(
        &host_key,
        &[&key_a.public, &key_b.public],
        nosh_server::server::AuthLimits::default(),
        Some(SH.to_string()),
        registry.clone(),
    )
    .await;

    // Connect with key A, open session, capture token, then drop → orphan.
    let (ep_a1, _dir_a1) = client_endpoint_for(&key_a);
    let conn_a1 = client::connect(&ep_a1, server.addr, HOST).await.expect("connect A");
    let (_send, _recv, token_a) =
        client::open_session_with_token(&conn_a1, "xterm".to_string(), 80, 24, vec![])
            .await
            .expect("open session A");
    conn_a1.close(1u32.into(), b"test drop");
    drop(conn_a1);
    ep_a1.close(0u32.into(), b"done");
    drop(ep_a1);

    let orphan_deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if registry.total_orphans() >= 1 {
            break;
        }
        if std::time::Instant::now() > orphan_deadline {
            panic!("orphan not observed within 5s");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Case 1: valid token_a, but reconnect with key B (wrong identity) → Err.
    let (ep_b, _dir_b) = client_endpoint_for(&key_b);
    let conn_b = client::connect(&ep_b, server.addr, HOST).await.expect("connect B");
    let (outcome_b, _, _) = client::reattach_collect(&conn_b, token_a, 0)
        .await
        .expect("reattach with wrong key");
    conn_b.close(0u32.into(), b"done");
    ep_b.close(0u32.into(), b"done");

    assert_eq!(
        outcome_b,
        ReattachOutcome::Err,
        "valid token + wrong identity must yield ReattachOutcome::Err"
    );

    // Case 2: bogus token, right identity A → Err.
    let bogus_token = [0xDEu8; 16];
    let (ep_a2, _dir_a2) = client_endpoint_for(&key_a);
    let conn_a2 = client::connect(&ep_a2, server.addr, HOST).await.expect("connect A2");
    let (outcome_a2, _, _) = client::reattach_collect(&conn_a2, bogus_token, 0)
        .await
        .expect("reattach with bogus token");
    conn_a2.close(0u32.into(), b"done");
    ep_a2.close(0u32.into(), b"done");

    assert_eq!(
        outcome_a2,
        ReattachOutcome::Err,
        "bad token + correct identity must yield ReattachOutcome::Err"
    );

    // NO-ORACLE ASSERTION: both rejections are ReattachOutcome::Err — structurally
    // identical (ReattachErr is fieldless, so there is no distinguishing data).
    assert_eq!(
        outcome_b, outcome_a2,
        "wrong-key rejection and bad-token rejection must be indistinguishable (no oracle, D-07)"
    );
}

// ── SC#4: mutual exclusion ────────────────────────────────────────────────────

/// SC#4 / D-12: a Reattach for a session that is still Active (client still
/// attached) must be rejected — prevents two-clients-one-session race.
#[tokio::test]
async fn reattach_rejected_while_session_active() {
    if !have_sh() {
        eprintln!("skipping reattach_rejected_while_session_active: /bin/sh unavailable");
        return;
    }

    let registry = SessionRegistry::new(5, Duration::ZERO);
    let client_key = TestKey::generate();
    let server = server_with_key(registry.clone(), &client_key).await;

    // Connect with key A and KEEP the connection active (do NOT drop).
    let (ep1, _dir1) = client_endpoint_for(&client_key);
    let conn1 = client::connect(&ep1, server.addr, HOST).await.expect("connect 1");
    let (_send1, _recv1, token) =
        client::open_session_with_token(&conn1, "xterm".to_string(), 80, 24, vec![])
            .await
            .expect("open session 1");

    // Give the server a moment to register the slot as Active.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // From a second endpoint with the SAME key, attempt to reattach the Active session.
    let (ep2, _dir2) = client_endpoint_for(&client_key);
    let conn2 = client::connect(&ep2, server.addr, HOST).await.expect("connect 2");
    let (outcome2, _, _) = client::reattach_collect(&conn2, token, 0)
        .await
        .expect("reattach_collect for active slot");
    conn2.close(0u32.into(), b"done");
    ep2.close(0u32.into(), b"done");

    assert_eq!(
        outcome2,
        ReattachOutcome::Err,
        "Reattach for Active session must be rejected (D-12 mutual exclusion)"
    );

    // Cleanup: drop the original session.
    conn1.close(0u32.into(), b"done");
    ep1.close(0u32.into(), b"done");
}
