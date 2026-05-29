//! `nosh-auth` — SSH-key mutual authentication for nosh.
//!
//! Phase 2 implements SSH-key cert-pinning enforced inside the TLS 1.3
//! handshake:
//!
//! - [`keys`] — Ed25519 SSH key loading, SPKI construction, and
//!   `authorized_keys`/`known_hosts` handling (Ed25519 only — decision D-12).
//! - [`signer`] — the ssh-agent / in-process Ed25519 signers, the self-signed
//!   cert minting, and the rustls `SigningKey` that routes the client
//!   `CertificateVerify` signature through ssh-agent (AUTH-04).
//! - [`verifier`] — the client-side [`HostKeyVerifier`] (SPKI pinning against
//!   `known_hosts` + TOFU) and the server-side [`AuthorizedKeysVerifier`]
//!   (`ClientCertVerifier` pinning against `authorized_keys`). Both keep REAL
//!   TLS signature verification — never stubbed (research PITFALL 5).

pub mod keys;
pub mod signer;
pub mod verifier;

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

pub use keys::{load_authorized_keys, load_host_key, NoshPublicKey, ED25519_SPKI_LEN};
pub use signer::{
    mint_self_signed_cert, AgentSigner, AgentSigningKey, InProcessEd25519Signer,
    NoshClientCertResolver, NoshServerCertResolver, RawEd25519Signer,
};
pub use verifier::{AuthorizedKeysVerifier, HostKeyVerifier};
