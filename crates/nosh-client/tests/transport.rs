//! Phase 1 transport integration tests — the verification gate (D-08).
//!
//! Each test spins up an in-process `nosh-server` bound to `127.0.0.1:0`
//! (ephemeral port, avoids CI collisions), runs the accept loop on a background
//! task, then drives a real `nosh-client` connection. Covers TRANS-01..05.

use std::net::SocketAddr;
use std::time::Duration;

use bytes::Bytes;
use nosh_client::client;

/// Start an in-process server on an ephemeral loopback port; return its
/// address and the accept-loop task handle (aborted on drop by the caller).
async fn spawn_server() -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let bind: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let endpoint = nosh_server::make_endpoint(bind).expect("server endpoint");
    let addr = endpoint.local_addr().expect("server local_addr");
    let handle = tokio::spawn(async move {
        let _ = nosh_server::run_accept_loop(endpoint).await;
    });
    (addr, handle)
}

async fn connect(addr: SocketAddr) -> (quinn::Endpoint, quinn::Connection) {
    let endpoint = client::make_endpoint().expect("client endpoint");
    let conn = client::connect(&endpoint, addr).await.expect("connect");
    (endpoint, conn)
}

/// TRANS-01: TLS 1.3 handshake completes and the negotiated ALPN is nosh/0.
/// `client::connect` asserts the ALPN internally — a successful connect proves it.
#[tokio::test]
async fn handshake_and_alpn() {
    let (addr, server) = spawn_server().await;
    let (_ep, conn) = connect(addr).await;
    // If we got here, the handshake succeeded and ALPN matched nosh/0.
    assert!(conn.close_reason().is_none());
    server.abort();
}

/// TRANS-02: bytes echoed over a reliable bidi stream arrive intact.
#[tokio::test]
async fn stream_echo_intact() {
    let (addr, server) = spawn_server().await;
    let (_ep, conn) = connect(addr).await;

    let payload = b"the quick brown fox jumps over the lazy dog";
    let echoed = client::stream_echo_roundtrip(&conn, payload)
        .await
        .expect("stream round-trip");
    assert_eq!(echoed, payload, "stream echo must be byte-identical");
    server.abort();
}

/// TRANS-03: datagrams are explicitly enabled (max_datagram_size is Some on
/// the client) and a datagram round-trips intact.
#[tokio::test]
async fn datagram_roundtrip_enabled() {
    let (addr, server) = spawn_server().await;
    let (_ep, conn) = connect(addr).await;

    assert!(
        conn.max_datagram_size().is_some(),
        "datagrams must be enabled (max_datagram_size Some)"
    );
    let payload = Bytes::from_static(b"datagram-payload-123");
    let echoed = client::datagram_roundtrip(&conn, payload.clone())
        .await
        .expect("datagram round-trip");
    assert_eq!(echoed, payload, "datagram echo must be identical");
    server.abort();
}

/// TRANS-04: a stream echo and datagram round-trip run concurrently without
/// interfering with each other.
#[tokio::test]
async fn stream_and_datagram_coexist() {
    let (addr, server) = spawn_server().await;
    let (_ep, conn) = connect(addr).await;

    client::concurrent_roundtrip(&conn)
        .await
        .expect("concurrent stream + datagram round-trip");
    server.abort();
}

/// TRANS-05 (fast proxy): after a short idle the connection is still alive and
/// usable. Runs in the default suite to keep CI fast; the honest 60s proof is
/// `idle_survival_60s` below.
#[tokio::test]
async fn idle_survival_fast() {
    let (addr, server) = spawn_server().await;
    let (_ep, conn) = connect(addr).await;

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
    server.abort();
}

/// TRANS-05 (honest): a connection left idle for 60s — longer than the QUIC
/// default 30s idle timeout — does NOT drop, because the client keep-alive
/// (15s) keeps it warm. Ignored by default (slow); run with:
/// `cargo test --workspace -- --ignored`.
#[tokio::test]
#[ignore = "slow: real 60s idle-survival proof; run with --ignored"]
async fn idle_survival_60s() {
    let (addr, server) = spawn_server().await;
    let (_ep, conn) = connect(addr).await;

    tokio::time::sleep(Duration::from_secs(60)).await;
    assert!(
        conn.close_reason().is_none(),
        "connection must survive 60s idle (keep-alive vs idle timeout)"
    );
    let echoed = client::stream_echo_roundtrip(&conn, b"alive-after-60s")
        .await
        .expect("stream after 60s idle");
    assert_eq!(echoed, b"alive-after-60s");
    server.abort();
}
