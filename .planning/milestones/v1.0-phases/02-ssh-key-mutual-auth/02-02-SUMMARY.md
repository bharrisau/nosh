# Plan 02-02 Summary: SSH-key SPKI-pinning verifiers

**Status:** Complete

## What was built
- `nosh-auth/src/verifier.rs` rewritten:
  - `HostKeyVerifier` (`ServerCertVerifier`) — pins the server host key against
    `known_hosts`; TOFU on first contact (records + proceeds), hard-fail on
    mismatch. `supported_verify_schemes` = `[ED25519]`.
  - `AuthorizedKeysVerifier` (`ClientCertVerifier`) — `client_auth_mandatory`,
    pins client cert SPKI against `authorized_keys`; unknown → `Err`.
  - Both keep REAL `verify_tls12/tls13_signature` delegation to the
    CryptoProvider — never stubbed (PITFALL 5).

## Verification
- `cargo test -p nosh-auth`: 2 new unit tests pass —
  `authorized_keys_accepts_known_rejects_unknown` and
  `host_key_tofu_then_match_then_mismatch`.

## Decisions honored
D-01, D-02, D-03, D-05, D-10, D-12; AUTH-01/AUTH-02/AUTH-03.
