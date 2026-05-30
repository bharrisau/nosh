---
plan: 04-A
title: "nosh-auth — Identity Extraction Surface + Fingerprint Helper"
status: complete
completed: 2026-05-30
wave: 1
tasks_total: 5
tasks_complete: 5
key-files:
  created:
    - crates/nosh-auth/src/keys.rs
    - crates/nosh-auth/src/lib.rs
    - crates/nosh-auth/Cargo.toml
deviations: none
---

# Plan A Summary

## What Was Built

Extended `nosh-auth` with two public APIs required by Plan B:

1. **`NoshPublicKey::fingerprint() -> String`** — returns `SHA256:<43-char-base64-no-pad>` over the raw 32-byte Ed25519 key material, matching `ssh-keygen -l -E sha256` format. Raw key bytes never in output (D-07). Implemented using `sha2` + `base64` crates.

2. **`nosh_key_from_spki(spki: &[u8]) -> Option<NoshPublicKey>`** — public wrapper exposing the SPKI→NoshPublicKey parse logic. Returns `None` for any non-Ed25519 or malformed SPKI. Reuses same validation as `verifier.rs::parse_ed25519_from_spki` (kept self-contained in `keys.rs`). Exported from `nosh_auth` crate root.

## Tasks Completed

| Task | Description | Commit |
|------|-------------|--------|
| A-1 | Added `sha2 = "0.10"` and `base64 = "0.22"` to nosh-auth Cargo.toml | 9987572 |
| A-2 | Implemented `NoshPublicKey::fingerprint()` | e875492 |
| A-3 | Added unit tests for `fingerprint()` (format, distinct keys) | d2467c6 |
| A-4 | Implemented `nosh_key_from_spki()` | 5dc6d1c |
| A-5 | Added unit tests + exported `nosh_key_from_spki` from lib.rs | 6fddbda |

## Test Results

- `cargo test -p nosh-auth`: **11 passed, 1 ignored** (agent test requires ssh-agent on PATH)
- `cargo check -p nosh-server`: exits 0 (no server changes — Plan B does those)

## Deviations

None. All tasks completed exactly as specified.

## Self-Check: PASSED

- `NoshPublicKey::fingerprint()` exists, returns `SHA256:<43-char-base64-no-pad>` ✓
- `nosh_key_from_spki` is `pub` in `nosh-auth` ✓
- `nosh_key_from_spki` returns `None` for non-Ed25519/malformed SPKI ✓
- `nosh_key_from_spki` roundtrips: `nosh_key_from_spki(&key.spki_der()) == Some(key)` ✓
- `cargo test -p nosh-auth` exits 0 ✓
- No changes to `nosh-server`, `nosh-proto`, or `nosh-client` ✓
