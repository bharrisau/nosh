---
phase: 02
title: SSH-Key Mutual Auth
status: passed
verified: 2026-05-29
verifier: inline (no subagent runtime available)
---

# Phase 2 Verification: SSH-Key Mutual Auth

Goal-backward verification against the ROADMAP Phase 2 success criteria and the
AUTH-01..05 requirements. Method: inspect source for the load-bearing invariants
and run the automated test suite (including the `--ignored` live ssh-agent and
60s idle tests).

## Build & quality gates
- `cargo build --workspace --all-targets` — **pass** (clean).
- `cargo test --workspace` — **pass**: nosh-auth 6 unit, auth.rs 6, transport.rs 5,
  nosh-proto 3; 0 failures. Two gated tests (`agent_ed25519_handshake_live`,
  `idle_survival_60s`) also **pass** when run with `--ignored`.
- `cargo clippy --workspace --all-targets` — **pass** (no warnings).

## Success criteria (ROADMAP)

### 1. Unknown client key rejected at the TLS handshake; no session code runs
**PASS.** `AuthorizedKeysVerifier::verify_client_cert` returns
`Err(ApplicationVerificationFailure)` for any SPKI not in `authorized_keys`
(`verifier.rs`). The server moved off `with_no_client_auth()` to
`with_client_cert_verifier` (`server.rs:63`), so verification is mandatory and
runs inside the handshake — the echo handler never executes for a rejected key.
Evidence: `unknown_client_key_rejected` (no usable session) +
`authorized_keys_accepts_known_rejects_unknown` unit test.

### 2. Host-key mismatch aborts the client; TOFU on first contact
**PASS.** `HostKeyVerifier::verify_server_cert`: recorded-entry mismatch →
`Err` (hard fail, no overwrite, D-02); absent host → record + `Ok` (TOFU, D-01).
Evidence: `host_key_mismatch_aborts`, `tofu_first_contact_records`, and the
`host_key_tofu_then_match_then_mismatch` unit test.

### 3. Known Ed25519 key via ssh-agent succeeds end-to-end; forged
CertificateVerify rejected (signature not stubbed)
**PASS.** `verify_tls13_signature` delegates to the ring CryptoProvider in both
verifiers (`verifier.rs:104,184`) — never stubbed (PITFALL 5). Happy path:
`mutual_auth_inprocess_happy_path` and the **live** `agent_ed25519_handshake_live`
(real ssh-agent) both complete the handshake + echo. Forgery:
`forged_certificate_verify_rejected` presents the authorized SPKI but signs with
a different key → no usable session.

### 4. Private key never loaded directly — all CertificateVerify signing via the
ssh-agent socket
**PASS.** The client only ever reads the **public** key
(`client.rs:43`, `read_openssh_file` → `PublicKey`); signing routes through
`AgentSigner` → `ssh_agent_client_rs::Client::sign` (`signer.rs`). No
`PrivateKey`/key-file read exists on the client signing path. Validated live by
`agent_ed25519_handshake_live` and `agent_ed25519_sign_roundtrip`.

### 5. Unauthenticated flood does not exhaust memory; un-authed conns close on
timeout
**PASS.** `run_accept_loop` holds a `tokio::sync::Semaphore` permit per
in-progress connection (refusing over `max_concurrent`) and wraps the handshake
in `tokio::time::timeout(auth_timeout, …)`, dropping half-open connections on
elapse (`server.rs`). Both are flag-overridable (D-13). Evidence:
`preauth_flood_bounded` (small cap + short timeout; a legitimate client still
connects under flood).

## Requirements coverage
| Req | Status | Evidence |
|-----|--------|----------|
| AUTH-01 | ✓ | `unknown_client_key_rejected`; `with_client_cert_verifier` |
| AUTH-02 | ✓ | `host_key_mismatch_aborts`, `tofu_first_contact_records` |
| AUTH-03 | ✓ | `forged_certificate_verify_rejected`; real `verify_tls13_signature` |
| AUTH-04 | ✓ | `agent_ed25519_handshake_live` (LIVE), `agent_ed25519_sign_roundtrip` |
| AUTH-05 | ✓ | `preauth_flood_bounded` |

## Notes / residual items
- The live ssh-agent Ed25519 round-trip was executed and **passes** in this
  environment (ssh-agent + ssh-keygen present). It is `#[ignore]`-gated so CI
  hosts without an agent skip cleanly rather than fail. If a CI environment
  lacks an agent, treat `agent_ed25519_handshake_live` as a human-verification
  item; everywhere else it is automated.
- Ed25519-only is enforced at load (clear error for other key types, D-12);
  RSA/ECDSA + RFC 7250 RPK remain deferred per CONTEXT.
- TOFU keys on the connect host string (sufficient granularity for this spike);
  hashed-host / wildcard known_hosts handling is out of scope.

**Verification status: passed.**
