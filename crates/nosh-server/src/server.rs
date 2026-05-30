//! Server-side QUIC endpoint setup and the per-connection PTY session pump.
//!
//! Exposed as library functions so the integration tests can drive an
//! in-process server. Phase 2 enforces SSH-key mutual auth inside the TLS
//! handshake (client cert pinned against `authorized_keys`) and caps concurrent
//! unauthenticated connections. Phase 3 replaces the echo loops with a real PTY
//! login-shell session framed over a single bidi QUIC stream (D-01).
//!
//! Phase 5 adds session persistence: a `SessionRegistry` tracks every session so
//! that a transport-level disconnect (network loss, crash) orphans the session
//! (PTY stays open, no SIGHUP — Pitfall #7 / D-02) while an explicit
//! `SessionClose` or normal shell exit tears down immediately (D-01).

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use nosh_auth::{AuthorizedKeysVerifier, NoshServerCertResolver};
use rustls::pki_types::CertificateDer;
use nosh_proto::Message;
use quinn::crypto::rustls::{HandshakeData, QuicServerConfig};
use tokio::sync::mpsc;

use crate::registry::SessionRegistry;
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
///
/// The `registry` is constructed by the caller (main.rs or tests) from CLI/env
/// config and shared into every connection task. The reaper is spawned once here.
pub async fn run_accept_loop(
    endpoint: quinn::Endpoint,
    registry: Arc<SessionRegistry>,
    limits: AuthLimits,
    shell_override: Option<String>,
) -> anyhow::Result<()> {
    // Spawn the background zombie/idle reaper once for this server instance.
    let _reaper = registry.spawn_reaper();

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
        let registry = registry.clone();
        tokio::spawn(async move {
            // The permit bounds PRE-AUTH state only (D-13): it is released the
            // moment the handshake resolves (success, failure, or timeout), so
            // long-lived authenticated sessions do not consume pre-auth capacity.
            if let Err(e) = handle_connection(incoming, timeout, permit, shell, registry).await {
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
/// QUIC application close code for peer identity extraction failure (should
/// never happen on an AuthorizedKeysVerifier-enforced connection — D-04).
const CLOSE_AUTH: u32 = 2;
/// PTY output read chunk size.
const PTY_CHUNK: usize = 8 * 1024;

/// How the session loop ended (D-02).
///
/// Used to decide between orphan-on-transport-loss (keep MasterPty open,
/// no SIGHUP — Pitfall #7) and immediate teardown (shell exit or clean
/// client-initiated close).
enum SessionEnd {
    /// The shell process exited with an exit code.
    ShellExited(i32),
    /// The client sent an explicit `SessionClose` (or unexpected `SessionOpen`).
    /// Typing `exit` in the shell triggers this path after the shell exits
    /// and the server sends its own SessionClose first (ShellExited). This
    /// variant is for client-initiated close before the shell exits.
    ClientClosed,
    /// A send/recv error or a read error — the transport was lost unexpectedly.
    /// The session must be ORPHANED, NOT torn down (D-01/D-02, Pitfall #7).
    TransportLost,
}

/// Handle one connection: after auth, drive a real PTY login-shell session over
/// a single bidirectional stream until the shell exits or the client
/// disconnects.
async fn handle_connection(
    incoming: quinn::Incoming,
    auth_timeout: Duration,
    permit: tokio::sync::OwnedSemaphorePermit,
    shell_override: Option<String>,
    registry: Arc<SessionRegistry>,
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

    // D-04/D-05: extract the authenticated peer identity immediately after the
    // handshake completes — before any session work. AuthorizedKeysVerifier
    // enforces client auth, so a resolved connection must always have a parseable
    // peer identity. If extraction nonetheless fails, close with CLOSE_AUTH and
    // log an error. An unauthenticated session is impossible.
    let peer_identity = match extract_peer_identity(&conn) {
        Some(k) => k,
        None => {
            tracing::error!(%peer, "connection passed auth but peer identity could not be extracted — closing");
            conn.close(CLOSE_AUTH.into(), b"peer identity extraction failed");
            return Ok(());
        }
    };

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

    run_session(conn, peer, peer_identity, send, recv, shell_override, registry).await
}

/// Drive a single PTY session over the established bidi stream.
///
/// Phase 5: builds a `SessionSlot`, registers it Active, feeds every outgoing
/// PTY chunk into the slot's `SequencedOutputBuffer`, and at session end
/// subdivides the outcome:
/// - `ShellExited` / `ClientClosed` → immediate teardown + `registry.remove`
/// - `TransportLost` → orphan (NO SIGHUP, keep MasterPty open, D-01/D-02/Pitfall #7)
async fn run_session(
    conn: quinn::Connection,
    peer: SocketAddr,
    identity: nosh_auth::NoshPublicKey,
    mut send: quinn::SendStream,
    mut recv: quinn::RecvStream,
    shell_override: Option<String>,
    registry: Arc<SessionRegistry>,
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
    let (sess, reader, writer) =
        session::open(&passwd, &term, cols, rows, &client_env, identity).context("open session")?;

    // Capture the identity raw bytes for registry key lookups before the
    // Session moves into the slot.
    let session_id = sess.session_id;
    let username = sess.username.clone();
    let fingerprint = sess.identity.fingerprint();
    let identity_raw = *sess.identity.key32();

    let span = tracing::info_span!(
        "session",
        %session_id,
        %peer,
        username = %username,
        identity = %fingerprint,
    );
    let _enter = span.enter();
    tracing::info!(%term, cols, rows, child_pid = ?sess.child_pid(), "session open");

    // Move the Session into a SessionSlot and register it as Active.
    // The slot keeps MasterPty alive for the duration; resize goes through it.
    let slot = crate::registry::SessionSlot::new(sess);
    registry.register_active(slot.clone());

    // Take the child FROM the session INSIDE the slot so its exit can be awaited
    // concurrently. We need the child for the wait_task, but the slot's session
    // lock must be used for resize. Taking the child here means try_wait in the
    // slot's session returns None (child gone), which is fine — reaper uses
    // slot.try_wait() for already-orphaned sessions only.
    let child = {
        let mut guard = slot.session.lock().unwrap();
        guard.take_child().context("session has no child to wait on")?
    };
    // Wait for the shell exit on a dedicated task; the JoinHandle resolves once
    // with the exit code (SESS-08). On orphan we DETACH (not abort) so the shell
    // keeps running; the reaper observes exit via the slot's try_wait seam
    // (which uses the held child — but since we took the child here, we re-put
    // a None; the reaper falls back to SIGHUP+drop). See Pitfall #7.
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

    // Pump until the shell exits, the client closes cleanly, or the transport
    // is lost (D-02). The outcome drives the post-loop teardown/orphan split.
    let session_end: SessionEnd = loop {
        tokio::select! {
            // Shell exited: capture the code and tell the client.
            res = &mut wait_task => {
                break SessionEnd::ShellExited(res.unwrap_or(1));
            }
            // PTY output ready: sequence it into the output buffer and frame it
            // to the client (D-10). Drain remaining output even as the shell is
            // exiting so the last bytes are delivered.
            chunk = out_rx.recv() => {
                match chunk {
                    Some(data) => {
                        // Feed into the sequenced output buffer (D-10) before sending.
                        slot.push_output(&data);
                        if nosh_proto::write_message(&mut send, &Message::PtyData { data })
                            .await
                            .is_err()
                        {
                            // Send failed → transport lost (not a clean close).
                            break SessionEnd::TransportLost;
                        }
                    }
                    None => {
                        // Output pump ended (PTY EOF). Await the exit code.
                        break SessionEnd::ShellExited((&mut wait_task).await.unwrap_or(1));
                    }
                }
            }
            // Client → server frames.
            msg = nosh_proto::read_message(&mut recv) => {
                match msg {
                    Ok(Message::PtyData { data }) => {
                        // Update last_active while client is driving input (D-03).
                        slot.touch();
                        if in_tx.send(data).await.is_err() {
                            break SessionEnd::TransportLost;
                        }
                    }
                    Ok(Message::Resize { cols, rows }) => {
                        // Route resize through the slot delegate (D-02 / plan notes).
                        slot.touch();
                        if let Err(e) = slot.resize(cols, rows) {
                            tracing::warn!("resize failed: {e}");
                        } else {
                            tracing::debug!(cols, rows, "resize");
                        }
                    }
                    Ok(Message::SessionClose { .. }) | Ok(Message::SessionOpen { .. }) => {
                        // Client sent an explicit close (or unexpected reopen).
                        // D-01: explicit SessionClose → teardown, NOT orphan.
                        break SessionEnd::ClientClosed;
                    }
                    Err(_) => {
                        // Stream/connection closed without a SessionClose → transport loss.
                        // D-02: this is NOT a clean close; orphan the session (Pitfall #7).
                        break SessionEnd::TransportLost;
                    }
                }
            }
        }
    };

    // Stop the input pump channel (unblocks the writer task).
    drop(in_tx);

    match session_end {
        SessionEnd::ShellExited(exit_code) => {
            // Shell exited normally: drain ALL remaining PTY output (the output
            // reader thread closes `out_rx` on PTY EOF, so recv() eventually
            // yields None), then deliver the exit code and close cleanly with a
            // structured reason (SESS-08/09). Draining to channel close avoids a
            // race where the shell's final bytes are still in flight.
            loop {
                match tokio::time::timeout(Duration::from_millis(200), out_rx.recv()).await {
                    Ok(Some(data)) => {
                        slot.push_output(&data);
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
            // Shell already exited — remove the slot from the registry (D-01).
            registry.remove(&identity_raw, session_id);
        }

        SessionEnd::ClientClosed => {
            // Client sent an explicit SessionClose (typing exit/quitting cleanly).
            // D-01: must NOT leave a lingering session. SIGHUP the shell and reap.
            tracing::info!("client closed session; reaping shell");
            // The wait_task owns the child; SIGHUP via the slot's session sighup
            // (which SIGHUPs by child_pid, which is still recorded on the Session
            // even though the child was taken).
            slot.sighup();
            let _ = tokio::time::timeout(Duration::from_secs(5), &mut wait_task).await;
            conn.close(CLOSE_OK.into(), b"client closed");
            // Clean close — remove from registry immediately (D-01).
            registry.remove(&identity_raw, session_id);
        }

        SessionEnd::TransportLost => {
            // Transport-level disconnect (network loss, crash, failed send/recv).
            // D-02 / Pitfall #7: do NOT SIGHUP, do NOT reap, do NOT drop the
            // Session — the MasterPty stays open because the Session lives on
            // inside the slot held by the registry.
            tracing::info!("transport lost; orphaning session (PTY kept alive, no SIGHUP)");
            // Transition the slot to Orphaned; the registry enforces the cap.
            registry.orphan(&slot);

            // EXIT-DETECTION (Phase 5 BLOCKER fix): the shell child was taken
            // into `wait_task`, so the slot's `try_wait()` is permanently None
            // and the reaper can never see this orphan's shell exit. Instead of
            // detaching `wait_task` (which would leak the SessionSlot + MasterPty
            // forever under the default idle_timeout=0), spawn a watcher that
            // KEEPS the shell running, awaits its eventual exit, then removes the
            // specific slot instance from the registry — releasing the MasterPty
            // and freeing the per-identity cap slot.
            //
            // `remove_slot` is instance-keyed (Arc::ptr_eq), so once Phase 6
            // reattach swaps a live connection onto a slot, a stale watcher from
            // a prior orphan generation cannot evict the reattached slot. It is
            // also idempotent: if the LRU cap already evicted this slot (and
            // SIGHUP'd it), the watcher's later removal is a harmless no-op.
            let watcher_registry = registry.clone();
            let watcher_slot = slot.clone();
            tokio::spawn(async move {
                // Await the shell's own exit — do NOT abort; the shell must keep
                // running while the orphan is alive (preserves SC#1).
                let _exit = wait_task.await;
                tracing::info!(
                    session_id = %watcher_slot.session_id,
                    "orphaned shell exited; removing slot (PTY released)"
                );
                watcher_registry.remove_slot(&watcher_slot);
            });
        }
    }

    // Best-effort: ensure the blocking I/O tasks unwind (the PTY master fd stays
    // open via the slot on the orphan path — aborting the I/O bridge tasks does
    // not close it).
    output_reader.abort();
    input_writer.abort();
    Ok(())
}

/// Extract the `NoshPublicKey` from the peer's TLS client cert after the
/// handshake completes. Returns `None` if the peer has no identity, the
/// downcast fails, or the cert is not a valid Ed25519 SPKI.
///
/// Used by `handle_connection` to enforce D-04/D-05: identity is extracted
/// before any session work, and the connection is closed if extraction fails.
fn extract_peer_identity(conn: &quinn::Connection) -> Option<nosh_auth::NoshPublicKey> {
    let certs = conn
        .peer_identity()?
        .downcast::<Vec<CertificateDer<'static>>>()
        .ok()?;
    let leaf = certs.first()?;
    let spki = nosh_auth::keys::extract_spki_from_cert(leaf).ok()?;
    nosh_auth::nosh_key_from_spki(&spki)
}

/// Treat orderly connection teardown as a clean loop exit, not an error.
fn clean_exit(e: quinn::ConnectionError) -> anyhow::Result<()> {
    use quinn::ConnectionError::*;
    match e {
        ApplicationClosed(_) | LocallyClosed | ConnectionClosed(_) | TimedOut => Ok(()),
        other => Err(other.into()),
    }
}

#[cfg(test)]
mod tests {
    /// CLOSE_AUTH defensive branch: verify the building blocks that
    /// `extract_peer_identity` delegates to correctly return `None` for
    /// non-Ed25519 / malformed SPKI bytes, triggering the CLOSE_AUTH path.
    ///
    /// `extract_peer_identity` itself cannot be called in unit tests because
    /// `quinn::Connection` is not mockable. This test exercises the exact logic
    /// path: `nosh_key_from_spki(spki)` returns `None` for bad input, which is
    /// the condition that drives `handle_connection` to emit CLOSE_AUTH and
    /// `return Ok(())` without opening a session.
    #[test]
    fn extract_peer_identity_none_path_building_blocks() {
        // Wrong length → None (would trigger CLOSE_AUTH in handle_connection).
        assert!(
            nosh_auth::nosh_key_from_spki(&[0u8; 43]).is_none(),
            "43-byte SPKI must produce None → CLOSE_AUTH"
        );
        assert!(
            nosh_auth::nosh_key_from_spki(&[]).is_none(),
            "empty SPKI must produce None → CLOSE_AUTH"
        );
        // Wrong OID prefix → None.
        let mut bad_spki = nosh_auth::keys::ed25519_spki_der(&[1u8; 32]);
        bad_spki[0] ^= 0xff;
        assert!(
            nosh_auth::nosh_key_from_spki(&bad_spki).is_none(),
            "wrong SPKI prefix must produce None → CLOSE_AUTH"
        );
        // Valid Ed25519 SPKI → Some (the happy-path: identity extraction succeeds,
        // CLOSE_AUTH is NOT triggered, and the key matches what was put in).
        let key = nosh_auth::NoshPublicKey::from_raw([0x55u8; 32]);
        let spki = key.spki_der();
        let extracted = nosh_auth::nosh_key_from_spki(&spki)
            .expect("valid Ed25519 SPKI must extract successfully (no CLOSE_AUTH)");
        assert_eq!(
            extracted, key,
            "extracted identity must equal the original key (IDENT-01)"
        );
    }
}
