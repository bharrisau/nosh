//! Server-side QUIC endpoint setup and the per-connection PTY session pump.
//!
//! Exposed as library functions so the integration tests can drive an
//! in-process server. Phase 2 enforces SSH-key mutual auth inside the TLS
//! handshake (client cert pinned against `authorized_keys`) and caps concurrent
//! unauthenticated connections. Phase 3 replaces the echo loops with a real PTY
//! login-shell session framed over a single bidi QUIC stream (D-01).

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use nosh_auth::{AuthorizedKeysVerifier, NoshServerCertResolver};
use nosh_proto::Message;
use quinn::crypto::rustls::{HandshakeData, QuicServerConfig};
use tokio::sync::mpsc;

use crate::session;

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

    let quic_crypto =
        QuicServerConfig::try_from(rustls_cfg).context("convert rustls server config to QUIC")?;
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

/// Accept connections forever, capping concurrent PRE-AUTH (half-open)
/// handshakes and enforcing an auth-completion timeout (AUTH-05 / D-13). The
/// per-connection permit is released as soon as the handshake resolves, so the
/// cap bounds unauthenticated state rather than total live sessions.
pub async fn run_accept_loop(
    endpoint: quinn::Endpoint,
    limits: AuthLimits,
    shell_override: Option<String>,
) -> anyhow::Result<()> {
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
        let shell = shell_override.clone();
        tokio::spawn(async move {
            // The permit bounds PRE-AUTH state only (D-13): it is released the
            // moment the handshake resolves (success, failure, or timeout), so
            // long-lived authenticated sessions do not consume pre-auth capacity.
            if let Err(e) = handle_connection(incoming, timeout, permit, shell).await {
                tracing::warn!("connection handler ended: {e:#}");
            }
        });
    }
    Ok(())
}

/// QUIC application close code for an orderly session end.
const CLOSE_OK: u32 = 0;
/// QUIC application close code for a protocol violation (bad first frame).
const CLOSE_PROTOCOL: u32 = 1;
/// PTY output read chunk size.
const PTY_CHUNK: usize = 8 * 1024;

/// Handle one connection: after auth, drive a real PTY login-shell session over
/// a single bidirectional stream until the shell exits or the client
/// disconnects.
async fn handle_connection(
    incoming: quinn::Incoming,
    auth_timeout: Duration,
    permit: tokio::sync::OwnedSemaphorePermit,
    shell_override: Option<String>,
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
    // Auth is complete: release the pre-auth permit so the now-authenticated
    // session no longer counts against the pre-auth concurrency cap (D-13).
    drop(permit);
    let peer = conn.remote_address();

    // Log the negotiated ALPN for observability.
    let alpn = conn
        .handshake_data()
        .and_then(|hd| hd.downcast::<HandshakeData>().ok())
        .and_then(|hd| hd.protocol.clone())
        .map(|p| String::from_utf8_lossy(&p).into_owned())
        .unwrap_or_else(|| "<none>".to_string());
    tracing::info!(%peer, alpn = %alpn, "connection accepted");

    // The client opens exactly one bidi stream and sends SessionOpen first.
    let (send, recv) = match conn.accept_bi().await {
        Ok(pair) => pair,
        Err(e) => return clean_exit(e),
    };

    run_session(conn, peer, send, recv, shell_override).await
}

/// Drive a single PTY session over the established bidi stream.
async fn run_session(
    conn: quinn::Connection,
    peer: SocketAddr,
    mut send: quinn::SendStream,
    mut recv: quinn::RecvStream,
    shell_override: Option<String>,
) -> anyhow::Result<()> {
    // First frame must be SessionOpen.
    let open = match nosh_proto::read_message(&mut recv).await {
        Ok(Message::SessionOpen {
            term,
            cols,
            rows,
            env,
        }) => (term, cols, rows, env),
        Ok(other) => {
            tracing::warn!(?other, "expected SessionOpen as first frame");
            conn.close(CLOSE_PROTOCOL.into(), b"expected SessionOpen");
            return Ok(());
        }
        Err(e) => {
            tracing::warn!("failed to read SessionOpen: {e}");
            conn.close(CLOSE_PROTOCOL.into(), b"bad first frame");
            return Ok(());
        }
    };
    let (term, cols, rows, client_env) = open;

    let passwd = session::lookup_self(shell_override.as_deref());
    let (mut sess, reader, writer) =
        session::open(&passwd, &term, cols, rows, &client_env, None).context("open session")?;

    let session_id = sess.session_id;
    let username = sess.username.clone();
    let span = tracing::info_span!("session", %session_id, %peer, username = %username);
    let _enter = span.enter();
    tracing::info!(%term, cols, rows, child_pid = ?sess.child_pid(), "session open");

    // Take the child so its exit can be awaited concurrently (the select loop
    // needs `&mut sess` for resize, so the child cannot live inside `sess`).
    let child = sess
        .take_child()
        .context("session has no child to wait on")?;
    // Wait for the shell exit on a dedicated task; the JoinHandle resolves once
    // with the exit code (SESS-08). On disconnect we abort it and reap manually.
    let mut wait_task = tokio::spawn(session::wait_child(child));

    // OUTPUT pump: a blocking thread reads PTY output and forwards chunks; an
    // async task drains them into PtyData frames on the stream.
    let (out_tx, mut out_rx) = mpsc::channel::<Vec<u8>>(64);
    let mut reader = reader;
    let output_reader = tokio::task::spawn_blocking(move || {
        let mut buf = [0u8; PTY_CHUNK];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break, // PTY EOF: shell closed.
                Ok(n) => {
                    if out_tx.blocking_send(buf[..n].to_vec()).is_err() {
                        break; // receiver gone — stop reading.
                    }
                }
                Err(_) => break,
            }
        }
    });

    // INPUT pump: writes from the client stream go to the PTY. The blocking
    // writer is moved into a dedicated blocking task fed by a channel.
    let (in_tx, mut in_rx) = mpsc::channel::<Vec<u8>>(64);
    let mut writer = writer;
    let input_writer = tokio::task::spawn_blocking(move || {
        while let Some(bytes) = in_rx.blocking_recv() {
            if writer.write_all(&bytes).is_err() || writer.flush().is_err() {
                break;
            }
        }
    });

    // Pump until the shell exits (Some(code)) or the client disconnects (None).
    let session_outcome = loop {
        tokio::select! {
            // Shell exited: capture the code and tell the client.
            res = &mut wait_task => {
                break Some(res.unwrap_or(1));
            }
            // PTY output ready: frame it to the client. Drain remaining output
            // even as the shell is exiting so the last bytes are delivered.
            chunk = out_rx.recv() => {
                match chunk {
                    Some(data) => {
                        if nosh_proto::write_message(&mut send, &Message::PtyData { data })
                            .await
                            .is_err()
                        {
                            break None;
                        }
                    }
                    None => {
                        // Output pump ended (PTY EOF). Await the exit code.
                        break Some((&mut wait_task).await.unwrap_or(1));
                    }
                }
            }
            // Client → server frames.
            msg = nosh_proto::read_message(&mut recv) => {
                match msg {
                    Ok(Message::PtyData { data }) => {
                        if in_tx.send(data).await.is_err() {
                            break None;
                        }
                    }
                    Ok(Message::Resize { cols, rows }) => {
                        if let Err(e) = sess.resize(cols, rows) {
                            tracing::warn!("resize failed: {e}");
                        } else {
                            tracing::debug!(cols, rows, "resize");
                        }
                    }
                    Ok(Message::SessionClose { .. }) | Ok(Message::SessionOpen { .. }) => {
                        break None; // client signalled end (or unexpected reopen)
                    }
                    Err(_) => break None, // stream/connection closed by the client
                }
            }
        }
    };

    // Stop the input pump.
    drop(in_tx);

    match session_outcome {
        Some(exit_code) => {
            // Shell exited normally: drain ALL remaining PTY output (the output
            // reader thread closes `out_rx` on PTY EOF, so recv() eventually
            // yields None), then deliver the exit code and close cleanly with a
            // structured reason (SESS-08/09). Draining to channel close avoids a
            // race where the shell's final bytes are still in flight.
            loop {
                match tokio::time::timeout(Duration::from_millis(200), out_rx.recv()).await {
                    Ok(Some(data)) => {
                        let _ =
                            nosh_proto::write_message(&mut send, &Message::PtyData { data }).await;
                    }
                    Ok(None) => break, // output channel closed: all output sent
                    Err(_) => break,   // no more output within the window
                }
            }
            tracing::info!(exit_code, "shell exited");
            let _ = nosh_proto::write_message(
                &mut send,
                &Message::SessionClose {
                    exit_code,
                    reason: "shell exited".to_string(),
                },
            )
            .await;
            let _ = send.finish();
            // Wait until the client has acknowledged reading the finished stream
            // (so the SessionClose frame is delivered, not truncated), then the
            // server closes the connection with a structured application code
            // (SESS-09). `stopped()` resolves once the peer has consumed/acked
            // the stream; a short bounded fallback covers a client that lingers.
            let _ = tokio::time::timeout(Duration::from_secs(2), send.stopped()).await;
            conn.close(CLOSE_OK.into(), b"shell exited");
        }
        None => {
            // Client disconnected (or protocol end): SIGHUP the shell, then let
            // the in-flight wait task reap it so no zombie/orphan remains
            // (SESS-10). The child was moved into `wait_task`; SIGHUP unblocks
            // its blocking `wait()`, which reaps. Await it (bounded) for
            // deterministic teardown.
            tracing::info!("client disconnected; reaping shell");
            sess.sighup();
            let _ = tokio::time::timeout(Duration::from_secs(5), &mut wait_task).await;
            conn.close(CLOSE_OK.into(), b"client disconnected");
        }
    }

    // Best-effort: ensure the blocking I/O tasks unwind.
    output_reader.abort();
    input_writer.abort();
    Ok(())
}

/// Treat orderly connection teardown as a clean loop exit, not an error.
fn clean_exit(e: quinn::ConnectionError) -> anyhow::Result<()> {
    use quinn::ConnectionError::*;
    match e {
        ApplicationClosed(_) | LocallyClosed | ConnectionClosed(_) | TimedOut => Ok(()),
        other => Err(other.into()),
    }
}
