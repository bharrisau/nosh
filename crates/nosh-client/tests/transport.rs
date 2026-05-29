//! Phase 1 transport integration tests — the verification gate (D-08).
//!
//! Each test spins up an in-process `nosh-server` bound to `127.0.0.1:0`
//! (ephemeral port, avoids CI collisions), runs the accept loop on a background
//! task, then drives a real `nosh-client` connection. Covers TRANS-01..05.

use std::time::Duration;

use bytes::Bytes;
use nosh_client::client;
use nosh_server::server::AuthLimits;

mod common;
use common::{TestKey, TestServer, HOST};

/// Start an in-process mutually-authenticated server; return the test server
/// handle plus the client's keys/trust dir so the transport proofs run over a
/// real authenticated link (Phase 1 proofs preserved under Phase 2 auth).
struct Harness {
    server: TestServer,
    client_key: TestKey,
    _host_key: TestKey,
    dir: tempfile::TempDir,
}

async fn spawn_server() -> Harness {
    let host_key = TestKey::generate();
    let client_key = TestKey::generate();
    let server = common::spawn_server(&host_key, &[&client_key.public], AuthLimits::default()).await;
    Harness {
        server,
        client_key,
        _host_key: host_key,
        dir: tempfile::tempdir().unwrap(),
    }
}

async fn connect(h: &Harness) -> (quinn::Endpoint, quinn::Connection) {
    let kh = h.dir.path().join("known_hosts");
    let endpoint = common::client_endpoint(h.client_key.client_identity(), kh).expect("client endpoint");
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

/// TRANS-02: bytes echoed over a reliable bidi stream arrive intact.
#[tokio::test]
async fn stream_echo_intact() {
    let h = spawn_server().await;
    let (_ep, conn) = connect(&h).await;

    let payload = b"the quick brown fox jumps over the lazy dog";
    let echoed = client::stream_echo_roundtrip(&conn, payload)
        .await
        .expect("stream round-trip");
    assert_eq!(echoed, payload, "stream echo must be byte-identical");
}

/// TRANS-03: datagrams are explicitly enabled (max_datagram_size is Some on
/// the client) and a datagram round-trips intact.
#[tokio::test]
async fn datagram_roundtrip_enabled() {
    let h = spawn_server().await;
    let (_ep, conn) = connect(&h).await;

    assert!(
        conn.max_datagram_size().is_some(),
        "datagrams must be enabled (max_datagram_size Some)"
    );
    let payload = Bytes::from_static(b"datagram-payload-123");
    let echoed = client::datagram_roundtrip(&conn, payload.clone())
        .await
        .expect("datagram round-trip");
    assert_eq!(echoed, payload, "datagram echo must be identical");
}

/// TRANS-04: a stream echo and datagram round-trip run concurrently without
/// interfering with each other.
#[tokio::test]
async fn stream_and_datagram_coexist() {
    let h = spawn_server().await;
    let (_ep, conn) = connect(&h).await;

    client::concurrent_roundtrip(&conn)
        .await
        .expect("concurrent stream + datagram round-trip");
}

/// TRANS-05 (fast proxy): after a short idle the connection is still alive and
/// usable. Runs in the default suite to keep CI fast; the honest 60s proof is
/// `idle_survival_60s` below.
#[tokio::test]
async fn idle_survival_fast() {
    let h = spawn_server().await;
    let (_ep, conn) = connect(&h).await;

    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(
        conn.close_reason().is_none(),
        "connection must survive a short idle"
    );
    // Still usable after idle.
    let echoed = client::stream_echo_roundtrip(&conn, b"after-idle")
        .await
        .expect("stream after idle");
    assert_eq!(echoed, b"after-idle");
}

/// TRANS-05 (honest): a connection left idle for 60s — longer than the QUIC
/// default 30s idle timeout — does NOT drop, because the client keep-alive
/// (15s) keeps it warm. Ignored by default (slow); run with:
/// `cargo test --workspace -- --ignored`.
#[tokio::test]
#[ignore = "slow: real 60s idle-survival proof; run with --ignored"]
async fn idle_survival_60s() {
    let h = spawn_server().await;
    let (_ep, conn) = connect(&h).await;

    tokio::time::sleep(Duration::from_secs(60)).await;
    assert!(
        conn.close_reason().is_none(),
        "connection must survive 60s idle (keep-alive vs idle timeout)"
    );
    let echoed = client::stream_echo_roundtrip(&conn, b"alive-after-60s")
        .await
        .expect("stream after 60s idle");
    assert_eq!(echoed, b"alive-after-60s");
}
