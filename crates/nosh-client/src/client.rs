//! Client-side QUIC endpoint setup, connect-with-ALPN-assert, and the
//! stream/datagram round-trip helpers that prove the Phase 1 transport.
//!
//! Exposed as library functions so the integration tests (Plan 04) reuse them.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Context;
use bytes::Bytes;
use quinn::crypto::rustls::{HandshakeData, QuicClientConfig};
use nosh_auth::PlaceholderServerVerifier;

/// Generous read limit for echoed streams in this skeleton.
const READ_LIMIT: usize = 64 * 1024;

/// Build a quinn `ClientConfig` using the placeholder server verifier (the
/// Phase 2 seam), ALPN `nosh/0`, and the shared transport config WITH keep-alive
/// enabled (the client drives keep-alive — TRANS-05).
pub fn build_client_config() -> anyhow::Result<quinn::ClientConfig> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let provider = rustls::crypto::CryptoProvider::get_default()
        .context("no default CryptoProvider installed")?
        .clone();

    let mut rustls_cfg = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(PlaceholderServerVerifier::new(provider)))
        .with_no_client_auth();
    rustls_cfg.alpn_protocols = vec![nosh_proto::ALPN.to_vec()];

    let quic_crypto =
        QuicClientConfig::try_from(rustls_cfg).context("convert rustls client config to QUIC")?;
    let mut client_config = quinn::ClientConfig::new(Arc::new(quic_crypto));
    // true = enable keep-alive (TRANS-05).
    client_config.transport_config(Arc::new(nosh_proto::transport_config(true)));

    Ok(client_config)
}

/// Build a client `Endpoint` (bound to an ephemeral local UDP port) with the
/// nosh client config as its default.
pub fn make_endpoint() -> anyhow::Result<quinn::Endpoint> {
    let mut endpoint = quinn::Endpoint::client("0.0.0.0:0".parse().unwrap())
        .context("create client endpoint")?;
    endpoint.set_default_client_config(build_client_config()?);
    Ok(endpoint)
}

/// Connect to `server_addr` and assert the negotiated ALPN is `nosh/0`
/// (TRANS-01). Returns the established connection.
pub async fn connect(
    endpoint: &quinn::Endpoint,
    server_addr: SocketAddr,
) -> anyhow::Result<quinn::Connection> {
    let conn = endpoint
        .connect(server_addr, "localhost")
        .context("start connect")?
        .await
        .context("await connection")?;

    let alpn = conn
        .handshake_data()
        .and_then(|hd| hd.downcast::<HandshakeData>().ok())
        .and_then(|hd| hd.protocol.clone());
    anyhow::ensure!(
        alpn.as_deref() == Some(nosh_proto::ALPN),
        "ALPN mismatch: negotiated {:?}, expected {:?}",
        alpn,
        nosh_proto::ALPN
    );

    Ok(conn)
}

/// Open a bidirectional stream, send `payload`, and return the echoed bytes
/// (TRANS-02). The caller asserts the result equals `payload`.
pub async fn stream_echo_roundtrip(
    conn: &quinn::Connection,
    payload: &[u8],
) -> anyhow::Result<Vec<u8>> {
    let (mut send, mut recv) = conn.open_bi().await.context("open_bi")?;
    send.write_all(payload).await.context("stream write")?;
    send.finish().context("stream finish")?;
    let echoed = recv
        .read_to_end(READ_LIMIT)
        .await
        .context("stream read_to_end")?;
    Ok(echoed)
}

/// Send a datagram and return the echoed datagram (TRANS-03/04). Asserts
/// datagrams are enabled (`max_datagram_size().is_some()`) and the payload fits.
pub async fn datagram_roundtrip(
    conn: &quinn::Connection,
    payload: Bytes,
) -> anyhow::Result<Bytes> {
    let max = conn
        .max_datagram_size()
        .context("datagrams not enabled (max_datagram_size is None)")?;
    anyhow::ensure!(
        payload.len() <= max,
        "datagram payload {} exceeds max_datagram_size {max}",
        payload.len()
    );
    conn.send_datagram(payload).context("send_datagram")?;
    let echoed = conn.read_datagram().await.context("read_datagram")?;
    Ok(echoed)
}

/// Run a stream echo and a datagram round-trip CONCURRENTLY, proving streams
/// and datagrams coexist on one connection without interference (TRANS-04).
pub async fn concurrent_roundtrip(conn: &quinn::Connection) -> anyhow::Result<()> {
    let stream_payload = b"concurrent-stream-payload".to_vec();
    let datagram_payload = Bytes::from_static(b"concurrent-datagram-payload");

    let (stream_echo, datagram_echo) = tokio::try_join!(
        stream_echo_roundtrip(conn, &stream_payload),
        datagram_roundtrip(conn, datagram_payload.clone()),
    )?;

    anyhow::ensure!(
        stream_echo == stream_payload,
        "concurrent stream echo mismatch"
    );
    anyhow::ensure!(
        datagram_echo == datagram_payload,
        "concurrent datagram echo mismatch"
    );
    Ok(())
}
