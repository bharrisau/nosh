//! Ed25519 signing abstractions:
//!
//! - [`RawEd25519Signer`] — a tiny trait producing a raw 64-byte Ed25519
//!   signature over arbitrary bytes. Two impls: [`AgentSigner`] (routes to
//!   ssh-agent — the private key is never read, AUTH-04/D-04) and
//!   [`InProcessEd25519Signer`] (an in-process `ed25519-dalek` key, used for the
//!   host key and as the test/agent-unavailable fallback).
//! - [`mint_self_signed_cert`] — mints an ephemeral X.509 cert whose SPKI **is**
//!   the SSH Ed25519 public key (D-09), signed by the same key via the signer.
//! - [`AgentSigningKey`] — a `rustls::sign::SigningKey` whose `sign()` produces
//!   the TLS 1.3 `CertificateVerify` signature via the inner signer. rustls
//!   passes the fully-constructed CertificateVerify input (RFC 8446 §4.4.3) as
//!   `message`; we sign it raw — we MUST NOT reconstruct it (PITFALL 8).

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use rustls::pki_types::{CertificateDer, SubjectPublicKeyInfoDer};
use rustls::sign::{Signer, SigningKey};
use rustls::{SignatureAlgorithm, SignatureScheme};

use crate::keys::ed25519_spki_der;

/// Produces a raw 64-byte Ed25519 signature over `msg`.
pub trait RawEd25519Signer: Send + Sync + std::fmt::Debug {
    /// Sign `msg` with the Ed25519 private key, returning the 64-byte signature.
    fn sign(&self, msg: &[u8]) -> anyhow::Result<[u8; 64]>;
    /// The raw 32-byte Ed25519 public key.
    fn public_key32(&self) -> [u8; 32];
}

/// Signs via the ssh-agent at `socket_path` for the given Ed25519 identity.
/// The private key is never handled by nosh (AUTH-04 / decision D-04).
#[derive(Debug, Clone)]
pub struct AgentSigner {
    socket_path: PathBuf,
    public_key: ssh_key::PublicKey,
    key32: [u8; 32],
}

impl AgentSigner {
    /// Build an agent signer for `public_key`, talking to the agent socket at
    /// `socket_path` (typically `$SSH_AUTH_SOCK`). Rejects non-Ed25519 keys.
    pub fn new(socket_path: PathBuf, public_key: ssh_key::PublicKey) -> anyhow::Result<Self> {
        let key32 = public_key
            .key_data()
            .ed25519()
            .map(|e| e.0)
            .context("ssh-agent identity is not Ed25519 (Ed25519 only in this milestone)")?;
        Ok(Self {
            socket_path,
            public_key,
            key32,
        })
    }
}

impl RawEd25519Signer for AgentSigner {
    fn sign(&self, msg: &[u8]) -> anyhow::Result<[u8; 64]> {
        let mut client = ssh_agent_client_rs::Client::connect(&self.socket_path)
            .with_context(|| format!("connect ssh-agent at {}", self.socket_path.display()))?;
        let sig = client
            .sign(self.public_key.clone(), msg)
            .context("ssh-agent sign request failed")?;
        let bytes = sig.as_bytes();
        bytes
            .try_into()
            .map_err(|_| anyhow::anyhow!("ssh-agent returned a {}-byte signature, expected 64", bytes.len()))
    }

    fn public_key32(&self) -> [u8; 32] {
        self.key32
    }
}

/// An in-process Ed25519 signer (the server host key, and the test fallback when
/// no ssh-agent is available). Holds the private key directly — appropriate for a
/// daemon-loaded host key (D-06) and for tests, NOT for the client identity.
#[derive(Debug)]
pub struct InProcessEd25519Signer {
    key: ed25519_dalek::SigningKey,
}

impl InProcessEd25519Signer {
    /// Wrap an `ed25519-dalek` signing key.
    pub fn new(key: ed25519_dalek::SigningKey) -> Self {
        Self { key }
    }

    /// Build from an OpenSSH Ed25519 `PrivateKey` (the host key file path).
    pub fn from_ssh_private(private: &ssh_key::PrivateKey) -> anyhow::Result<Self> {
        let kp = private
            .key_data()
            .ed25519()
            .context("host key is not Ed25519 (Ed25519 only in this milestone)")?;
        let key = ed25519_dalek::SigningKey::from_bytes(&kp.private.to_bytes());
        Ok(Self::new(key))
    }

    /// Generate a fresh random key (tests).
    pub fn generate() -> Self {
        use ed25519_dalek::SigningKey;
        let mut seed = [0u8; 32];
        getrandom_seed(&mut seed);
        Self::new(SigningKey::from_bytes(&seed))
    }
}

/// Fill `buf` with OS randomness (small helper to avoid pulling rand directly).
fn getrandom_seed(buf: &mut [u8; 32]) {
    use std::io::Read;
    // /dev/urandom is always available on the Linux spike target.
    let mut f = std::fs::File::open("/dev/urandom").expect("open /dev/urandom");
    f.read_exact(buf).expect("read /dev/urandom");
}

impl RawEd25519Signer for InProcessEd25519Signer {
    fn sign(&self, msg: &[u8]) -> anyhow::Result<[u8; 64]> {
        use ed25519_dalek::Signer as _;
        Ok(self.key.sign(msg).to_bytes())
    }

    fn public_key32(&self) -> [u8; 32] {
        self.key.verifying_key().to_bytes()
    }
}

/// rcgen `SigningKey` adapter: reports the SSH Ed25519 public key as the cert
/// SPKI and delegates the TBS signature to the inner [`RawEd25519Signer`].
/// Holds the 32 raw public-key bytes so `der_bytes()` can return a borrow.
#[derive(Debug)]
struct RawKeyHolder {
    inner: Arc<dyn RawEd25519Signer>,
    key32: [u8; 32],
}

impl rcgen::PublicKeyData for RawKeyHolder {
    fn der_bytes(&self) -> &[u8] {
        &self.key32
    }
    fn algorithm(&self) -> &'static rcgen::SignatureAlgorithm {
        &rcgen::PKCS_ED25519
    }
}

impl rcgen::SigningKey for RawKeyHolder {
    fn sign(&self, msg: &[u8]) -> Result<Vec<u8>, rcgen::Error> {
        self.inner
            .sign(msg)
            .map(|s| s.to_vec())
            .map_err(|_| rcgen::Error::RingUnspecified)
    }
}

/// Mint an ephemeral self-signed X.509 cert whose SubjectPublicKeyInfo IS the
/// signer's Ed25519 public key (decision D-09). The cert is self-signed using
/// the same key via `signer`. Our pinning verifiers do not rely on the cert's
/// self-signature — they pin on SPKI and verify the live CertificateVerify — but
/// minting a valid self-signature keeps the cert well-formed.
pub fn mint_self_signed_cert(
    signer: &Arc<dyn RawEd25519Signer>,
) -> anyhow::Result<CertificateDer<'static>> {
    let holder = RawKeyHolder {
        inner: signer.clone(),
        key32: signer.public_key32(),
    };
    let mut params = rcgen::CertificateParams::new(vec!["nosh".to_string()])
        .context("build certificate params")?;
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "nosh");
    let cert = params
        .self_signed(&holder)
        .context("mint self-signed nosh identity cert")?;
    Ok(cert.der().clone())
}

/// A `rustls::sign::SigningKey` that produces the client's `CertificateVerify`
/// signature via an [`RawEd25519Signer`] (Ed25519 only).
#[derive(Debug)]
pub struct AgentSigningKey {
    inner: Arc<dyn RawEd25519Signer>,
    spki: Vec<u8>,
}

impl AgentSigningKey {
    /// Build from any raw Ed25519 signer.
    pub fn new(inner: Arc<dyn RawEd25519Signer>) -> Self {
        let spki = ed25519_spki_der(&inner.public_key32());
        Self { inner, spki }
    }
}

impl SigningKey for AgentSigningKey {
    fn choose_scheme(&self, offered: &[SignatureScheme]) -> Option<Box<dyn Signer>> {
        if offered.contains(&SignatureScheme::ED25519) {
            Some(Box::new(Ed25519HandshakeSigner {
                inner: self.inner.clone(),
            }))
        } else {
            None
        }
    }

    fn public_key(&self) -> Option<SubjectPublicKeyInfoDer<'_>> {
        Some(SubjectPublicKeyInfoDer::from(self.spki.clone()))
    }

    fn algorithm(&self) -> SignatureAlgorithm {
        SignatureAlgorithm::ED25519
    }
}

/// A `ResolvesServerCert` presenting the nosh host-key identity cert, signing
/// the server `CertificateVerify` with the host key (D-06, in-process). Reuses
/// the same Ed25519 [`AgentSigningKey`] wrapper as the client.
#[derive(Debug)]
pub struct NoshServerCertResolver {
    certified: Arc<rustls::sign::CertifiedKey>,
}

impl NoshServerCertResolver {
    /// Build from the host identity cert and the host Ed25519 signing key.
    pub fn new(cert: CertificateDer<'static>, key: Arc<AgentSigningKey>) -> Self {
        Self {
            certified: Arc::new(rustls::sign::CertifiedKey::new(vec![cert], key)),
        }
    }
}

impl rustls::server::ResolvesServerCert for NoshServerCertResolver {
    fn resolve(&self, _client_hello: rustls::server::ClientHello<'_>) -> Option<Arc<rustls::sign::CertifiedKey>> {
        Some(self.certified.clone())
    }
}

/// A `ResolvesClientCert` that always presents the nosh identity cert and signs
/// the `CertificateVerify` via the inner [`AgentSigningKey`] (Ed25519 only).
#[derive(Debug)]
pub struct NoshClientCertResolver {
    certified: Arc<rustls::sign::CertifiedKey>,
}

impl NoshClientCertResolver {
    /// Build from the identity cert and the agent-backed signing key.
    pub fn new(cert: CertificateDer<'static>, key: Arc<AgentSigningKey>) -> Self {
        Self {
            certified: Arc::new(rustls::sign::CertifiedKey::new(vec![cert], key)),
        }
    }
}

impl rustls::client::ResolvesClientCert for NoshClientCertResolver {
    fn resolve(
        &self,
        _root_hint_subjects: &[&[u8]],
        sigschemes: &[SignatureScheme],
    ) -> Option<Arc<rustls::sign::CertifiedKey>> {
        if sigschemes.contains(&SignatureScheme::ED25519) {
            Some(self.certified.clone())
        } else {
            None
        }
    }

    fn has_certs(&self) -> bool {
        true
    }
}

/// The per-handshake `Signer`. `message` is the fully-built TLS 1.3
/// CertificateVerify input (PITFALL 8) — signed raw, no reconstruction.
#[derive(Debug)]
struct Ed25519HandshakeSigner {
    inner: Arc<dyn RawEd25519Signer>,
}

impl Signer for Ed25519HandshakeSigner {
    fn sign(&self, message: &[u8]) -> Result<Vec<u8>, rustls::Error> {
        self.inner
            .sign(message)
            .map(|s| s.to_vec())
            .map_err(|e| rustls::Error::General(format!("Ed25519 signing failed: {e}")))
    }

    fn scheme(&self) -> SignatureScheme {
        SignatureScheme::ED25519
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::extract_spki_from_cert;

    #[test]
    fn inprocess_sign_verifies() {
        use ed25519_dalek::{Verifier, VerifyingKey};
        let signer = InProcessEd25519Signer::generate();
        let pk32 = signer.public_key32();
        let msg = b"the-quic-certificate-verify-input";
        let sig = signer.sign(msg).unwrap();
        let vk = VerifyingKey::from_bytes(&pk32).unwrap();
        vk.verify(msg, &ed25519_dalek::Signature::from_bytes(&sig))
            .expect("signature must verify");
    }

    #[test]
    fn minted_cert_spki_matches_key() {
        let signer: Arc<dyn RawEd25519Signer> = Arc::new(InProcessEd25519Signer::generate());
        let cert = mint_self_signed_cert(&signer).unwrap();
        let spki = extract_spki_from_cert(&cert).unwrap();
        assert_eq!(spki, ed25519_spki_der(&signer.public_key32()));
    }

    /// Live ssh-agent round-trip (AUTH-04). Ignored unless ssh-agent + ssh-keygen
    /// are available; run with `--ignored`.
    #[test]
    #[ignore = "requires ssh-agent and ssh-keygen on PATH"]
    fn agent_ed25519_sign_roundtrip() {
        use ed25519_dalek::{Verifier, VerifyingKey};
        let agent = match crate::test_support::EphemeralAgent::start() {
            Some(a) => a,
            None => {
                eprintln!("skipping: ssh-agent/ssh-keygen unavailable");
                return;
            }
        };
        let signer = AgentSigner::new(agent.socket_path(), agent.public_key()).unwrap();
        let msg = b"agent-signed-certificate-verify";
        let sig = signer.sign(msg).unwrap();
        let vk = VerifyingKey::from_bytes(&signer.public_key32()).unwrap();
        vk.verify(msg, &ed25519_dalek::Signature::from_bytes(&sig))
            .expect("agent signature must verify");
    }
}
