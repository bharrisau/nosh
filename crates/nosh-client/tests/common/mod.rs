//! Shared test harness for the integration tests: builds an in-process,
//! SSH-key-mutually-authenticated nosh server + client using throwaway Ed25519
//! keys and temp trust files. Used by both `transport.rs` (transport proofs
//! over an authenticated link) and `auth.rs` (the AUTH-01..05 tests).

#![allow(dead_code)]

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ed25519_dalek::SigningKey;
use nosh_auth::{InProcessEd25519Signer, NoshPublicKey, RawEd25519Signer};
use nosh_client::client::{self, ClientIdentity};
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
    use std::io::Read;
    let mut f = std::fs::File::open("/dev/urandom").unwrap();
    f.read_exact(buf).unwrap();
}

/// A running in-process server with its trust-file scratch dir.
pub struct TestServer {
    pub addr: SocketAddr,
    pub handle: tokio::task::JoinHandle<()>,
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
    let handle = tokio::spawn(async move {
        let _ = server::run_accept_loop(endpoint, limits).await;
    });
    TestServer {
        addr,
        handle,
        _dir: dir,
    }
}

/// Build a client endpoint pinning the server against `known_hosts`.
pub fn client_endpoint(
    identity: ClientIdentity,
    known_hosts: PathBuf,
) -> anyhow::Result<quinn::Endpoint> {
    client::make_endpoint(&identity, known_hosts, HOST)
}

/// A temp known_hosts path (empty → TOFU on first contact).
pub fn empty_known_hosts(dir: &Path) -> PathBuf {
    dir.join("known_hosts")
}
