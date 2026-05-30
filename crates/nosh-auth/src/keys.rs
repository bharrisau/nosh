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
#[derive(Clone, PartialEq, Eq)]
pub struct NoshPublicKey {
    key32: [u8; 32],
}

/// Print the OpenSSH fingerprint rather than the raw key bytes (D-07).
/// A `{:?}` log of `NoshPublicKey` must never expose `key32`.
impl std::fmt::Debug for NoshPublicKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NoshPublicKey")
            .field("fingerprint", &self.fingerprint())
            .finish()
    }
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

    /// Returns the OpenSSH-style SHA256 fingerprint of this key: `SHA256:<base64>`.
    ///
    /// The raw private/public key bytes are NEVER included — only the hash (D-07).
    /// Matches `ssh-keygen -l -E sha256` exactly: SHA256 over the SSH wire-format
    /// public-key blob (`string("ssh-ed25519") || string(key32)` with 4-byte
    /// big-endian length prefixes), base64 without padding.
    pub fn fingerprint(&self) -> String {
        let ed = ssh_key::public::Ed25519PublicKey(self.key32);
        let pk = PublicKey::from(ed);
        format!("{}", pk.fingerprint(ssh_key::HashAlg::Sha256))
    }
}

/// Load an OpenSSH `authorized_keys` file into pinned keys (match on key only;
/// options/comments are ignored — decision D-07).
///
/// Unsupported key types (non-Ed25519) and malformed lines are logged via
/// `tracing::warn` and skipped — this matches `sshd(8)` behaviour. The
/// accepted-key set is a strict subset of the set that
/// `NoshPublicKey::from_openssh_line` accepts, so skipping a bad line is
/// fail-closed: no malformed line is ever coerced into an accepted key
/// (T-09-04 security invariant).
pub fn load_authorized_keys(path: &Path) -> anyhow::Result<Vec<NoshPublicKey>> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("read authorized_keys {}", path.display()))?;
    let mut keys = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        match NoshPublicKey::from_openssh_line(line) {
            Ok(key) => keys.push(key),
            Err(e) => {
                // Log only the key-type token (first whitespace-delimited field)
                // and the parse error — never the full line or key material (D-07).
                // Cap the logged token: for a malformed line with no whitespace,
                // split_whitespace().next() returns the entire line, which could
                // be a multi-kilobyte base64 blob (IN-03 / D-07 invariant).
                let key_type_raw = line.split_whitespace().next().unwrap_or("<empty>");
                let key_type = if key_type_raw.len() > 64 {
                    "<malformed-no-whitespace>"
                } else {
                    key_type_raw
                };
                tracing::warn!(
                    key_type,
                    error = %e,
                    "authorized_keys entry unsupported or malformed; skipping (sshd behaviour)"
                );
            }
        }
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

/// Parse a `NoshPublicKey` from a 44-byte Ed25519 `SubjectPublicKeyInfo` DER.
///
/// Returns `None` for any non-Ed25519 or malformed SPKI — the caller must
/// treat this as an auth failure and close the connection (D-04).
///
/// This is the public extraction surface used by the server to thread the
/// authenticated peer identity into the session after the TLS handshake
/// (D-05). It reuses the same validation as [`AuthorizedKeysVerifier`] so
/// the identity stored in the session is provably the authorized key.
pub fn nosh_key_from_spki(spki: &[u8]) -> Option<NoshPublicKey> {
    if spki.len() != ED25519_SPKI_LEN {
        return None;
    }
    let expected_prefix = ed25519_spki_der(&[0u8; 32]);
    if spki[..12] != expected_prefix[..12] {
        return None;
    }
    let mut key32 = [0u8; 32];
    key32.copy_from_slice(&spki[12..]);
    Some(NoshPublicKey::from_raw(key32))
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
    fn nosh_key_from_spki_roundtrip() {
        let original = NoshPublicKey::from_raw([42u8; 32]);
        let spki = original.spki_der();
        let recovered = nosh_key_from_spki(&spki).expect("valid Ed25519 SPKI must parse");
        assert_eq!(original, recovered);
    }

    #[test]
    fn nosh_key_from_spki_rejects_wrong_length() {
        assert!(nosh_key_from_spki(&[0u8; 43]).is_none(), "43 bytes must be rejected");
        assert!(nosh_key_from_spki(&[0u8; 45]).is_none(), "45 bytes must be rejected");
        assert!(nosh_key_from_spki(&[]).is_none(), "empty must be rejected");
    }

    #[test]
    fn nosh_key_from_spki_rejects_wrong_prefix() {
        let mut bad_spki = NoshPublicKey::from_raw([1u8; 32]).spki_der();
        bad_spki[0] ^= 0xff; // corrupt first byte of prefix
        assert!(nosh_key_from_spki(&bad_spki).is_none(), "wrong prefix must be rejected");
    }

    #[test]
    fn fingerprint_format() {
        // SHA256 of the SSH wire blob for an all-zeros Ed25519 key, base64-no-pad.
        // Golden value: computed via `ssh-keygen -l -E sha256` and independently
        // via SHA256(sshstring("ssh-ed25519") || sshstring([0u8;32])).
        let key = NoshPublicKey::from_raw([0u8; 32]);
        let fp = key.fingerprint();
        assert!(fp.starts_with("SHA256:"), "fingerprint must start with SHA256: — got {fp:?}");
        let b64_part = &fp["SHA256:".len()..];
        assert_eq!(b64_part.len(), 43, "SHA256 base64-no-pad is 43 chars — got {b64_part:?}");
        // Must not contain '=' padding characters.
        assert!(!b64_part.contains('='), "base64-no-pad must not contain padding: {fp:?}");
        // The raw key bytes must not appear in the fingerprint output.
        assert!(!fp.contains('\0'), "raw key bytes must not appear in fingerprint");
        // Golden-vector assertion: must match OpenSSH wire-blob hashing, NOT raw-key
        // hashing. `ssh-keygen -l -E sha256` for a zero-byte Ed25519 public key
        // produces this exact value.
        assert_eq!(
            fp, "SHA256:kmYcvdi2GkPeWxB6XLjrZB8JHsy2Hm8luHMFp9GMvqk",
            "fingerprint must match OpenSSH SHA256 wire-blob format (D-07)"
        );
    }

    #[test]
    fn fingerprint_two_different_keys_differ() {
        let k1 = NoshPublicKey::from_raw([1u8; 32]);
        let k2 = NoshPublicKey::from_raw([2u8; 32]);
        assert_ne!(k1.fingerprint(), k2.fingerprint(), "distinct keys must have distinct fingerprints");
    }

    // ── authorized_keys warn+skip tests (T-09-04 / ROBUST-01) ─────────────────

    /// A minimal valid ssh-ed25519 public key line for test fixtures.
    /// (Key: all-zero 32 bytes, base64-encoded in the OpenSSH wire format.)
    const VALID_ED25519_LINE: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIP5OAQXqCeqHDMTf0SnL0jxqJTMFZKzw3LZ7dLVJqPiB test-comment";

    /// A representative ssh-rsa public key line (not Ed25519, not supported).
    const RSA_LINE: &str =
        "ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAAAQQCt6d20tlxUoRqfVJCM8tFg1/tgIH6N0B7H9IFaobIWA2mz4HhTHnxaSKb8JOa/bD7f8p3IlA7rL1h1yN4JxX7 rsa-comment";

    /// A completely malformed / garbage line.
    const GARBAGE_LINE: &str = "not-a-key garbage here $$$";

    /// Test 1: mixed file loads exactly one key (the Ed25519 one).
    #[test]
    fn mixed_authorized_keys_loads_one_ed25519() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("authorized_keys");
        let content = format!("{}\n{}\n{}\n", VALID_ED25519_LINE, RSA_LINE, GARBAGE_LINE);
        fs::write(&path, content).unwrap();

        let keys = load_authorized_keys(&path)
            .expect("load_authorized_keys must succeed even with mixed entries");

        assert_eq!(
            keys.len(),
            1,
            "mixed file must produce exactly 1 key (the Ed25519 one)"
        );

        // Fingerprint of the loaded key must match the known Ed25519 fixture.
        let expected = NoshPublicKey::from_openssh_line(VALID_ED25519_LINE)
            .expect("test fixture must parse")
            .fingerprint();
        assert_eq!(
            keys[0].fingerprint(),
            expected,
            "loaded key fingerprint must match the Ed25519 fixture"
        );
    }

    /// Test 2: a file with only unsupported/malformed lines loads 0 keys and
    /// returns Ok (not Err) — warn+skip is fail-closed, not fail-open.
    #[test]
    fn all_bad_authorized_keys_returns_empty_ok() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("authorized_keys");
        let content = format!("{}\n{}\n", RSA_LINE, GARBAGE_LINE);
        fs::write(&path, content).unwrap();

        let keys = load_authorized_keys(&path)
            .expect("load_authorized_keys must return Ok even if no keys parse");

        assert!(
            keys.is_empty(),
            "all-bad file must produce 0 keys, got {}",
            keys.len()
        );
    }

    /// Test 3 (fail-closed): the set of accepted keys after skipping bad lines
    /// is a subset of the original; no malformed line becomes an accepted key.
    #[test]
    fn skip_is_fail_closed_no_bad_key_admitted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("authorized_keys");
        // Mix: one valid Ed25519, one RSA (rejected by from_openssh_line), one garbage.
        let content = format!("{}\n{}\n{}\n", VALID_ED25519_LINE, RSA_LINE, GARBAGE_LINE);
        fs::write(&path, content).unwrap();

        let keys = load_authorized_keys(&path).unwrap();

        // None of the accepted keys' fingerprints can match the RSA or garbage lines
        // (which have no valid Ed25519 representation). This trivially holds when
        // len == 1, but assert explicitly so the invariant is documented.
        for k in &keys {
            // The only accepted key must be the Ed25519 one.
            let fp = k.fingerprint();
            let expected_fp = NoshPublicKey::from_openssh_line(VALID_ED25519_LINE)
                .unwrap()
                .fingerprint();
            assert_eq!(
                fp, expected_fp,
                "accepted key must match the valid Ed25519 fixture, not a malformed entry"
            );
        }
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
