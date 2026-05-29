//! Server-side QUIC endpoint setup and per-connection echo handlers.
//!
//! Exposed as library functions so the integration tests (Plan 04) can drive an
//! in-process server. Phase 1 uses `with_no_client_auth()` — client auth is
//! Phase 2.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Context;
use quinn::crypto::rustls::{HandshakeData, QuicServerConfig};
use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer};

/// Build a quinn `ServerConfig`: an ephemeral rcgen self-signed cert, ALPN
/// `nosh/0`, no client auth (Phase 1), and the shared transport config.
pub fn build_server_config() -> anyhow::Result<quinn::ServerConfig> {
    // Ensure a process-wide default CryptoProvider is installed (ring).
    let _ = rustls::crypto::ring::default_provider().install_default();

    // Ephemeral self-signed cert for "localhost" (dev placeholder; Phase 2
    // replaces the trust model with SSH-key pinning).
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_string()])
        .context("generate self-signed cert")?;
    let cert_der = CertificateDer::from(cert.cert);
    let key_der = PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der());

    let mut rustls_cfg = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key_der.into())
        .context("build rustls server config")?;
    rustls_cfg.alpn_protocols = vec![nosh_proto::ALPN.to_vec()];

    let quic_crypto = QuicServerConfig::try_from(rustls_cfg)
        .context("convert rustls server config to QUIC")?;
    let mut server_config = quinn::ServerConfig::with_crypto(Arc::new(quic_crypto));
    // Server does not drive keep-alive (the client does).
    server_config.transport_config(Arc::new(nosh_proto::transport_config(false)));

    Ok(server_config)
}

/// Build a quinn server `Endpoint` bound to `addr`.
pub fn make_endpoint(addr: SocketAddr) -> anyhow::Result<quinn::Endpoint> {
    let endpoint = quinn::Endpoint::server(build_server_config()?, addr)
        .with_context(|| format!("bind server endpoint to {addr}"))?;
    Ok(endpoint)
}

/// Accept connections forever, spawning a handler per connection.
pub async fn run_accept_loop(endpoint: quinn::Endpoint) -> anyhow::Result<()> {
    while let Some(incoming) = endpoint.accept().await {
        tokio::spawn(async move {
            if let Err(e) = handle_connection(incoming).await {
                tracing::warn!("connection handler ended: {e:#}");
            }
        });
    }
    Ok(())
}

/// Handle one connection: echo bidi-stream bytes and echo datagrams,
/// concurrently, until the peer closes or the connection times out.
async fn handle_connection(incoming: quinn::Incoming) -> anyhow::Result<()> {
    let conn = incoming.await.context("accept connection")?;
    let peer = conn.remote_address();

    // Log the negotiated ALPN for observability.
    let alpn = conn
        .handshake_data()
        .and_then(|hd| hd.downcast::<HandshakeData>().ok())
        .and_then(|hd| hd.protocol.clone())
        .map(|p| String::from_utf8_lossy(&p).into_owned())
        .unwrap_or_else(|| "<none>".to_string());
    tracing::info!(%peer, alpn = %alpn, "connection accepted");

    let stream_conn = conn.clone();
    let datagram_conn = conn.clone();

    let stream_task = async move { stream_echo_loop(stream_conn).await };
    let datagram_task = async move { datagram_echo_loop(datagram_conn).await };

    // Run both pumps concurrently; the connection closing ends both.
    tokio::select! {
        r = stream_task => r?,
        r = datagram_task => r?,
    }
    Ok(())
}

/// Accept bidirectional streams and echo their bytes back (TRANS-02).
async fn stream_echo_loop(conn: quinn::Connection) -> anyhow::Result<()> {
    loop {
        match conn.accept_bi().await {
            Ok((mut send, mut recv)) => {
                tokio::spawn(async move {
                    match recv.read_to_end(nosh_proto::codec::MAX_FRAME_LEN).await {
                        Ok(buf) => {
                            if let Err(e) = send.write_all(&buf).await {
                                tracing::warn!("stream echo write failed: {e}");
                                return;
                            }
                            let _ = send.finish();
                            tracing::debug!(bytes = buf.len(), "echoed stream");
                        }
                        Err(e) => tracing::warn!("stream read failed: {e}"),
                    }
                });
            }
            Err(e) => return clean_exit(e),
        }
    }
}

/// Receive datagrams and echo them straight back (TRANS-03/04).
async fn datagram_echo_loop(conn: quinn::Connection) -> anyhow::Result<()> {
    loop {
        match conn.read_datagram().await {
            Ok(bytes) => {
                // Only echo if datagrams are enabled and the payload fits the
                // current path limit (PITFALL 2). On loopback this always holds.
                match conn.max_datagram_size() {
                    Some(max) if bytes.len() <= max => {
                        if let Err(e) = conn.send_datagram(bytes.clone()) {
                            tracing::warn!("datagram echo failed: {e}");
                        } else {
                            tracing::debug!(bytes = bytes.len(), "echoed datagram");
                        }
                    }
                    Some(max) => tracing::warn!(
                        "datagram {} exceeds max_datagram_size {max}; dropping",
                        bytes.len()
                    ),
                    None => tracing::warn!("datagrams not enabled; dropping"),
                }
            }
            Err(e) => return clean_exit(e),
        }
    }
}

/// Treat orderly connection teardown as a clean loop exit, not an error.
fn clean_exit(e: quinn::ConnectionError) -> anyhow::Result<()> {
    use quinn::ConnectionError::*;
    match e {
        ApplicationClosed(_) | LocallyClosed | ConnectionClosed(_) | TimedOut => Ok(()),
        other => Err(other.into()),
    }
}
