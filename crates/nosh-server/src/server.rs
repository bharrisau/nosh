//! Server-side QUIC endpoint setup and per-connection echo handlers.
//!
//! Exposed as library functions so the integration tests (Plan 04) can drive an
//! in-process server. Phase 2 enforces SSH-key mutual auth inside the TLS
//! handshake (client cert pinned against `authorized_keys`) and caps
//! concurrent unauthenticated connections.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use nosh_auth::{AuthorizedKeysVerifier, NoshServerCertResolver};
use quinn::crypto::rustls::{HandshakeData, QuicServerConfig};

/// Pre-auth DoS limits for the accept loop (decision D-13 / FOOTGUN-3).
#[derive(Clone, Copy, Debug)]
pub struct AuthLimits {
    /// Max concurrent in-progress (pre-auth) handshakes.
    pub max_concurrent: usize,
    /// Time a connection has to complete the TLS handshake before being dropped.
    pub auth_timeout: Duration,
}

impl Default for AuthLimits {
    fn default() -> Self {
        Self {
            max_concurrent: 64,
            auth_timeout: Duration::from_secs(5),
        }
    }
}

/// Build a quinn `ServerConfig` enforcing SSH-key mutual auth.
///
/// - Server presents a self-signed cert whose SPKI is the host key's Ed25519
///   public key (D-06/D-09); it signs its own `CertificateVerify` with the host
///   key loaded from `host_key_path`.
/// - Clients must present a cert whose SPKI is in `authorized_keys_path`
///   (AUTH-01), enforced by [`AuthorizedKeysVerifier`].
pub fn build_server_config(
    host_key_path: &Path,
    authorized_keys_path: &Path,
) -> anyhow::Result<quinn::ServerConfig> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let provider = Arc::new(rustls::crypto::ring::default_provider());

    // Load the Ed25519 host key (daemon model — from a file, D-06) and mint a
    // self-signed cert whose SPKI is the host public key.
    let host_priv = nosh_auth::load_host_key(host_key_path)?;
    let host_signer: Arc<dyn nosh_auth::RawEd25519Signer> = Arc::new(
        nosh_auth::InProcessEd25519Signer::from_ssh_private(&host_priv)?,
    );
    let host_cert = nosh_auth::mint_self_signed_cert(&host_signer)?;
    let host_signing_key = Arc::new(nosh_auth::AgentSigningKey::new(host_signer));

    // Authorized client keys (AUTH-01 / D-07).
    let authorized = nosh_auth::load_authorized_keys(authorized_keys_path)?;
    let client_verifier = Arc::new(AuthorizedKeysVerifier::new(authorized, provider.clone()));

    let mut rustls_cfg = rustls::ServerConfig::builder()
        .with_client_cert_verifier(client_verifier)
        .with_cert_resolver(Arc::new(NoshServerCertResolver::new(
            host_cert,
            host_signing_key,
        )));
    rustls_cfg.alpn_protocols = vec![nosh_proto::ALPN.to_vec()];

    let quic_crypto = QuicServerConfig::try_from(rustls_cfg)
        .context("convert rustls server config to QUIC")?;
    let mut server_config = quinn::ServerConfig::with_crypto(Arc::new(quic_crypto));
    server_config.transport_config(Arc::new(nosh_proto::transport_config(false)));

    Ok(server_config)
}

/// Build a quinn server `Endpoint` bound to `addr` with the given trust files.
pub fn make_endpoint(
    addr: SocketAddr,
    host_key_path: &Path,
    authorized_keys_path: &Path,
) -> anyhow::Result<quinn::Endpoint> {
    let endpoint = quinn::Endpoint::server(
        build_server_config(host_key_path, authorized_keys_path)?,
        addr,
    )
    .with_context(|| format!("bind server endpoint to {addr}"))?;
    Ok(endpoint)
}

/// Accept connections forever, capping concurrent half-open handshakes and
/// enforcing an auth-completion timeout (AUTH-05 / D-13).
pub async fn run_accept_loop(endpoint: quinn::Endpoint, limits: AuthLimits) -> anyhow::Result<()> {
    let permits = Arc::new(tokio::sync::Semaphore::new(limits.max_concurrent));
    while let Some(incoming) = endpoint.accept().await {
        // Bound concurrent pre-auth connections: if all permits are taken,
        // refuse rather than allocate unbounded per-connection state.
        let permit = match permits.clone().try_acquire_owned() {
            Ok(p) => p,
            Err(_) => {
                tracing::warn!(
                    "pre-auth connection cap ({}) reached; refusing connection",
                    limits.max_concurrent
                );
                incoming.refuse();
                continue;
            }
        };
        let timeout = limits.auth_timeout;
        tokio::spawn(async move {
            // The permit is held for the whole handshake+session; dropping it
            // on any exit path releases capacity.
            let _permit = permit;
            if let Err(e) = handle_connection(incoming, timeout).await {
                tracing::warn!("connection handler ended: {e:#}");
            }
        });
    }
    Ok(())
}

/// Handle one connection: echo bidi-stream bytes and echo datagrams,
/// concurrently, until the peer closes or the connection times out.
async fn handle_connection(
    incoming: quinn::Incoming,
    auth_timeout: Duration,
) -> anyhow::Result<()> {
    // AUTH-05: bound the time a connection may stay half-open. The TLS handshake
    // (including client-cert verification) completes when `incoming` resolves;
    // if it does not within the timeout, drop the connection.
    let conn = match tokio::time::timeout(auth_timeout, incoming).await {
        Ok(res) => res.context("accept connection")?,
        Err(_) => {
            tracing::warn!("connection did not complete auth within timeout; dropping");
            return Ok(());
        }
    };
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
