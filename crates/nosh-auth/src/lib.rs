//! `nosh-auth` — SSH-key authentication for nosh.
//!
//! **Phase 1 ships only a placeholder server verifier.** Phase 2 fills this
//! crate with the real SSH-key cert-pinning verifiers
//! (`ServerCertVerifier`/`ClientCertVerifier` checking SPKI against
//! `known_hosts`/`authorized_keys`) plus ssh-agent signing. The placeholder is
//! deliberately structured as the seam Phase 2 replaces, and it already
//! delegates real TLS signature verification so that swap is minimal and the
//! skeleton is never an open MITM hole (research PITFALL 5).

pub mod verifier;

pub use verifier::PlaceholderServerVerifier;
