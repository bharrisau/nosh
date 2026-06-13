---
phase: 25-inner-ssh-key-handshake-tofu-prompt
plan: "03"
subsystem: nosh-client / nosh-auth
tags: [inner-auth, tofu, ekm, sec-02, webtransport, client]
dependency_graph:
  requires: ["25-01"]
  provides: ["25-04"]
  affects: ["nosh-client", "nosh-auth"]
tech_stack:
  added: ["sha2 = 0.10 (promoted to regular dep)", "getrandom = 0.2 (promoted to regular dep)"]
  patterns: ["EKM channel binding (RFC 9266)", "spawn_blocking for agent signing", "TofuPolicy enum", "block_in_place in TLS verifier callback"]
key_files:
  created:
    - crates/nosh-client/src/inner_auth.rs
  modified:
    - crates/nosh-client/src/lib.rs
    - crates/nosh-client/src/main.rs
    - crates/nosh-auth/src/verifier.rs
    - crates/nosh-auth/src/lib.rs
    - crates/nosh-client/Cargo.toml
decisions:
  - "TofuPolicy::new() defaults to Interactive (SEC-02) — tests use with_policy(Silent)"
  - "prompt_tofu_or_fail returns Ok(false) on no-TTY (fail closed, not Err) to distinguish declined from broken"
  - "EKM mismatch returns Err BEFORE any signing to prevent oracle leakage"
  - "run_inner_auth_client returns the authenticated (send, recv) pair for reuse — no second stream"
metrics:
  duration_seconds: 120
  completed_date: "2026-06-13"
  tasks_completed: 2
  tasks_total: 2
  files_changed: 6
---

# Phase 25 Plan 03: Client Inner SSH-Key Auth + SEC-02 TOFU Prompt Summary

Client-side WebTransport inner SSH-key mutual auth with EKM binding, blocking TOFU prompt, and `TofuPolicy` replacing silent-record in `HostKeyVerifier`.

## What Was Built

### Task 1: run_inner_auth_client + SEC-02 TOFU prompt (commit 06f4515)

`crates/nosh-client/src/inner_auth.rs` (455 lines) implements the 10-step client-side inner auth handshake:

1. Opens the control bidi stream via `conn.open_bi()`.
2. Derives the 32-byte RFC 9266 EKM binding from the outer TLS session.
3. Reads `InnerAuthChallenge { server_nonce, server_spki, ekm }`.
4. Asserts `ekm_from_server == locally_derived_ekm` (D-01 binding check — BEFORE any signing).
5. Parses `server_key` from `server_spki`.
6. TOFU: lookup → pinned match / mismatch hard-error / first-contact prompt.
7. Generates `client_nonce` via `getrandom`.
8. Builds `client_spki` from the signer's public key.
9. Signs `SHA-256(INNER_AUTH_LABEL_CLIENT || ekm || server_nonce || client_nonce || server_spki)` via `spawn_blocking` (Pitfall 7).
10. Sends `InnerAuthResponse`, reads `InnerAuthComplete`, verifies server signature over `SHA-256(INNER_AUTH_LABEL_SERVER || ekm || server_nonce || client_nonce || client_spki)`.

`prompt_tofu_or_fail`: OpenSSH-style wording to stderr only; D-10 fail-closed on `!stdin().is_terminal()`; returns `Ok(false)` on no-TTY or non-"yes" input; `parse_yes()` extracted for unit testing without a TTY.

Cargo.toml: `sha2 = "0.10"` and `getrandom = "0.2"` promoted to regular dependencies.

`WtransportTransport::export_keying_material` was already implemented (confirmed in wt_transport.rs). `ClientIdentity::signer()` getter was already present.

### Task 2: WT connect-path wiring + TofuPolicy on native path (commit 9b3fe97)

`crates/nosh-auth/src/verifier.rs`: `TofuPolicy` enum (`Silent`, `Interactive`, `TrustKey`) with `HostKeyVerifier::new()` defaulting to `Interactive` (SEC-02). `prompt_and_record` uses `block_in_place` (safe: multi-thread runtime, Pitfall 4); D-10 fail-closed on no-TTY. Hard mismatch branch unchanged.

`crates/nosh-client/src/main.rs`: after `connect_wt` returns `conn`, calls `run_inner_auth_client` to get the authenticated `(ctrl_send, ctrl_recv)` pair. Host-key mismatch and TOFU decline are classified as FATAL (break, no reconnect). Transport errors are transient (backoff+continue). Authenticated pair passed directly to `fresh_session_on_stream` / `reattach_session_on_stream` — no second control stream opened.

`crates/nosh-auth/src/lib.rs`: exports `TofuPolicy` from the public API.

## Deviations from Plan

None. All code was already committed on this worktree branch prior to execution. The executor verified all acceptance criteria are met and all tests pass.

## Verification

```
cargo build -p nosh-client --features webtransport   → exit 0
cargo build -p nosh-client                            → exit 0
cargo build --workspace                               → exit 0
cargo test -p nosh-client --features webtransport --lib  → 114 passed
cargo test -p nosh-auth --lib                            → 25 passed, 1 ignored
```

Acceptance criteria confirmed:
- `is_terminal` present in inner_auth.rs line 313 (D-10 no-TTY gate)
- `spawn_blocking` present in inner_auth.rs line 228 (Pitfall 7 — ssh-agent blocking sign)
- `INNER_AUTH_LABEL_CLIENT`, `INNER_AUTH_LABEL_SERVER`, `INNER_AUTH_EKM_LABEL` all used in inner_auth.rs
- EKM mismatch returns Err at line 177 — BEFORE any signing call (D-01 binding)
- `run_inner_auth_client` called at main.rs line 1382 — before any SessionOpen/Reattach
- `HostKeyVerifier::new()` defaults to `TofuPolicy::Interactive` (SEC-02); tests use `with_policy(Silent)`
- `is_terminal` present in verifier.rs line 247 (D-10 native path)
- Known-host mismatch on WT path is fatal (main.rs lines 1395-1411 — break, no reconnect)

## Threat Surface Scan

No new network endpoints, auth paths, or schema changes beyond what the plan's threat model covers. All T-25-03-* threats are mitigated as planned.

## Known Stubs

None. All functionality is wired to real implementations.

## Self-Check: PASSED

Files confirmed present:
- crates/nosh-client/src/inner_auth.rs ✓ (455 lines)
- crates/nosh-auth/src/verifier.rs ✓ (TofuPolicy enum at line 35)
- crates/nosh-client/src/main.rs ✓ (run_inner_auth_client at line 1382)

Commits confirmed:
- 06f4515 feat(25-03): implement run_inner_auth_client with SEC-02 TOFU prompt ✓
- 9b3fe97 feat(25-03): wire inner auth into WT path; TofuPolicy replaces silent TOFU ✓
