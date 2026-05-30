//! Integration tests for `ClientIdentity::from_identity_file` (Plan 08-01 Task 5).
//!
//! These tests validate the `FileSigner` end-to-end on Linux: write an
//! Ed25519 key to disk, load it via `from_identity_file`, and run a real
//! mutual-auth PTY session against an in-process server. This proves WIN-02's
//! signing path on Linux CI without requiring a Windows box (D-03 opt-in).

use nosh_client::client::{self, ClientIdentity};
use nosh_server::server::AuthLimits;
use ssh_key::LineEnding;

mod common;
use common::{TestKey, HOST};

/// D-03 / WIN-02: load a client key from disk via `from_identity_file`,
/// authenticate against an in-process server, and run a real PTY session.
/// Validates the full `FileSigner` path end-to-end on Linux.
#[tokio::test]
async fn identity_file_mutual_auth_happy_path() {
    if !common::have_sh() {
        eprintln!("skipping: /bin/sh unavailable");
        return;
    }

    let host_key = TestKey::generate();
    let client_key = TestKey::generate();

    // Authorize the client key on the server.
    let server = common::spawn_server_with_shell(
        &host_key,
        &[&client_key.public],
        AuthLimits::default(),
        Some("/bin/sh".to_string()),
    )
    .await;

    // Write the client private key to a temp file (as an OpenSSH key file).
    let dir = tempfile::tempdir().unwrap();
    let key_path = dir.path().join("id_ed25519");
    client_key
        .ssh_private()
        .write_openssh_file(&key_path, LineEnding::LF)
        .unwrap();

    // Build ClientIdentity from the on-disk key file (NOT from ssh-agent).
    let identity = ClientIdentity::from_identity_file(&key_path)
        .expect("from_identity_file must succeed for an unencrypted Ed25519 key");

    // Build endpoint with TOFU known_hosts.
    let kh = dir.path().join("known_hosts");
    let endpoint = common::client_endpoint(identity, kh).unwrap();

    // Connect with mutual auth using the file-backed identity.
    let conn = client::connect(&endpoint, server.addr, HOST, std::time::Duration::from_secs(30))
        .await
        .expect("file-key mutual auth should succeed");

    // Prove the authenticated session is usable (runs a real PTY round-trip).
    assert!(
        common::session_marker_usable(&conn, "file-key-marker").await,
        "a session must run over the file-key-authenticated link (WIN-02)"
    );
}

/// D-03: a non-existent path produces a descriptive error (includes the path).
#[tokio::test]
async fn identity_file_missing_is_error() {
    let path = std::path::Path::new("/tmp/nosh-test-nonexistent-key-12345");
    let result = ClientIdentity::from_identity_file(path);
    assert!(result.is_err(), "missing file must produce an error");
    let msg = result.err().unwrap().to_string();
    assert!(
        msg.contains("nosh-test-nonexistent-key-12345"),
        "error must include the path, got: {msg}"
    );
}
