//! Phase 1 transport integration tests — preserved under Phase 2 auth and
//! Phase 3's PTY session (the server no longer runs echo loops, so transport
//! usability is now proven by running a real session over the link, D-08/D-02).
//!
//! Each test spins up an in-process `nosh-server` bound to `127.0.0.1:0`
//! (ephemeral port), runs the accept loop on a background task, then drives a
//! real `nosh-client` connection. Covers TRANS-01..05.

use std::time::Duration;

use nosh_client::client;
use nosh_server::server::AuthLimits;

mod common;
use common::{TestKey, TestServer, HOST};

/// Start an in-process mutually-authenticated server (forcing `/bin/sh` so the
/// session-usability probes are portable) plus the client's keys/trust dir.
struct Harness {
    server: TestServer,
    client_key: TestKey,
    _host_key: TestKey,
    dir: tempfile::TempDir,
}

async fn spawn_server() -> Harness {
    let host_key = TestKey::generate();
    let client_key = TestKey::generate();
    let server = common::spawn_server_with_shell(
        &host_key,
        &[&client_key.public],
        AuthLimits::default(),
        Some("/bin/sh".to_string()),
    )
    .await;
    Harness {
        server,
        client_key,
        _host_key: host_key,
        dir: tempfile::tempdir().unwrap(),
    }
}

async fn connect(h: &Harness) -> (quinn::Endpoint, quinn::Connection) {
    let kh = h.dir.path().join("known_hosts");
    let endpoint =
        common::client_endpoint(h.client_key.client_identity(), kh).expect("client endpoint");
    let conn = client::connect(&endpoint, h.server.addr, HOST)
        .await
        .expect("connect");
    (endpoint, conn)
}

/// TRANS-01: TLS 1.3 handshake completes and the negotiated ALPN is nosh/0.
/// `client::connect` asserts the ALPN internally — a successful connect proves it.
#[tokio::test]
async fn handshake_and_alpn() {
    let h = spawn_server().await;
    let (_ep, conn) = connect(&h).await;
    // If we got here, the handshake succeeded and ALPN matched nosh/0.
    assert!(conn.close_reason().is_none());
}

/// TRANS-02: a reliable bidi stream carries data intact — now proven by running
/// a PTY session over it and getting the shell's output back byte-for-byte.
#[tokio::test]
async fn stream_session_usable() {
    if !common::have_sh() {
        eprintln!("skipping: /bin/sh unavailable");
        return;
    }
    let h = spawn_server().await;
    let (_ep, conn) = connect(&h).await;
    assert!(
        common::session_marker_usable(&conn, "transport-marker-42").await,
        "the reliable stream must carry a usable session (TRANS-02)"
    );
}

/// TRANS-03: datagrams are explicitly enabled (max_datagram_size is Some on the
/// client after the handshake). Datagrams carry no session traffic this
/// milestone (D-02) — the negotiation/enablement is what we assert here.
#[tokio::test]
async fn datagram_enabled() {
    let h = spawn_server().await;
    let (_ep, conn) = connect(&h).await;

    assert!(
        conn.max_datagram_size().is_some(),
        "datagrams must be enabled/negotiated (max_datagram_size Some)"
    );
}

/// TRANS-05 (fast proxy): after a short idle the connection is still alive and a
/// session still runs over it. Runs in the default suite to keep CI fast; the
/// honest 60s proof is `idle_survival_60s` below.
#[tokio::test]
async fn idle_survival_fast() {
    if !common::have_sh() {
        eprintln!("skipping: /bin/sh unavailable");
        return;
    }
    let h = spawn_server().await;
    let (_ep, conn) = connect(&h).await;

    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(
        conn.close_reason().is_none(),
        "connection must survive a short idle"
    );
    // Still usable after idle.
    assert!(
        common::session_marker_usable(&conn, "after-idle-ok").await,
        "session must still run after a short idle (TRANS-05)"
    );
}

/// TRANS-05 (honest): a connection left idle for 60s — longer than the QUIC
/// default 30s idle timeout — does NOT drop, because the client keep-alive
/// (15s) keeps it warm. Ignored by default (slow); run with:
/// `cargo test --workspace -- --ignored`.
#[tokio::test]
#[ignore = "slow: real 60s idle-survival proof; run with --ignored"]
async fn idle_survival_60s() {
    if !common::have_sh() {
        eprintln!("skipping: /bin/sh unavailable");
        return;
    }
    let h = spawn_server().await;
    let (_ep, conn) = connect(&h).await;

    tokio::time::sleep(Duration::from_secs(60)).await;
    assert!(
        conn.close_reason().is_none(),
        "connection must survive 60s idle (keep-alive vs idle timeout)"
    );
    assert!(
        common::session_marker_usable(&conn, "alive-after-60s").await,
        "session must still run after 60s idle (TRANS-05)"
    );
}
