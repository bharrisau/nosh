---
id: 04-A
title: "Expose identity extraction helpers and add NoshPublicKey::fingerprint"
wave: 1
depends_on: []
files_modified:
  - crates/nosh-auth/src/keys.rs
  - crates/nosh-auth/src/lib.rs
autonomous: true
requirements:
  - IDENT-01
---

# Plan A: nosh-auth — Identity Extraction Surface + Fingerprint Helper

## Objective

Extend `nosh-auth` with two public APIs that `nosh-server` will use in Plan B:

1. `nosh_auth::keys::nosh_key_from_spki(spki: &[u8]) -> Option<NoshPublicKey>` — a public wrapper exposing the SPKI→NoshPublicKey parse logic already used privately in `verifier.rs`, so `server.rs` can extract the peer identity without duplicating the parsing.
2. `NoshPublicKey::fingerprint() -> String` — returns `SHA256:<base64-no-pad>` over the raw 32-byte Ed25519 key, matching OpenSSH fingerprint format.

Both are in `nosh-auth` (the crate that owns key material). No changes to the TLS handshake, verifier, or server in this plan.

## Context

- D-07: fingerprint helper on `NoshPublicKey`, SHA256 over raw key, `SHA256:` prefix, OpenSSH convention.
- D-04/D-05 (Plan B): `server.rs` calls `extract_spki_from_cert` (already pub) then `nosh_key_from_spki` (to expose here) to get the `NoshPublicKey` from the connection's peer cert.
- Claude's Discretion (CONTEXT.md): "expose a public `nosh_key_from_spki` in `nosh-auth`" is the preferred reuse path over making `verifier.rs`'s private function pub.
- `sha2` and `base64` (v0.22) are already in Cargo.lock as transitive deps; add them as explicit deps of `nosh-auth`.

---

## Task A-1: Add `sha2` and `base64` to nosh-auth Cargo.toml

<read_first>
- crates/nosh-auth/Cargo.toml
- Cargo.lock (grep: sha2, base64)
</read_first>

<action>
Add to `[dependencies]` in `crates/nosh-auth/Cargo.toml`:
  - `sha2 = "0.10"`
  - `base64 = "0.22"`

Both are already in the workspace lockfile as transitive deps; adding them explicitly pins them as direct deps of nosh-auth. No workspace.toml changes needed (these are not workspace deps currently — add them directly in nosh-auth/Cargo.toml, not as workspace references).
</action>

<acceptance_criteria>
- `crates/nosh-auth/Cargo.toml` contains `sha2 = "0.10"` in `[dependencies]`
- `crates/nosh-auth/Cargo.toml` contains `base64 = "0.22"` in `[dependencies]`
- `cargo check -p nosh-auth` exits 0 (no missing dep errors)
</acceptance_criteria>

---

## Task A-2: Add `NoshPublicKey::fingerprint()` to keys.rs

<read_first>
- crates/nosh-auth/src/keys.rs (full file — read before editing)
</read_first>

<action>
In `impl NoshPublicKey` (after the existing `to_openssh_line` method), add:

```rust
/// Returns the OpenSSH-style SHA256 fingerprint of this key: `SHA256:<base64>`.
///
/// The raw private/public key bytes are NEVER included — only the hash (D-07).
/// Fingerprint is SHA256 over the raw 32-byte Ed25519 key material, base64
/// without padding, matching `ssh-keygen -l -E sha256` output.
pub fn fingerprint(&self) -> String {
    use base64::Engine as _;
    use sha2::Digest as _;
    let hash = sha2::Sha256::new()
        .chain_update(self.key32)
        .finalize();
    format!(
        "SHA256:{}",
        base64::engine::general_purpose::STANDARD_NO_PAD.encode(hash)
    )
}
```

Add the following `use` imports at the top of the file (if not already present):
- These are method-local uses inside `fingerprint()` — no top-level imports needed (the `use` statements inside the function body are sufficient).
</action>

<acceptance_criteria>
- `crates/nosh-auth/src/keys.rs` contains `pub fn fingerprint(&self) -> String`
- The function body uses `sha2::Sha256` and `base64::engine::general_purpose::STANDARD_NO_PAD`
- The format string is `"SHA256:{}"` with the base64-no-pad encoded hash
- `cargo check -p nosh-auth` exits 0
- The new unit test (Task A-3) passes: fingerprint of a known key produces a string starting with `SHA256:` and the base64 portion is exactly 43 characters
</acceptance_criteria>

---

## Task A-3: Add unit test for `fingerprint()`

<read_first>
- crates/nosh-auth/src/keys.rs (the `#[cfg(test)] mod tests` block at the bottom)
</read_first>

<action>
In the existing `#[cfg(test)] mod tests` block in `keys.rs`, add after the existing tests:

```rust
#[test]
fn fingerprint_format() {
    // SHA256 of 32 zero bytes, base64-no-pad = 43 chars, prefixed with "SHA256:".
    let key = NoshPublicKey::from_raw([0u8; 32]);
    let fp = key.fingerprint();
    assert!(fp.starts_with("SHA256:"), "fingerprint must start with SHA256: — got {fp:?}");
    let b64_part = &fp["SHA256:".len()..];
    assert_eq!(b64_part.len(), 43, "SHA256 base64-no-pad is 43 chars — got {b64_part:?}");
    // Must not contain '=' padding characters.
    assert!(!b64_part.contains('='), "base64-no-pad must not contain padding: {fp:?}");
    // The raw key bytes must not appear in the fingerprint output.
    assert!(!fp.contains('\0'), "raw key bytes must not appear in fingerprint");
}

#[test]
fn fingerprint_two_different_keys_differ() {
    let k1 = NoshPublicKey::from_raw([1u8; 32]);
    let k2 = NoshPublicKey::from_raw([2u8; 32]);
    assert_ne!(k1.fingerprint(), k2.fingerprint(), "distinct keys must have distinct fingerprints");
}
```
</action>

<acceptance_criteria>
- `cargo test -p nosh-auth fingerprint_format` exits 0
- `cargo test -p nosh-auth fingerprint_two_different_keys_differ` exits 0
</acceptance_criteria>

---

## Task A-4: Add public `nosh_key_from_spki` function to keys.rs

<read_first>
- crates/nosh-auth/src/keys.rs (full file)
- crates/nosh-auth/src/verifier.rs:218-229 (`parse_ed25519_from_spki` — this is the logic to replicate/expose)
</read_first>

<action>
Add a new public function to `keys.rs` (after `extract_spki_from_cert`):

```rust
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
```

This is the same logic as `verifier.rs::parse_ed25519_from_spki` — do NOT call into verifier.rs; keep keys.rs self-contained.
</action>

<acceptance_criteria>
- `crates/nosh-auth/src/keys.rs` contains `pub fn nosh_key_from_spki(spki: &[u8]) -> Option<NoshPublicKey>`
- The function returns `None` for a slice that is not 44 bytes
- The function returns `None` for a 44-byte slice with the wrong SPKI prefix
- The function returns `Some(NoshPublicKey)` for a valid Ed25519 SPKI
- `cargo check -p nosh-auth` exits 0
</acceptance_criteria>

---

## Task A-5: Add unit test for `nosh_key_from_spki` and export from lib.rs

<read_first>
- crates/nosh-auth/src/keys.rs (tests block)
- crates/nosh-auth/src/lib.rs
</read_first>

<action>
1. In `keys.rs` tests block, add:

```rust
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
```

2. In `crates/nosh-auth/src/lib.rs`, add `nosh_key_from_spki` to the `pub use keys::` line:

Current line:
```rust
pub use keys::{load_authorized_keys, load_host_key, NoshPublicKey, ED25519_SPKI_LEN};
```

New line:
```rust
pub use keys::{load_authorized_keys, load_host_key, nosh_key_from_spki, NoshPublicKey, ED25519_SPKI_LEN};
```
</action>

<acceptance_criteria>
- `cargo test -p nosh-auth nosh_key_from_spki_roundtrip` exits 0
- `cargo test -p nosh-auth nosh_key_from_spki_rejects` exits 0 (both reject tests)
- `crates/nosh-auth/src/lib.rs` exports `nosh_key_from_spki` in the `pub use keys::` line
- `nosh_auth::nosh_key_from_spki` is accessible from an external crate (`cargo check -p nosh-server` with a test import compiles)
- `cargo test -p nosh-auth` exits 0 (all nosh-auth tests pass)
</acceptance_criteria>

---

## Verification

```bash
cargo test -p nosh-auth
cargo check -p nosh-server
```

All `nosh-auth` tests pass. `nosh-server` still compiles (no server changes yet — Plan B does those). The new public APIs are available for Plan B.

<must_haves>
## Truths that must hold

- `NoshPublicKey::fingerprint()` exists, returns `SHA256:<43-char-base64-no-pad>`, never logs raw key bytes
- `nosh_key_from_spki` is `pub` in `nosh-auth` — accessible as `nosh_auth::nosh_key_from_spki`
- `nosh_key_from_spki` returns `None` for any non-Ed25519 or malformed SPKI
- `nosh_key_from_spki` roundtrips: `nosh_key_from_spki(&key.spki_der()) == Some(key)`
- `cargo test -p nosh-auth` exits 0
- No changes to `nosh-server`, `nosh-proto`, or `nosh-client` in this plan
</must_haves>
