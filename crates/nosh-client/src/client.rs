//! Client-side QUIC endpoint setup, connect-with-ALPN-assert, and the
//! stream/datagram round-trip helpers that prove the Phase 1 transport.
//!
//! Exposed as library functions so the integration tests (Plan 04) reuse them.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use bytes::Bytes;
use nosh_auth::{
    AgentSigningKey, FileSigner, HostKeyVerifier, NoshClientCertResolver, RawEd25519Signer,
};
#[cfg(unix)]
use nosh_auth::AgentSigner;
use nosh_proto::Message;
use quinn::crypto::rustls::{HandshakeData, QuicClientConfig};

/// Generous read limit for echoed streams in this skeleton.
const READ_LIMIT: usize = 64 * 1024;

/// The client's signing identity (Ed25519). The `CertificateVerify` signature
/// is produced by the inner [`RawEd25519Signer`] — for production this is an
/// [`AgentSigner`] (ssh-agent; private key never read, AUTH-04).
pub struct ClientIdentity {
    signer: Arc<dyn RawEd25519Signer>,
}

impl ClientIdentity {
    /// Build an identity from a raw Ed25519 signer (in-process; for tests).
    pub fn from_signer(signer: Arc<dyn RawEd25519Signer>) -> Self {
        Self { signer }
    }

    /// Build an identity from an on-disk OpenSSH Ed25519 private key file.
    ///
    /// This is platform-agnostic (opt-in on Linux; the ONLY auth path on
    /// Windows — D-03/D-04). Key material is loaded in the narrowest scope
    /// inside [`FileSigner`] (D-05): the seed is zeroized and the
    /// `ssh_key::PrivateKey` is dropped at end of the constructor.
    ///
    /// Passphrase-encrypted keys are rejected with actionable guidance (D-06).
    pub fn from_identity_file(path: &Path) -> anyhow::Result<Self> {
        let signer = FileSigner::from_path(path)?;
        Ok(Self {
            signer: Arc::new(signer),
        })
    }

    /// Build an identity backed by ssh-agent.
    ///
    /// Only available on Unix (ssh-agent uses Unix domain sockets — WIN-01).
    /// On Windows, use [`from_identity_file`][Self::from_identity_file] instead.
    ///
    /// `socket_path` is the agent socket (`SSH_AUTH_SOCK`). `identity_pub`, when
    /// `Some`, selects which agent key to use (path to a `.pub`); when `None`,
    /// the agent's single key is used (error if 0 or >1).
    #[cfg(unix)]
    pub fn from_agent(socket_path: PathBuf, identity_pub: Option<&Path>) -> anyhow::Result<Self> {
        let public_key = match identity_pub {
            Some(p) => ssh_key::PublicKey::read_openssh_file(p)
                .with_context(|| format!("read identity public key {}", p.display()))?,
            None => {
                let mut client = ssh_agent_connect(&socket_path)?;
                #[allow(deprecated)]
                let mut ids = client
                    .list_identities()
                    .context("list ssh-agent identities")?;
                match ids.len() {
                    1 => ids.remove(0),
                    0 => anyhow::bail!("ssh-agent has no identities; add one with ssh-add"),
                    n => anyhow::bail!("ssh-agent has {n} identities; specify one with --identity"),
                }
            }
        };
        let signer = AgentSigner::new(socket_path, public_key)?;
        Ok(Self {
            signer: Arc::new(signer),
        })
    }
}

#[cfg(unix)]
fn ssh_agent_connect(path: &Path) -> anyhow::Result<ssh_agent_client_rs::Client> {
    ssh_agent_client_rs::Client::connect(path)
        .with_context(|| format!("connect ssh-agent at {}", path.display()))
}

/// Build a quinn `ClientConfig` with SSH-key mutual auth: pin the server host
/// key against `known_hosts` (TOFU, AUTH-02) and present the agent-signed
/// client identity cert (AUTH-04). ALPN `nosh/0`; keep-alive enabled (TRANS-05).
pub fn build_client_config(
    identity: &ClientIdentity,
    known_hosts: PathBuf,
    host: impl Into<String>,
) -> anyhow::Result<quinn::ClientConfig> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let provider = rustls::crypto::CryptoProvider::get_default()
        .context("no default CryptoProvider installed")?
        .clone();

    // Mint the client identity cert whose SPKI is the SSH key (the one agent
    // signature for the cert self-signature is acceptable — the private key is
    // still never read by nosh).
    let cert = nosh_auth::mint_self_signed_cert(&identity.signer)?;
    let signing_key = Arc::new(AgentSigningKey::new(identity.signer.clone()));
    let resolver = Arc::new(NoshClientCertResolver::new(cert, signing_key));

    let verifier = Arc::new(HostKeyVerifier::new(known_hosts, host, provider));

    let mut rustls_cfg = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_client_cert_resolver(resolver);
    rustls_cfg.alpn_protocols = vec![nosh_proto::ALPN.to_vec()];

    let quic_crypto =
        QuicClientConfig::try_from(rustls_cfg).context("convert rustls client config to QUIC")?;
    let mut client_config = quinn::ClientConfig::new(Arc::new(quic_crypto));
    // true = enable keep-alive (TRANS-05).
    client_config.transport_config(Arc::new(nosh_proto::transport_config(true)));

    Ok(client_config)
}

/// Build a client `Endpoint` (ephemeral local UDP port) with a nosh client
/// config (mutual auth) as its default.
pub fn make_endpoint(
    identity: &ClientIdentity,
    known_hosts: PathBuf,
    host: impl Into<String>,
) -> anyhow::Result<quinn::Endpoint> {
    let mut endpoint =
        quinn::Endpoint::client("0.0.0.0:0".parse().unwrap()).context("create client endpoint")?;
    endpoint.set_default_client_config(build_client_config(identity, known_hosts, host)?);
    Ok(endpoint)
}

/// Build a client `Endpoint` with a CALLER-SUPPLIED transport config, used by the
/// test harness to inject a qlog stream (D-05) without altering the production
/// path. The transport config replaces the one that `build_client_config` would
/// normally produce; all other aspects (mutual auth, ALPN) are unchanged.
///
/// Use `nosh_proto::transport_config(true)` as the base and layer additional
/// options (e.g. `.qlog_stream(Some(stream))`) before passing in.
pub fn make_endpoint_with_transport(
    identity: &ClientIdentity,
    known_hosts: PathBuf,
    host: impl Into<String>,
    transport: quinn::TransportConfig,
) -> anyhow::Result<quinn::Endpoint> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let provider = rustls::crypto::CryptoProvider::get_default()
        .context("no default CryptoProvider installed")?
        .clone();

    let cert = nosh_auth::mint_self_signed_cert(&identity.signer)?;
    let signing_key = Arc::new(AgentSigningKey::new(identity.signer.clone()));
    let resolver = Arc::new(NoshClientCertResolver::new(cert, signing_key));
    let verifier = Arc::new(HostKeyVerifier::new(known_hosts, host, provider));

    let mut rustls_cfg = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_client_cert_resolver(resolver);
    rustls_cfg.alpn_protocols = vec![nosh_proto::ALPN.to_vec()];

    let quic_crypto =
        QuicClientConfig::try_from(rustls_cfg).context("convert rustls client config to QUIC")?;
    let mut client_config = quinn::ClientConfig::new(Arc::new(quic_crypto));
    client_config.transport_config(Arc::new(transport));

    let mut endpoint =
        quinn::Endpoint::client("0.0.0.0:0".parse().unwrap()).context("create client endpoint")?;
    endpoint.set_default_client_config(client_config);
    Ok(endpoint)
}

/// Connect to `server_addr` and assert the negotiated ALPN is `nosh/0`
/// (TRANS-01). Returns the established connection.
///
/// `connect_timeout` bounds the QUIC handshake establishment wait. If no
/// server responds within the timeout, an error naming the address and timeout
/// is returned (T-09-05 DoS hardening — a dead/black-hole server no longer
/// hangs the client indefinitely).
pub async fn connect(
    endpoint: &quinn::Endpoint,
    server_addr: SocketAddr,
    host: &str,
    connect_timeout: std::time::Duration,
) -> anyhow::Result<quinn::Connection> {
    let connecting = endpoint
        .connect(server_addr, host)
        .context("start connect")?;
    let conn = tokio::time::timeout(connect_timeout, connecting)
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "connection to {}:{} timed out after {}s (no response from server)",
                server_addr.ip(),
                server_addr.port(),
                connect_timeout.as_secs_f64().round() as u64,
            )
        })?
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
pub async fn datagram_roundtrip(conn: &quinn::Connection, payload: Bytes) -> anyhow::Result<Bytes> {
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

// ---------------------------------------------------------------------------
// Phase 3: interactive PTY session over a single bidi stream (D-01).
// ---------------------------------------------------------------------------

/// RAII guard that puts the local terminal in raw mode and restores it on
/// `Drop` (SESS-03). Drop fires on normal return, on panic (unwind runs Drop),
/// and on the error path after abrupt network loss (the guard is held in the
/// run scope). It does NOT fire on `SIGKILL` — that is the documented
/// human-verification case.
///
/// On Windows, crossterm's `enable_raw_mode` clears ENABLE_LINE_INPUT,
/// ENABLE_ECHO_INPUT, and ENABLE_PROCESSED_INPUT but does NOT set
/// ENABLE_VIRTUAL_TERMINAL_INPUT — so arrow keys, PageUp/Down, and other
/// special keys are NOT delivered as ANSI escape sequences, and Ctrl-C is
/// consumed as a console signal (exit 130) rather than forwarded to the
/// remote as 0x03. This guard adds a `#[cfg(windows)]` extension that:
///   - sets ENABLE_VIRTUAL_TERMINAL_INPUT on stdin so special keys encode ANSI
///   - ensures ENABLE_PROCESSED_INPUT is cleared so Ctrl-C arrives as 0x03
///   - sets ENABLE_VIRTUAL_TERMINAL_PROCESSING on stdout so server ANSI renders
///   - saves both original modes and restores them in Drop before disable_raw_mode
///
/// (STATE.md 2026-05-30 finding: Phase 8 D-02 partial gap — VT input not enabled)
pub struct RawModeGuard {
    /// Original stdin console mode (Windows only; saves original for restore in Drop).
    #[cfg(windows)]
    orig_stdin_mode: u32,
    /// Original stdout console mode (Windows only; saves original for restore in Drop).
    #[cfg(windows)]
    orig_stdout_mode: u32,
}

impl RawModeGuard {
    /// Enter raw mode. The returned guard restores cooked mode when dropped.
    pub fn enable() -> std::io::Result<Self> {
        crossterm::terminal::enable_raw_mode()?;

        // ── Windows console VT-input extension ──────────────────────────────────
        // crossterm's enable_raw_mode on Windows clears line/echo/processed-input
        // but does NOT set ENABLE_VIRTUAL_TERMINAL_INPUT on the stdin handle.
        // Without it, special keys (arrows, PageUp/Down) are not encoded as ANSI
        // escape sequences, and Ctrl-C terminates the process (exit 130) instead
        // of being delivered to the read loop as byte 0x03.
        //
        // Stdin handle flags (STD_INPUT_HANDLE):
        //   ENABLE_PROCESSED_INPUT         0x0001 — must be CLEARED (Ctrl-C → 0x03)
        //   ENABLE_LINE_INPUT              0x0002 — cleared by crossterm already
        //   ENABLE_ECHO_INPUT              0x0004 — cleared by crossterm already
        //   ENABLE_VIRTUAL_TERMINAL_INPUT  0x0200 — must be SET (ANSI escape sequences)
        //
        // Stdout handle flags (STD_OUTPUT_HANDLE; numeric values are independent
        // of the stdin flags — 0x0004 on stdout is a DIFFERENT constant than on stdin):
        //   ENABLE_VIRTUAL_TERMINAL_PROCESSING 0x0004 — must be SET (render ANSI from server)
        #[cfg(windows)]
        {
            use std::io;
            use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
            use windows_sys::Win32::System::Console::{
                GetConsoleMode, GetStdHandle, SetConsoleMode,
                ENABLE_ECHO_INPUT, ENABLE_LINE_INPUT, ENABLE_PROCESSED_INPUT,
                ENABLE_VIRTUAL_TERMINAL_INPUT, ENABLE_VIRTUAL_TERMINAL_PROCESSING,
                STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
            };

            // -- stdin --
            // SAFETY: GetStdHandle returns a borrowed handle; we do not close it.
            let stdin_handle = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
            if stdin_handle == INVALID_HANDLE_VALUE {
                // Undo crossterm's raw mode before returning.
                let _ = crossterm::terminal::disable_raw_mode();
                return Err(io::Error::last_os_error());
            }
            let mut orig_stdin_mode: u32 = 0;
            // SAFETY: valid handle; valid pointer; GetConsoleMode is safe to call.
            if unsafe { GetConsoleMode(stdin_handle, &mut orig_stdin_mode) } == 0 {
                let _ = crossterm::terminal::disable_raw_mode();
                return Err(io::Error::last_os_error());
            }
            let new_stdin_mode = (orig_stdin_mode | ENABLE_VIRTUAL_TERMINAL_INPUT)
                & !(ENABLE_PROCESSED_INPUT | ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT);
            // SAFETY: valid handle; SetConsoleMode is safe to call.
            if unsafe { SetConsoleMode(stdin_handle, new_stdin_mode) } == 0 {
                let _ = crossterm::terminal::disable_raw_mode();
                return Err(io::Error::last_os_error());
            }

            // -- stdout --
            // SAFETY: GetStdHandle returns a borrowed handle; we do not close it.
            let stdout_handle = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
            if stdout_handle == INVALID_HANDLE_VALUE {
                // Restore stdin, then undo crossterm.
                let _ = unsafe { SetConsoleMode(stdin_handle, orig_stdin_mode) };
                let _ = crossterm::terminal::disable_raw_mode();
                return Err(io::Error::last_os_error());
            }
            let mut orig_stdout_mode: u32 = 0;
            // SAFETY: valid handle and pointer.
            if unsafe { GetConsoleMode(stdout_handle, &mut orig_stdout_mode) } == 0 {
                let _ = unsafe { SetConsoleMode(stdin_handle, orig_stdin_mode) };
                let _ = crossterm::terminal::disable_raw_mode();
                return Err(io::Error::last_os_error());
            }
            let new_stdout_mode = orig_stdout_mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING;
            // SAFETY: valid handle.
            if unsafe { SetConsoleMode(stdout_handle, new_stdout_mode) } == 0 {
                let _ = unsafe { SetConsoleMode(stdin_handle, orig_stdin_mode) };
                let _ = crossterm::terminal::disable_raw_mode();
                return Err(io::Error::last_os_error());
            }

            return Ok(Self { orig_stdin_mode, orig_stdout_mode });
        }

        #[cfg(not(windows))]
        Ok(Self {})
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        // Restore Windows console modes BEFORE disable_raw_mode so the console
        // is in a sane state even if disable_raw_mode is called during panic unwind.
        //
        // Invariant: if `enable()` returned `Ok(Self {...})` then both handles
        // were valid at construction time. They should still be valid here, but
        // if the process detached from its console after `enable()` returned,
        // `GetStdHandle` may return INVALID_HANDLE_VALUE (-1 as isize) or NULL
        // (0). Guard against both before calling SetConsoleMode so restoration is
        // attempted safely and skipped cleanly if the console detached (WR-01).
        #[cfg(windows)]
        {
            use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
            use windows_sys::Win32::System::Console::{
                GetStdHandle, SetConsoleMode, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
            };
            // SAFETY: GetStdHandle returns a borrowed handle; we do not close it.
            let stdin_handle = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
            let stdout_handle = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
            // Only restore if we have plausibly valid handles; ignore errors
            // regardless — best effort in Drop (T-09-02 mitigation).
            // Note: in windows-sys 0.59 HANDLE is `*mut c_void` (was `isize` in 0.52),
            // so guard against NULL with is_null(), not `!= 0`.
            if stdin_handle != INVALID_HANDLE_VALUE && !stdin_handle.is_null() {
                let _ = unsafe { SetConsoleMode(stdin_handle, self.orig_stdin_mode) };
            }
            if stdout_handle != INVALID_HANDLE_VALUE && !stdout_handle.is_null() {
                let _ = unsafe { SetConsoleMode(stdout_handle, self.orig_stdout_mode) };
            }
        }
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

/// Collect the client's whitelisted, SendEnv-style environment (D-05 / D-09):
/// `TERM`, `LANG`, `TZ`, and every `LC_*`. NEVER includes `SSH_AUTH_SOCK` or
/// `LD_*` (the server also re-filters deny-by-default, but the client does not
/// even offer them). Returned as ordered pairs for deterministic encoding.
///
/// `TERM` is defaulted to `xterm-256color` when not set locally; `LANG` is
/// defaulted to `en_US.UTF-8` when not set. This matters most on Windows where
/// neither is typically set, but also makes headless tests deterministic. The
/// remote server re-filters env deny-by-default; both vars are on its whitelist.
pub fn collect_client_env() -> Vec<(String, String)> {
    let mut env = Vec::new();
    let mut has_term = false;
    let mut has_lang = false;
    for (k, v) in std::env::vars() {
        if k == "TERM" {
            has_term = true;
            env.push((k, v));
        } else if k == "LANG" {
            has_lang = true;
            env.push((k, v));
        } else if k == "TZ" || k.starts_with("LC_") {
            env.push((k, v));
        }
    }
    // Inject defaults when not present in the local environment.
    if !has_term {
        env.push(("TERM".to_string(), "xterm-256color".to_string()));
    }
    if !has_lang {
        env.push(("LANG".to_string(), "en_US.UTF-8".to_string()));
    }
    env
}

// ── Phase 6: reattach helpers ──────────────────────────────────────────────

/// Outcome of a reattach attempt.
#[derive(Debug, Clone, PartialEq)]
pub enum ReattachOutcome {
    /// Reattach succeeded. The client MUST replace its stored token with
    /// `new_token` immediately (single-use rotation, D-05).
    /// Token MUST NOT be logged — log only the identity fingerprint.
    Ok {
        /// Rotated single-use reattach token.
        new_token: [u8; 16],
        /// The sequence number of the first replayed chunk.
        replaying_from_seq: u64,
        /// `true` when the server's buffer was truncated below the requested
        /// resume point. Display a notice to the user (D-09).
        truncated: bool,
    },
    /// Reattach failed — session gone, token expired, wrong identity, or
    /// already-attached (D-07 opaque/uniform error). Do NOT retry indefinitely
    /// on this outcome; the session is terminal (D-11).
    Err,
}

/// Open the session bidi stream, send `SessionOpen`, and read the server's
/// `SessionOpened { token }` response (Phase 6 D-03). Returns the stream pair
/// plus the initial reattach token.
///
/// The token MUST NOT be logged — log only the identity fingerprint (D-07).
pub async fn open_session_with_token(
    conn: &quinn::Connection,
    term: String,
    cols: u16,
    rows: u16,
    env: Vec<(String, String)>,
) -> anyhow::Result<(quinn::SendStream, quinn::RecvStream, [u8; 16])> {
    let (send, mut recv) = open_session(conn, term, cols, rows, env).await?;
    // Read the next frame; the server sends SessionOpened immediately after
    // registering the slot (before any PTY output — guaranteed by the server).
    match nosh_proto::read_message(&mut recv).await {
        Ok(Message::SessionOpened { token }) => Ok((send, recv, token)),
        // W3 / D-07: never Debug a frame (could carry a token) — use the variant name.
        Ok(other) => anyhow::bail!("expected SessionOpened, got {}", other.variant_name()),
        Err(e) => anyhow::bail!("failed to read SessionOpened: {e}"),
    }
}

/// Send a `Reattach` frame as the FIRST frame on a new connection's bidi stream.
///
/// `last_acked_seq` is the next-expected-seq == the COUNT of output chunks the
/// client has applied (as documented on `Message::Reattach`). Use the value
/// tracked by the client (`highest_applied`); 0 if the client applied nothing
/// since the last fresh open. The server replays all chunks with
/// `seq >= last_acked_seq`.
pub async fn send_reattach(
    send: &mut quinn::SendStream,
    token: [u8; 16],
    last_acked_seq: u64,
) -> anyhow::Result<()> {
    nosh_proto::write_message(send, &Message::Reattach { token, last_acked_seq })
        .await
        .context("send Reattach")
}

/// Send a periodic `Ack { seq }` frame (D-08 continuous acking). `seq` is the
/// next-expected-seq == count of output chunks the client has applied (same
/// convention as `Message::Reattach::last_acked_seq`).
pub async fn send_ack(send: &mut quinn::SendStream, seq: u64) -> anyhow::Result<()> {
    nosh_proto::write_message(send, &Message::Ack { seq })
        .await
        .context("send Ack")
}

/// Read the server's reply to a `Reattach` frame (the first frame on the new
/// stream after sending `Reattach`). Returns `ReattachOutcome::Ok` or `::Err`.
pub async fn await_reattach_reply(recv: &mut quinn::RecvStream) -> anyhow::Result<ReattachOutcome> {
    match nosh_proto::read_message(recv).await {
        Ok(Message::ReattachOk {
            new_token,
            replaying_from_seq,
            truncated,
        }) => Ok(ReattachOutcome::Ok {
            new_token,
            replaying_from_seq,
            truncated,
        }),
        Ok(Message::ReattachErr) => Ok(ReattachOutcome::Err),
        // W3 / D-07: never Debug a frame (could carry a token) — use the variant name.
        Ok(other) => anyhow::bail!("unexpected reply to Reattach: {}", other.variant_name()),
        Err(e) => anyhow::bail!("failed to read reattach reply: {e}"),
    }
}

/// Headless reattach driver for integration tests. Opens a bidi stream, sends
/// `Reattach { token, last_acked_seq }`, awaits the reply:
/// - On `ReattachOutcome::Ok`: collects subsequent `PtyData` until `SessionClose`
///   or stream close (like `collect_until_close`), returns the output + exit code.
/// - On `ReattachOutcome::Err` (including connection-level errors from the server
///   closing the connection after sending ReattachErr): returns
///   `(ReattachOutcome::Err, vec![], 0)`.
pub async fn reattach_collect(
    conn: &quinn::Connection,
    token: [u8; 16],
    last_acked_seq: u64,
) -> anyhow::Result<(ReattachOutcome, Vec<u8>, i32)> {
    let (mut send, mut recv) = conn.open_bi().await.context("open bi for reattach")?;
    send_reattach(&mut send, token, last_acked_seq).await?;
    // await_reattach_reply may fail with a connection error if the server closed
    // the connection (on rejection, the server sends ReattachErr then closes the
    // connection). Treat any read error here as a ReattachErr outcome — the
    // server has indicated rejection by closing the connection.
    let outcome = match await_reattach_reply(&mut recv).await {
        Ok(o) => o,
        Err(_) => ReattachOutcome::Err,
    };
    match outcome {
        ReattachOutcome::Err => Ok((ReattachOutcome::Err, Vec::new(), 0)),
        ref ok @ ReattachOutcome::Ok { .. } => {
            let ok_clone = ok.clone();
            let (output, exit_code) = collect_until_close(&mut recv).await?;
            Ok((ok_clone, output, exit_code))
        }
    }
}

/// Open the session bidi stream and send the `SessionOpen` frame.
pub async fn open_session(
    conn: &quinn::Connection,
    term: String,
    cols: u16,
    rows: u16,
    env: Vec<(String, String)>,
) -> anyhow::Result<(quinn::SendStream, quinn::RecvStream)> {
    let (mut send, recv) = conn.open_bi().await.context("open session stream")?;
    nosh_proto::write_message(
        &mut send,
        &Message::SessionOpen {
            term,
            cols,
            rows,
            env,
        },
    )
    .await
    .context("send SessionOpen")?;
    Ok((send, recv))
}

/// Send keystrokes (or any input bytes) as a `PtyData` frame.
pub async fn send_input(send: &mut quinn::SendStream, bytes: &[u8]) -> anyhow::Result<()> {
    nosh_proto::write_message(
        send,
        &Message::PtyData {
            data: bytes.to_vec(),
        },
    )
    .await
    .context("send PtyData")
}

/// Send a window resize (SESS-05).
pub async fn send_resize(send: &mut quinn::SendStream, cols: u16, rows: u16) -> anyhow::Result<()> {
    nosh_proto::write_message(send, &Message::Resize { cols, rows })
        .await
        .context("send Resize")
}

/// Headless session driver for tests: open a session, write `input_script` as a
/// single `PtyData` frame, then read frames collecting all PTY output until a
/// `SessionClose` arrives (or the stream closes). Returns the collected output
/// bytes and the exit code. No terminal/raw-mode involvement, so it runs in CI.
///
/// Phase 6: reads and discards the `SessionOpened` frame (the initial reattach
/// token) that the server now sends right after session open.
pub async fn run_session_collect(
    conn: &quinn::Connection,
    term: &str,
    cols: u16,
    rows: u16,
    env: Vec<(String, String)>,
    input_script: &[u8],
) -> anyhow::Result<(Vec<u8>, i32)> {
    // open_session_with_token reads the SessionOpened frame and discards the token.
    let (mut send, mut recv, _token) =
        open_session_with_token(conn, term.to_string(), cols, rows, env).await?;
    send_input(&mut send, input_script).await?;
    collect_until_close(&mut recv).await
}

/// Read frames from `recv`, appending `PtyData` payloads to a buffer, until a
/// `SessionClose` (returning its exit code) or the stream closes (exit code 0).
pub async fn collect_until_close(recv: &mut quinn::RecvStream) -> anyhow::Result<(Vec<u8>, i32)> {
    let mut output = Vec::new();
    loop {
        match nosh_proto::read_message(recv).await {
            Ok(Message::PtyData { data }) => output.extend_from_slice(&data),
            Ok(Message::SessionClose { exit_code, .. }) => return Ok((output, exit_code)),
            Ok(_) => {} // ignore unexpected control frames in the headless driver
            Err(_) => return Ok((output, 0)), // stream closed without an explicit close
        }
    }
}
