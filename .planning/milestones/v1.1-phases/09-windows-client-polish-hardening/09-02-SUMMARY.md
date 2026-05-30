---
phase: 09-windows-client-polish-hardening
plan: "02"
subsystem: nosh-auth, nosh-client
tags: [robustness, authorized-keys, timeout, windows, hygiene]
dependency_graph:
  requires: [09-01]
  provides: [ROBUST-01, WIN-02-hygiene]
  affects: [crates/nosh-auth, crates/nosh-client]
tech_stack:
  added: []
  patterns: [warn-and-skip, tokio-timeout, cfg-unix-gate]
key_files:
  created: []
  modified:
    - crates/nosh-auth/src/keys.rs
    - crates/nosh-auth/src/signer.rs
    - crates/nosh-client/src/client.rs
    - crates/nosh-client/src/main.rs
    - crates/nosh-client/tests/auth.rs
    - crates/nosh-client/tests/session.rs
    - crates/nosh-client/tests/reattach.rs
    - crates/nosh-client/tests/identity_file.rs
    - crates/nosh-client/tests/transport.rs
    - crates/nosh-client/tests/persistence.rs
    - crates/nosh-client/tests/migration.rs
decisions:
  - "load_authorized_keys: match instead of ? — fail-closed per line, not fail-closed for whole file"
  - "Log key_type token only (first whitespace field), not full line or key material (D-07)"
  - "connect_timeout: Duration added as explicit parameter (not a global constant) to allow test override"
  - "All 20 test call sites updated to Duration::from_secs(30) — generous timeout, avoids flakiness"
  - "#[cfg(unix)] on PathBuf import — not moved to impl block, keeps the import declaration readable"
metrics:
  duration_minutes: 25
  completed_date: "2026-05-30T07:21:14Z"
  tasks_completed: 3
  files_changed: 11
---

# Phase 9 Plan 02: authorized_keys Warn+Skip, Connect Timeout, PathBuf Gate

Three robustness and hygiene fixes from live validation: tolerant authorized_keys parser, bounded connect, and a silent Windows warning.

## Tasks Completed

### Task 1 (TDD): authorized_keys warn+skip (ROBUST-01)

Changed `load_authorized_keys` in `crates/nosh-auth/src/keys.rs`:

- Replace `NoshPublicKey::from_openssh_line(line)?` with an `Ok/Err` match.
- On `Err`: `tracing::warn!` with `key_type` (first whitespace field of the line) and the parse error; `continue`.
- On `Ok`: push key as before.
- Fail-closed invariant (T-09-04): skipping a bad line never widens the accepted-key set; the accepted set is a strict subset of what `from_openssh_line` accepts.
- Updated doc comment: "unsupported/unparseable lines are warned and skipped (sshd behaviour)".

**Tests**: 3 new unit tests added:
1. `mixed_authorized_keys_loads_one_ed25519`: file with Ed25519 + RSA + garbage → exactly 1 key with correct fingerprint.
2. `all_bad_authorized_keys_returns_empty_ok`: file with only RSA + garbage → `Ok(vec![])`.
3. `skip_is_fail_closed_no_bad_key_admitted`: accepted key must match the Ed25519 fixture, not any malformed entry.

All 3 pass.

### Task 2: client connect timeout + --connect-timeout flag

- `client::connect` gains a `connect_timeout: std::time::Duration` parameter.
- `endpoint.connect(...)?.await` replaced with `tokio::time::timeout(connect_timeout, connecting).await` with error: `"connection to {ip}:{port} timed out after {N}s (no response from server)"`.
- `main.rs Args`: `--connect-timeout <secs>` with `default_value_t = 10`; converted to `Duration::from_secs` and passed to `connect()`.
- **20 test call sites** updated across 7 test files to `Duration::from_secs(30)` — all still pass.

### Task 3: gate PathBuf import in signer.rs

- `use std::path::PathBuf` in `crates/nosh-auth/src/signer.rs` is gated `#[cfg(unix)]`.
- PathBuf is only used by the `#[cfg(unix)]` `AgentSigner` struct fields and constructor.
- Unix build unchanged; Windows build no longer emits unused-import warning.

## Verification

### Validated on Linux
- `cargo test -p nosh-auth authorized_keys`: 3/3 new tests pass + existing tests unaffected
- `cargo build --workspace`: clean
- `cargo test --workspace`: all 97+ tests pass, no regressions
- `grep -n "tracing::warn" crates/nosh-auth/src/keys.rs`: line 125 confirms warn is present
- `grep -n "PathBuf" crates/nosh-auth/src/signer.rs`: line 19 is #[cfg(unix)]-gated

### Requires Windows Host
- No unused-import warning for PathBuf on a Windows build of `nosh-auth`

## Deviations from Plan

None — plan executed exactly as written. All 20 connect call sites updated (plan advisory confirmed correct count).

## Known Stubs

None.

## Threat Flags

None — no new network endpoints, auth paths, or schema changes introduced.

## Self-Check: PASSED
- `crates/nosh-auth/src/keys.rs`: FOUND, contains tracing::warn and new tests
- `crates/nosh-auth/src/signer.rs`: FOUND, PathBuf gated #[cfg(unix)]
- `crates/nosh-client/src/client.rs`: FOUND, connect takes Duration parameter
- Commits 5af3757, 43ba8ac: FOUND
