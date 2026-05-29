//! Placeholder client-side TLS verifier — the Phase 2 cert-pinning seam.
//!
//! Phase 1 does NOT pin keys yet (that is Phase 2's SSH `known_hosts`/TOFU
//! work). But the signature-verification machinery here is REAL: it delegates
//! to the rustls `CryptoProvider`. This keeps the skeleton honest — a fully
//! stubbed verifier that no-ops `verify_tls13_signature` is a MITM hole even at
//! the skeleton stage (research PITFALL 5) — and makes Phase 2's swap minimal.

use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{verify_tls12_signature, verify_tls13_signature, CryptoProvider};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, Error, SignatureScheme};

/// A `ServerCertVerifier` that accepts ANY server certificate (no pinning yet)
/// but performs REAL TLS signature verification via the `CryptoProvider`.
///
/// PLACEHOLDER: `verify_server_cert` accepts any cert. Phase 2 replaces this
/// method with SSH-key SPKI pinning against `known_hosts` (TOFU on first
/// contact). The signature methods below are already correct and will not need
/// to change.
#[derive(Debug)]
pub struct PlaceholderServerVerifier {
    provider: Arc<CryptoProvider>,
}

impl PlaceholderServerVerifier {
    /// Build a verifier delegating signature checks to `provider`.
    pub fn new(provider: Arc<CryptoProvider>) -> Self {
        Self { provider }
    }
}

impl ServerCertVerifier for PlaceholderServerVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        // TODO(phase-2): replace with SSH-key SPKI pinning against known_hosts
        // (TOFU on first contact). PLACEHOLDER: accept any cert for the
        // transport skeleton. The signature verification below stays REAL so
        // this swap is the only change Phase 2 needs here.
        Ok(ServerCertVerified::assertion())
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
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}
