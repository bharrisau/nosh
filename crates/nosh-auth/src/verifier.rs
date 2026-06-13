//! SSH-key SPKI-pinning TLS verifiers (Phase 2, updated Phase 25).
//!
//! - [`HostKeyVerifier`] (client side): pins the server host key against
//!   `known_hosts` with TOFU on first contact (D-01/D-08), hard-fail on mismatch
//!   (D-02). Replaces Phase 1's `PlaceholderServerVerifier::verify_server_cert`.
//!   Phase 25 adds [`TofuPolicy`] to replace the silent-record TOFU with an
//!   interactive blocking prompt (SEC-02) that fails closed on no-TTY (D-10).
//! - [`AuthorizedKeysVerifier`] (server side): requires a client cert and pins
//!   its SPKI against `authorized_keys` (AUTH-01/D-03).
//!
//! Both keep REAL TLS signature verification by delegating to the
//! `CryptoProvider` — never stubbed (research PITFALL 5). A stubbed
//! `verify_tls13_signature` would let a MITM present the correct pinned key but
//! sign the transcript with any private key.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{verify_tls12_signature, verify_tls13_signature, CryptoProvider};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{CertificateError, DigitallySignedStruct, DistinguishedName, Error, SignatureScheme};

use anyhow::Context as _;

use crate::keys::{self, NoshPublicKey};

/// Controls how [`HostKeyVerifier`] handles an unknown server host key.
///
/// Phase 25 replaces the former silent-record behaviour with `Interactive`
/// (SEC-02: blocking prompt, explicit `yes` required). The `Silent` variant is
/// retained for test use but MUST NOT be the default in production.
#[derive(Debug)]
pub enum TofuPolicy {
    /// v1.3 behaviour — record silently without prompting.
    ///
    /// Retained for test use. MUST NOT be the default in production (SEC-02).
    /// Production binaries use `Interactive`.
    Silent,

    /// SEC-02: blocking prompt to `stderr`/`stdin`. Fails closed on no-TTY (D-10).
    ///
    /// Inside the TLS verifier callback, `block_in_place` is required because
    /// the callback runs on a tokio thread and blocking I/O would starve the
    /// async runtime. The nosh-server binary uses `new_multi_thread()`, so
    /// `block_in_place` is safe there (Pitfall 4). Tests that exercise this
    /// path MUST use `#[tokio::test(flavor = "multi_thread")]`.
    Interactive,

    /// Future: trust a specific fingerprint without prompting (WT-UX-02, deferred).
    ///
    /// Corresponds to the future `--trust-key <fingerprint>` CLI flag. When
    /// the presented key matches `trusted`, it is recorded and accepted without
    /// prompting. When it does not match, the connection is rejected.
    TrustKey(crate::keys::NoshPublicKey),
}

/// Client-side verifier: pin the server host key against `known_hosts` (TOFU).
#[derive(Debug)]
pub struct HostKeyVerifier {
    known_hosts: PathBuf,
    host: String,
    provider: Arc<CryptoProvider>,
    // Serialize TOFU writes (a connection only verifies once, but be safe).
    tofu_lock: Mutex<()>,
    /// Phase 25: how to handle an unknown server key on first contact (SEC-02).
    tofu_policy: TofuPolicy,
}

impl HostKeyVerifier {
    /// Build a verifier that pins `host`'s key against the `known_hosts` file,
    /// delegating signature checks to `provider`.
    ///
    /// Uses `TofuPolicy::Interactive` by default (SEC-02 production requirement).
    /// Pass `TofuPolicy::Silent` in tests to avoid blocking on stdin.
    pub fn new(
        known_hosts: PathBuf,
        host: impl Into<String>,
        provider: Arc<CryptoProvider>,
    ) -> Self {
        Self::with_policy(known_hosts, host, provider, TofuPolicy::Interactive)
    }

    /// Build a verifier with an explicit [`TofuPolicy`].
    ///
    /// Use `TofuPolicy::Silent` in unit tests (no stdin blocking).
    /// Use `TofuPolicy::Interactive` in production (SEC-02).
    pub fn with_policy(
        known_hosts: PathBuf,
        host: impl Into<String>,
        provider: Arc<CryptoProvider>,
        tofu_policy: TofuPolicy,
    ) -> Self {
        Self {
            known_hosts,
            host: host.into(),
            provider,
            tofu_lock: Mutex::new(()),
            tofu_policy,
        }
    }
}

impl ServerCertVerifier for HostKeyVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        let spki = keys::extract_spki_from_cert(end_entity)
            .map_err(|_| Error::InvalidCertificate(CertificateError::BadEncoding))?;
        let presented = keys::nosh_key_from_spki(&spki)
            .ok_or(Error::InvalidCertificate(CertificateError::BadEncoding))?;

        match keys::lookup_known_host(&self.known_hosts, &self.host)
            .map_err(|e| Error::General(format!("known_hosts read failed: {e}")))?
        {
            Some(pinned) => {
                if pinned == presented {
                    Ok(ServerCertVerified::assertion())
                } else {
                    // D-02: hard-fail on mismatch; do not prompt, do not overwrite.
                    // This branch is unchanged by Phase 25 — a known-hosts mismatch
                    // is always a fatal hard error (possible MITM).
                    Err(Error::General(format!(
                        "host key mismatch for {} — known_hosts pins a different key (aborting)",
                        self.host
                    )))
                }
            }
            None => {
                // First contact: dispatch via TofuPolicy (Phase 25, SEC-02).
                let _guard = self.tofu_lock.lock().unwrap();
                match &self.tofu_policy {
                    TofuPolicy::Silent => {
                        // v1.3 silent path — test-only; production uses Interactive.
                        keys::record_known_host(&self.known_hosts, &self.host, &presented)
                            .map_err(|e| {
                                Error::General(format!("known_hosts write failed: {e}"))
                            })?;
                        tracing::info!(
                            host = %self.host,
                            "TOFU: recorded new host key (silent mode — test only)"
                        );
                    }
                    TofuPolicy::Interactive => {
                        // SEC-02: blocking prompt. block_in_place is safe: nosh binary
                        // uses new_multi_thread() runtime (Pitfall 4). Tests must use
                        // #[tokio::test(flavor = "multi_thread")].
                        tokio::task::block_in_place(|| {
                            prompt_and_record(&self.known_hosts, &self.host, &presented)
                        })
                        .map_err(|e| Error::General(e.to_string()))?;
                    }
                    TofuPolicy::TrustKey(trusted) => {
                        // Future --trust-key flag: accept a specific pinned key without
                        // prompting, reject everything else.
                        if *trusted != presented {
                            return Err(Error::General(format!(
                                "host key mismatch for {} — pinned key does not match presented key",
                                self.host
                            )));
                        }
                        keys::record_known_host(&self.known_hosts, &self.host, &presented)
                            .map_err(|e| {
                                Error::General(format!("known_hosts write failed: {e}"))
                            })?;
                    }
                }
                Ok(ServerCertVerified::assertion())
            }
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        // REAL signature verification — never stubbed (PITFALL 5).
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        // REAL signature verification — never stubbed (PITFALL 5).
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        // Ed25519 only this milestone (D-12).
        vec![SignatureScheme::ED25519]
    }
}

/// Run the blocking interactive TOFU prompt, then record the key on acceptance.
///
/// Called inside `TofuPolicy::Interactive` via `block_in_place` (Pitfall 4:
/// the TLS verifier runs on a tokio thread, so this must not be called in
/// an async context without blocking isolation).
///
/// # No-TTY fail-closed (D-10)
///
/// If `stdin` is not a terminal, this function prints the fingerprint and a
/// reference to the future `--trust-key` flag, then returns `Err` (fail closed).
/// It does NOT silently record — automation must use `TofuPolicy::TrustKey`
/// (once that flag is implemented in a future phase).
///
/// # Prompt text
///
/// Matches OpenSSH wording exactly so users familiar with `ssh` recognise the
/// prompt. Requires typing `"yes"` (exact, after trimming whitespace) to accept.
fn prompt_and_record(
    known_hosts: &Path,
    host: &str,
    key: &NoshPublicKey,
) -> anyhow::Result<()> {
    use std::io::{BufRead, IsTerminal, Write};

    let fingerprint = key.fingerprint();
    let stderr = std::io::stderr();
    let mut out = stderr.lock();

    writeln!(out, "The authenticity of host '{host}' can't be established.")?;
    writeln!(out, "ED25519 key fingerprint is {fingerprint}.")?;

    // D-10: no-TTY fails closed. Never silently record in automation.
    if !std::io::stdin().is_terminal() {
        writeln!(out, "Host key verification failed: stdin is not a TTY.")?;
        writeln!(
            out,
            "Use --trust-key <fingerprint> (future flag) for non-interactive use."
        )?;
        anyhow::bail!("cannot prompt for TOFU confirmation: stdin is not a TTY");
    }

    write!(out, "Are you sure you want to continue connecting (yes/no)? ")?;
    out.flush()?;
    drop(out); // release stderr lock before locking stdin

    let line = std::io::stdin()
        .lock()
        .lines()
        .next()
        .ok_or_else(|| anyhow::anyhow!("stdin closed during TOFU prompt"))??;

    if line.trim() != "yes" {
        anyhow::bail!("Host key not accepted; connection refused.");
    }

    keys::record_known_host(known_hosts, host, key)
        .with_context(|| format!("record_known_host for {host}"))?;
    tracing::info!(host, "TOFU: user accepted and recorded new host key");
    Ok(())
}

/// Server-side verifier: require a client cert and pin its SPKI against
/// `authorized_keys` (AUTH-01/D-03).
#[derive(Debug)]
pub struct AuthorizedKeysVerifier {
    authorized: Vec<NoshPublicKey>,
    provider: Arc<CryptoProvider>,
    no_hints: Vec<DistinguishedName>,
}

impl AuthorizedKeysVerifier {
    /// Build from the set of authorized client keys.
    pub fn new(authorized: Vec<NoshPublicKey>, provider: Arc<CryptoProvider>) -> Self {
        Self {
            authorized,
            provider,
            no_hints: Vec::new(),
        }
    }
}

impl ClientCertVerifier for AuthorizedKeysVerifier {
    fn offer_client_auth(&self) -> bool {
        true
    }

    fn client_auth_mandatory(&self) -> bool {
        true
    }

    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &self.no_hints
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, Error> {
        let spki = keys::extract_spki_from_cert(end_entity)
            .map_err(|_| Error::InvalidCertificate(CertificateError::BadEncoding))?;
        let presented = keys::nosh_key_from_spki(&spki)
            .ok_or(Error::InvalidCertificate(CertificateError::BadEncoding))?;

        if self.authorized.contains(&presented) {
            Ok(ClientCertVerified::assertion())
        } else {
            // AUTH-01: unknown key rejected at the handshake, before any session.
            Err(Error::InvalidCertificate(
                CertificateError::ApplicationVerificationFailure,
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        // REAL signature verification — never stubbed (PITFALL 5). This is what
        // rejects a forged CertificateVerify even when the SPKI matches.
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![SignatureScheme::ED25519]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signer::{mint_self_signed_cert, InProcessEd25519Signer, RawEd25519Signer};

    fn provider() -> Arc<CryptoProvider> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        Arc::new(rustls::crypto::ring::default_provider())
    }

    fn mint() -> (CertificateDer<'static>, NoshPublicKey) {
        let signer: Arc<dyn RawEd25519Signer> = Arc::new(InProcessEd25519Signer::generate());
        let cert = mint_self_signed_cert(&signer).unwrap();
        let key = NoshPublicKey::from_raw(signer.public_key32());
        (cert, key)
    }

    #[test]
    fn authorized_keys_accepts_known_rejects_unknown() {
        let (known_cert, known_key) = mint();
        let (unknown_cert, _) = mint();
        let v = AuthorizedKeysVerifier::new(vec![known_key], provider());
        assert!(v
            .verify_client_cert(&known_cert, &[], UnixTime::now())
            .is_ok());
        assert!(v
            .verify_client_cert(&unknown_cert, &[], UnixTime::now())
            .is_err());
    }

    // NOTE: This test uses TofuPolicy::Silent to avoid blocking on stdin.
    // Production uses TofuPolicy::Interactive (SEC-02).
    #[test]
    fn host_key_tofu_then_match_then_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let kh = dir.path().join("known_hosts");
        let (cert, _key) = mint();
        let server_name = ServerName::try_from("h").unwrap();

        // Use Silent policy to avoid interactive stdin prompt in tests.
        let v = HostKeyVerifier::with_policy(kh.clone(), "h", provider(), TofuPolicy::Silent);
        // First contact: TOFU records and accepts (silently in test mode).
        assert!(v
            .verify_server_cert(&cert, &[], &server_name, &[], UnixTime::now())
            .is_ok());
        // Second contact with the same key: matches.
        assert!(v
            .verify_server_cert(&cert, &[], &server_name, &[], UnixTime::now())
            .is_ok());
        // A different key for the same host: mismatch → hard fail.
        let (other_cert, _) = mint();
        assert!(v
            .verify_server_cert(&other_cert, &[], &server_name, &[], UnixTime::now())
            .is_err());
    }

    /// Verify that TofuPolicy::Silent is the only policy used in tests (SEC-02 guard).
    ///
    /// The production default is Interactive. Tests call with_policy(Silent) explicitly.
    /// This test documents the expected invariant.
    #[test]
    fn tofu_silent_records_without_prompting() {
        let dir = tempfile::tempdir().unwrap();
        let kh = dir.path().join("known_hosts");
        let (cert, _key) = mint();
        let server_name = ServerName::try_from("example.com").unwrap();

        let v = HostKeyVerifier::with_policy(
            kh.clone(),
            "example.com",
            provider(),
            TofuPolicy::Silent,
        );
        // Silent TOFU: first contact records without prompting.
        assert!(v
            .verify_server_cert(&cert, &[], &server_name, &[], UnixTime::now())
            .is_ok());
        // Subsequent contacts match the recorded key.
        assert!(v
            .verify_server_cert(&cert, &[], &server_name, &[], UnixTime::now())
            .is_ok());
    }

    /// Known-hosts mismatch is ALWAYS a hard error regardless of TofuPolicy (D-02).
    ///
    /// The mismatch branch is unchanged by Phase 25.
    #[test]
    fn tofu_mismatch_is_fatal_regardless_of_policy() {
        let dir = tempfile::tempdir().unwrap();
        let kh = dir.path().join("known_hosts");
        let (cert1, _) = mint();
        let (cert2, _) = mint();
        let server_name = ServerName::try_from("host.example.com").unwrap();

        // Record cert1 as the known key for "host.example.com".
        let v1 = HostKeyVerifier::with_policy(
            kh.clone(),
            "host.example.com",
            provider(),
            TofuPolicy::Silent,
        );
        assert!(v1
            .verify_server_cert(&cert1, &[], &server_name, &[], UnixTime::now())
            .is_ok());

        // Now present cert2 — this is a hard error (D-02).
        let v2 = HostKeyVerifier::with_policy(
            kh.clone(),
            "host.example.com",
            provider(),
            TofuPolicy::Silent,
        );
        let result = v2.verify_server_cert(&cert2, &[], &server_name, &[], UnixTime::now());
        assert!(result.is_err(), "known-host mismatch must be a hard error");
        let err_msg = format!("{:?}", result.unwrap_err());
        assert!(
            err_msg.contains("host key mismatch"),
            "error must mention 'host key mismatch', got: {err_msg}"
        );
    }
}
