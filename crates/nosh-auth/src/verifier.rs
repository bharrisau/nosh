//! SSH-key SPKI-pinning TLS verifiers (Phase 2).
//!
//! - [`HostKeyVerifier`] (client side): pins the server host key against
//!   `known_hosts` with TOFU on first contact (D-01), hard-fail on mismatch
//!   (D-02). Replaces Phase 1's `PlaceholderServerVerifier::verify_server_cert`.
//! - [`AuthorizedKeysVerifier`] (server side): requires a client cert and pins
//!   its SPKI against `authorized_keys` (AUTH-01/D-03).
//!
//! Both keep REAL TLS signature verification by delegating to the
//! `CryptoProvider` — never stubbed (research PITFALL 5). A stubbed
//! `verify_tls13_signature` would let a MITM present the correct pinned key but
//! sign the transcript with any private key.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{verify_tls12_signature, verify_tls13_signature, CryptoProvider};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{CertificateError, DigitallySignedStruct, DistinguishedName, Error, SignatureScheme};

use crate::keys::{self, NoshPublicKey};

/// Client-side verifier: pin the server host key against `known_hosts` (TOFU).
#[derive(Debug)]
pub struct HostKeyVerifier {
    known_hosts: PathBuf,
    host: String,
    provider: Arc<CryptoProvider>,
    // Serialize TOFU writes (a connection only verifies once, but be safe).
    tofu_lock: Mutex<()>,
}

impl HostKeyVerifier {
    /// Build a verifier that pins `host`'s key against the `known_hosts` file,
    /// delegating signature checks to `provider`.
    pub fn new(known_hosts: PathBuf, host: impl Into<String>, provider: Arc<CryptoProvider>) -> Self {
        Self {
            known_hosts,
            host: host.into(),
            provider,
            tofu_lock: Mutex::new(()),
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
        let presented = parse_ed25519_from_spki(&spki)
            .ok_or(Error::InvalidCertificate(CertificateError::BadEncoding))?;

        match keys::lookup_known_host(&self.known_hosts, &self.host)
            .map_err(|e| Error::General(format!("known_hosts read failed: {e}")))?
        {
            Some(pinned) => {
                if pinned == presented {
                    Ok(ServerCertVerified::assertion())
                } else {
                    // D-02: hard-fail on mismatch; do not prompt, do not overwrite.
                    Err(Error::General(format!(
                        "host key mismatch for {} — known_hosts pins a different key (aborting)",
                        self.host
                    )))
                }
            }
            None => {
                // D-01: TOFU — record and proceed silently.
                let _guard = self.tofu_lock.lock().unwrap();
                keys::record_known_host(&self.known_hosts, &self.host, &presented)
                    .map_err(|e| Error::General(format!("known_hosts write failed: {e}")))?;
                tracing::info!(host = %self.host, "TOFU: recorded new host key");
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
        verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        // REAL signature verification — never stubbed (PITFALL 5).
        verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        // Ed25519 only this milestone (D-12).
        vec![SignatureScheme::ED25519]
    }
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
        let presented = parse_ed25519_from_spki(&spki)
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
        verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        // REAL signature verification — never stubbed (PITFALL 5). This is what
        // rejects a forged CertificateVerify even when the SPKI matches.
        verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![SignatureScheme::ED25519]
    }
}

/// Extract the 32-byte Ed25519 key from a 44-byte Ed25519 SPKI DER, validating
/// the fixed prefix. Returns `None` for any non-Ed25519 / malformed SPKI.
fn parse_ed25519_from_spki(spki: &[u8]) -> Option<NoshPublicKey> {
    if spki.len() != keys::ED25519_SPKI_LEN {
        return None;
    }
    let expected_prefix = keys::ed25519_spki_der(&[0u8; 32]);
    if spki[..12] != expected_prefix[..12] {
        return None;
    }
    let mut key32 = [0u8; 32];
    key32.copy_from_slice(&spki[12..]);
    Some(NoshPublicKey::from_raw(key32))
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

    #[test]
    fn host_key_tofu_then_match_then_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let kh = dir.path().join("known_hosts");
        let (cert, _key) = mint();
        let server_name = ServerName::try_from("h").unwrap();

        let v = HostKeyVerifier::new(kh.clone(), "h", provider());
        // First contact: TOFU records and accepts.
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
}
