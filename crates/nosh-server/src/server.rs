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

    // D-01 / Pitfall #1 (ROAM-01): set migration(true) EXPLICITLY even though it
    // is the quinn default. A future quinn release could change this default, or a
    // stray audit edit could clear it, silently disabling connection migration
    // (roaming). Explicit is safe; implicit would kill the whole roaming value prop
    // without a compiler or test failure to catch it.
    server_config.migration(true);

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
    let (send, mut recv) = match conn.accept_bi().await {
        Ok(pair) => pair,
        Err(e) => return clean_exit(e),
    };

    // Phase 6 (D-04): dispatch on the first frame — SessionOpen → fresh session,
    // Reattach → reattach path, anything else → protocol close.
    match nosh_proto::read_message(&mut recv).await {
        Ok(Message::SessionOpen { term, cols, rows, env }) => {
            run_session(conn, peer, peer_identity, send, recv, SessionOpenParams {
                term, cols, rows, client_env: env, shell_override,
            }, registry).await
        }
        Ok(Message::Reattach { token, last_acked_seq }) => {
            run_reattach_session(conn, peer, peer_identity, send, recv, (token, last_acked_seq), registry).await
        }
        Ok(other) => {
            // W3 / D-07: NEVER Debug-log the message — SessionOpened / ReattachOk
            // would print a token. Log only the variant name (no payload).
            tracing::warn!(%peer, frame = other.variant_name(), "expected SessionOpen or Reattach as first frame");
            conn.close(CLOSE_PROTOCOL.into(), b"expected SessionOpen or Reattach");
            Ok(())
        }
        Err(e) => {
            tracing::warn!(%peer, "failed to read first frame: {e}");
            conn.close(CLOSE_PROTOCOL.into(), b"bad first frame");
            Ok(())
        }
    }
}

/// Session-open parameters (collapsed to reduce argument count past clippy's limit).
struct SessionOpenParams {
    term: String,
    cols: u16,
    rows: u16,
    client_env: Vec<(String, String)>,
    shell_override: Option<String>,
}

/// Drive a single PTY session over the established bidi stream.
///
/// Phase 5/6: builds a `SessionSlot`, registers it Active, feeds every outgoing
/// PTY chunk into the slot's `SequencedOutputBuffer`, and at session end
/// subdivides the outcome:
/// - `ShellExited` / `ClientClosed` → immediate teardown + `registry.remove`
/// - `TransportLost` → orphan (NO SIGHUP, keep MasterPty open, D-01/D-02/Pitfall #7)
///
/// Phase 6: after registering the slot, emits `SessionOpened { token }` so the
/// client can reattach later. Also handles `Ack { seq }` frames during the pump
/// loop (D-08 continuous acking).
async fn run_session(
    conn: quinn::Connection,
    peer: SocketAddr,
    identity: nosh_auth::NoshPublicKey,
    mut send: quinn::SendStream,
    mut recv: quinn::RecvStream,
    params: SessionOpenParams,
    registry: Arc<SessionRegistry>,
) -> anyhow::Result<()> {
    let SessionOpenParams { term, cols, rows, client_env, shell_override } = params;
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

    // Phase 6 (D-03): send SessionOpened immediately so the client has the
    // initial reattach token. Token MUST NOT be logged (D-07).
    let initial_token = slot.token();
    if nosh_proto::write_message(&mut send, &Message::SessionOpened { token: initial_token })
        .await
        .is_err()
    {
        // Transport already gone before the session even started.
        registry.remove(&identity_raw, session_id);
        return Ok(());
    }

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

    // Phase 6: store the writer in the slot so a reattach pump can reclaim it
    // on TransportLost. We start with the writer in the slot and take it into
    // the blocking input task; on clean exit we drop it (session over); on
    // TransportLost the input task stores the writer back into the slot when it
    // exits (W2 fix — reliable hand-back, no racy oneshot).
    slot.return_pty_writer(writer);

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
    // writer is taken from the slot. W2 fix: instead of recovering the writer
    // via a racy 200 ms oneshot, the task ALWAYS stores the writer back into the
    // slot when it exits (`in_tx` dropped on any session-end). This guarantees
    // an orphaned slot always has a usable writer, so a later reattach never
    // accepts a session it cannot drive. The task holds its own `Arc` clone of
    // the slot for the hand-back.
    let (in_tx, mut in_rx) = mpsc::channel::<Vec<u8>>(64);
    let writer_for_task = slot.take_pty_writer().expect("writer was just stored in slot");
    let slot_for_writer = slot.clone();
    let mut input_writer = tokio::task::spawn_blocking(move || {
        let mut writer = writer_for_task;
        while let Some(bytes) = in_rx.blocking_recv() {
            if writer.write_all(&bytes).is_err() || writer.flush().is_err() {
                break;
            }
        }
        // Hand the writer back to the slot unconditionally. On TransportLost the
        // reattach pump reclaims it from the slot; on clean exit the slot (and
        // its writer) is dropped with the session — harmless.
        slot_for_writer.return_pty_writer(writer);
    });

    // Pump until the shell exits, the client closes cleanly, or the transport
    // is lost (D-02). The outcome drives the post-loop teardown/orphan split.

    // OBS-01: poll conn.remote_address() to detect connection migration.
    // quinn 0.11 provides no direct migration callback; polling is the only
    // detection mechanism. 500 ms cadence bounds log frequency while remaining
    // responsive to human-visible roaming events.
    let mut last_seen_addr: SocketAddr = conn.remote_address();
    let mut migration_poll = tokio::time::interval(Duration::from_millis(500));
    migration_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

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
            // OBS-01: migration detection — fires at most every 500 ms.
            // Logs an INFO event if the peer address changed (QUIC connection
            // migration). Does NOT break the loop; purely observational.
            _ = migration_poll.tick() => {
                let cur = conn.remote_address();
                if cur != last_seen_addr {
                    tracing::info!(
                        session_id = %session_id,
                        old = %last_seen_addr,
                        new = %cur,
                        "connection migrated"
                    );
                    last_seen_addr = cur;
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
                    // Phase 6: client sends Ack{seq} periodically; trim the output buffer
                    // so acked bytes don't linger (D-08 continuous acking).
                    Ok(Message::Ack { seq }) => {
                        slot.touch();
                        slot.trim_acked(seq);
                    }
                    Ok(Message::SessionOpened { .. })
                    | Ok(Message::Reattach { .. })
                    | Ok(Message::ReattachOk { .. })
                    | Ok(Message::ReattachErr) => {
                        // Unexpected reattach control frames in a live session: treat as protocol error.
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

            // W2 fix: the input task stores the writer back into the slot on
            // exit. The `drop(in_tx)` above unblocks it; AWAIT its completion so
            // the writer is guaranteed to be in the slot BEFORE we orphan — no
            // racy 200 ms timeout that could leave the orphan writer-less and
            // permanently un-reattachable. We bound the await generously in case
            // the task is blocked inside a PTY write; on the rare timeout the
            // slot may lack a writer, and a later reattach will cleanly reject
            // (take_pty_writer None → re-orphan) rather than wedge.
            let _ = tokio::time::timeout(Duration::from_secs(5), &mut input_writer).await;

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

/// Phase 6: handle a cold reattach on a fresh QUIC connection (D-03/D-04/D-06).
///
/// 1. Authorize via `registry.reattach` (two-factor: token + TLS identity).
/// 2. Rotate the token and send `ReattachOk { new_token, replaying_from_seq, truncated }`.
/// 3. Replay buffered output (seq > last_acked_seq) as `PtyData` frames.
/// 4. Reclaim the PTY reader/writer from the slot and run the pump loop.
/// 5. On success, mark the slot `Active`; on failure at any step, re-orphan.
///
/// ALL rejection causes emit the same opaque `ReattachErr` wire frame (D-07
/// no-oracle invariant). Token and new_token are NEVER logged.
async fn run_reattach_session(
    conn: quinn::Connection,
    peer: SocketAddr,
    identity: nosh_auth::NoshPublicKey,
    mut send: quinn::SendStream,
    mut recv: quinn::RecvStream,
    reattach_params: ([u8; 16], u64), // (token, last_acked_seq)
    registry: Arc<crate::registry::SessionRegistry>,
) -> anyhow::Result<()> {
    use crate::registry::SessionSlot;
    let (token, last_acked_seq) = reattach_params;

    // ── Step 1: Two-factor reattach authorization ─────────────────────────────
    let slot = match registry.reattach(&token, &identity) {
        Ok(s) => s,
        Err(_) => {
            // ALL rejection causes take this identical path (D-07 no-oracle).
            // Log identity fingerprint only; never the token.
            tracing::info!(identity = %identity.fingerprint(), "reattach rejected");
            let _ = nosh_proto::write_message(&mut send, &Message::ReattachErr).await;
            // Finish the send stream so the client can read the ReattachErr frame
            // before the connection is closed.
            let _ = send.finish();
            let _ = tokio::time::timeout(Duration::from_millis(200), send.stopped()).await;
            conn.close(CLOSE_PROTOCOL.into(), b"reattach rejected");
            return Ok(());
        }
    };

    let session_id = slot.session_id;
    let identity_raw = *slot.identity.key32();
    let fingerprint = slot.identity.fingerprint();

    let span = tracing::info_span!(
        "reattach",
        %session_id,
        %peer,
        identity = %fingerprint,
    );
    let _enter = span.enter();

    // Helper: re-orphan the slot if we fail mid-rebind (slot is Reconnecting;
    // transition it back to Orphaned so it can be reattached again).
    let re_orphan = |slot: &Arc<SessionSlot>, registry: &Arc<crate::registry::SessionRegistry>| {
        registry.orphan(slot);
    };

    // ── Step 2: Compute replay, send ReattachOk, THEN commit the rotated token ─
    let (chunks, replaying_from_seq, truncated) = slot.replay_from(last_acked_seq);
    // W1 fix: mint a token CANDIDATE without rotating yet. The prior token stays
    // valid until the ReattachOk carrying this candidate is confirmed sent. If
    // the send fails, we re-orphan WITHOUT committing, so the client (which
    // still holds the prior token) can retry indefinitely (D-10). Committing
    // before the send — as the old code did — would, on send failure, leave the
    // slot holding a token the client never received → permanently
    // un-reattachable. MUST NOT be logged (D-07).
    let new_token = slot.mint_token_candidate();

    if nosh_proto::write_message(
        &mut send,
        &Message::ReattachOk { new_token, replaying_from_seq, truncated },
    )
    .await
    .is_err()
    {
        // ReattachOk never reached the client: the client still holds the prior
        // token. Do NOT commit the candidate — re-orphan with the token intact.
        re_orphan(&slot, &registry);
        return Ok(());
    }
    // ReattachOk is on the wire (reliable stream); the client will adopt
    // `new_token`. Commit it now so the slot and client agree on the live token.
    // The client updates its stored token the instant it reads ReattachOk, so
    // the rotation MUST be committed here — not deferred past replay, which
    // could fail after the client has already adopted the new token.
    slot.commit_token(new_token);

    // ── Step 3: Replay buffered output (D-09 no dup/gap within retained range) ─
    for (_seq, data) in &chunks {
        if nosh_proto::write_message(&mut send, &Message::PtyData { data: data.to_vec() })
            .await
            .is_err()
        {
            re_orphan(&slot, &registry);
            return Ok(());
        }
    }
    tracing::info!(
        replaying_from_seq,
        chunks = chunks.len(),
        truncated,
        "replay complete"
    );

    // ── Step 4: Reclaim PTY reader/writer ────────────────────────────────────
    // Reader: clone a new reader from the master (drain any bytes that
    // accumulated in the kernel PTY buffer while the session was orphaned).
    let reader = match slot.clone_pty_reader() {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("failed to clone PTY reader for reattach: {e}");
            re_orphan(&slot, &registry);
            return Ok(());
        }
    };

    // Writer: take from the slot (stored by the prior TransportLost path).
    let writer = match slot.take_pty_writer() {
        Some(w) => w,
        None => {
            tracing::warn!("PTY writer not available for reattach (session may have exited)");
            re_orphan(&slot, &registry);
            return Ok(());
        }
    };

    // ── Step 5: Transition to Active and start pump ──────────────────────────
    slot.mark_active();
    tracing::info!("reattach successful; session is Active");

    // Store the writer back in the slot for the next potential TransportLost.
    // We then follow the same pump pattern as run_session.
    slot.return_pty_writer(writer);

    // OUTPUT pump.
    let (out_tx, mut out_rx) = mpsc::channel::<Vec<u8>>(64);
    let mut reader = reader;
    let output_reader = tokio::task::spawn_blocking(move || {
        let mut buf = [0u8; PTY_CHUNK];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if out_tx.blocking_send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    // INPUT pump. W2 fix: store the writer back into the slot on exit (same as
    // run_session) so an orphaned-then-reattached session always has a usable
    // writer — no racy oneshot recovery.
    let (in_tx, mut in_rx) = mpsc::channel::<Vec<u8>>(64);
    let writer_for_task = slot.take_pty_writer().expect("writer was just stored");
    let slot_for_writer = slot.clone();
    let mut input_writer = tokio::task::spawn_blocking(move || {
        let mut w = writer_for_task;
        while let Some(bytes) = in_rx.blocking_recv() {
            if w.write_all(&bytes).is_err() || w.flush().is_err() {
                break;
            }
        }
        slot_for_writer.return_pty_writer(w);
    });

    // The wait_task is the orphan-exit watcher (wait_task from the original
    // run_session) — still alive and Arc::ptr_eq-bound to this slot. We do NOT
    // re-spawn a second wait_task here; the original watcher remains the durable
    // shell-exit observer. When the shell exits eventually, the original watcher
    // will call registry.remove_slot (idempotent). For the reattach pump we only
    // need a way to detect shell exit; we use a separate task that non-blockingly
    // polls every 500ms (the child was taken, so try_wait is None — but we can
    // poll the output channel EOF as the shell-exit signal).
    // The output pump closes when the PTY EOF is hit (shell exited or closed).

    let session_end: SessionEnd = loop {
        tokio::select! {
            chunk = out_rx.recv() => {
                match chunk {
                    Some(data) => {
                        slot.push_output(&data);
                        if nosh_proto::write_message(&mut send, &Message::PtyData { data })
                            .await
                            .is_err()
                        {
                            break SessionEnd::TransportLost;
                        }
                    }
                    None => {
                        // PTY EOF: shell exited. Shell exit code is tracked by the
                        // original wait_task watcher; we close with code 0 (approximate).
                        break SessionEnd::ShellExited(0);
                    }
                }
            }
            msg = nosh_proto::read_message(&mut recv) => {
                match msg {
                    Ok(Message::PtyData { data }) => {
                        slot.touch();
                        if in_tx.send(data).await.is_err() {
                            break SessionEnd::TransportLost;
                        }
                    }
                    Ok(Message::Resize { cols, rows }) => {
                        slot.touch();
                        if let Err(e) = slot.resize(cols, rows) {
                            tracing::warn!("resize failed on reattach: {e}");
                        }
                    }
                    Ok(Message::SessionClose { .. }) => {
                        break SessionEnd::ClientClosed;
                    }
                    Ok(Message::Ack { seq }) => {
                        slot.touch();
                        slot.trim_acked(seq);
                    }
                    Ok(_) => {} // ignore unexpected frames
                    Err(_) => {
                        break SessionEnd::TransportLost;
                    }
                }
            }
        }
    };

    drop(in_tx);

    match session_end {
        SessionEnd::ShellExited(_exit_code) => {
            tracing::info!("shell exited during reattach session");
            // The original watcher will call remove_slot. Send SessionClose.
            // We don't have the exact exit code (the original wait_task has it);
            // send 0 as approximate. The client will see the connection close.
            let _ = nosh_proto::write_message(
                &mut send,
                &Message::SessionClose {
                    exit_code: 0,
                    reason: "shell exited".to_string(),
                },
            )
            .await;
            let _ = send.finish();
            let _ = tokio::time::timeout(Duration::from_secs(2), send.stopped()).await;
            conn.close(CLOSE_OK.into(), b"shell exited");
            // Remove the slot (the original watcher may also do this; remove is idempotent).
            registry.remove(&identity_raw, session_id);
        }
        SessionEnd::ClientClosed => {
            tracing::info!("client closed reattach session");
            slot.sighup();
            conn.close(CLOSE_OK.into(), b"client closed");
            registry.remove(&identity_raw, session_id);
        }
        SessionEnd::TransportLost => {
            tracing::info!("transport lost during reattach; re-orphaning");
            // W2 fix: await the input task so it stores the writer back into the
            // slot BEFORE we orphan — the re-orphaned slot always has a usable
            // writer for the next reattach.
            let _ = tokio::time::timeout(Duration::from_secs(5), &mut input_writer).await;
            registry.orphan(&slot);
            // The original exit watcher is still alive; no new watcher needed.
        }
    }

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
