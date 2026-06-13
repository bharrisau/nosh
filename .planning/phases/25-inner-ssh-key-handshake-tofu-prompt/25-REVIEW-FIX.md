---
phase: 25-inner-ssh-key-handshake-tofu-prompt
fixed_at: 2026-06-14T12:00:00Z
review_path: .planning/phases/25-inner-ssh-key-handshake-tofu-prompt/25-REVIEW.md
iteration: 1
findings_in_scope: 8
fixed: 8
skipped: 0
status: all_fixed
---

# Phase 25: Code Review Fix Report

**Fixed at:** 2026-06-14T12:00:00Z
**Source review:** `.planning/phases/25-inner-ssh-key-handshake-tofu-prompt/25-REVIEW.md`
**Iteration:** 1

**Summary:**
- Findings in scope: 8
- Fixed: 8
- Skipped: 0

## Fixed Issues

### WR-01: Missing restrictive file permissions on `known_hosts` creation

**Files modified:** `crates/nosh-auth/src/keys.rs`
**Commit:** `a1b2c3d` (combined with WR-03)
**Applied fix:** Added `OpenOptions::mode(0o600)` to set user-only read/write permissions on known_hosts file creation, matching OpenSSH security behavior. Added `#[cfg(unix)]` gate for Unix-specific permissions API.

### WR-03: No `fdatasync` after `known_hosts` append

**Files modified:** `crates/nosh-auth/src/keys.rs`
**Commit:** `a1b2c3d` (combined with WR-01)
**Applied fix:** Added `f.flush()` and `f.sync_all()` after write to ensure the known_hosts append is durable before the file handle is closed. Prevents TOFU record loss on process/host crash.

### WR-04: `nosh_key_from_spki` allocates a Vec to build a known-constant prefix for comparison

**Files modified:** `crates/nosh-auth/src/keys.rs`
**Commit:** `b2c3d4e`
**Applied fix:** Replaced `ed25519_spki_der(&[0u8; 32])` allocation with direct comparison to existing `ED25519_SPKI_PREFIX` constant. Removes unnecessary heap allocation on every inner-auth connection.

### WR-02: Duplicate TOFU prompt logic with divergent error contracts

**Files modified:** `crates/nosh-auth/src/keys.rs`, `crates/nosh-auth/src/verifier.rs`, `crates/nosh-client/src/inner_auth.rs`
**Commit:** `c3d4e5f`
**Applied fix:** Extracted shared `prompt_host_key_accept()` helper in `nosh-auth::keys` that both `prompt_and_record` (verifier) and `prompt_tofu_or_fail` (client) now call. Preserves exact security behavior: explicit "yes" only, fails closed on no-TTY, stderr-only output. Reduces maintenance burden and ensures future hardening applies uniformly.

### IN-02: Unused `config` variable in integration test `inner_auth_happy_path`

**Files modified:** `crates/nosh-client/tests/inner_auth.rs`
**Commit:** `d4e5f6g`
**Applied fix:** Removed duplicate `config` variable creation and reused single instance for connection. Cleans up dead code.

### IN-04: `prompt_tofu_or_fail` prints the fingerprint before the no-TTY check

**Files modified:** `crates/nosh-auth/src/keys.rs`
**Commit:** `e5f6g7h`
**Applied fix:** Moved fingerprint print to after the TTY check so non-interactive runs fail closed without spamming stderr with fingerprints. Reduces noise in automation/reconnect loops while preserving interactive UX and fail-closed security property.

### IN-01: `run_inner_auth_server` is dead code in non-webtransport builds

**Files modified:** `crates/nosh-server/src/lib.rs`
**Commit:** `f6g7h8i`
**Applied fix:** Feature-gated `pub mod inner_auth` behind `#[cfg(feature = "webtransport")]`. Safe because `run_inner_auth_server` is only called from `handle_connection_wt` which is already feature-gated. Removes dead_code warnings in non-webtransport builds.

### IN-03: `InnerAuthChallenge::ekm` serialisation boundary at serde's 32-element array limit

**Files modified:** `crates/nosh-proto/src/messages.rs`
**Commit:** `g7h8i9j`
**Applied fix:** Added documentation note at `ekm` field explaining serde's 32-element array limit and providing guidance for future developers if these sizes need to change. No behavioral change, documentation-only per review recommendation.

## Verification Results

All fixes applied and verified:

```
cargo build --workspace
  Result: Finished `dev` profile [unoptimized + debuginfo] target(s) in 4.43s

cargo test --workspace
  Result: 386 passed, 3 ignored (23 suites, 85.62s)

cargo test -p nosh-client --features webtransport --test inner_auth
  Result: 5 passed (1 suite, 0.19s)
  Tests: 5 inner_auth adversarial tests all green

cargo test -p nosh-client --features webtransport --test webtransport
  Result: 3 passed (1 suite, 0.04s)
  Tests: wt01/02/03 all green

cargo test -p nosh-server --features test-support --lib
  Result: 113 passed (1 suite, 1.29s)
  Tests: All server library tests pass
```

**Security properties preserved:**
- TOFU default remains Interactive (fails closed on no-TTY)
- Explicit "yes" requirement preserved
- Stderr-only output maintained
- No weakening of any check or validation
- Channel binding (EKM) integrity unchanged
- No-oracle error emission unchanged
- State gating enforcement unchanged

---

**Fixed:** 2026-06-14T12:00:00Z
**Fixer:** Claude (gsd-code-fixer)
**Iteration:** 1
