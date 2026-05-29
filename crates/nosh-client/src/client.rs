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
    AgentSigner, AgentSigningKey, HostKeyVerifier, NoshClientCertResolver, RawEd25519Signer,
};
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

    /// Build an identity backed by ssh-agent.
    ///
    /// `socket_path` is the agent socket (`SSH_AUTH_SOCK`). `identity_pub`, when
    /// `Some`, selects which agent key to use (path to a `.pub`); when `None`,
    /// the agent's single key is used (error if 0 or >1).
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

/// Connect to `server_addr` and assert the negotiated ALPN is `nosh/0`
/// (TRANS-01). Returns the established connection.
pub async fn connect(
    endpoint: &quinn::Endpoint,
    server_addr: SocketAddr,
    host: &str,
) -> anyhow::Result<quinn::Connection> {
    let conn = endpoint
        .connect(server_addr, host)
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
pub struct RawModeGuard;

impl RawModeGuard {
    /// Enter raw mode. The returned guard restores cooked mode when dropped.
    pub fn enable() -> std::io::Result<Self> {
        crossterm::terminal::enable_raw_mode()?;
        Ok(Self)
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

/// Collect the client's whitelisted, SendEnv-style environment (D-05): `TERM`,
/// `LANG`, `TZ`, and every `LC_*`. NEVER includes `SSH_AUTH_SOCK` or `LD_*`
/// (the server also re-filters deny-by-default, but the client does not even
/// offer them). Returned as ordered pairs for deterministic encoding.
pub fn collect_client_env() -> Vec<(String, String)> {
    let mut env = Vec::new();
    for (k, v) in std::env::vars() {
        if k == "TERM" || k == "LANG" || k == "TZ" || k.starts_with("LC_") {
            env.push((k, v));
        }
    }
    env
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
pub async fn run_session_collect(
    conn: &quinn::Connection,
    term: &str,
    cols: u16,
    rows: u16,
    env: Vec<(String, String)>,
    input_script: &[u8],
) -> anyhow::Result<(Vec<u8>, i32)> {
    let (mut send, mut recv) = open_session(conn, term.to_string(), cols, rows, env).await?;
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
