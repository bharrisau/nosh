//! SSH key material: loading OpenSSH keys, Ed25519 SPKI construction, and
//! `authorized_keys`/`known_hosts` parsing.
//!
//! **Ed25519 only** for this milestone (decision D-12). Any other key type is
//! rejected at load time with a clear message rather than producing a confusing
//! downstream handshake failure.

use std::fs;
use std::io::Write;
use std::path::Path;

use anyhow::{bail, Context};
use rustls::pki_types::CertificateDer;
use ssh_key::{PrivateKey, PublicKey};

/// The fixed DER prefix of an Ed25519 `SubjectPublicKeyInfo` (RFC 8410):
/// `SEQUENCE { SEQUENCE { OID 1.3.101.112 }, BIT STRING (32 bytes) }`.
/// The 32-byte raw key follows this 12-byte prefix for a total of 44 bytes.
const ED25519_SPKI_PREFIX: [u8; 12] = [
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
];

/// Length of a complete Ed25519 SPKI in DER bytes.
pub const ED25519_SPKI_LEN: usize = 44;

/// Build the 44-byte Ed25519 `SubjectPublicKeyInfo` DER for a raw 32-byte key.
pub fn ed25519_spki_der(key32: &[u8; 32]) -> Vec<u8> {
    let mut spki = Vec::with_capacity(ED25519_SPKI_LEN);
    spki.extend_from_slice(&ED25519_SPKI_PREFIX);
    spki.extend_from_slice(key32);
    spki
}

/// A pinned nosh public identity: an Ed25519 key reduced to its raw 32 bytes
/// (and, by extension, its SPKI). Used for both `authorized_keys` (server) and
/// `known_hosts` (client) comparisons — equality is SPKI equality.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NoshPublicKey {
    key32: [u8; 32],
}

impl NoshPublicKey {
    /// Construct from raw 32 Ed25519 bytes.
    pub fn from_raw(key32: [u8; 32]) -> Self {
        Self { key32 }
    }

    /// Extract the Ed25519 key from an OpenSSH `PublicKey`, rejecting non-Ed25519.
    pub fn from_ssh_public(pk: &PublicKey) -> anyhow::Result<Self> {
        match pk.key_data().ed25519() {
            Some(ed) => Ok(Self { key32: ed.0 }),
            None => bail!(
                "unsupported key type {} (Ed25519 only in this milestone)",
                pk.algorithm()
            ),
        }
    }

    /// Parse an OpenSSH single-line public key string.
    pub fn from_openssh_line(line: &str) -> anyhow::Result<Self> {
        let pk = PublicKey::from_openssh(line).context("parse OpenSSH public key")?;
        Self::from_ssh_public(&pk)
    }

    /// The raw 32-byte Ed25519 public key.
    pub fn key32(&self) -> &[u8; 32] {
        &self.key32
    }

    /// The 44-byte Ed25519 SPKI DER for this key.
    pub fn spki_der(&self) -> Vec<u8> {
        ed25519_spki_der(&self.key32)
    }

    /// Render as an OpenSSH `ssh-ed25519 <base64>` line (no comment).
    pub fn to_openssh_line(&self) -> anyhow::Result<String> {
        let ed = ssh_key::public::Ed25519PublicKey(self.key32);
        let pk = PublicKey::from(ed);
        pk.to_openssh().context("encode OpenSSH public key")
    }
}

/// Load an OpenSSH `authorized_keys` file into pinned keys (match on key only;
/// options/comments are ignored — decision D-07). Non-Ed25519 lines are an error.
pub fn load_authorized_keys(path: &Path) -> anyhow::Result<Vec<NoshPublicKey>> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("read authorized_keys {}", path.display()))?;
    let mut keys = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        keys.push(
            NoshPublicKey::from_openssh_line(line)
                .with_context(|| format!("authorized_keys entry: {line}"))?,
        );
    }
    Ok(keys)
}

/// Load an Ed25519 host private key from an OpenSSH key file (decision D-06).
/// Non-Ed25519 keys are rejected.
pub fn load_host_key(path: &Path) -> anyhow::Result<PrivateKey> {
    let key = PrivateKey::read_openssh_file(path)
        .with_context(|| format!("read host key {}", path.display()))?;
    if key.public_key().key_data().ed25519().is_none() {
        bail!(
            "unsupported host key type {} (Ed25519 only in this milestone)",
            key.algorithm()
        );
    }
    Ok(key)
}

/// Look up a host's pinned key in a `known_hosts`-style file.
///
/// Returns `Ok(Some(key))` if the host has a recorded Ed25519 key,
/// `Ok(None)` if the host is absent (or the file does not exist).
/// Matching is by the host token in column 1 (decision D-05; simplified for the
/// spike — no hashed-host or wildcard handling).
pub fn lookup_known_host(path: &Path, host: &str) -> anyhow::Result<Option<NoshPublicKey>> {
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("read known_hosts {}", path.display())),
    };
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.splitn(2, char::is_whitespace);
        let hosts = parts.next().unwrap_or("");
        let keypart = match parts.next() {
            Some(k) => k.trim(),
            None => continue,
        };
        if hosts.split(',').any(|h| h == host) {
            // Only Ed25519 entries are understood this milestone.
            if let Ok(k) = NoshPublicKey::from_openssh_line(keypart) {
                return Ok(Some(k));
            }
        }
    }
    Ok(None)
}

/// Append a host's Ed25519 key to `known_hosts` (TOFU first-contact — D-01).
/// Creates the file (and parent dir) if necessary.
pub fn record_known_host(path: &Path, host: &str, key: &NoshPublicKey) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).ok();
        }
    }
    let line = format!("{host} {}\n", key.to_openssh_line()?);
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("open known_hosts {} for append", path.display()))?;
    f.write_all(line.as_bytes())
        .with_context(|| format!("append to known_hosts {}", path.display()))?;
    Ok(())
}

/// Extract the `SubjectPublicKeyInfo` DER from an X.509 certificate.
///
/// Used by both verifiers to pin on the SPKI bytes (decision D-09/D-10) rather
/// than walking a PKI chain. Returns the raw SPKI DER as presented in the cert.
pub fn extract_spki_from_cert(cert: &CertificateDer<'_>) -> anyhow::Result<Vec<u8>> {
    use x509_parser::prelude::FromDer;
    let (_, parsed) = x509_parser::certificate::X509Certificate::from_der(cert.as_ref())
        .map_err(|e| anyhow::anyhow!("parse peer certificate: {e}"))?;
    Ok(parsed.tbs_certificate.subject_pki.raw.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spki_is_44_bytes_with_prefix() {
        let key = [7u8; 32];
        let spki = ed25519_spki_der(&key);
        assert_eq!(spki.len(), ED25519_SPKI_LEN);
        assert_eq!(&spki[..12], &ED25519_SPKI_PREFIX);
        assert_eq!(&spki[12..], &key);
    }

    #[test]
    fn known_hosts_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("known_hosts");
        let key = NoshPublicKey::from_raw([3u8; 32]);
        assert_eq!(lookup_known_host(&path, "host").unwrap(), None);
        record_known_host(&path, "host", &key).unwrap();
        assert_eq!(lookup_known_host(&path, "host").unwrap(), Some(key));
        assert_eq!(lookup_known_host(&path, "other").unwrap(), None);
    }
}
