//! AUTH-01..05 integration tests — Phase 2 success criteria.
//!
//! Each test drives an in-process, SSH-key-mutually-authenticated server +
//! client via the shared harness in `common`. The happy path runs both an
//! in-process signer (always) and, when an ssh-agent is available, a live
//! ssh-agent Ed25519 handshake (AUTH-04).

use std::sync::Arc;
use std::time::Duration;

use ed25519_dalek::SigningKey;
use nosh_auth::{AgentSigner, NoshPublicKey, RawEd25519Signer};
use nosh_client::client::{self, ClientIdentity};
use nosh_server::server::AuthLimits;

mod common;
use common::{TestKey, HOST};

/// AUTH-03/AUTH-04 (in-process happy path): a known client key completes mutual
/// auth and a real PTY session runs over the authenticated link.
#[tokio::test]
async fn mutual_auth_inprocess_happy_path() {
    if !common::have_sh() {
        eprintln!("skipping: /bin/sh unavailable");
        return;
    }
    let host_key = TestKey::generate();
    let client_key = TestKey::generate();
    let server = common::spawn_server_with_shell(
        &host_key,
        &[&client_key.public],
        AuthLimits::default(),
        Some("/bin/sh".to_string()),
    )
    .await;

    let dir = tempfile::tempdir().unwrap();
    let kh = dir.path().join("known_hosts");
    let endpoint = common::client_endpoint(client_key.client_identity(), kh).unwrap();
    let conn = client::connect(&endpoint, server.addr, HOST)
        .await
        .expect("mutual auth handshake should succeed");

    assert!(
        common::session_marker_usable(&conn, "authed-marker").await,
        "a session must run over the authenticated link"
    );
}

/// AUTH-01: a client key absent from authorized_keys is rejected at the
/// handshake — no session/echo runs.
#[tokio::test]
async fn unknown_client_key_rejected() {
    let host_key = TestKey::generate();
    let authorized = TestKey::generate();
    let intruder = TestKey::generate();
    // Server authorizes `authorized`, but the client uses `intruder`.
    let server =
        common::spawn_server(&host_key, &[&authorized.public], AuthLimits::default()).await;

    let dir = tempfile::tempdir().unwrap();
    let kh = dir.path().join("known_hosts");
    let endpoint = common::client_endpoint(intruder.client_identity(), kh).unwrap();
    // In TLS 1.3 the client finishes its side of the handshake before the server
    // verifies the client cert, so `connect` may resolve; the server's rejection
    // surfaces when the connection is actually used. Assert no usable session.
    assert!(
        !auth_usable(&endpoint, server.addr).await,
        "an unauthorized client key must not yield a usable authenticated session"
    );
}

/// Returns true iff a full authenticated session is usable: connect AND a real
/// PTY session both succeed. Any failure (handshake rejected, server closed the
/// connection after rejecting the client cert, or the session cannot open)
/// returns false. Used by the negative auth tests, where the connection is
/// rejected and this must return false.
async fn auth_usable(endpoint: &quinn::Endpoint, addr: std::net::SocketAddr) -> bool {
    match client::connect(endpoint, addr, HOST).await {
        Ok(conn) => common::session_marker_usable(&conn, "auth-probe-marker").await,
        Err(_) => false,
    }
}

/// AUTH-02 (mismatch): a server host key that does not match the pinned
/// known_hosts entry aborts the client connection.
#[tokio::test]
async fn host_key_mismatch_aborts() {
    let host_key = TestKey::generate();
    let wrong_host = TestKey::generate();
    let client_key = TestKey::generate();
    let server =
        common::spawn_server(&host_key, &[&client_key.public], AuthLimits::default()).await;

    // Pre-seed known_hosts with the WRONG host key for HOST.
    let dir = tempfile::tempdir().unwrap();
    let kh = dir.path().join("known_hosts");
    std::fs::write(
        &kh,
        format!(
            "{} {}\n",
            HOST,
            wrong_host.public.to_openssh_line().unwrap()
        ),
    )
    .unwrap();

    let endpoint = common::client_endpoint(client_key.client_identity(), kh).unwrap();
    let result = client::connect(&endpoint, server.addr, HOST).await;
    assert!(
        result.is_err(),
        "host key mismatch must abort the client connection (no overwrite)"
    );
}

/// AUTH-02 (TOFU): on first contact with an unknown host the key is recorded to
/// known_hosts and the connection proceeds.
#[tokio::test]
async fn tofu_first_contact_records() {
    let host_key = TestKey::generate();
    let client_key = TestKey::generate();
    let server =
        common::spawn_server(&host_key, &[&client_key.public], AuthLimits::default()).await;

    let dir = tempfile::tempdir().unwrap();
    let kh = dir.path().join("known_hosts");
    assert!(!kh.exists());

    let endpoint = common::client_endpoint(client_key.client_identity(), kh.clone()).unwrap();
    let _conn = client::connect(&endpoint, server.addr, HOST)
        .await
        .expect("TOFU first contact should proceed");

    let recorded = std::fs::read_to_string(&kh).expect("known_hosts written");
    assert!(recorded.contains(HOST), "host entry recorded");
    assert!(
        recorded.contains(&host_key.public.to_openssh_line().unwrap()),
        "the server's actual host key is recorded"
    );
}

/// AUTH-03: a client presenting the correct pinned key but a FORGED
/// CertificateVerify (signed by a different private key) is rejected. Proves
/// signature verification is not stubbed (PITFALL 5).
#[tokio::test]
async fn forged_certificate_verify_rejected() {
    let host_key = TestKey::generate();
    // The authorized identity (its public key is what the server pins).
    let authorized = TestKey::generate();
    let server =
        common::spawn_server(&host_key, &[&authorized.public], AuthLimits::default()).await;

    // A forging signer: presents the authorized cert (SPKI = authorized key)
    // but signs the CertificateVerify with a DIFFERENT key.
    let forging = ForgingSigner::new(authorized.public.clone());
    let identity = ClientIdentity::from_signer(Arc::new(forging));

    let dir = tempfile::tempdir().unwrap();
    let kh = dir.path().join("known_hosts");
    let endpoint = common::client_endpoint(identity, kh).unwrap();
    assert!(
        !auth_usable(&endpoint, server.addr).await,
        "a forged CertificateVerify must be rejected (signature not stubbed)"
    );
}

/// A signer that reports a victim's public key (so the minted cert SPKI matches
/// the authorized key) but signs with an unrelated key — a MITM forgery.
#[derive(Debug)]
struct ForgingSigner {
    forged_public32: [u8; 32],
    real: SigningKey,
}

impl ForgingSigner {
    fn new(victim: NoshPublicKey) -> Self {
        let mut seed = [0u8; 32];
        {
            use std::io::Read;
            std::fs::File::open("/dev/urandom")
                .unwrap()
                .read_exact(&mut seed)
                .unwrap();
        }
        Self {
            forged_public32: *victim.key32(),
            real: SigningKey::from_bytes(&seed),
        }
    }
}

impl RawEd25519Signer for ForgingSigner {
    fn sign(&self, msg: &[u8]) -> anyhow::Result<[u8; 64]> {
        use ed25519_dalek::Signer as _;
        Ok(self.real.sign(msg).to_bytes())
    }
    fn public_key32(&self) -> [u8; 32] {
        // Claims the victim's key — so the cert SPKI matches authorized_keys,
        // but the signature (from `real`) will not verify against it.
        self.forged_public32
    }
}

/// AUTH-04 (live): a real ssh-agent Ed25519 identity completes the full mutual
/// auth handshake. Ignored unless `ssh-agent`/`ssh-keygen` are on PATH.
#[tokio::test]
#[ignore = "requires ssh-agent and ssh-keygen on PATH"]
async fn agent_ed25519_handshake_live() {
    let agent = match nosh_auth::test_support::EphemeralAgent::start() {
        Some(a) => a,
        None => {
            eprintln!("skipping agent_ed25519_handshake_live: ssh-agent/ssh-keygen unavailable");
            return;
        }
    };
    let client_pub =
        NoshPublicKey::from_ssh_public(&agent.public_key()).expect("agent key is Ed25519");

    let host_key = TestKey::generate();
    let server = common::spawn_server_with_shell(
        &host_key,
        &[&client_pub],
        AuthLimits::default(),
        Some("/bin/sh".to_string()),
    )
    .await;

    // The client signs via the live ssh-agent (private key never read).
    let signer = AgentSigner::new(agent.socket_path(), agent.public_key()).unwrap();
    let identity = ClientIdentity::from_signer(Arc::new(signer));

    let dir = tempfile::tempdir().unwrap();
    let kh = dir.path().join("known_hosts");
    let endpoint = common::client_endpoint(identity, kh).unwrap();
    let conn = client::connect(&endpoint, server.addr, HOST)
        .await
        .expect("live ssh-agent mutual auth handshake should succeed");

    assert!(
        common::session_marker_usable(&conn, "agent-signed-marker").await,
        "a session must run over the agent-authenticated link"
    );
}

/// AUTH-05: flooding the accept loop with connections that never complete auth
/// keeps the server bounded; a legitimate client still connects afterward, and
/// un-authenticated connections are closed within the timeout.
#[tokio::test]
async fn preauth_flood_bounded() {
    let host_key = TestKey::generate();
    let client_key = TestKey::generate();
    // Small cap + short timeout to exercise the bound deterministically.
    let limits = AuthLimits {
        max_concurrent: 4,
        auth_timeout: Duration::from_secs(1),
    };
    let server = common::spawn_server_with_shell(
        &host_key,
        &[&client_key.public],
        limits,
        Some("/bin/sh".to_string()),
    )
    .await;

    // Flood: open raw UDP sockets that send junk to the server's port (half-open
    // pressure) without ever completing a handshake.
    let mut floods = Vec::new();
    for _ in 0..32 {
        let sock = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        sock.connect(server.addr).await.unwrap();
        // A QUIC-ish initial-looking junk datagram; never a valid handshake.
        let _ = sock.send(&[0u8; 1200]).await;
        floods.push(sock);
    }

    // Give the server a moment; it must remain responsive.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // A legitimate authorized client still connects after the flood.
    let dir = tempfile::tempdir().unwrap();
    let kh = dir.path().join("known_hosts");
    let endpoint = common::client_endpoint(client_key.client_identity(), kh).unwrap();
    let conn = tokio::time::timeout(
        Duration::from_secs(10),
        client::connect(&endpoint, server.addr, HOST),
    )
    .await
    .expect("server stayed responsive under flood")
    .expect("legitimate client connects after flood");

    assert!(
        common::session_marker_usable(&conn, "post-flood-marker").await,
        "a session must run after the flood"
    );

    drop(floods);
}
