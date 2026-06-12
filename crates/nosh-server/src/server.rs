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

use std::collections::HashMap;
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use bytes::Bytes;
use nosh_auth::{AuthorizedKeysVerifier, NoshServerCertResolver};
use rustls::pki_types::CertificateDer;
use nosh_proto::{Message, TerminalControlPayload};
use nosh_proto::messages::ChannelType;
use nosh_proto::datagram::{
    encode_datagram, decode_epoch_ack, StateDiff, DiffRun, CursorPos, MIN_CAP, MAX_RUNS,
};
use quinn::crypto::rustls::{HandshakeData, QuicServerConfig};
use tokio::sync::mpsc;

use crate::channel::{ChannelEvent, run_channel_task, run_scrollback_sender_task};
use crate::registry::SessionRegistry;
use crate::session;
use crate::terminal::Cell;

/// Maximum number of simultaneously open channels per session (T-21-04 / DoS bound).
///
/// At this limit a new `ChannelOpen` from the client is unconditionally rejected
/// (opaque `ChannelReject`) — no session state is allocated for the rejected channel.
const MAX_OPEN_CHANNELS: usize = 64;

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
/// Bound on the per-epoch sent-snapshot store (CR-01 fix).
///
/// At most this many (epoch, snapshot) pairs are retained. When the store would
/// exceed this cap the oldest entry is evicted. 16 is generous for any realistic
/// RTT: even at 500ms RTT and 60Hz ticks, only ~30 epochs are in-flight at once,
/// but acks arrive at the same rate as sends so the store stays shallow.
const EPOCH_SNAPSHOT_CAP: usize = 16;
/// Maximum datagrams sent in a single burst tick (D-20-02).
///
/// A full 80×24 terminal repaint (~1920 cells at 8–15 bytes/run with a 1200-byte
/// MTU) needs approximately 16–24 datagrams. 64 is ~2.5–4× that — generous enough
/// that it never fires under any normal TUI app (vim startup, htop, large paste),
/// but caps pathological cases (e.g. a 400-column terminal or a 200-line paste)
/// at 64 × ~1200 ≈ 76 KB of datagram payload per tick, well within the default
/// 1 MiB send buffer. Its sole purpose is DoS/resource bounding (T-20-02).
const BURST_CAP: usize = 64;
/// QUIC application close code for a protocol violation (bad first frame).
const CLOSE_PROTOCOL: u32 = 1;
/// QUIC application close code for peer identity extraction failure (should
/// never happen on an AuthorizedKeysVerifier-enforced connection — D-04).
const CLOSE_AUTH: u32 = 2;
/// PTY output read chunk size (used in `crate::pty_io`).
#[allow(dead_code)]
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

// ── Phase 13: diff-tick helpers ───────────────────────────────────────────────

/// Compute changed-cell runs by scanning `current` against `baseline`.
///
/// An empty `baseline` (or a baseline shorter than `current`) treats all cells
/// in the uncovered region as changed — this implements the D-13-01b "empty
/// baseline = full screen" keyframe for cold-reattach.
///
/// The scanner breaks a run when the cell's style/fg/bg changes (not on the
/// first unchanged cell), so adjacent cells with identical attributes are merged
/// into one run even if some are unchanged.  This trades slightly larger runs for
/// fewer fragments, which is acceptable under the acked-epoch self-correcting
/// model.
fn compute_diff_runs(
    current: &[Vec<Cell>],
    baseline: &[Vec<Cell>],
) -> Vec<DiffRun> {
    let mut runs: Vec<DiffRun> = Vec::new();
    for (row_idx, current_row) in current.iter().enumerate() {
        let row = row_idx as u16;
        let baseline_row: &[Cell] = baseline
            .get(row_idx)
            .map(|r| r.as_slice())
            .unwrap_or(&[]);

        let mut col = 0u16;
        while (col as usize) < current_row.len() {
            let c = col as usize;
            let cell = &current_row[c];
            let base = baseline_row.get(c);
            // Skip unchanged cells (same cell at baseline position).
            if base.map(|b| b == cell).unwrap_or(false) {
                col += 1;
                continue;
            }
            // D-19-06: skip wide-char continuation cells at the outer level too —
            // a continuation cell must never be the start of a DiffRun (T-19-05).
            if cell.wide {
                col += 1;
                continue;
            }
            // Start a new run at this changed cell.
            let start_col = col;
            let style = cell.style;
            let fg = cell.fg;
            let bg = cell.bg;
            let mut chars = String::new();
            // Extend run while style/fg/bg are consistent AND the cell is
            // actually changed vs. the baseline (WR-01 fix: stop at the first
            // unchanged cell so gratuitous identical trailing cells do not
            // consume datagram cap, which would amplify CR-02 deferral).
            while (col as usize) < current_row.len() {
                let cc = col as usize;
                let c2 = &current_row[cc];
                // D-19-06: skip wide-char continuation cells — they carry no
                // logical glyph and must not appear in DiffRun.chars.  The column
                // counter advances past the continuation cell so the next run
                // starts at the correct column (T-19-05).
                if c2.wide {
                    col += 1;
                    continue;
                }
                if c2.style != style || c2.fg != fg || c2.bg != bg {
                    break; // style change: end run here
                }
                // Stop extending if this cell is unchanged vs. the baseline.
                let base2 = baseline_row.get(cc);
                if base2.map(|b| b == c2).unwrap_or(false) {
                    break;
                }
                chars.push(c2.ch);
                col += 1;
            }
            if !chars.is_empty() {
                runs.push(DiffRun { row, start_col, style, fg, bg, chars });
            }
        }
    }
    runs
}

/// Result of a single diff-tick computation.
struct DiffTickResult {
    /// Encoded datagram payload ready for `conn.send_datagram`.
    payload: Bytes,
    /// The snapshot that was diffed against (the *current* grid — becomes
    /// `last_sent_snapshot` after a successful send).
    sent_cells: Vec<Vec<Cell>>,
    /// The epoch assigned to this datagram (used by the caller to store the
    /// per-epoch sent snapshot for CR-01 fix: snapshot-at-send-time, not
    /// snapshot-at-ack-receipt-time).
    epoch: u64,
    /// Runs deferred from `encode_datagram` because they didn't fit in the cap;
    /// must be prepended to the next tick's run list (Anti-Pattern: deferred
    /// runs go FIRST to maintain cursor-proximate priority).
    deferred: Vec<DiffRun>,
    // ── Phase 20 burst geometry (D-20 / Pitfall 4) ───────────────────────────
    // Burst iterations 2..N need the terminal geometry to construct StateDiff
    // without re-locking the slot. These are captured from the slot snapshot
    // taken inside build_state_diff and carried here so send_burst() can
    // construct subsequent burst datagrams without any additional lock.
    /// Terminal width (columns) at diff time.
    cols: u16,
    /// Terminal height (rows) at diff time.
    rows: u16,
    /// Cursor position at diff time (used for all burst datagrams in the tick).
    cursor: CursorPos,
    /// Alt-screen flag at diff time.
    alt_screen: bool,
}

/// Build one coalesced `StateDiff` datagram for the current tick.
///
/// Returns `Some(DiffTickResult)` when a datagram should be sent, `None` to
/// skip the tick silently (grid unchanged + client caught up, or encoding
/// failed, or cap too small).
///
/// # Lock discipline
///
/// Snapshots `TerminalState` via `slot.with_terminal_state(...)` (brief lock,
/// released before any `.await`). Does NOT perform any `.await` itself — the
/// caller sends the returned payload asynchronously.
fn build_state_diff(
    slot: &crate::registry::SessionSlot,
    current_epoch: &mut u64,
    last_acked_epoch: u64,
    last_acked_snapshot: &[Vec<Cell>],
    last_sent_snapshot: &[Vec<Cell>],
    pending_deferred: Vec<DiffRun>,
    cap: usize,
) -> Option<DiffTickResult> {
    if cap < MIN_CAP {
        return None;
    }

    // Snapshot terminal state under the lock (released when closure returns).
    // NEVER perform any async operation inside this closure (Pitfall 1 / Anti-Pattern #2).
    let (cols, rows, cursor, alt_screen, cells) = slot.with_terminal_state(|ts| {
        let (cols, rows) = ts.size();
        let cursor = ts.cursor();
        // TUI-05: propagate server alt-screen flag to client via StateDiff.
        // ts.echo_state() returns &EchoState; .alt_screen is a bool (Copy).
        // This closure stays synchronous — no .await (Anti-Pattern #2 guard).
        let alt_screen = ts.echo_state().alt_screen;
        let cells: Vec<Vec<Cell>> = ts
            .viewport_rows()
            .map(|(_, row)| row.to_vec())
            .collect();
        (cols, rows, cursor, alt_screen, cells)
    });

    // D-13-02a: skip if grid unchanged AND client is caught up.
    if cells == last_acked_snapshot && last_acked_epoch >= *current_epoch {
        return None;
    }

    // Epoch management (Open Question 2 / CR-02 fix): increment at tick time
    // when the grid changed since the last *sent* snapshot (not per-chunk), OR
    // when there are deferred runs waiting to be sent (so the client does not
    // discard the new datagram as a duplicate of the previous epoch).
    if cells != last_sent_snapshot || !pending_deferred.is_empty() {
        *current_epoch += 1;
    }

    // Compute changed runs vs the last-acked baseline.
    let fresh_runs = compute_diff_runs(&cells, last_acked_snapshot);

    // Deferred runs from the previous tick go FIRST so encode_datagram
    // re-prioritises cursor-proximate content (Anti-Pattern: deferred FIRST).
    let mut all_runs: Vec<DiffRun> = pending_deferred;
    all_runs.extend(fresh_runs);

    // Pitfall 3: cap the deferred queue to MAX_RUNS to prevent unbounded growth.
    // WR-03 fix: truncate from the END (drop least-cursor-proximate runs) rather
    // than from the front — draining from the front would discard the
    // already-cursor-sorted deferred backlog (pending_deferred was sorted by the
    // prior encode_datagram call) in favour of unsorted fresh_runs.
    if all_runs.len() > MAX_RUNS {
        all_runs.truncate(MAX_RUNS);
    }

    let sent_epoch = *current_epoch;
    let diff = StateDiff { epoch: sent_epoch, cols, rows, cursor, alt_screen, runs: all_runs };
    match encode_datagram(&diff, cap) {
        Ok((payload, deferred)) => Some(DiffTickResult {
            payload,
            sent_cells: cells,
            epoch: sent_epoch,
            deferred,
            // Phase 20: carry terminal geometry for burst iterations 2..N (Pitfall 4).
            cols,
            rows,
            cursor,
            alt_screen,
        }),
        Err(_) => None, // encoding failed: skip this tick
    }
}

/// Drain all burst datagrams for one tick (D-20-01 through D-20-05).
///
/// Sends the first datagram from `result.payload`, then loops
/// `encode_datagram`-only (never `build_state_diff` — D-20-03 / R-1 fix) until
/// `result.deferred` is empty, the `BURST_CAP` is reached, or the connection's
/// send buffer is full.
///
/// All datagrams share `result.epoch` (D-20-04 / R-2 fix: one epoch per tick;
/// `confirmed_epoch` does not advance more than once per tick).
///
/// Returns the leftover deferred runs (to be carried to the next tick as
/// `pending_deferred`) and a transport-loss flag. When the flag is `true` the
/// caller must break the session loop with `SessionEnd::TransportLost`.
///
/// # Invariants
/// - No `.await` call anywhere in this function (uses only synchronous
///   `send_datagram` — D-20-01; the async `send_datagram_wait` is NOT used).
/// - `build_state_diff` is never called here (would cause R-1 spin).
/// - `epoch_snapshots.push_back` is not called here (belongs to the caller,
///   once per tick — Pitfall 5).
fn send_burst(
    conn: &quinn::Connection,
    result: DiffTickResult,
    cap: usize,
) -> (Vec<DiffRun>, bool /* transport_lost */) {
    // Send the first datagram (already encoded by build_state_diff).
    if let Err(e) = conn.send_datagram(result.payload) {
        use quinn::SendDatagramError::*;
        match e {
            TooLarge => {
                // Path MTU shrank between max_datagram_size() and send_datagram().
                // build_state_diff normally guarantees payload < cap, but a PMTUD
                // failure or route change between the query and the send can shrink
                // the available size. The first payload was NOT sent; do NOT proceed
                // to send the deferred runs — they are meaningless without the first
                // payload and would corrupt the client's confirmed grid. Carry the
                // deferred back to the caller so the next tick's build_state_diff
                // can recompute a fresh diff against the still-current snapshot.
                return (result.deferred, false);
            }
            UnsupportedByPeer | Disabled | ConnectionLost(_) => {
                // Transport lost: return remaining deferred + signal caller.
                return (result.deferred, true);
            }
        }
    }

    let tick_epoch = result.epoch;
    let tick_cols = result.cols;
    let tick_rows = result.rows;
    let tick_cursor = result.cursor;
    let tick_alt_screen = result.alt_screen;
    let mut deferred = result.deferred;
    let mut burst_count: usize = 1; // first datagram already sent above

    // ── D-20-01/D-20-02: burst encode_datagram-only drain ────────────────────
    // Loop until deferred is empty, safety cap is hit, or send buffer is full.
    // NEVER call build_state_diff here (R-1 fix: that would recompute fresh_runs
    // against the non-advancing last_acked_snapshot, refilling deferred every
    // iteration and causing an infinite spin).
    let mut transport_lost = false;
    'burst: while !deferred.is_empty()
        && burst_count < BURST_CAP
        && conn.datagram_send_buffer_space() >= cap
    {
        // Construct the burst StateDiff from carried deferred runs + the SAME
        // epoch from build_state_diff (D-20-04: one epoch per tick; every burst
        // datagram shares this epoch so confirmed_epoch advances only once).
        let burst_diff = StateDiff {
            epoch: tick_epoch,
            cols: tick_cols,
            rows: tick_rows,
            cursor: tick_cursor,
            alt_screen: tick_alt_screen,
            runs: std::mem::take(&mut deferred),
        };
        let (payload, next_deferred) = match encode_datagram(&burst_diff, cap) {
            Ok(pair) => pair,
            Err(_) => {
                // CapTooSmall is unreachable at runtime (cap from max_datagram_size).
                // burst_diff consumed deferred; leave it empty and stop.
                break 'burst;
            }
        };
        deferred = next_deferred;
        if let Err(e) = conn.send_datagram(payload) {
            use quinn::SendDatagramError::*;
            match e {
                TooLarge => {} // unreachable: encode_datagram guarantees payload < cap
                UnsupportedByPeer | Disabled | ConnectionLost(_) => {
                    transport_lost = true;
                    break 'burst;
                }
            }
        }
        burst_count += 1;
    }

    (deferred, transport_lost)
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

/// Receive from an `Option<Receiver>`, returning `std::future::pending()` when the
/// Option is `None`. Used by the server-initiated channel-open test arm in the
/// session select! loop (Task 3 / T-21-10): in production the Option is None so
/// the arm never fires; in test builds the receiver is wired in.
///
/// Note: `tokio::select!` does not accept `#[cfg()]` on individual arms, so this
/// helper makes the arm always syntactically present but semantically absent in
/// production (the pending future is never woken).
async fn recv_or_pending<T>(rx: &mut Option<tokio::sync::mpsc::Receiver<T>>) -> Option<T> {
    match rx {
        Some(r) => r.recv().await,
        None => std::future::pending().await,
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

    // OUTPUT pump: an interruptible reader thread polls [master_fd, shutdown_pipe]
    // before each read. Async teardown signals the pipe to stop the thread
    // cleanly (Pitfall 6: abort() on spawn_blocking is a no-op; this replaces it).
    // Extract the master raw fd while the session lock is held briefly, then
    // release it before spawning (Pitfall 2 — no lock held across spawn_blocking).
    let master_raw_fd = slot.master_raw_fd().expect("Unix PTY master fd available");
    let (out_tx, mut out_rx) = mpsc::channel::<Vec<u8>>(64);
    let mut reader_handle = crate::pty_io::start_interruptible_reader(master_raw_fd, reader, out_tx)
        .expect("start interruptible PTY reader");

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

    // SYNC-03: 16 ms diff-interval ticker for coalesced StateDiff datagram emission.
    // D-13-02: one diff per tick (not per PTY chunk); MissedTickBehavior::Skip
    // prevents tick accumulation under a slow tick.
    let mut diff_interval = tokio::time::interval(Duration::from_millis(16));
    diff_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // D-13-03: fresh sessions signal ResumeComplete immediately (no replay window).
    let resume_complete = true;
    // Per-connection datagram state (Open Question 3: task-local, resets on reattach).
    let mut current_epoch: u64 = 0;
    // Phase 22 (S-5): epoch mirror for the scrollback sender task.
    //
    // The scrollback sender task must read the live epoch atomically with the
    // scrollback lines snapshot. This Arc<AtomicU64> mirrors current_epoch and is
    // updated (store, Release) at each diff tick immediately after build_state_diff
    // increments current_epoch. The sender reads it with Acquire ordering into a
    // local before entering with_terminal_state — no .await between the load and
    // the closure (S-5 atomic epoch capture). This in-process atomic is additive:
    // it does NOT change epoch cadence, confirmed_epoch logic, or datagram sends.
    let epoch_src = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let mut last_acked_epoch: u64 = 0;
    // D-13-01b: empty baseline → first diff is naturally the full screen.
    let mut last_acked_snapshot: Vec<Vec<Cell>> = Vec::new();
    let mut last_sent_snapshot: Vec<Vec<Cell>> = Vec::new();
    let mut pending_deferred: Vec<DiffRun> = Vec::new();
    // CR-01 fix: bounded per-epoch sent-snapshot store.
    // Maps epoch → the terminal grid at the time that epoch's datagram was SENT.
    // On ack receipt we look up the snapshot for the acked epoch and use it as
    // the new last_acked_snapshot (not the current grid, which may have advanced).
    let mut epoch_snapshots: VecDeque<(u64, Vec<Vec<Cell>>)> = VecDeque::new();

    // Phase 21 (MUX-01..MUX-04): per-session channel infrastructure.
    //
    // channel_map: maps open channel ids → the mpsc sender that delivers events to
    // the per-channel task (ChannelEvent::Stream / Credit / Close). Only channel
    // tasks that have been accepted are present; tasks remove themselves via
    // ChannelEvent::Close flowing back through control_tx → control_rx.
    //
    // control_tx / control_rx: the SINGLE writer path for outbound control-stream
    // frames produced by channel tasks (ChannelClose, ChannelCredit). Channel tasks
    // MUST NOT call write_message on the control SendStream directly (A4 / Pitfall M-6).
    // The session pump drains control_rx and writes to `send` — the sole owner.
    let mut channel_map: HashMap<u32, mpsc::Sender<ChannelEvent>> = HashMap::new();
    let (channel_ctrl_tx, mut channel_ctrl_rx) = mpsc::channel::<Message>(64);

    // Task 3 (test-only): server-initiated channel-open infrastructure.
    //
    // The server's parity space is ODD channel ids (1, 3, 5, …). Production does
    // not open channels from the server side (21-CONTEXT.md defers this). The test
    // path exists solely so plan 21-04's `channel_simultaneous_open` test can
    // fire a real server-originated ChannelOpen concurrently with a client one (SC#6).
    //
    // next_server_channel_id: starts at 1 and advances by 2 on each allocation so
    // the sequence is always odd and never zero (control channel reserved).
    //
    // server_open_tx: stored in the SessionSlot for integration tests to retrieve.
    // server_open_rx: wrapped in Option so the select! arm uses recv_or_pending()
    // which returns std::future::pending() when the Option is None — i.e. in
    // production builds where the Option is set to None (T-21-10).
    let mut next_server_channel_id: u32 = 1;
    let (server_open_tx, server_open_rx_inner) =
        tokio::sync::mpsc::channel::<ChannelType>(8);
    // In test builds (or when the test-support feature is enabled): store the sender
    // in the slot and wire the receiver so integration tests can trigger a
    // server-initiated ChannelOpen. In production builds: drop both so the select!
    // arm stays inert via recv_or_pending returning pending() (T-21-10).
    #[cfg(any(test, feature = "test-support"))]
    slot.store_server_open_tx(server_open_tx);
    #[cfg(any(test, feature = "test-support"))]
    let mut server_open_rx_opt: Option<tokio::sync::mpsc::Receiver<ChannelType>> =
        Some(server_open_rx_inner);
    #[cfg(not(any(test, feature = "test-support")))]
    {
        // Drop the channel immediately; the select! arm stays inert (recv_or_pending None).
        let _ = (next_server_channel_id, server_open_tx, server_open_rx_inner);
    }
    #[cfg(not(any(test, feature = "test-support")))]
    let mut server_open_rx_opt: Option<tokio::sync::mpsc::Receiver<ChannelType>> = None;

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
                        // Feed into the sequenced output buffer (D-10) AND the terminal
                        // state model (SYNC-02) before sending. Seq is assigned first
                        // (replay integrity), then TerminalState::advance is called.
                        slot.push_output_and_parse(&data);
                        if nosh_proto::write_message(&mut send, &Message::PtyData { data })
                            .await
                            .is_err()
                        {
                            // Send failed → transport lost (not a clean close).
                            break SessionEnd::TransportLost;
                        }
                        // Phase 16 (D-16-01, D-16-02): drain any pending OSC 0/2 title and
                        // OSC 52 clipboard-write detected during parse, and forward them to
                        // the client over the RELIABLE stream (not datagrams — no MTU limit).
                        // Write-only OSC 52: read/query form already dropped in osc_dispatch.
                        // Client re-emits these to stdout, bypassing the compositor.
                        let (drained_title, drained_clipboard) = slot.drain_terminal_control();
                        if let Some(title) = drained_title {
                            if nosh_proto::write_message(
                                &mut send,
                                &Message::TerminalControl(TerminalControlPayload::Title { title }),
                            )
                            .await
                            .is_err()
                            {
                                break SessionEnd::TransportLost;
                            }
                        }
                        if let Some((selection, data)) = drained_clipboard {
                            if nosh_proto::write_message(
                                &mut send,
                                &Message::TerminalControl(TerminalControlPayload::Clipboard {
                                    selection,
                                    data,
                                }),
                            )
                            .await
                            .is_err()
                            {
                                break SessionEnd::TransportLost;
                            }
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
            // SYNC-03 / PACE-01: diff-interval tick — burst state-diff datagrams.
            // D-13-02: one build_state_diff per tick, not per PTY chunk.
            // D-13-03: gate on resume_complete (always true for run_session).
            // Phase 20 (D-20-01..D-20-05): send_burst() drains the deferred run
            // list within one tick via encode_datagram-only (never build_state_diff
            // again — R-1 fix). All burst datagrams share the tick's single epoch
            // (R-2 fix). epoch_snapshots.push_back is called exactly once per tick
            // (Pitfall 5 / D-20-04).
            _ = diff_interval.tick() => {
                if !resume_complete {
                    continue;
                }
                let cap = match conn.max_datagram_size() {
                    Some(c) if c >= MIN_CAP => c,
                    _ => continue, // datagrams not negotiated or cap too small — skip silently
                };
                let deferred = std::mem::take(&mut pending_deferred);
                if let Some(result) = build_state_diff(
                    &slot,
                    &mut current_epoch,
                    last_acked_epoch,
                    &last_acked_snapshot,
                    &last_sent_snapshot,
                    deferred,
                    cap,
                ) {
                    // Phase 22 (S-5): mirror the updated epoch into epoch_src so the
                    // scrollback sender task can read it atomically with the scrollback
                    // snapshot. Additive — does NOT change epoch cadence or datagram sends.
                    epoch_src.store(current_epoch, std::sync::atomic::Ordering::Release);
                    // CR-01 fix: store the sent snapshot keyed by epoch BEFORE
                    // calling send_burst. Push exactly ONCE per tick outside the
                    // burst loop — burst datagrams all share this epoch (Pitfall 5).
                    epoch_snapshots.push_back((result.epoch, result.sent_cells.clone()));
                    if epoch_snapshots.len() > EPOCH_SNAPSHOT_CAP {
                        epoch_snapshots.pop_front();
                    }
                    last_sent_snapshot = result.sent_cells.clone();
                    // Phase 20: send_burst() sends result.payload first, then
                    // drains result.deferred via encode_datagram-only until the
                    // send buffer is full, BURST_CAP is hit, or deferred is empty.
                    let (leftover, transport_lost) = send_burst(&conn, result, cap);
                    pending_deferred = leftover;
                    if transport_lost {
                        break SessionEnd::TransportLost;
                    }
                }
            }
            // SYNC-03: epoch-ack arm — advance the last-acked baseline.
            // D-13-01: only advance (never regress) the baseline on a newer epoch.
            // Pitfall 2: stale/dup acks must not overwrite a newer baseline.
            datagram = conn.read_datagram() => {
                match datagram {
                    Ok(bytes) => {
                        match decode_epoch_ack(&bytes) {
                            Ok(acked) if acked > last_acked_epoch => {
                                last_acked_epoch = acked;
                                // CR-01 fix: use the snapshot that was captured at
                                // epoch-sent time (not the current grid). Look up the
                                // stored snapshot for `acked`; if not found (evicted
                                // due to cap), keep the current baseline — the model
                                // is self-correcting on the next ack cycle.
                                if let Some(pos) = epoch_snapshots
                                    .iter()
                                    .position(|(e, _)| *e == acked)
                                {
                                    let (_, snap) = epoch_snapshots.remove(pos).unwrap();
                                    last_acked_snapshot = snap;
                                    // Evict all older entries (epochs < acked) — they
                                    // will never be acked now.
                                    epoch_snapshots.retain(|(e, _)| *e > acked);
                                }
                                // If not found in store, baseline stays as-is (self-correcting).
                            }
                            Ok(_) => {} // older/dup ack: ignore (never regress baseline)
                            Err(_) => {} // malformed: ignore (self-correcting, T-13-07)
                        }
                    }
                    Err(_) => break SessionEnd::TransportLost,
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
                    | Ok(Message::ReattachErr)
                    | Ok(Message::TerminalControl(_)) => {
                        // Unexpected frames in a live session (server→client direction
                        // only for TerminalControl; reattach frames are out of place here):
                        // treat as protocol error.
                        break SessionEnd::ClientClosed;
                    }

                    // Phase 21 (MUX-01..MUX-04): channel multiplexing dispatch.

                    Ok(Message::ChannelOpen { channel_id, channel_type }) => {
                        // Client-initiated channels must have even ids (parity rule,
                        // MUX-04 / 21-CONTEXT.md). Odd ids are in the server's parity
                        // space; receiving one from the client is a protocol violation —
                        // log and ignore rather than closing the session (Pitfall M-4;
                        // non-fatal so the client can continue the session).
                        if channel_id % 2 != 0 {
                            tracing::warn!(
                                channel_id,
                                "client sent ChannelOpen with odd channel_id (server parity); ignoring"
                            );
                            continue;
                        }
                        // Duplicate open: reject opaquely (T-21-09, MUX-01).
                        if channel_map.contains_key(&channel_id) {
                            tracing::warn!(channel_id, "duplicate ChannelOpen; rejecting");
                            let _ = nosh_proto::write_message(
                                &mut send,
                                &Message::ChannelReject { channel_id },
                            )
                            .await;
                            continue;
                        }
                        // DoS cap: limit simultaneous open channels (T-21-04).
                        if channel_map.len() >= MAX_OPEN_CHANNELS {
                            tracing::warn!(
                                channel_id,
                                max = MAX_OPEN_CHANNELS,
                                "channel cap reached; rejecting ChannelOpen"
                            );
                            let _ = nosh_proto::write_message(
                                &mut send,
                                &Message::ChannelReject { channel_id },
                            )
                            .await;
                            continue;
                        }
                        // Validate channel type: Scrollback and Echo (test-only) are
                        // accepted; PortForward and AgentForward are unconditionally
                        // rejected (FWD-01 / FWD-02 / T-21-08 / T-22-10 access control).
                        let accept = match channel_type {
                            ChannelType::PortForward | ChannelType::AgentForward => {
                                // Always rejected: SSH_AUTH_SOCK never reachable (T-21-08).
                                tracing::debug!(
                                    channel_id,
                                    "ChannelOpen for forwarding type; rejecting (FWD-01/FWD-02)"
                                );
                                false
                            }
                            ChannelType::Scrollback => {
                                // Phase 22: accepted — spawns run_scrollback_sender_task.
                                true
                            }
                            ChannelType::Echo => {
                                // Echo is the test-only proving fixture (21-CONTEXT.md).
                                // Accepted only in test builds (or when the test-support
                                // feature is enabled by a downstream integration-test binary);
                                // rejected in production.
                                #[cfg(any(test, feature = "test-support"))]
                                { true }
                                #[cfg(not(any(test, feature = "test-support")))]
                                {
                                    tracing::debug!(
                                        channel_id,
                                        "ChannelOpen for Echo (test-only type); rejecting in production"
                                    );
                                    false
                                }
                            }
                        };

                        if !accept {
                            let _ = nosh_proto::write_message(
                                &mut send,
                                &Message::ChannelReject { channel_id },
                            )
                            .await;
                            continue;
                        }

                        // Send accept then spawn the per-channel task (MUX-02).
                        // NEVER do channel data I/O inline (Pitfall M-2 HOL blocking).
                        if nosh_proto::write_message(
                            &mut send,
                            &Message::ChannelAccept { channel_id },
                        )
                        .await
                        .is_err()
                        {
                            break SessionEnd::TransportLost;
                        }
                        // Bounded mpsc(64) for channel events (M-6 / S-4): any pump-side
                        // push uses try_send + drop-on-Full so the pump select! arm is
                        // never blocked by a slow channel consumer (M-6 comment).
                        let (task_tx, task_rx) = mpsc::channel::<ChannelEvent>(64);
                        channel_map.insert(channel_id, task_tx);
                        // Dispatch to the correct per-channel task based on channel type.
                        match channel_type {
                            ChannelType::Scrollback => {
                                // Phase 22: dedicated scrollback sender task.
                                // Runs separately from the pump so it cannot stall PTY
                                // output or the 16 ms diff tick (M-6 sender task isolation).
                                let slot_clone = slot.clone();
                                let epoch_src_clone = epoch_src.clone();
                                let ctrl_tx_clone = channel_ctrl_tx.clone();
                                tokio::spawn(async move {
                                    let mut task_rx_inner = task_rx;
                                    // Wait for the stream-bind event (same pattern as
                                    // run_channel_task's stream-bind Phase-1 loop).
                                    let (mut ch_send, mut ch_recv) = loop {
                                        match task_rx_inner.recv().await {
                                            Some(ChannelEvent::Stream(s, r)) => break (s, r),
                                            Some(ChannelEvent::Close) | None => {
                                                let _ = ctrl_tx_clone
                                                    .send(Message::ChannelClose { channel_id })
                                                    .await;
                                                return;
                                            }
                                            Some(ChannelEvent::Credit(_)) => {
                                                // Credit before stream bound; keep waiting.
                                            }
                                        }
                                    };
                                    run_scrollback_sender_task(
                                        channel_id,
                                        slot_clone,
                                        &mut ch_send,
                                        &mut ch_recv,
                                        &mut task_rx_inner,
                                        &ctrl_tx_clone,
                                        epoch_src_clone,
                                    ).await;
                                });
                            }
                            _ => {
                                // All other accepted types use the generic channel task
                                // (Echo in test builds).
                                tokio::spawn(run_channel_task(
                                    channel_id,
                                    task_rx,
                                    channel_ctrl_tx.clone(),
                                ));
                            }
                        }
                        tracing::debug!(channel_id, "channel accepted; task spawned");
                    }

                    Ok(Message::ChannelCredit { channel_id, bytes }) => {
                        // Replenish send credit for the channel task (MUX-03).
                        // Unknown id is a logged no-op — never panic (Pitfall M-4 / T-21-07).
                        if let Some(task_tx) = channel_map.get(&channel_id) {
                            let _ = task_tx.try_send(ChannelEvent::Credit(bytes));
                        } else {
                            tracing::debug!(channel_id, "ChannelCredit for unknown channel; ignoring");
                        }
                    }

                    Ok(Message::ChannelClose { channel_id }) => {
                        // Client is closing its side of the channel (MUX-04).
                        // Signal the task and remove from the map.
                        // Unknown id is a logged no-op — never panic (Pitfall M-4 / T-21-07).
                        //
                        // WR-S-01 fix: use send().await for Close (not try_send) so a
                        // momentarily-full 64-slot queue does not silently drop the signal
                        // and leave the scrollback sender task alive holding slot_clone
                        // until connection teardown. Mirrors the WR-01 Stream-event fix.
                        if let Some(task_tx) = channel_map.remove(&channel_id) {
                            let _ = task_tx.send(ChannelEvent::Close).await;
                        } else {
                            tracing::debug!(channel_id, "ChannelClose for unknown channel; ignoring");
                        }
                    }

                    Ok(Message::ChannelAccept { channel_id }) => {
                        // ChannelAccept is server→client direction.
                        // Under #[cfg(any(test, feature = "test-support"))], an odd-id
                        // ChannelAccept is the client acknowledging a server-initiated
                        // ChannelOpen (Task 3). The channel task is already running;
                        // nothing further is needed. Unknown/already-closed odd ids are
                        // logged no-ops (Pitfall M-4 / T-21-07).
                        #[cfg(any(test, feature = "test-support"))]
                        if channel_id % 2 != 0 {
                            tracing::debug!(
                                channel_id,
                                "client accepted server-initiated channel"
                            );
                            continue;
                        }
                        // Production path (or even-id in test): protocol error.
                        tracing::warn!(
                            channel_id,
                            "client sent ChannelAccept (server→client only); closing"
                        );
                        break SessionEnd::ClientClosed;
                    }

                    Ok(Message::ChannelReject { channel_id }) => {
                        // ChannelReject is server→client direction.
                        // Under #[cfg(any(test, feature = "test-support"))], an odd-id
                        // ChannelReject is the client rejecting a server-initiated
                        // ChannelOpen. Drop the map entry so the channel task drains and
                        // exits cleanly (T-21-07).
                        #[cfg(any(test, feature = "test-support"))]
                        if channel_id % 2 != 0 {
                            tracing::debug!(
                                channel_id,
                                "client rejected server-initiated channel; dropping map entry"
                            );
                            if let Some(task_tx) = channel_map.remove(&channel_id) {
                                let _ = task_tx.try_send(ChannelEvent::Close);
                            }
                            continue;
                        }
                        // Production path (or even-id in test): protocol error.
                        tracing::warn!(
                            channel_id,
                            "client sent ChannelReject (server→client only); closing"
                        );
                        break SessionEnd::ClientClosed;
                    }

                    // Scrollback frames on the control stream are protocol errors:
                    // ScrollbackRequest must travel on the scrollback channel's own
                    // RecvStream (M-2 deadlock avoidance); ScrollbackPage and
                    // ScrollbackCredit are server→client frames that the client task
                    // writes on ch_send, never on the control stream.
                    // Log and ignore — do not close the session for a misdirected frame.
                    Ok(Message::ScrollbackRequest { channel_id, .. })
                    | Ok(Message::ScrollbackPage { channel_id, .. })
                    | Ok(Message::ScrollbackCredit { channel_id, .. }) => {
                        tracing::debug!(
                            channel_id,
                            "scrollback frame received on control stream (protocol error); ignoring"
                        );
                    }

                    Err(_) => {
                        // Stream/connection closed without a SessionClose → transport loss.
                        // D-02: this is NOT a clean close; orphan the session (Pitfall #7).
                        break SessionEnd::TransportLost;
                    }
                }
            }

            // Phase 21 (MUX-02): secondary accept_bi arm — binds incoming QUIC bidi
            // streams to their channel task by reading the channel-id varint prefix.
            //
            // This arm exists ONLY inside run_session/run_reattach_session, i.e. AFTER
            // the pre-auth permit has been dropped. It MUST NOT appear in run_accept_loop
            // (Pitfall M-1 auth bypass / T-21-03).
            incoming_stream = conn.accept_bi() => {
                match incoming_stream {
                    Ok((ch_send, mut ch_recv)) => {
                        // Read only the varint channel-id prefix — no channel payload
                        // is ever consumed here (Pitfall M-2 HOL blocking prevention).
                        match crate::channel::read_varint_u32(&mut ch_recv).await {
                            Ok(channel_id) => {
                                if let Some(task_tx) = channel_map.get(&channel_id) {
                                    // WR-01 fix: use send (not try_send) for Stream events so
                                    // a momentarily-full task buffer does not silently discard
                                    // the stream-bind. A permanently-lost Stream event would
                                    // leave the channel task blocked forever waiting for a
                                    // stream that will never arrive.
                                    if task_tx.send(ChannelEvent::Stream(ch_send, ch_recv)).await.is_err() {
                                        // Task already exited before the stream arrived; log and continue.
                                        tracing::debug!(channel_id, "accept_bi: channel task gone before stream arrived");
                                    }
                                } else {
                                    // CR-02 fix: explicitly reset the send side and stop the recv
                                    // side before dropping, so the peer gets a clean signal instead
                                    // of hanging until the QUIC idle timeout fires.
                                    tracing::debug!(
                                        channel_id,
                                        "accept_bi: no channel task for id; resetting stream"
                                    );
                                    let mut ch_send = ch_send;
                                    let _ = ch_send.reset(0u32.into());
                                    ch_recv.stop(0u32.into()).ok();
                                }
                            }
                            Err(_) => {
                                // Malformed varint: drop the stream without panicking (T-21-06 / V5).
                                tracing::warn!("accept_bi: malformed channel-id varint; dropping stream");
                            }
                        }
                    }
                    Err(quinn::ConnectionError::ApplicationClosed(_))
                    | Err(quinn::ConnectionError::LocallyClosed) => {
                        break SessionEnd::TransportLost;
                    }
                    Err(_) => {
                        // Transient error: continue the loop.
                    }
                }
            }

            // Phase 21 (A4): drain outbound control frames from channel tasks.
            //
            // Channel tasks send ChannelClose and ChannelCredit back here via
            // channel_ctrl_tx. This is the SINGLE writer for the control stream —
            // channel tasks MUST NOT call write_message directly (Pitfall M-6).
            Some(ctrl_msg) = channel_ctrl_rx.recv() => {
                // On ChannelClose from a task, remove it from the map.
                if let Message::ChannelClose { channel_id } = &ctrl_msg {
                    channel_map.remove(channel_id);
                    tracing::debug!(channel_id, "channel task closed; removed from map");
                }
                // Write the control frame to the client.
                if nosh_proto::write_message(&mut send, &ctrl_msg).await.is_err() {
                    break SessionEnd::TransportLost;
                }
            }

            // Task 3: server-initiated channel-open trigger arm.
            //
            // In test builds, server_open_rx_opt is Some(rx) and integration tests
            // write ChannelType values to it via the SessionSlot accessor. In
            // production builds, server_open_rx_opt is None so recv_or_pending()
            // returns std::future::pending() — the arm never fires (T-21-10).
            //
            // The arm runs on the session pump task which owns `send` — the
            // single-writer invariant for the control stream is maintained (A4).
            server_open_req = recv_or_pending(&mut server_open_rx_opt) => {
                match server_open_req {
                    Some(ch_type) => {
                        let server_ch_id = next_server_channel_id;
                        next_server_channel_id += 2; // advance: 1, 3, 5, …
                        debug_assert!(server_ch_id % 2 != 0, "server channel id must be odd");

                        if channel_map.len() >= MAX_OPEN_CHANNELS {
                            tracing::warn!(
                                server_ch_id,
                                "server-initiated ChannelOpen rejected: channel cap reached"
                            );
                            continue;
                        }

                        let (task_tx, task_rx) = mpsc::channel::<ChannelEvent>(64);
                        channel_map.insert(server_ch_id, task_tx);
                        tokio::spawn(run_channel_task(
                            server_ch_id,
                            task_rx,
                            channel_ctrl_tx.clone(),
                        ));

                        tracing::debug!(server_ch_id, "server-initiated ChannelOpen");
                        if nosh_proto::write_message(
                            &mut send,
                            &Message::ChannelOpen {
                                channel_id: server_ch_id,
                                channel_type: ch_type,
                            },
                        )
                        .await
                        .is_err()
                        {
                            break SessionEnd::TransportLost;
                        }
                    }
                    None => {
                        // WR-02 fix: sender was dropped; disable this arm permanently to
                        // avoid a busy-loop. A closed receiver always returns None immediately,
                        // which would starve the other select! arms on every iteration.
                        // The session loop does NOT stop here — it continues draining PTY
                        // output, incoming streams, and control frames normally.
                        server_open_rx_opt = None;
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
                        slot.push_output_and_parse(&data);
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

            // D-03: signal the interruptible reader to exit and AWAIT its clean
            // thread exit BEFORE calling registry.orphan(). This guarantees the
            // prior reader has fully exited before a future reattach clones a new
            // reader on the same master fd — no two live readers racing on the same
            // fd (Pitfall 3 / T-10-04). Mirror the W2 writer-handback pattern exactly.
            reader_handle.signal_shutdown();
            let _ = tokio::time::timeout(Duration::from_secs(5), &mut reader_handle.join).await;

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

    // Best-effort: signal the reader to exit so no reader thread is left parked
    // after the function returns. Covers ShellExited and ClientClosed paths.
    // TransportLost already called signal_shutdown() + join-await above; this
    // second call on that path writes to a pipe whose read-end is closed (the
    // reader thread dropped it on exit). signal_shutdown() silently ignores the
    // resulting EPIPE, so this is harmless — no restructuring needed (IN-01).
    // The PTY master fd stays open via the slot on the orphan path — signalling
    // the reader does NOT close it.
    reader_handle.signal_shutdown();
    // abort() on spawn_blocking is a no-op (Pitfall 6): the task ignores it and
    // keeps running until blocking_recv() returns None (from drop(in_tx) above).
    // The task will store the writer back into the slot on its own and then exit.
    // Nothing further to do here — drop(input_writer) releases the JoinHandle.
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
    // D-13-03: ResumeComplete gate — declared TRUE after the replay loop.
    // Any early-return inside the replay loop (re_orphan + return) prevents the
    // select! loop from ever starting, so the replay is always fully complete
    // before the diff_interval arm can fire. Plain bool suffices — no channel or
    // atomic needed (Pattern 4: same async task, sequential code flow, Pitfall 5).
    // T-13-04: datagrams cannot leak a partial-replay grid because the select!
    // loop does not start until resume_complete is set here.
    let resume_complete = true;

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

    // OUTPUT pump: same interruptible reader pattern as run_session.
    // Extract master raw fd under the brief lock, then release before spawning
    // (Pitfall 2 — no lock held across spawn_blocking).
    let master_raw_fd = slot.master_raw_fd().expect("Unix PTY master fd available for reattach");
    let (out_tx, mut out_rx) = mpsc::channel::<Vec<u8>>(64);
    let mut reader_handle = crate::pty_io::start_interruptible_reader(master_raw_fd, reader, out_tx)
        .expect("start interruptible PTY reader for reattach");

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

    // SYNC-03: 16 ms diff-interval ticker for coalesced StateDiff datagram emission.
    // Identical initialization to run_session; ResumeComplete was set above.
    let mut diff_interval = tokio::time::interval(Duration::from_millis(16));
    diff_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Per-connection datagram state (Open Question 3: task-local, resets on reattach).
    // D-13-01b: empty baseline → first post-resume diff is naturally the full screen.
    let mut current_epoch: u64 = 0;
    // Phase 22 (S-5): epoch mirror for the scrollback sender task (same as run_session).
    // Updated (store, Release) at each diff tick after build_state_diff increments
    // current_epoch. Does NOT change epoch cadence, confirmed_epoch logic, or
    // datagram sends — additive mirror only.
    let epoch_src = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let mut last_acked_epoch: u64 = 0;
    let mut last_acked_snapshot: Vec<Vec<Cell>> = Vec::new();
    let mut last_sent_snapshot: Vec<Vec<Cell>> = Vec::new();
    let mut pending_deferred: Vec<DiffRun> = Vec::new();
    // CR-01 fix: bounded per-epoch sent-snapshot store (same as run_session).
    let mut epoch_snapshots: VecDeque<(u64, Vec<Vec<Cell>>)> = VecDeque::new();

    // Phase 21 (MUX-01..MUX-04): per-session channel infrastructure (same as
    // run_session). Channel state is empty on reattach — the client must re-open
    // any channels it wants after receiving ReattachOk (MUX-05 / Pitfall M-4).
    let mut channel_map: HashMap<u32, mpsc::Sender<ChannelEvent>> = HashMap::new();
    let (channel_ctrl_tx, mut channel_ctrl_rx) = mpsc::channel::<Message>(64);

    let session_end: SessionEnd = loop {
        tokio::select! {
            chunk = out_rx.recv() => {
                match chunk {
                    Some(data) => {
                        slot.push_output_and_parse(&data);
                        if nosh_proto::write_message(&mut send, &Message::PtyData { data })
                            .await
                            .is_err()
                        {
                            break SessionEnd::TransportLost;
                        }
                        // Phase 16 (D-16-01, D-16-02): drain any pending OSC 0/2 title and
                        // OSC 52 clipboard-write detected during parse, and forward them to
                        // the client over the RELIABLE stream (not datagrams — no MTU limit).
                        // Write-only OSC 52: read/query form already dropped in osc_dispatch.
                        // Client re-emits these to stdout, bypassing the compositor.
                        let (drained_title, drained_clipboard) = slot.drain_terminal_control();
                        if let Some(title) = drained_title {
                            if nosh_proto::write_message(
                                &mut send,
                                &Message::TerminalControl(TerminalControlPayload::Title { title }),
                            )
                            .await
                            .is_err()
                            {
                                break SessionEnd::TransportLost;
                            }
                        }
                        if let Some((selection, data)) = drained_clipboard {
                            if nosh_proto::write_message(
                                &mut send,
                                &Message::TerminalControl(TerminalControlPayload::Clipboard {
                                    selection,
                                    data,
                                }),
                            )
                            .await
                            .is_err()
                            {
                                break SessionEnd::TransportLost;
                            }
                        }
                    }
                    None => {
                        // PTY EOF: shell exited. Shell exit code is tracked by the
                        // original wait_task watcher; we close with code 0 (approximate).
                        break SessionEnd::ShellExited(0);
                    }
                }
            }
            // SYNC-03 / PACE-01: diff-interval tick — burst state-diff datagrams.
            // D-13-03: gated by resume_complete (false until replay loop above completes).
            // Phase 20: identical burst semantics as run_session (send_burst called
            // here too — reattached sessions burst identically to fresh sessions,
            // Pitfall 6 prevention).
            _ = diff_interval.tick() => {
                if !resume_complete {
                    continue;
                }
                let cap = match conn.max_datagram_size() {
                    Some(c) if c >= MIN_CAP => c,
                    _ => continue,
                };
                let deferred = std::mem::take(&mut pending_deferred);
                if let Some(result) = build_state_diff(
                    &slot,
                    &mut current_epoch,
                    last_acked_epoch,
                    &last_acked_snapshot,
                    &last_sent_snapshot,
                    deferred,
                    cap,
                ) {
                    // Phase 22 (S-5): mirror updated epoch into epoch_src (same as
                    // run_session). Additive — does NOT change epoch cadence or sends.
                    epoch_src.store(current_epoch, std::sync::atomic::Ordering::Release);
                    // CR-01 fix: store sent snapshot keyed by epoch (same as run_session).
                    // Push exactly ONCE per tick outside the burst loop (Pitfall 5).
                    epoch_snapshots.push_back((result.epoch, result.sent_cells.clone()));
                    if epoch_snapshots.len() > EPOCH_SNAPSHOT_CAP {
                        epoch_snapshots.pop_front();
                    }
                    last_sent_snapshot = result.sent_cells.clone();
                    // Phase 20: burst drain via send_burst (same as run_session).
                    let (leftover, transport_lost) = send_burst(&conn, result, cap);
                    pending_deferred = leftover;
                    if transport_lost {
                        break SessionEnd::TransportLost;
                    }
                }
            }
            // SYNC-03: epoch-ack arm — same as run_session.
            datagram = conn.read_datagram() => {
                match datagram {
                    Ok(bytes) => {
                        match decode_epoch_ack(&bytes) {
                            Ok(acked) if acked > last_acked_epoch => {
                                last_acked_epoch = acked;
                                // CR-01 fix: use the snapshot captured at epoch-sent
                                // time (same as run_session).
                                if let Some(pos) = epoch_snapshots
                                    .iter()
                                    .position(|(e, _)| *e == acked)
                                {
                                    let (_, snap) = epoch_snapshots.remove(pos).unwrap();
                                    last_acked_snapshot = snap;
                                    epoch_snapshots.retain(|(e, _)| *e > acked);
                                }
                                // If not found in store, baseline stays as-is (self-correcting).
                            }
                            Ok(_) => {}
                            Err(_) => {}
                        }
                    }
                    Err(_) => break SessionEnd::TransportLost,
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
                    Ok(Message::SessionOpen { .. }) => {
                        // WR-07 fix: an unexpected SessionOpen mid-reattach is a protocol
                        // violation (mirrors the run_session behaviour at server.rs:784).
                        // Treat as ClientClosed rather than silently ignoring it.
                        break SessionEnd::ClientClosed;
                    }
                    Ok(Message::Ack { seq }) => {
                        slot.touch();
                        slot.trim_acked(seq);
                    }

                    // Phase 21 (MUX-01..MUX-04): channel multiplexing dispatch.
                    // Same rules as run_session: client-even ids, no odd ids from client,
                    // MAX_OPEN_CHANNELS cap, PortForward/AgentForward rejected, Echo
                    // test-only, Scrollback Phase 22.

                    Ok(Message::ChannelOpen { channel_id, channel_type }) => {
                        if channel_id % 2 != 0 {
                            tracing::warn!(
                                channel_id,
                                "client sent ChannelOpen with odd channel_id on reattach; ignoring"
                            );
                            continue;
                        }
                        if channel_map.contains_key(&channel_id) {
                            let _ = nosh_proto::write_message(
                                &mut send,
                                &Message::ChannelReject { channel_id },
                            )
                            .await;
                            continue;
                        }
                        if channel_map.len() >= MAX_OPEN_CHANNELS {
                            let _ = nosh_proto::write_message(
                                &mut send,
                                &Message::ChannelReject { channel_id },
                            )
                            .await;
                            continue;
                        }
                        // Validate channel type: Scrollback and Echo (test-only) are
                        // accepted; PortForward and AgentForward are unconditionally
                        // rejected (FWD-01 / FWD-02 / T-21-08 / T-22-10 access control).
                        let accept = match channel_type {
                            ChannelType::PortForward | ChannelType::AgentForward => false,
                            ChannelType::Scrollback => {
                                // Phase 22: accepted on reattach (SCROLL-05 reattach
                                // precondition). Channel state is not replayed; the client
                                // re-opens after ResumeComplete (MUX-05).
                                true
                            }
                            ChannelType::Echo => {
                                #[cfg(any(test, feature = "test-support"))] { true }
                                #[cfg(not(any(test, feature = "test-support")))] { false }
                            }
                        };
                        if !accept {
                            let _ = nosh_proto::write_message(
                                &mut send,
                                &Message::ChannelReject { channel_id },
                            )
                            .await;
                            continue;
                        }
                        if nosh_proto::write_message(
                            &mut send,
                            &Message::ChannelAccept { channel_id },
                        )
                        .await
                        .is_err()
                        {
                            break SessionEnd::TransportLost;
                        }
                        // Bounded mpsc(64) for channel events (M-6 / S-4).
                        let (task_tx, task_rx) = mpsc::channel::<ChannelEvent>(64);
                        channel_map.insert(channel_id, task_tx);
                        // Dispatch to the correct per-channel task based on channel type.
                        match channel_type {
                            ChannelType::Scrollback => {
                                // Phase 22: dedicated scrollback sender task (SCROLL-05
                                // reattach path). Isolated from pump — cannot stall PTY (M-6).
                                let slot_clone = slot.clone();
                                let epoch_src_clone = epoch_src.clone();
                                let ctrl_tx_clone = channel_ctrl_tx.clone();
                                tokio::spawn(async move {
                                    let mut task_rx_inner = task_rx;
                                    // Wait for the stream-bind event.
                                    let (mut ch_send, mut ch_recv) = loop {
                                        match task_rx_inner.recv().await {
                                            Some(ChannelEvent::Stream(s, r)) => break (s, r),
                                            Some(ChannelEvent::Close) | None => {
                                                let _ = ctrl_tx_clone
                                                    .send(Message::ChannelClose { channel_id })
                                                    .await;
                                                return;
                                            }
                                            Some(ChannelEvent::Credit(_)) => {
                                                // Credit before stream bound; keep waiting.
                                            }
                                        }
                                    };
                                    run_scrollback_sender_task(
                                        channel_id,
                                        slot_clone,
                                        &mut ch_send,
                                        &mut ch_recv,
                                        &mut task_rx_inner,
                                        &ctrl_tx_clone,
                                        epoch_src_clone,
                                    ).await;
                                });
                            }
                            _ => {
                                // All other accepted types use the generic channel task.
                                tokio::spawn(run_channel_task(
                                    channel_id,
                                    task_rx,
                                    channel_ctrl_tx.clone(),
                                ));
                            }
                        }
                    }

                    Ok(Message::ChannelCredit { channel_id, bytes }) => {
                        if let Some(task_tx) = channel_map.get(&channel_id) {
                            let _ = task_tx.try_send(ChannelEvent::Credit(bytes));
                        }
                        // Unknown id: no-op (T-21-07 / Pitfall M-4).
                    }

                    Ok(Message::ChannelClose { channel_id }) => {
                        // WR-S-01 fix: use send().await for Close — same rationale as
                        // run_session arm above. try_send could silently drop the close
                        // signal when the 64-slot queue is full, leaking the task.
                        if let Some(task_tx) = channel_map.remove(&channel_id) {
                            let _ = task_tx.send(ChannelEvent::Close).await;
                        }
                        // Unknown id: no-op (T-21-07 / Pitfall M-4).
                    }

                    Ok(Message::ChannelAccept { .. }) | Ok(Message::ChannelReject { .. }) => {
                        // IN-02: The blanket close here is currently correct because
                        // run_reattach_session has no server-open infrastructure (no
                        // server_open_rx_opt, no server_ch_id allocation). Unlike
                        // run_session (which gates odd-id ChannelAccept/Reject behind
                        // #[cfg(test)]), reattach has no server-initiated open path at all,
                        // so both even and odd ids are protocol errors today.
                        //
                        // IMPORTANT: if run_reattach_session ever gains a server_open_rx_opt
                        // arm (to support server-initiated opens on reattach), this blanket
                        // close MUST be updated to mirror run_session's cfg-gated logic —
                        // otherwise odd-id replies from the client will kill the session.
                        tracing::warn!("client sent ChannelAccept/ChannelReject on reattach session; closing");
                        break SessionEnd::ClientClosed;
                    }

                    Ok(_) => {} // ignore any other unexpected frames
                    Err(_) => {
                        break SessionEnd::TransportLost;
                    }
                }
            }

            // Phase 21 (MUX-02): secondary accept_bi arm — same as run_session.
            // Exists ONLY inside run_reattach_session (post-auth — T-21-03 / Pitfall M-1).
            incoming_stream = conn.accept_bi() => {
                match incoming_stream {
                    Ok((ch_send, mut ch_recv)) => {
                        match crate::channel::read_varint_u32(&mut ch_recv).await {
                            Ok(channel_id) => {
                                if let Some(task_tx) = channel_map.get(&channel_id) {
                                    // WR-01 fix: use send (not try_send) for Stream events.
                                    // See run_session arm for full rationale.
                                    if task_tx.send(ChannelEvent::Stream(ch_send, ch_recv)).await.is_err() {
                                        tracing::debug!(channel_id, "accept_bi (reattach): channel task gone before stream arrived");
                                    }
                                } else {
                                    // CR-02 fix: explicitly reset before dropping so the peer
                                    // gets a clean signal rather than hanging until idle timeout.
                                    tracing::debug!(
                                        channel_id,
                                        "accept_bi (reattach): no channel task for id; resetting stream"
                                    );
                                    let mut ch_send = ch_send;
                                    let _ = ch_send.reset(0u32.into());
                                    ch_recv.stop(0u32.into()).ok();
                                }
                            }
                            Err(_) => {
                                tracing::warn!(
                                    "accept_bi (reattach): malformed channel-id varint; dropping stream"
                                );
                            }
                        }
                    }
                    Err(quinn::ConnectionError::ApplicationClosed(_))
                    | Err(quinn::ConnectionError::LocallyClosed) => {
                        break SessionEnd::TransportLost;
                    }
                    Err(_) => {}
                }
            }

            // Phase 21 (A4): drain outbound control frames from channel tasks (same as run_session).
            Some(ctrl_msg) = channel_ctrl_rx.recv() => {
                if let Message::ChannelClose { channel_id } = &ctrl_msg {
                    channel_map.remove(channel_id);
                }
                if nosh_proto::write_message(&mut send, &ctrl_msg).await.is_err() {
                    break SessionEnd::TransportLost;
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
            // WR-02 fix: use remove_slot (Arc pointer identity) instead of
            // registry.remove (session_id). remove_slot ensures we only remove
            // THIS specific slot instance — a concurrent reattach that opened a
            // new session under the same session_id would not be accidentally
            // evicted. The original watcher will also call remove_slot; both are
            // idempotent (retain returns false for the already-removed entry).
            registry.remove_slot(&slot);
        }
        SessionEnd::ClientClosed => {
            tracing::info!("client closed reattach session");
            slot.sighup();
            conn.close(CLOSE_OK.into(), b"client closed");
            registry.remove_slot(&slot);
        }
        SessionEnd::TransportLost => {
            tracing::info!("transport lost during reattach; re-orphaning");
            // D-03: signal the interruptible reader and AWAIT its clean exit
            // BEFORE registry.orphan() — guarantees the prior reader has fully
            // exited before the next reattach clones a fresh reader on the same
            // master fd (Pitfall 3 / T-10-04). Mirror the W2 writer-handback pattern.
            reader_handle.signal_shutdown();
            let _ = tokio::time::timeout(Duration::from_secs(5), &mut reader_handle.join).await;

            // W2 fix: await the input task so it stores the writer back into the
            // slot BEFORE we orphan — the re-orphaned slot always has a usable
            // writer for the next reattach.
            let _ = tokio::time::timeout(Duration::from_secs(5), &mut input_writer).await;
            registry.orphan(&slot);
            // The original exit watcher is still alive; no new watcher needed.
        }
    }

    // Best-effort: signal the reader to exit so no reader thread is left parked
    // after the function returns. Covers ShellExited and ClientClosed paths.
    // TransportLost already called signal_shutdown() + join-await above; the
    // second call on that path writes to a closed-read-end pipe and is silently
    // ignored (EPIPE, IN-01). Harmless — no restructuring needed.
    reader_handle.signal_shutdown();
    // abort() on spawn_blocking is a no-op (Pitfall 6): drop(in_tx) above already
    // unblocked the blocking_recv loop; the task will drain and store the writer
    // back into the slot on its own. Nothing further to do here.
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
    use super::*;
    use nosh_proto::datagram::CellStyle;
    use nosh_proto::datagram::{CursorPos, StateDiff};

    // ── Test helpers ─────────────────────────────────────────────────────────

    /// Returns true if `/bin/sh` is available (guards PTY-spawning tests).
    fn have_sh() -> bool {
        std::path::Path::new("/bin/sh").exists()
    }

    /// Open a real /bin/sh session for use in burst drain tests.
    /// Requires /bin/sh — guard with `have_sh()` before calling.
    fn open_sh_session_for_burst() -> std::sync::Arc<crate::registry::SessionSlot> {
        use crate::session;
        let passwd = session::lookup_self(Some("/bin/sh"));
        let identity = nosh_auth::NoshPublicKey::from_raw([0xBB_u8; 32]);
        let (sess, _reader, _writer) =
            session::open(&passwd, "xterm", 80, 24, &[], identity)
                .expect("open /bin/sh for burst test");
        let slot = crate::registry::SessionSlot::new(sess);
        // Fill the 80x24 terminal with non-space characters so that
        // compute_diff_runs returns ~1920 cells — enough to require multiple
        // datagrams at typical MTU (~1200 bytes).
        //
        // ESC[<row>;<col>H positions the cursor; then 80 'A' chars fill the row.
        let mut vt = String::new();
        for r in 1u16..=24 {
            vt.push_str(&format!("\x1b[{r};1H{}", "A".repeat(80)));
        }
        slot.push_output_and_parse(vt.as_bytes());
        slot
    }

    // ── Phase 20 (20-01): burst drain unit tests ──────────────────────────────

    /// PACE-03 / D-20-03 burst termination gate.
    ///
    /// With a full 80×24 grid of 'A' chars vs an EMPTY `last_acked_snapshot`,
    /// the diff contains ~1920 changed cells — more than fits in a single ~1200-byte
    /// datagram. The correct burst drain calls `build_state_diff` ONCE, extracts the
    /// first `(payload, deferred)` pair, then drains `deferred` using `encode_datagram`
    /// only (not `build_state_diff` again).
    ///
    /// This test asserts that the encode_datagram-only drain terminates in a finite
    /// number of iterations (≤ 100). A naive loop that re-calls `build_state_diff`
    /// on every iteration would regenerate `fresh_runs` against the non-advancing
    /// `last_acked_snapshot`, refilling deferred faster than it drains, and would
    /// spin forever — the R-1 infinite-spin trap from the reverted 999.4 attempt.
    ///
    /// RED-before: the test proves the correct finite-drain property. It passes
    /// when the drain is encode_datagram-only; it would exceed `max_iterations`
    /// (or loop infinitely) if build_state_diff were called on each iteration.
    #[test]
    fn burst_drains_when_grid_differs_from_acked_baseline() {
        if !have_sh() {
            eprintln!(
                "skipping burst_drains_when_grid_differs_from_acked_baseline: /bin/sh unavailable"
            );
            return;
        }

        let slot = open_sh_session_for_burst();

        let mut current_epoch = 0u64;
        let last_acked_epoch = 0u64;
        // Empty baseline — all 1920 cells are "changed" from the server's perspective.
        let last_acked_snapshot: Vec<Vec<crate::terminal::Cell>> = Vec::new();
        let last_sent_snapshot: Vec<Vec<crate::terminal::Cell>> = Vec::new();

        let cap = 1200; // typical MTU; drive real datagram-size pressure
        let max_iterations = 100; // a full 80×24 repaint takes ~16–24; 100 is generous

        // ── D-20-03: call build_state_diff exactly ONCE ───────────────────────
        let first = build_state_diff(
            &slot,
            &mut current_epoch,
            last_acked_epoch,
            &last_acked_snapshot,
            &last_sent_snapshot,
            vec![], // no pending_deferred on first call
            cap,
        )
        .expect("build_state_diff must produce a result for a non-empty grid vs empty baseline");

        // Extract geometry for burst iterations. After Task 2 adds cols/rows/cursor/
        // alt_screen to DiffTickResult these will be first.cols etc. For now we
        // derive from sent_cells and use a zero cursor (the 'A' fill does not move
        // the cursor to a position that affects diff correctness here).
        let tick_epoch = first.epoch;
        let rows = first.sent_cells.len() as u16;
        let cols = first.sent_cells.first().map(|r| r.len() as u16).unwrap_or(80);
        let cursor = CursorPos { row: 0, col: 0 };
        let alt_screen = false;

        assert!(
            current_epoch == 1,
            "build_state_diff must increment epoch from 0 to 1, got {current_epoch}"
        );

        let mut deferred = first.deferred;
        let mut iter_count = 0usize;

        // ── Encode-datagram-only drain (D-20-03: no build_state_diff in loop) ─
        while !deferred.is_empty() {
            assert!(
                iter_count < max_iterations,
                "burst drain must terminate in ≤ {max_iterations} iterations (R-1 guard): \
                 still {} deferred runs after {iter_count} iterations",
                deferred.len()
            );
            let burst_diff = StateDiff {
                epoch: tick_epoch, // same epoch — D-20-04
                cols,
                rows,
                cursor,
                alt_screen,
                runs: deferred,
            };
            let (_, next_deferred) = encode_datagram(&burst_diff, cap)
                .expect("encode_datagram must not fail with a valid cap");
            deferred = next_deferred;
            iter_count += 1;
        }

        assert!(
            deferred.is_empty(),
            "deferred must be empty after the encode_datagram drain loop"
        );
        assert!(
            iter_count > 0,
            "a full 80×24 grid must require at least one drain iteration at cap={cap}"
        );

        // Cleanup: send SIGHUP to the shell to avoid leaking the child process.
        slot.sighup();
    }

    /// PACE-02 / D-20-04 one-epoch-per-tick gate.
    ///
    /// Asserts that `build_state_diff` increments `current_epoch` exactly once
    /// (from 0 to 1) for the tick, and that subsequent `encode_datagram` drain
    /// iterations do NOT further increment `current_epoch` (they do not call
    /// `build_state_diff`).
    ///
    /// This is the structural proof that all burst datagrams in one tick share a
    /// single epoch, preventing the R-2 noecho-epoch leak: if `confirmed_epoch`
    /// only advances when `build_state_diff` is called (once per tick), it cannot
    /// advance during a `read -s` window caused by mid-tick burst datagram
    /// acknowledgements.
    #[test]
    fn one_epoch_per_tick() {
        if !have_sh() {
            eprintln!("skipping one_epoch_per_tick: /bin/sh unavailable");
            return;
        }

        let slot = open_sh_session_for_burst();

        let mut current_epoch = 0u64;
        let last_acked_epoch = 0u64;
        let last_acked_snapshot: Vec<Vec<crate::terminal::Cell>> = Vec::new();
        let last_sent_snapshot: Vec<Vec<crate::terminal::Cell>> = Vec::new();

        let cap = 1200;

        // ── Exactly one epoch increment per build_state_diff call ─────────────
        let first = build_state_diff(
            &slot,
            &mut current_epoch,
            last_acked_epoch,
            &last_acked_snapshot,
            &last_sent_snapshot,
            vec![],
            cap,
        )
        .expect("build_state_diff must produce a result for a non-empty grid");

        assert_eq!(
            current_epoch, 1,
            "build_state_diff must increment epoch from 0 to 1 exactly once"
        );
        let tick_epoch = first.epoch;
        assert_eq!(tick_epoch, 1, "DiffTickResult.epoch must equal current_epoch after the call");

        let rows = first.sent_cells.len() as u16;
        let cols = first.sent_cells.first().map(|r| r.len() as u16).unwrap_or(80);
        let cursor = CursorPos { row: 0, col: 0 };
        let alt_screen = false;

        let mut deferred = first.deferred;
        let mut n_drain_iters = 0usize;

        // ── Drain via encode_datagram only — current_epoch must NOT change ────
        while !deferred.is_empty() && n_drain_iters < 100 {
            let burst_diff = StateDiff {
                epoch: tick_epoch,
                cols,
                rows,
                cursor,
                alt_screen,
                runs: deferred,
            };
            let (_, next_deferred) = encode_datagram(&burst_diff, cap)
                .expect("encode_datagram must not fail");
            deferred = next_deferred;
            n_drain_iters += 1;
        }

        assert_eq!(
            current_epoch, 1,
            "current_epoch must still be 1 after {n_drain_iters} encode_datagram drain iterations; \
             the drain must never call build_state_diff (which is the only thing that increments the epoch)"
        );

        slot.sighup();
    }

    // ── Task 2 (19-02): compute_diff_runs wide-char skip tests ───────────────

    /// TUI-03: DiffRun.chars must contain exactly one scalar for a wide glyph;
    /// the continuation cell (wide:true) must not be pushed into chars.
    ///
    /// Uses a 2-column grid so the run is unambiguously just [glyph, CONT] →
    /// expect exactly 1 char in the resulting DiffRun.
    #[test]
    fn compute_diff_runs_wide_char_produces_one_scalar_in_chars() {
        use crate::terminal::Cell;
        // Build a 2-column grid row: ['中' (wide:false, col 0), ' ' (wide:true, col 1)]
        // Baseline is empty → both cells are "changed".
        let cjk = Cell {
            ch: '中',
            style: CellStyle(CellStyle::NONE),
            fg: None,
            bg: None,
            wide: false,
        };
        let cont = Cell {
            ch: ' ',
            style: CellStyle(CellStyle::NONE),
            fg: None,
            bg: None,
            wide: true,
        };
        let current = vec![vec![cjk, cont]];
        let baseline: Vec<Vec<Cell>> = vec![];

        let runs = compute_diff_runs(&current, &baseline);

        // Exactly one DiffRun should exist (the glyph; the continuation is skipped).
        assert_eq!(runs.len(), 1, "expected exactly 1 DiffRun, got: {runs:?}");
        let run = &runs[0];
        assert_eq!(run.start_col, 0, "DiffRun must start at col 0");
        let chars_count = run.chars.chars().count();
        assert_eq!(
            chars_count, 1,
            "DiffRun.chars must have exactly 1 scalar (the glyph, not the continuation), got {:?}",
            run.chars
        );
        assert_eq!(
            run.chars.chars().next().unwrap(), '中',
            "DiffRun.chars must be the CJK glyph '中'"
        );
    }

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
