---
phase: 25-inner-ssh-key-handshake-tofu-prompt
reviewed: 2026-06-14T00:00:00Z
depth: deep
files_reviewed: 22
files_reviewed_list:
  - crates/nosh-proto/src/messages.rs
  - crates/nosh-proto/src/codec.rs
  - crates/nosh-proto/src/transport_trait.rs
  - crates/nosh-proto/src/lib.rs
  - crates/nosh-auth/src/verifier.rs
  - crates/nosh-auth/src/keys.rs
  - crates/nosh-auth/src/lib.rs
  - crates/nosh-server/src/inner_auth.rs
  - crates/nosh-server/src/wt_transport.rs
  - crates/nosh-server/src/main.rs
  - crates/nosh-server/src/server.rs
  - crates/nosh-server/src/lib.rs
  - crates/nosh-client/src/inner_auth.rs
  - crates/nosh-client/src/client.rs
  - crates/nosh-client/src/wt_transport.rs
  - crates/nosh-client/src/main.rs
  - crates/nosh-client/src/lib.rs
  - crates/nosh-client/tests/common/mod.rs
  - crates/nosh-client/tests/inner_auth.rs
findings:
  critical: 0
  warning: 4
  info: 4
  total: 8
status: issues_found
---

# Phase 25: Code Review Report

**Reviewed:** 2026-06-14
**Depth:** deep
**Files Reviewed:** 22
**Status:** issues_found

## Summary

Phase 25 implements the inner SSH-key mutual authentication handshake and interactive TOFU prompt over WebTransport -- the load-bearing trust boundary when nosh runs behind a terminating HTTP/3 proxy. Every scrutinised security invariant (enforcement reachability, channel binding, no-oracle uniform error emission, signature length validation, nonce freshness, state-gating, and TOFU fail-closed) is satisfied by the implementation.

No blocking security vulnerabilities were found. The four warnings and four informational items below cover hardening gaps, code duplication, and minor quality issues.

---

## Structural Findings (fallow)

No structural pre-pass findings were provided for this phase.

---

## Narrative Findings (AI reviewer)

### Enforcement Reachability (Item 1 from scope)

**Finding: `run_inner_auth_server` IS invoked when `InnerAuthMode::Required` -- the enforcement path is live.**

The call chain is:
1. `main.rs:214` passes `InnerAuthMode::Required` to `run_wt_accept_loop`.
2. `wt_transport.rs:414` spawns each connection handler with the `auth_mode` enum value.
3. `handle_connection_wt` (line 492) matches `InnerAuthMode::Required` and calls `crate::inner_auth::run_inner_auth_server(...)`.
4. No `cfg` gate wraps the call. The Phase-24 `cfg!(any(test, feature = "test-support"))` skip has been removed.

In non-`webtransport`-feature builds the `inner_auth` module compiles but `run_inner_auth_server` is unreachable (dead code) because `handle_connection_wt` and the rest of `wt_transport.rs` are gated behind `#[cfg(feature = "webtransport")]`. This is feature-gating, not a code-path gap -- production builds that enable the feature reach the gate. See IN-01 below.

**Verdict: No gap. Enforcement is correct.**

### Signature, Length, and Nonce Validation (Items 2 and 6 from scope)

**`client_sig` (Vec<u8> on wire): Server validates `client_sig.len() != 64` at line 172 of `inner_auth.rs` before the `try_into()` conversion. Correct.**

**`server_sig` (Vec<u8> on wire): Client validates `server_sig_vec.as_slice().try_into()` and maps the error at line 260-264 of `client inner_auth.rs`. Correct.**

**Nonces**: Both `server_nonce` and `client_nonce` are `[u8; 32]` (fixed-size arrays) in the `Message` enum, so postcard deserialisation enforces exact 32-byte length structurally. No separate validation needed. Freshness via `getrandom` CSPRNG (single-use per call). Correct.

**`ekm`**: Also `[u8; 32]` -- structurally validated by postcard. Correct.

**`server_spki` / `client_spki`**: Both are `Vec<u8>` with no explicit length check in the inner-auth handlers. Length validation is delegated to `nosh_key_from_spki` (keys.rs:273), which rejects any SPKI not exactly `ED25519_SPKI_LEN` (44 bytes). This is correct because a malformed SPKI returns `None` and the caller treats it as an auth failure. No panic path exists.

**Verdict: All wire-decoded fields are validated. No panics from malformed attacker input.**

### Channel Binding (Item 3 from scope)

The EKM binding is load-bearing on both sides:

1. **Server**: `inner_auth.rs:141-144` derives EKM via `transport.export_keying_material(...)`, includes it in both `client_transcript` and `server_transcript` hashes, and serialises it in `InnerAuthChallenge::ekm`.
2. **Client**: `inner_auth.rs:154-156` derives EKM independently, then at line 177 checks `ekm_from_server != ekm` (D-01 binding check), failing BEFORE any signing occurs. The EKM is included in the signed `transcript_client_signs` and `transcript_server_signs` hashes.
3. **Transcript labels**: `INNER_AUTH_LABEL_CLIENT` (`b"nosh-inner-auth-v1\0"`) and `INNER_AUTH_LABEL_SERVER` (`b"nosh-inner-auth-v1-server\0"`) are distinct, preventing cross-role transcript substitution (Pitfall 3 / D-01).

The adversarial test `inner_auth_tampered_channel_binding_fails` (inner_auth.rs test 2) proves the binding is executable: flipping one byte of the EKM causes the server to reject.

**Verdict: Channel binding is load-bearing and correct.**

### No-Oracle / Uniform Error Emission (Item 4 from scope)

`Message::InnerAuthFail` is declared fieldless (messages.rs:398-409). The `codec::inner_auth_fail_is_fieldless` test (codec.rs:327-337) guards the invariant: it asserts the encoded length is exactly 1 byte. Adding a field would fail this test before merge.

The server failure helper (`fail()` at inner_auth.rs:96-103) sends `InnerAuthFail` then returns a local `anyhow::Error` with a reason string. The reason goes to the caller (`handle_connection_wt`), which logs it at `tracing::warn!(%peer, "inner auth failed: {e:#}")`. The wire sees ONLY the fieldless variant. The reasons logged locally are uniform categories ("client_sig wrong length", "unexpected first frame", "read error", "client sig verify failed", "client_spki parse failed", "client key not in authorized_keys") -- NO nonce, signature, or key bytes are logged.

The `verify_ed25519_spki` function in keys.rs returns `false` uniformly for malformed SPKI, wrong key, and tampered message -- no distinguishing error variant.

**Verdict: No oracle. Wire and timing channels are uniform.**

### State Gating (MH-1) (Item 5 from scope)

In `handle_connection_wt` (wt_transport.rs:464-559), the dispatch on `SessionOpen` / `Reattach` (line 523) happens AFTER the `InnerAuthMode::Required` match arm returns `Ok(identity)` (line 502). There is no control-flow path that reaches the session dispatch before the auth gate. The `InnerAuthMode::TestBypass` arm (line 512-518) is explicitly an opt-in per-call argument, never passed by `main.rs`.

The adversarial test `inner_auth_session_open_before_auth` (inner_auth.rs test 3) proves MH-1: sending `SessionOpen` before completing inner auth results in the server sending `InnerAuthFail` or closing the connection -- no `SessionOpened` is ever received.

In the live-session pump (server.rs:1291-1304), inner-auth frames (`InnerAuthChallenge`, `InnerAuthResponse`, `InnerAuthComplete`, `InnerAuthFail`) received during an active session are treated as protocol errors and trigger `SessionEnd::ClientClosed`. This is correct and fail-closed.

**Verdict: State gating is correct. No pre-auth session path exists.**

### TOFU Prompt (Item 7 from scope)

1. **Production default**: `HostKeyVerifier::new()` (verifier.rs:77-83) defaults to `TofuPolicy::Interactive`. `build_client_config` (client.rs:105-110) defaults to `TofuPolicy::Interactive`. `main.rs` production paths pass no explicit policy, so `Interactive` is the default. **Confirmed.**
2. **Silent test seam**: `HostKeyVerifier::with_policy(..., TofuPolicy::Silent)` is used ONLY in test code via `common/mod.rs` and explicit test calls. **No production path uses Silent.**
3. **Fail-closed on no-TTY**: Both `prompt_and_record` (verifier.rs:247-253) and `prompt_tofu_or_fail` (client inner_auth.rs:312-321) check `std::io::stdin().is_terminal()`. The former returns `Err`; the latter returns `Ok(false)` which the caller converts to an error. Both print the fingerprint and a message referencing `--trust-key` before failing. **Correct.**
4. **Explicit "yes" only**: `parse_yes("yes")` requires exact match after trimming. Empty, "YES", "Yes", "y", "ok", etc. all return false. **Correct.**
5. **No record on decline**: Both prompt functions only call `record_known_host` after acceptance is confirmed. The adversarial test `tofu_no_tty_fails_closed` verifies the `known_hosts` file remains empty after failure. **Correct.**
6. **Only stderr, never stdout/PTY**: Both prompt functions write exclusively to `stderr`. **Correct (D-08).**

**Verdict: TOFU security properties are satisfied.**

### Environment Sanitisation (Item 8 from scope)

No Phase 25 file touches environment sanitisation code. The `SSH_AUTH_SOCK` variable is not forwarded (checked via grep -- zero hits in inner_auth or wt_transport modules). The existing `collect_client_env` (client.rs:514-537) and server-side env filtering in `session.rs` are unchanged by this phase.

**Verdict: Env sanitisation is not weakened.**

---

## Warnings

### WR-01: Missing restrictive file permissions on `known_hosts` creation

**File:** `crates/nosh-auth/src/keys.rs:197-211`
**Issue:** `record_known_host` opens (creates) the `known_hosts` file with `OpenOptions::new().create(true).append(true).open(path)`, which inherits the process's umask -- producing a file with default permissions (typically `0o644`, world-readable on many systems). The `known_hosts` file contains host keys (SPKI pinned public keys) which, while not secret, represent a privacy concern and could aid an attacker in host tracking across networks. OpenSSH's `ssh` client creates `~/.ssh/known_hosts` with `0o600` (user-only read/write). nosh should mirror this.

**Fix:**
```rust
use std::os::unix::fs::OpenOptionsExt;
let mut f = fs::OpenOptions::new()
    .create(true)
    .append(true)
    .mode(0o600)           // user-only read/write
    .open(path)
    .with_context(|| format!("open known_hosts {} for append", path.display()))?;
```

The `~/.ssh` directory typically has `0o700` permissions, so the directory layer provides some protection, but defence-in-depth warrants fixing the file mode directly. This is not yet a blocker because the directory permissions of `~/.ssh` gate access, but it is a hardening gap.

**Severity: WARNING**

---

### WR-02: Duplicate TOFU prompt logic with divergent error contracts

**File:** `crates/nosh-client/src/inner_auth.rs:302-334` and `crates/nosh-auth/src/verifier.rs:232-274`
**Issue:** `prompt_tofu_or_fail` (client inner_auth) and `prompt_and_record` (verifier.rs) implement nearly identical OpenSSH-style TOFU prompts. Both:
- Print the same fingerprint line
- Check `std::io::stdin().is_terminal()`
- Accept only exact `"yes"` input

However, their error contracts diverge:
- `prompt_tofu_or_fail` returns `anyhow::Result<bool>` (returns `Ok(false)` on no-TTY/decline; caller converts to error)
- `prompt_and_record` returns `anyhow::Result<()>` (returns `Err` on no-TTY/decline directly)

This duplication means any future hardening (e.g., setting stdin O_NONBLOCK before reading to prevent a stuck prompt, or adding `--trust-key` flag support) requires two coordinated changes and risks one side being updated without the other. The prompt-and-record-then-verify-test invariants are exercised by different test suites (`inner_auth.rs` tests and `verifier::tests`), so divergence may not be caught by existing tests.

**Fix:** Extract the prompt UI into a shared helper in `nosh-auth::keys` with a single signature (e.g., `pub fn prompt_host_key_accept(host: &str, fingerprint: &str) -> anyhow::Result<bool>`), and call it from both `prompt_and_record` and `prompt_tofu_or_fail`. The recording step (`record_known_host`) remains at each call site since the two paths have different known_hosts path resolution.

**Severity: WARNING** (maintenance risk, not a current bug)

---

### WR-03: No `fdatasync` after `known_hosts` append

**File:** `crates/nosh-auth/src/keys.rs:197-211`
**Issue:** `record_known_host` calls `f.write_all(line.as_bytes())` but never calls `f.flush()` or `f.sync_data()` (fdatasync). The file is implicitly flushed when `f` is dropped at end-of-scope, but if the process (or host) crashes before the `File`'s `Drop` impl runs (i.e. between the `write_all` syscall returning and the `drop` calling `close`), the `known_hosts` append may be lost from the OS buffer. On the next connection, the user will be re-prompted for the same host (TOFU fatigue). Since the key is the same, the security property holds, but the UX degrades and frequent re-prompts train users to type "yes" reflexively.

Note: This matches OpenSSH's behaviour -- `ssh` also does not `fsync` `known_hosts`. However, given TOFU fatigue is a real-world security concern, nosh should consider hardening this at the application layer.

**Fix:**
```rust
f.write_all(line.as_bytes())
    .with_context(|| format!("append to known_hosts {}", path.display()))?;
f.flush().context("flush known_hosts")?;
// Optionally: f.sync_data().context("sync known_hosts")?;
```

At minimum, adding `f.flush()` ensures the Rust buffer is drained to the OS before `drop`. Adding `f.sync_data()` would additionally request an OS-level barrier, at the cost of a latency hit on every first-connection TOFU accept.

**Severity: WARNING** (robustness gap, not a correctness issue)

---

### WR-04: `nosh_key_from_spki` allocates a Vec to build a known-constant prefix for comparison

**File:** `crates/nosh-auth/src/keys.rs:277-278`
**Issue:** The SPKI validation in `nosh_key_from_spki` calls `ed25519_spki_der(&[0u8; 32])` to generate the expected 12-byte DER prefix for comparison. This allocates a `Vec<u8>` on the heap for every call, including the hot path (every `InnerAuthResponse` verification). The constant `ED25519_SPKI_PREFIX: [u8; 12]` already exists at line 19-21 and holds the identical bytes. The allocation is unnecessary and occurs on every inner-auth connection.

**Fix:**
```rust
// Replace:
let expected_prefix = ed25519_spki_der(&[0u8; 32]);
if spki[..12] != expected_prefix[..12] {
    return None;
}

// With:
if spki[..12] != ED25519_SPKI_PREFIX {
    return None;
}
```

This removes the allocation entirely and makes the intent clearer. The `ed25519_spki_der` doc-comment at line 27 is the canonical specification of the prefix; the constant at line 19 is the canonical storage.

**Severity: WARNING** (code quality; allocation on the auth hot path)

---

## Info

### IN-01: `run_inner_auth_server` is dead code in non-webtransport builds

**File:** `crates/nosh-server/src/inner_auth.rs:131`  
**Issue:** The entire `inner_auth` module is NOT gated behind `#[cfg(feature = "webtransport")]`, but its sole caller (`handle_connection_wt` in `wt_transport.rs`) IS so gated. In non-webtransport builds the module compiles and `run_inner_auth_server` is unreachable (dead code). This is acknowledged in the doc-comment and is not a bug -- it is a feature-gating choice that keeps the module compilable in all configurations. However, it means `cargo clippy` with `--no-default-features` will produce dead-code warnings.

**Fix:** Either gate `pub mod inner_auth` behind `#[cfg(feature = "webtransport")]` in `lib.rs`, or add `#[allow(dead_code)]` to `run_inner_auth_server` with a comment explaining the feature-gating rationale. Prefer the former (feature-gate at the module level) for clarity.

**Severity: INFO**

---

### IN-02: Unused `config` variable in integration test `inner_auth_happy_path`

**File:** `crates/nosh-client/tests/inner_auth.rs:93-96`  
**Issue:** The variable `config` is created at line 93 but never used (line 95 creates `config_for_first` which is the one passed to `connect_wt`). This is a dead variable.

**Fix:**
```rust
// Replace:
let config = client_config_with_pinning(&server.cert_hash);
let url = format!("https://127.0.0.1:{}/nosh", server.addr.port());
let config_for_first = client_config_with_pinning(&server.cert_hash);

// With:
let url = format!("https://127.0.0.1:{}/nosh", server.addr.port());
let config = client_config_with_pinning(&server.cert_hash);
// And reuse `config` below.
```

**Severity: INFO**

---

### IN-03: `InnerAuthChallenge::ekm` serialisation boundary at serde's 32-element array limit

**File:** `crates/nosh-proto/src/messages.rs:361`  
**Issue:** The `ekm` field in `InnerAuthChallenge` is `[u8; 32]` -- exactly at serde's derive limit of 32 elements for fixed-size arrays. If the EKM length were increased to 33 bytes (e.g. switching to a different hash function), the derive macro would fail to compile (serde only derives `Serialize`/`Deserialize` for arrays of length <= 32). The `server_nonce` and `client_nonce` fields share the same 32-byte boundary. This is not a bug today but is a fragile spot worth documenting more prominently or guarding with a compile-time assertion.

**Fix:** Add a `const_assert!(std::mem::size_of::<[u8; 32]>() == 32)` or a comment near the `InnerAuthChallenge` definition that calls attention to the serde-array-size limit.

**Severity: INFO**

---

### IN-04: `prompt_tofu_or_fail` prints the fingerprint before the no-TTY check but `prompt_and_record` handles it identically

**File:** `crates/nosh-client/src/inner_auth.rs:309-310` and `crates/nosh-auth/src/verifier.rs:242-244`  
**Issue:** Both functions print the fingerprint to stderr BEFORE checking `stdin.is_terminal()`. This is intentional (the fingerprint is useful diagnostic output even in non-interactive contexts), and both functions do it the same way, so there is no divergence. However, the fingerprint is printed even when the function will immediately fail due to no-TTY -- in a tight reconnect loop this could spam stderr. This is harmless but wasteful in automation.

**Fix:** Consider printing a more concise "unknown host key (use --trust-key for automation)" message on no-TTY, or suppress repeated warnings in backoff loops. Not critical for this phase.

**Severity: INFO**

---

_Reviewed: 2026-06-14T00:00:00Z_
_Reviewer: Claude (gsd-code-reviewer)_
_Depth: deep_
