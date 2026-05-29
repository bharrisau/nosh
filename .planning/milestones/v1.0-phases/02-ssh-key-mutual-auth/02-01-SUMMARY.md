# Plan 02-01 Summary: Auth primitives + ssh-agent signing round-trip

**Status:** Complete

## What was built
- `nosh-auth/src/keys.rs` — Ed25519-only SSH key loading (`load_authorized_keys`,
  `load_host_key`, `lookup_known_host`/`record_known_host`), `ed25519_spki_der`,
  `NoshPublicKey`, and `extract_spki_from_cert` (x509-parser).
- `nosh-auth/src/signer.rs` — `RawEd25519Signer` trait; `AgentSigner` (ssh-agent,
  private key never read) and `InProcessEd25519Signer` (host key / fallback);
  `mint_self_signed_cert` via a custom `rcgen::SigningKey` (cert SPKI = SSH key);
  `AgentSigningKey` (`rustls::sign::SigningKey`) signing the CertificateVerify raw.
- `nosh-auth/src/test_support.rs` — `EphemeralAgent` harness (spawns ssh-agent,
  ssh-keygen Ed25519, ssh-add) behind a `test-support` feature.

## Verification
- `cargo test -p nosh-auth`: 4 unit tests pass (SPKI round-trip, known_hosts
  round-trip, minted-cert SPKI matches key, in-process Ed25519 sign verifies).
- Live `agent_ed25519_sign_roundtrip` (`--ignored`) passes against a real
  ssh-agent — the riskiest path, validated EARLY before wiring.

## Decisions honored
D-04, D-06, D-07, D-09, D-12; AUTH-03/AUTH-04 signing path.
