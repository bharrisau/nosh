//! Shared test harness for the integration tests: builds an in-process,
//! SSH-key-mutually-authenticated nosh server + client using throwaway Ed25519
//! keys and temp trust files. Used by both `transport.rs` (transport proofs
//! over an authenticated link) and `auth.rs` (the AUTH-01..05 tests).

#![allow(dead_code)]

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ed25519_dalek::SigningKey;
use nosh_auth::{InProcessEd25519Signer, NoshPublicKey, RawEd25519Signer, TofuPolicy};
use nosh_client::client::{self, ClientIdentity};
use nosh_client::quinn_transport::QuinnTransport;
use nosh_server::registry::SessionRegistry;
use nosh_server::server::{self, AuthLimits};
use ssh_key::private::Ed25519Keypair;
use ssh_key::{LineEnding, PrivateKey};
use tempfile::TempDir;

/// The QUIC SNI / known_hosts host key used across tests.
pub const HOST: &str = "localhost";

/// A throwaway Ed25519 keypair usable as a `RawEd25519Signer`, a pinned public
/// key, and (via its seed) an OpenSSH private-key file.
pub struct TestKey {
    seed: [u8; 32],
    pub signer: Arc<dyn RawEd25519Signer>,
    pub public: NoshPublicKey,
}

impl TestKey {
    pub fn generate() -> Self {
        let mut seed = [0u8; 32];
        fill_random(&mut seed);
        Self::from_seed(seed)
    }

    pub fn from_seed(seed: [u8; 32]) -> Self {
        let dalek = SigningKey::from_bytes(&seed);
        let inproc = InProcessEd25519Signer::new(dalek);
        let public = NoshPublicKey::from_raw(inproc.public_key32());
        Self {
            seed,
            signer: Arc::new(inproc),
            public,
        }
    }

    /// The matching OpenSSH `PrivateKey` (for writing a host-key file).
    pub fn ssh_private(&self) -> PrivateKey {
        let kp = Ed25519Keypair::from_seed(&self.seed);
        PrivateKey::from(kp)
    }

    /// A `ClientIdentity` backed by this key's in-process signer.
    pub fn client_identity(&self) -> ClientIdentity {
        ClientIdentity::from_signer(self.signer.clone())
    }
}

fn fill_random(buf: &mut [u8; 32]) {
    getrandom::getrandom(buf).expect("getrandom failed");
}

/// A running in-process server with its trust-file scratch dir.
pub struct TestServer {
    pub addr: SocketAddr,
    pub handle: tokio::task::JoinHandle<()>,
    /// The shared session registry — tests can query orphan counts, etc.
    pub registry: Arc<SessionRegistry>,
    _dir: TempDir,
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

/// Write the host key file + authorized_keys, then start the server.
pub async fn spawn_server(
    host_key: &TestKey,
    authorized: &[&NoshPublicKey],
    limits: AuthLimits,
) -> TestServer {
    spawn_server_with_shell(host_key, authorized, limits, None).await
}

/// Like [`spawn_server`] but lets the session tests force a specific login shell
/// (e.g. `/bin/sh`) for portability via the server `--shell`-equivalent param.
pub async fn spawn_server_with_shell(
    host_key: &TestKey,
    authorized: &[&NoshPublicKey],
    limits: AuthLimits,
    shell_override: Option<String>,
) -> TestServer {
    // Default registry: cap=5, idle_timeout=0 (disabled). Tests that need to
    // inspect the registry can call spawn_server_with_registry instead.
    let registry = SessionRegistry::new(5, Duration::ZERO);
    spawn_server_with_registry(host_key, authorized, limits, shell_override, registry).await
}

/// Full-control spawn: caller supplies its own `Arc<SessionRegistry>` so it can
/// assert orphan counts, inject custom caps/timeouts, etc.
pub async fn spawn_server_with_registry(
    host_key: &TestKey,
    authorized: &[&NoshPublicKey],
    limits: AuthLimits,
    shell_override: Option<String>,
    registry: Arc<SessionRegistry>,
) -> TestServer {
    let dir = tempfile::tempdir().unwrap();
    let host_key_path = dir.path().join("host_ed25519");
    let auth_path = dir.path().join("authorized_keys");

    host_key
        .ssh_private()
        .write_openssh_file(&host_key_path, LineEnding::LF)
        .unwrap();

    let mut ak = String::new();
    for k in authorized {
        ak.push_str(&k.to_openssh_line().unwrap());
        ak.push('\n');
    }
    std::fs::write(&auth_path, ak).unwrap();

    let bind: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let endpoint =
        server::make_endpoint(bind, &host_key_path, &auth_path).expect("server endpoint");
    let addr = endpoint.local_addr().expect("server local_addr");
    let registry_for_task = registry.clone();
    let handle = tokio::spawn(async move {
        let _ = server::run_accept_loop(endpoint, registry_for_task, limits, shell_override).await;
    });
    TestServer {
        addr,
        handle,
        registry,
        _dir: dir,
    }
}

/// Build a client endpoint pinning the server against `known_hosts`.
///
/// Uses `TofuPolicy::Silent` to avoid blocking on stdin during tests (tests are
/// not exercising the interactive prompt; the no-TTY fail-closed and real-auth
/// adversarial paths are covered in Phase 25-04).
pub fn client_endpoint(
    identity: ClientIdentity,
    known_hosts: PathBuf,
) -> anyhow::Result<quinn::Endpoint> {
    client::make_endpoint_with_policy(&identity, known_hosts, HOST, TofuPolicy::Silent)
}

/// Build a client endpoint that writes a qlog trace to `qlog_path` (D-05).
///
/// Identical to `client_endpoint` except the client `TransportConfig` carries a
/// `QlogStream` writing to `qlog_path` (created/truncated). If qlog stream
/// construction fails (e.g. file creation fails or `into_stream()` returns None),
/// the endpoint is built WITHOUT qlog and a warning is printed — qlog setup
/// failure must not fail the connection; Plan 02's qlog artifact assertion will
/// surface the missing file.
///
/// Uses `TofuPolicy::Silent` to avoid blocking on stdin during tests (tests are
/// not exercising the interactive prompt; the no-TTY fail-closed and real-auth
/// adversarial paths are covered in Phase 25-04).
pub fn client_endpoint_with_qlog(
    identity: ClientIdentity,
    known_hosts: PathBuf,
    qlog_path: &Path,
) -> anyhow::Result<quinn::Endpoint> {
    // Build a transport config starting from the standard client settings
    // (keep-alive enabled on the client side — TRANS-05).
    let mut transport = nosh_proto::transport_config(true);

    // Attach the qlog stream. Open/create (truncate) the target file.
    match std::fs::File::create(qlog_path) {
        Ok(file) => {
            let boxed: Box<dyn std::io::Write + Send + Sync> = Box::new(file);
            let mut qlog_cfg = quinn::QlogConfig::default();
            qlog_cfg.writer(boxed);
            qlog_cfg.title(Some("nosh-migration-test".into()));
            match qlog_cfg.into_stream() {
                Some(stream) => {
                    transport.qlog_stream(Some(stream));
                }
                None => {
                    eprintln!(
                        "[qlog] QlogConfig::into_stream() returned None; endpoint will run \
                         without qlog (qlog file will not be created)"
                    );
                }
            }
        }
        Err(e) => {
            eprintln!(
                "[qlog] failed to create qlog file at {}: {e}; endpoint will run without qlog",
                qlog_path.display()
            );
        }
    }

    client::make_endpoint_with_transport_and_policy(
        &identity,
        known_hosts,
        HOST,
        transport,
        TofuPolicy::Silent,
    )
}

/// Bind a fresh `127.0.0.1:0` UDP socket. Used by `rebind_client` to allocate
/// the new local address before handing the socket to quinn.
pub fn fresh_loopback_socket() -> std::io::Result<std::net::UdpSocket> {
    std::net::UdpSocket::bind("127.0.0.1:0")
}

/// Force a QUIC path change by rebinding the client endpoint onto a fresh
/// loopback UDP socket (D-02). Returns the new local address. This triggers
/// QUIC path validation (PATH_CHALLENGE / PATH_RESPONSE) on the existing
/// connection — the same connection continues with no new TLS handshake.
pub fn rebind_client(endpoint: &quinn::Endpoint) -> std::io::Result<std::net::SocketAddr> {
    let socket = fresh_loopback_socket()?;
    let new_addr = socket.local_addr()?;
    endpoint.rebind(socket)?;
    Ok(new_addr)
}

/// A temp known_hosts path (empty → TOFU on first contact).
pub fn empty_known_hosts(dir: &Path) -> PathBuf {
    dir.join("known_hosts")
}

/// True if `/bin/sh` is available (session-usability checks need a shell).
pub fn have_sh() -> bool {
    std::path::Path::new("/bin/sh").exists()
}

// ── WebTransport test harness (webtransport feature only) ─────────────────────

/// A running in-process WebTransport server with its bound address.
///
/// Spawns a `wtransport::Endpoint<Server>` on an ephemeral loopback port using
/// a freshly generated self-signed outer TLS cert, then runs
/// `run_wt_accept_loop` with the test-support auth bypass on a background task.
#[cfg(feature = "webtransport")]
pub struct WtTestServer {
    pub addr: SocketAddr,
    /// SHA-256 hash of the self-signed TLS cert — use with
    /// `wtransport::ClientConfig::builder().with_server_certificate_hashes([hash])`.
    pub cert_hash: wtransport::tls::Sha256Digest,
    pub registry: Arc<SessionRegistry>,
    handle: tokio::task::JoinHandle<()>,
}

#[cfg(feature = "webtransport")]
impl Drop for WtTestServer {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

/// Spawn an in-process WebTransport server on an ephemeral loopback port.
///
/// Uses `Identity::self_signed` (wtransport `self-signed` feature) to generate
/// a fresh outer TLS cert valid for ≤ 14 days. The server runs
/// `run_wt_accept_loop` with an explicit `InnerAuthMode::TestBypass` on a
/// background task — this bypass is an opt-in for Phase-24 shell-pump tests
/// (wt01/wt02/wt03) that exercise the datagram pump and raw-QUIC rejection, not
/// inner auth. Real inner auth is tested via the adversarial tests in Plan 04.
///
/// The returned `WtTestServer` carries the bound `addr` and the cert's
/// SHA-256 `hash` so the test client can use `with_server_certificate_hashes`.
///
/// Returns `None` if the server could not start (endpoint bind / identity
/// generation failed). Callers should `expect` for shell-pump tests and check
/// for downgrade-rejection tests.
#[cfg(feature = "webtransport")]
pub async fn spawn_wt_server(shell: Option<String>) -> Option<WtTestServer> {
    use nosh_server::wt_transport::{run_wt_accept_loop, InnerAuthMode};
    use nosh_server::server::AuthLimits;
    use wtransport::{Identity, ServerConfig, Endpoint};

    let bind: SocketAddr = "127.0.0.1:0".parse().unwrap();

    // Generate a self-signed TLS identity for the outer layer (D-04 test variant).
    // SANs must match the host we dial ("127.0.0.1") so wtransport's SNI check passes.
    let identity = Identity::self_signed(&["localhost", "127.0.0.1"])
        .expect("self-signed identity generation must not fail");

    // Capture the cert hash BEFORE consuming identity (clone_identity required because
    // Identity is not Clone directly — it exposes clone_identity() to make it explicit).
    let cert_hash = identity
        .certificate_chain()
        .as_slice()
        .first()
        .expect("self-signed identity has exactly one cert")
        .hash();

    let server_config = ServerConfig::builder()
        .with_bind_address(bind)
        .with_identity(identity)
        .build();

    let endpoint = Endpoint::server(server_config).expect("bind WT test endpoint");
    let addr = endpoint.local_addr().expect("WT endpoint local_addr");

    let registry = SessionRegistry::new(5, std::time::Duration::ZERO);
    let registry_for_task = registry.clone();

    // Dummy host signer and empty authorized list — not used because TestBypass
    // skips inner auth entirely. Provide valid types to satisfy the function signature.
    let dummy_key = TestKey::generate();
    let host_signer: std::sync::Arc<dyn nosh_auth::RawEd25519Signer> = dummy_key.signer.clone();
    let authorized: std::sync::Arc<Vec<NoshPublicKey>> = std::sync::Arc::new(vec![]);

    let handle = tokio::spawn(async move {
        let _ = run_wt_accept_loop(
            endpoint,
            registry_for_task,
            AuthLimits::default(),
            shell,
            authorized,
            host_signer,
            // Explicit TestBypass: Phase-24 tests exercise the datagram pump and
            // raw-QUIC rejection, not inner auth. Plan 04 converts wt01 to real auth.
            InnerAuthMode::TestBypass,
        )
        .await;
    });

    Some(WtTestServer { addr, cert_hash, registry, handle })
}

/// Prove an authenticated connection yields a USABLE session: open a PTY
/// session, echo a unique marker via the remote shell, and confirm it comes
/// back. Replaces the Phase 2 stream-echo usability probe now that the server
/// runs a real PTY session instead of echo loops. Requires the server to have
/// been started with `--shell /bin/sh` (see `spawn_server_with_shell`).
pub async fn session_marker_usable(conn: &quinn::Connection, marker: &str) -> bool {
    // Wrap the quinn::Connection in QuinnTransport so it satisfies &dyn NoshTransport.
    let qt = QuinnTransport(conn.clone());
    let script = format!("printf '%s\\n' {marker}; exit 0\n");
    match tokio::time::timeout(
        std::time::Duration::from_secs(15),
        client::run_session_collect(&qt, "xterm", 80, 24, Vec::new(), script.as_bytes()),
    )
    .await
    {
        Ok(Ok((out, _code))) => String::from_utf8_lossy(&out).contains(marker),
        _ => false,
    }
}
