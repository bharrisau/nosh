---
phase: 08-windows-client
plan: "01"
subsystem: nosh-auth, nosh-client
tags: [windows, auth, filesigner, platform-gating, security]
dependency_graph:
  requires: []
  provides: [FileSigner, ClientIdentity::from_identity_file, cfg-unix-agent-gate]
  affects: [nosh-auth, nosh-client]
tech_stack:
  added: [zeroize (direct dep, was transitive via ed25519-dalek)]
  patterns: [cfg(unix) dependency gate, ZeroizeOnDrop, redacted Debug]
key_files:
  created:
    - crates/nosh-client/tests/identity_file.rs
  modified:
    - crates/nosh-auth/Cargo.toml
    - crates/nosh-auth/src/signer.rs
    - crates/nosh-auth/src/lib.rs
    - crates/nosh-client/Cargo.toml
    - crates/nosh-client/src/client.rs
decisions:
  - "FileSigner holds only ed25519_dalek::SigningKey (ZeroizeOnDrop); seed is explicitly zeroized in from_path constructor (D-05)"
  - "AgentSigner gated #[cfg(unix)] in signer.rs + [target.'cfg(unix)'.dependencies] in both Cargo.tomls; this is the only cfg in nosh-auth — a build-availability gate, not a behavioral fork"
  - "warn_if_loose_permissions combined into same task as FileSigner (tasks 1+4 merged): #[cfg(unix)] mode() check, #[cfg(not(unix))] ACL-gap doc warning"
  - "ENCRYPTED_KEY_FIXTURE embedded as a string constant (fixture approach) since ssh-key encryption feature not added per D-06"
metrics:
  duration_minutes: 40
  completed: "2026-05-30T05:09:35Z"
  tasks_completed: 5
  files_changed: 7
---

# Phase 8 Plan 01: FileSigner + ssh-agent gating + from_identity_file Summary

FileSigner (on-disk Ed25519 signer, zeroized, encrypted-key detection), cfg(unix)-gated AgentSigner, and platform-agnostic ClientIdentity::from_identity_file — the auth foundation for the Windows client.

## What Was Built

### FileSigner (nosh-auth/src/signer.rs)
A new `RawEd25519Signer` impl that:
- Loads an OpenSSH Ed25519 private key from disk via `ssh_key::PrivateKey::read_openssh_file`
- Detects passphrase-encrypted keys via `is_encrypted()` and returns a clear, actionable error (D-06) — no `decrypt()` call, no encryption feature added
- Extracts the 32-byte seed, builds `ed25519_dalek::SigningKey::from_bytes(&seed)`, then immediately `zeroize::Zeroize::zeroize(&mut seed)` (D-05)
- The `ssh_key::PrivateKey` is dropped at end of `from_path` (narrow scope / Pitfall 12)
- Implements manual `Debug` that prints only the SHA256 fingerprint — never the key bytes
- Documented as the Windows-scoped exception to "never handle the private key" (WIN-02)

### warn_if_loose_permissions (non-fatal, D-10)
- `#[cfg(unix)]`: checks `mode() & 0o077 != 0` → `tracing::warn!` (group/other-accessible)
- `#[cfg(not(unix))]`: documents ACL gap — warns that Windows ACLs cannot be read via `std::fs::Permissions` (Pitfall 13)
- Called before encrypted-key check so warning fires even for keys that then fail
- Always non-fatal; metadata errors silently ignored

### ssh-agent dependency gate (WIN-01 partial)
- `ssh-agent-client-rs` moved to `[target.'cfg(unix)'.dependencies]` in both `nosh-auth/Cargo.toml` and `nosh-client/Cargo.toml`
- `AgentSigner` struct + all its impls annotated `#[cfg(unix)]` in signer.rs
- `agent_ed25519_sign_roundtrip` test annotated `#[cfg(unix)]`
- `nosh-auth/src/lib.rs` re-exports `AgentSigner` under `#[cfg(unix)]`; all other types remain un-gated
- nosh-auth platform note added to lib.rs documenting this is the ONLY cfg — a build-availability gate

### ClientIdentity::from_identity_file (nosh-client/src/client.rs)
- Platform-agnostic constructor (no `#[cfg]`) wrapping `FileSigner::from_path`
- `from_agent` and `ssh_agent_connect` annotated `#[cfg(unix)]`
- `nosh_auth::AgentSigner` import also gated `#[cfg(unix)]`

### Tests
Unit tests in signer.rs:
- `filesigner_sign_verifies`: write key → load → sign → verify with dalek VerifyingKey
- `filesigner_rejects_encrypted`: encrypted fixture → assert error contains "passphrase-encrypted" and not key bytes
- `filesigner_debug_redacts`: assert Debug contains "fingerprint"/"SHA256:" and is short
- `loose_permissions_warns_but_loads` (`#[cfg(unix)]`): chmod 0644 → assert load succeeds (non-fatal)

Integration test `tests/identity_file.rs`:
- `identity_file_mutual_auth_happy_path`: write key to tempfile → `from_identity_file` → mutual auth → real PTY session marker round-trip
- `identity_file_missing_is_error`: assert missing path errors with path in message

## Test Results (Validated on Linux)

```
cargo test -p nosh-auth --lib          → 15 passed, 1 ignored (agent test)
cargo test -p nosh-client --test identity_file → 2 passed
cargo test --workspace                 → all tests pass
```

## Deviations from Plan

### Merged tasks 1 and 4
Tasks 1 (FileSigner) and 4 (permission warning) were implemented together since `warn_if_loose_permissions` belongs inside `from_path` and both were small. The `loose_permissions_warns_but_loads` test was added in the same commit per the plan.

## Known Stubs

None — FileSigner is fully functional and all tests pass.

## Threat Flags

None — this plan adds file-read code (the on-disk key load), but:
- It is protected by `is_encrypted()` detection (D-06)
- Key bytes are never logged (D-05 / manual Debug)
- Permission check fires before loading (D-10)
- No new network endpoints or auth paths added (from_identity_file is an alternative to from_agent, not additive)

## Self-Check: PASSED

- [x] `crates/nosh-auth/src/signer.rs` contains `struct FileSigner` and `fn from_path`
- [x] `crates/nosh-client/src/client.rs` contains `from_identity_file`
- [x] `crates/nosh-client/tests/identity_file.rs` exists with both tests
- [x] Commit 305e9a4 verified: `git log --oneline | head -1` → `305e9a4 feat(08-01): FileSigner...`
- [x] `cargo test --workspace` passes (all 15 auth unit tests + 2 identity_file integration tests)
