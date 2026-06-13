// WebTransport transport wrapper for nosh-server (Phase 24 / WT-02, WT-05).
//
// `WtransportTransport`, `WtransportSendStream`, and `WtransportRecvStream`
// implement the same `NoshTransport` / `NoshSendStream` / `NoshRecvStream`
// traits as the Quinn wrappers, adapting six API differences:
//
// 1. `send_datagram`: wtransport has 3 error variants (no `Disabled`); maps
//    `NotConnected` → `SendDatagramError::ConnectionLost`.
// 2. `datagram_send_buffer_space`: wtransport::Connection has NO such method —
//    reach through `quic_connection()` to the underlying `quinn::Connection`.
// 3. `max_datagram_size`: SAME method name, DIFFERENT semantics — wtransport's
//    value already subtracts WebTransport capsule (Quarter-Stream-ID) overhead
//    (D-03). DO NOT use `quic_connection().max_datagram_size()`.
// 4. `read_datagram`: `receive_datagram()` returns `wtransport::Datagram`; use
//    `dg.payload()` which returns the existing `Bytes` slice (zero-copy).
// 5. `open_bi`: DOUBLE AWAIT — `open_bi().await?` yields `OpeningBiStream`, then
//    a second `.await?` yields `(SendStream, RecvStream)`.
// 6. `WtransportSendStream::finish()` IS ASYNC (UNLIKE quinn 0.11 which is sync).
//    The wrapper body MUST call `.finish().await`. This is the OPPOSITE of the
//    quinn wrapper's "DO NOT .await" comment.
//
// Additional adapter:
// - `WtransportRecvStream` holds `Option<RecvStream>` because
//   `wtransport::RecvStream::stop(self)` is consuming; `.take()` adapts it to
//   the `&mut self` signature of `NoshRecvStream::stop`.
//
// Outer TLS (D-04/D-05):
// - `build_ca_cert_rustls_config`: loads operator PEM cert + key, builds a
//   `rustls::ServerConfig` with `with_no_client_auth()` (outer TLS is server
//   identity only; inner SSH-key auth is Phase 25).
// - `build_wt_server_config`: wraps the rustls config in a wtransport
//   `ServerConfig` via `.with_custom_tls()`.
// - `make_wt_endpoint`: builds the endpoint from cert/key paths.
//
// WT-05 (downgrade protection): a `--mode webtransport` server constructs ONLY
// a `wtransport::Endpoint<Server>`. A raw-QUIC client attempting to connect gets
// a protocol error at the HTTP/3 CONNECT upgrade stage and never produces an
// `IncomingSession`. No quinn endpoint is created in WT mode.
//
// This entire module is gated behind `#[cfg(feature = "webtransport")]`.
// Building without the feature excludes all WT code and preserves the native QUIC
// path unchanged (D-06).

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use anyhow::Context;
use async_trait::async_trait;
use bytes::Bytes;
use tokio::io::AsyncWriteExt as _;
use wtransport::endpoint::endpoint_side::Server;
use wtransport::{Connection, RecvStream, SendStream, VarInt};
use wtransport::error::SendDatagramError as WtSendDatagramError;

use nosh_proto::transport_trait::{NoshRecvStream, NoshSendStream, NoshTransport, SendDatagramError};
use crate::registry::SessionRegistry;
use crate::server::AuthLimits;

// ── WtransportTransport ────────────────────────────────────────────────────────

/// Newtype wrapper over `wtransport::Connection` implementing `NoshTransport`.
///
/// Plugs the WebTransport session into the Phase 23 transport-abstraction seam
/// so the session pump (`run_session` / `run_reattach_session`) is unchanged.
pub struct WtransportTransport(pub Connection);

#[async_trait]
impl NoshTransport for WtransportTransport {
    // Diff 1: 3-variant map (wtransport has NO Disabled variant).
    fn send_datagram(&self, data: Bytes) -> Result<(), SendDatagramError> {
        // send_datagram takes D: AsRef<[u8]>; Bytes: AsRef<[u8]>.
        self.0.send_datagram(data).map_err(|e| match e {
            WtSendDatagramError::TooLarge => SendDatagramError::TooLarge,
            WtSendDatagramError::UnsupportedByPeer => SendDatagramError::UnsupportedByPeer,
            WtSendDatagramError::NotConnected => {
                SendDatagramError::ConnectionLost("not connected".to_string())
            }
            // wtransport has NO Disabled variant — this match is exhaustive with 3 arms.
        })
    }

    // Diff 2: wtransport::Connection has no datagram_send_buffer_space() —
    // reach through to the underlying quinn::Connection (requires `quinn` feature on wtransport).
    fn datagram_send_buffer_space(&self) -> usize {
        self.0.quic_connection().datagram_send_buffer_space()
    }

    // Diff 3: D-03 — wtransport::Connection::max_datagram_size() ALREADY subtracts
    // WebTransport capsule overhead. DO NOT use quic_connection().max_datagram_size()
    // which returns the raw quinn value (too large).
    fn max_datagram_size(&self) -> Option<usize> {
        self.0.max_datagram_size()
    }

    // Diff 4: receive_datagram() returns Datagram; use .payload() for zero-copy Bytes.
    async fn read_datagram(&self) -> anyhow::Result<Bytes> {
        let dg = self.0.receive_datagram().await?;
        // Datagram::payload() returns a Bytes slice into the existing buffer — zero copy.
        Ok(dg.payload())
    }

    async fn accept_bi(&self) -> anyhow::Result<(Box<dyn NoshSendStream>, Box<dyn NoshRecvStream>)> {
        let (s, r) = self.0.accept_bi().await?;
        Ok((
            Box::new(WtransportSendStream(s)),
            Box::new(WtransportRecvStream(Some(r))),
        ))
    }

    // Diff 5: DOUBLE AWAIT — open_bi().await? yields OpeningBiStream; second .await? yields the pair.
    async fn open_bi(&self) -> anyhow::Result<(Box<dyn NoshSendStream>, Box<dyn NoshRecvStream>)> {
        let (s, r) = self.0.open_bi().await?.await?;
        Ok((
            Box::new(WtransportSendStream(s)),
            Box::new(WtransportRecvStream(Some(r))),
        ))
    }

    fn remote_address(&self) -> SocketAddr {
        self.0.remote_address()
    }

    // Override the default ZERO impl: delegate to the underlying quinn::Connection.
    fn rtt(&self) -> std::time::Duration {
        self.0.quic_connection().rtt()
    }

    // Override the default false impl: a closed WT connection has a close_reason.
    fn is_closed(&self) -> bool {
        self.0.quic_connection().close_reason().is_some()
    }

    fn close(&self, code: u32, reason: &[u8]) {
        // wtransport::Connection::close() takes (VarInt, &[u8]).
        // wtransport::VarInt does NOT impl From<u32>; use the explicit constructor.
        self.0.close(VarInt::from_u32(code), reason)
    }
}

// ── WtransportSendStream ───────────────────────────────────────────────────────

/// Newtype wrapper over `wtransport::SendStream` implementing `NoshSendStream`.
pub struct WtransportSendStream(pub SendStream);

#[async_trait]
impl NoshSendStream for WtransportSendStream {
    async fn write_all(&mut self, data: &[u8]) -> anyhow::Result<()> {
        Ok(self.0.write_all(data).await?)
    }

    async fn flush(&mut self) -> anyhow::Result<()> {
        Ok(self.0.flush().await?)
    }

    // Diff 6: UNLIKE quinn 0.11 (where finish() is SYNCHRONOUS and must NOT be awaited),
    // wtransport::SendStream::finish() IS ASYNC. This wrapper body MUST call .finish().await.
    // This is the INVERSE of the QuinnSendStream comment — don't confuse the two.
    async fn finish(&mut self) -> anyhow::Result<()> {
        Ok(self.0.finish().await?)
    }

    async fn stopped(&mut self) -> anyhow::Result<()> {
        let _result = self.0.stopped().await;
        Ok(())
    }

    fn reset(&mut self, code: u32) {
        // wtransport::VarInt does NOT impl From<u32>; use from_u32.
        let _ = self.0.reset(VarInt::from_u32(code));
    }
}

// ── WtransportRecvStream ───────────────────────────────────────────────────────

/// Newtype wrapper over `Option<wtransport::RecvStream>` implementing `NoshRecvStream`.
///
/// The inner `Option` is needed because `wtransport::RecvStream::stop(self)` is
/// consuming (takes ownership), while `NoshRecvStream::stop` takes `&mut self`.
/// `.take()` inside `stop()` satisfies the consuming requirement.
pub struct WtransportRecvStream(pub Option<RecvStream>);

#[async_trait]
impl NoshRecvStream for WtransportRecvStream {
    async fn read_exact(&mut self, buf: &mut [u8]) -> anyhow::Result<()> {
        match self.0.as_mut() {
            Some(s) => Ok(s.read_exact(buf).await?),
            None => Err(anyhow::anyhow!("stream already stopped")),
        }
    }

    async fn read(&mut self, buf: &mut [u8]) -> anyhow::Result<Option<usize>> {
        match self.0.as_mut() {
            Some(s) => Ok(s.read(buf).await?),
            None => Ok(None), // stopped → treat as EOF
        }
    }

    fn stop(&mut self, code: u32) {
        // .take() satisfies the consuming stop(self) requirement on RecvStream.
        if let Some(stream) = self.0.take() {
            stream.stop(VarInt::from_u32(code));
        }
    }
}

// ── Outer-TLS config helpers (D-04/D-05) ──────────────────────────────────────

/// Load a CA-signed operator certificate and private key from PEM files and
/// build a `rustls::ServerConfig` for the WebTransport outer TLS layer.
///
/// D-05: the outer TLS uses `with_no_client_auth()` — server identity only.
/// End-to-end mutual auth is performed by the inner SSH-key handshake (Phase 25).
///
/// ALPN is set to `h3` as required by WebTransport over HTTP/3; the native
/// `nosh/0` ALPN is NOT used for the outer layer.
pub fn build_ca_cert_rustls_config(
    cert_path: &Path,
    key_path: &Path,
) -> anyhow::Result<rustls::ServerConfig> {
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};
    use rustls::pki_types::pem::PemObject;

    // Install the ring crypto provider (idempotent — the native path already does this).
    let _ = rustls::crypto::ring::default_provider().install_default();

    // Load cert chain from PEM. The cert may be a chain (leaf + intermediates).
    let cert_bytes = std::fs::read(cert_path)
        .with_context(|| format!("read cert PEM from {}", cert_path.display()))?;
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(&cert_bytes)
        .collect::<Result<Vec<_>, _>>()
        .with_context(|| format!("parse cert PEM from {}", cert_path.display()))?;
    if certs.is_empty() {
        anyhow::bail!("no certificates found in {}", cert_path.display());
    }

    // Load private key from PEM (PKCS#8, SEC1, or PKCS#1 formats all handled).
    let key_bytes = std::fs::read(key_path)
        .with_context(|| format!("read key PEM from {}", key_path.display()))?;
    let key = PrivateKeyDer::from_pem_slice(&key_bytes)
        .with_context(|| format!("parse private key PEM from {}", key_path.display()))?;

    // Build the rustls ServerConfig with no client cert verification (D-05).
    let mut rustls_cfg = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .context("build rustls ServerConfig for WebTransport outer TLS")?;

    // ALPN: WebTransport over HTTP/3 requires h3.
    // Do NOT reuse nosh_proto::ALPN ("nosh/0") here — the outer layer is HTTP/3,
    // not the inner nosh protocol. The inner ALPN is negotiated inside the WT session.
    rustls_cfg.alpn_protocols = vec![b"h3".to_vec()];

    Ok(rustls_cfg)
}

/// Build a wtransport `ServerConfig` from the outer-TLS `rustls::ServerConfig`.
///
/// The `bind_addr` is typically `0.0.0.0:443` for production or `0.0.0.0:4433`
/// for dev/CI (UDP/443 requires root or `setcap CAP_NET_BIND_SERVICE`).
pub fn build_wt_server_config(
    bind_addr: SocketAddr,
    rustls_cfg: rustls::ServerConfig,
) -> anyhow::Result<wtransport::ServerConfig> {
    Ok(wtransport::ServerConfig::builder()
        .with_bind_address(bind_addr)
        .with_custom_tls(rustls_cfg)
        .build())
}

/// Build a wtransport `Endpoint<Server>` from operator cert and key PEM paths.
///
/// Combines `build_ca_cert_rustls_config` → `build_wt_server_config` →
/// `wtransport::Endpoint::server`.
///
/// # Privilege note
///
/// Binding UDP/443 requires root or `setcap CAP_NET_BIND_SERVICE`. For dev/CI
/// use `--port 4433` (unprivileged). If binding fails, the error message surfaces
/// the privilege requirement.
pub fn make_wt_endpoint(
    addr: SocketAddr,
    cert_path: &Path,
    key_path: &Path,
) -> anyhow::Result<wtransport::Endpoint<Server>> {
    let rustls_cfg = build_ca_cert_rustls_config(cert_path, key_path)?;
    let wt_config = build_wt_server_config(addr, rustls_cfg)?;
    wtransport::Endpoint::server(wt_config)
        .with_context(|| {
            let port = addr.port();
            if port < 1024 {
                format!(
                    "bind WebTransport endpoint to {addr}: port {port} requires root or \
                    `setcap CAP_NET_BIND_SERVICE`. Use --port 4433 for dev/CI."
                )
            } else {
                format!("bind WebTransport endpoint to {addr}")
            }
        })
}

// ── WT accept loop (Task 2) ────────────────────────────────────────────────────

/// Accept WebTransport connections forever, with the same pre-auth DoS caps as
/// `run_accept_loop` for native QUIC (AUTH-05 / D-13 / T-24-03-D).
///
/// WT-05 structural guarantee: this loop runs `wtransport::Endpoint::accept()`,
/// which only yields HTTP/3 WebTransport CONNECT upgrade requests. A raw-QUIC
/// client gets a protocol error at the upgrade stage and never produces an
/// `IncomingSession` — the server cannot accidentally serve non-WT QUIC traffic.
///
/// The `registry` reaper is spawned once. Each accepted connection is given a
/// semaphore permit (pre-auth cap) that is held until the auth phase completes
/// or the auth timeout expires.
pub async fn run_wt_accept_loop(
    endpoint: wtransport::Endpoint<Server>,
    registry: Arc<SessionRegistry>,
    limits: AuthLimits,
    shell_override: Option<String>,
) -> anyhow::Result<()> {
    // Spawn the background zombie/idle reaper once for this server instance.
    let _reaper = registry.spawn_reaper();

    let permits = Arc::new(tokio::sync::Semaphore::new(limits.max_concurrent));

    // Only WebTransport sessions surface here (WT-05: no raw-QUIC sessions).
    // wtransport's accept() returns IncomingSession directly (no Option) and
    // panics if the endpoint is closed — the loop runs until the process exits.
    loop {
        let incoming = endpoint.accept().await;
        // Pre-auth DoS cap (replicates run_accept_loop's semaphore guard).
        // wtransport has NO .refuse() on IncomingSession — just drop it.
        let permit = match permits.clone().try_acquire_owned() {
            Ok(p) => p,
            Err(_) => {
                tracing::warn!(
                    "pre-auth WT cap ({}) reached; dropping incoming WebTransport session",
                    limits.max_concurrent
                );
                continue; // drop `incoming` — no .refuse() available
            }
        };

        let auth_timeout = limits.auth_timeout;
        let shell = shell_override.clone();
        let registry = registry.clone();

        tokio::spawn(async move {
            let result = tokio::time::timeout(auth_timeout, async {
                // Three-step accept: IncomingSession → SessionRequest → Connection.
                let session_request = incoming.await
                    .context("WebTransport IncomingSession resolve")?;
                let conn: Connection = session_request.accept().await
                    .context("WebTransport session accept")?;

                // Auth phase complete — release the pre-auth slot so the semaphore
                // only caps half-open handshakes, not active sessions. This mirrors
                // run_accept_loop (server.rs) which drops the permit immediately after
                // the TLS handshake resolves and before handle_connection is called
                // (D-13 parity).
                drop(permit);

                // Box immediately — no wtransport-specific extraction needed after this
                // (unlike the quinn path which calls extract_peer_identity before boxing).
                let transport: Box<dyn NoshTransport> = Box::new(WtransportTransport(conn));
                handle_connection_wt(transport, registry, shell).await
            }).await;

            match result {
                Ok(Ok(())) => {}
                Ok(Err(e)) => tracing::warn!("WT connection handler error: {e:#}"),
                Err(_elapsed) => tracing::warn!("WT session auth timed out"),
            }
        });
    }
}

/// Handle one WebTransport connection after the outer TLS handshake.
///
/// Applies the inner-auth gate first:
/// - **Release builds** (`--features webtransport` without `test-support`):
///   reject immediately with `transport.close(1, ...)`. Phase 25 fills in the
///   real inner SSH-key handshake here.
/// - **Test-support builds** (`--features "webtransport test-support"` or
///   `#[cfg(test)]`): bypass inner auth with a synthetic identity so integration
///   tests can prove the session pump works over WebTransport.
///
/// After the inner-auth gate the dispatch is identical to `handle_connection`:
/// `accept_bi()` → read first frame → `SessionOpen`/`Reattach`/protocol-error.
pub(crate) async fn handle_connection_wt(
    conn: Box<dyn NoshTransport>,
    registry: Arc<SessionRegistry>,
    shell_override: Option<String>,
) -> anyhow::Result<()> {
    use nosh_proto::{Message, read_message_ns};
    use crate::server::{CLOSE_PROTOCOL, run_session, run_reattach_session, SessionOpenParams};

    let peer = conn.remote_address();

    // Inner-auth gate (T-24-03-E: test-support bypass must not reach release builds).
    #[cfg(any(test, feature = "test-support"))]
    let skip_inner_auth = true;
    #[cfg(not(any(test, feature = "test-support")))]
    let skip_inner_auth = false;

    if !skip_inner_auth {
        // Phase 25 fills in the real inner SSH-key handshake.
        // Release builds reject any connection lacking inner auth.
        tracing::warn!(%peer, "WebTransport inner auth not yet implemented; closing connection");
        conn.close(1, b"inner-auth-not-implemented");
        return Ok(());
    }

    // test-support path: bypass inner auth.
    // This block is only reachable when skip_inner_auth = true (test/test-support builds).
    tracing::warn!(%peer, "INNER AUTH BYPASSED — test-support mode; MUST NOT appear in release builds");

    // Derive a synthetic NoshPublicKey for the test identity (all-zero key).
    // Phase 25 replaces this with the real authenticated key from inner SSH-key auth.
    let peer_identity = nosh_auth::NoshPublicKey::from_raw([0u8; 32]);

    // Accept the first bidi stream and dispatch on the first frame.
    let (send, mut recv) = match conn.accept_bi().await {
        Ok(pair) => pair,
        Err(e) => {
            let _ = e;
            return Ok(());
        }
    };

    match read_message_ns(&mut *recv).await {
        Ok(Message::SessionOpen { term, cols, rows, env }) => {
            run_session(
                conn,
                peer,
                peer_identity,
                send,
                recv,
                SessionOpenParams { term, cols, rows, client_env: env, shell_override },
                registry,
            )
            .await
        }
        Ok(Message::Reattach { token, last_acked_seq }) => {
            run_reattach_session(
                conn,
                peer,
                peer_identity,
                send,
                recv,
                (token, last_acked_seq),
                registry,
            )
            .await
        }
        Ok(other) => {
            tracing::warn!(%peer, frame = other.variant_name(), "expected SessionOpen or Reattach as first WT frame");
            conn.close(CLOSE_PROTOCOL, b"expected SessionOpen or Reattach");
            Ok(())
        }
        Err(e) => {
            tracing::warn!(%peer, "failed to read first WT frame: {e}");
            conn.close(CLOSE_PROTOCOL, b"bad first frame");
            Ok(())
        }
    }
}
