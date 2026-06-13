---
phase: 25-inner-ssh-key-handshake-tofu-prompt
verified: 2026-06-14T00:00:00Z
status: passed
score: 5/5 must-haves verified
re_verification:
  previous_status: none
---

# Phase 25: Inner SSH-Key Handshake + TOFU Prompt — Verification Report

**Phase Goal:** A WebTransport session performs full mutual SSH-key authentication before any session or reattach frame is processed, with the exchange bound to the outer TLS session and an interactive TOFU prompt on first contact.

**Verified:** 2026-06-14
**Status:** PASSED
**Re-verification:** No — initial verification

## Goal Achievement

### Observable Truths (Success Criteria)

| # | Truth | Status | Evidence |
|---|-------|--------|----------|
| 1 | Control stream exchanges InnerAuth Challenge(18)/Response(19)/Complete(20)/Fail(21) in strict order before any SessionOpen/Reattach; discriminant-order test updated | ✓ VERIFIED | `messages.rs:353-414` defines variants 18-21 in order, append-only after ScrollbackCredit(17). `codec.rs:303-309` adds them to `message_discriminant_order_is_stable` at discriminants 18/19/20/21 (test passes). `wt_transport.rs:491-558` runs gate FIRST, only then reads first session frame. |
| 2 | Server challenge incorporates exported TLS keying material (RFC 9266) so a terminating proxy cannot replay | ✓ VERIFIED | `inner_auth.rs:141-144` exports 32-byte EKM via `transport.export_keying_material(..)`; server WT transport (`wt_transport.rs:144-154`) delegates to `quinn::Connection::export_keying_material` (real RFC 9266 tls-exporter, NOT a CSPRNG stand-in). EKM folded into both transcripts. |
| 3 | InnerAuthFail is fieldless — reveals neither key existence nor signature validity | ✓ VERIFIED | `messages.rs:414` `InnerAuthFail` is a fieldless variant. `codec.rs:328-335` `inner_auth_fail_is_fieldless` asserts it encodes to exactly 1 byte (test passes). All server failure paths route through `fail()` (`inner_auth.rs:96-103`) emitting the identical variant. |
| 4 | First contact with unknown server key → blocking SHA-256-fingerprint confirm prompt (requires "yes"), no PTY output until resolved | ✓ VERIFIED | `client/inner_auth.rs:202-214` calls `prompt_tofu_or_fail` → shared `keys::prompt_host_key_accept` (`keys.rs:250-282`): prints SHA-256 fingerprint, requires literal "yes" (`parse_yes`), stderr-only, blocks on stdin before returning the authenticated stream (so no SessionOpen/PTY output precedes it). |
| 5 | Server passing inner auth with key NOT in authorized_keys is rejected; client declining TOFU / no-TTY disconnects cleanly | ✓ VERIFIED | Server: `inner_auth.rs:216-219` `check_authorized_key` → `fail()` (no-oracle). Client: `inner_auth.rs:206-210` bails if TOFU declined; `keys.rs:254-263` returns `Ok(false)` on no-TTY → client bails, no record. Integration tests `inner_auth_unknown_client_key` + `tofu_no_tty_fails_closed` both pass and assert no session + no known_hosts write. |

**Score: 5/5 truths verified.**

### Adversarial Checks (A–F)

| Check | Finding | Verdict | Evidence |
|-------|---------|---------|----------|
| **A. Enforcement reachability** | Production `main.rs:207-215` WT arm passes `InnerAuthMode::Required` + real `authorized`/`host_signer`. `handle_connection_wt` (`wt_transport.rs:480-519`) accepts control stream then runs `run_inner_auth_server` BEFORE reading the first session frame (`read_message_ns` at `:523`). IN-01 gated `inner_auth` mod under `#[cfg(feature="webtransport")]` (`lib.rs:12-13`) — same gate as `wt_transport`; the webtransport build compiles and reaches the gate (confirmed by `cargo build -p nosh-server --features webtransport`). No path reaches a session without the gate when Required. | ✓ PASS | No CRITICAL. Production default is Required; gate is unconditionally upstream of session dispatch. |
| **B. Vacuous-test check** | All 5 tests use `spawn_wt_server_real_auth` (Required); zero use TestBypass (the only "TestBypass" string in `inner_auth.rs:65` is a comment). **PROBE:** scratch-edited the harness (`common/mod.rs:451`) to pass `TestBypass`, re-ran — `inner_auth_session_open_before_auth` FAILED with "MH-1 VIOLATED: server accepted SessionOpen before inner auth"; `inner_auth_tampered_channel_binding_fails` FAILED (no challenge under bypass); `inner_auth_happy_path` FAILED. Reverted (clean git diff). | ✓ PASS | Tests are genuinely non-vacuous — they fail loudly when the gate is neutered. |
| **C. Length validation** | `client_sig.len() != 64` checked before `try_into` (`inner_auth.rs:172-200`, guarded `.expect`). `verify_ed25519_spki` takes `&[u8;64]` (compile-time). `nosh_key_from_spki` checks `spki.len() != 44` before slicing (`keys.rs:340-348`). `[u8;32]` nonce/EKM fields deserialize via serde/postcard (exact-length or error, no panic). Client `server_sig` validated via fallible `try_into` (`client/inner_auth.rs:261-264`). | ✓ PASS | No reachable panic on attacker-controlled wire fields. |
| **D. Channel binding load-bearing** | Server folds EKM into `client_transcript` (verified, `inner_auth.rs:194-205`) AND `server_transcript` (signed, `:228-246`). Client folds EKM into both transcripts AND adds an explicit `ekm_from_server != ekm` hard bail before signing (`client/inner_auth.rs:177-182`). Mismatch ⇒ rejection, not a log. Probe in Check B confirms the tampered-EKM test fails when binding is absent. | ✓ PASS | EKM is part of the signed transcript on both sides; mismatch causes rejection. |
| **E. No oracle** | Every server failure (wrong frame, sig-verify fail, SPKI parse fail, key-not-authorized) emits the identical fieldless `InnerAuthFail` via `fail()`. Sig verify runs BEFORE the authorized_keys lookup (`inner_auth.rs:192-219`, fixed order — no key-existence timing branch). `inner_auth_unknown_client_key` asserts the wire error is opaque (no "authorized"/"not found"/"unknown"). | ✓ PASS | Uniform fieldless failure; fixed verification order. |
| **F. TOFU after WR-02 dedup** | Both `verifier::prompt_and_record` (`verifier.rs:235-252`) and `client::prompt_tofu_or_fail` (`client/inner_auth.rs:305-307`) delegate to the single shared `keys::prompt_host_key_accept`. Shared helper: blocks on stdin, requires literal "yes", fails closed `Ok(false)` on no-TTY, stderr-only, does NOT record on decline (caller records only on accept). Production default `HostKeyVerifier::new` ⇒ `TofuPolicy::Interactive` (`verifier.rs:77-83`); `Silent` is documented + used test-only. Inner-auth path has no Silent seam at all (always interactive/fail-closed). | ✓ PASS | Single shared helper preserves all security properties; production default is Interactive. |

### Required Artifacts

| Artifact | Expected | Status | Details |
|----------|----------|--------|---------|
| `crates/nosh-server/src/inner_auth.rs` | Server gate state machine | ✓ VERIFIED | 10-step gate, EKM binding, no-oracle fail, fixed verify order, spawn_blocking sign |
| `crates/nosh-server/src/wt_transport.rs` | Live accept loop invoking gate | ✓ VERIFIED | `handle_connection_wt` runs gate before session dispatch; Required default in main.rs |
| `crates/nosh-client/src/inner_auth.rs` | Client auth + TOFU | ✓ VERIFIED | EKM check, TOFU prompt, mutual verify, returns authenticated stream |
| `crates/nosh-auth/src/keys.rs` | Shared TOFU + key helpers | ✓ VERIFIED | `prompt_host_key_accept` shared, length-safe `nosh_key_from_spki`, mode 0o600 + sync_all |
| `crates/nosh-auth/src/verifier.rs` | Outer-TLS TOFU policy | ✓ VERIFIED | Interactive default; Silent test-only; mismatch always fatal |
| `crates/nosh-proto/src/messages.rs` | Wire variants 18-21 | ✓ VERIFIED | Append-only, fieldless Fail |
| `crates/nosh-client/tests/inner_auth.rs` | 5 adversarial tests | ✓ VERIFIED | All use Required harness; proven non-vacuous |
| `crates/nosh-client/tests/common/mod.rs` | Real-auth harness | ✓ VERIFIED | `spawn_wt_server_real_auth` threads real keys + Required |

### Key Link Verification

| From | To | Via | Status |
|------|-----|-----|--------|
| `main.rs` (WT arm) | `run_wt_accept_loop` | `InnerAuthMode::Required` | ✓ WIRED |
| `handle_connection_wt` | `run_inner_auth_server` | direct call before session frame read | ✓ WIRED |
| `client/inner_auth.rs` | `keys::prompt_host_key_accept` | `prompt_tofu_or_fail` delegation | ✓ WIRED |
| `verifier::prompt_and_record` | `keys::prompt_host_key_accept` | shared helper | ✓ WIRED |
| both transports | `quinn export_keying_material` | RFC 9266 EKM | ✓ WIRED |

### Behavioural Spot-Checks / Probe Execution

| Behaviour | Command | Result | Status |
|-----------|---------|--------|--------|
| Discriminant order + fieldless | `cargo test -p nosh-proto --lib -- discriminant fieldless` | 3 passed | ✓ PASS |
| Inner-auth integration (5 tests) | `cargo test -p nosh-client --features webtransport --test inner_auth` | 5 passed | ✓ PASS |
| Server transcript/EKM unit tests | `cargo test -p nosh-server --features webtransport,test-support --lib -- inner_auth` | 7 passed | ✓ PASS |
| nosh-auth key/verifier tests | `cargo test -p nosh-auth --lib` | 25 passed | ✓ PASS |
| Phase-24 regression | `cargo test -p nosh-client --features webtransport --test webtransport` | 3 passed | ✓ PASS |
| WT build (IN-01 gating intact) | `cargo build -p nosh-server --features webtransport` | Finished | ✓ PASS |
| **Vacuity probe (Check B)** | neuter harness to TestBypass, re-run MH-1/WT-3/happy | 3 FAILED as expected, then reverted clean | ✓ PASS |

### Requirements Coverage

| Requirement | Status | Evidence |
|-------------|--------|----------|
| WT-04 (mutual inner auth happy path) | ✓ SATISFIED | `inner_auth_happy_path` proves Ok + server accepts SessionOpen, shell-independent |
| SEC-02 (TOFU fail-closed, no oracle) | ✓ SATISFIED | `tofu_no_tty_fails_closed`, `inner_auth_unknown_client_key` |
| WT-3 (channel binding defeats proxy MITM) | ✓ SATISFIED | `inner_auth_tampered_channel_binding_fails` + probe |
| MH-1 (state-machine gate) | ✓ SATISFIED | `inner_auth_session_open_before_auth` + probe |

### Anti-Patterns Found

None. No TBD/FIXME/XXX/TODO/HACK/PLACEHOLDER markers in any Phase-25 source or test file. No stub returns, no hardcoded empty data feeding rendering, no neutered verification (real `verify_tls13_signature` delegation retained).

### Code-Review-Fix Integrity

All 8 review findings (WR-01..04, IN-01..04) were Warnings/Info — none security-weakening. Verified against actual code: WR-01/03 (mode 0o600 + flush/sync_all on known_hosts — `keys.rs:205-218`), WR-02 (shared `prompt_host_key_accept` — preserves all security properties, confirmed by Check F), WR-04 (constant-prefix comparison), IN-01 (feature-gate, gate still reachable in WT build — confirmed by Check A), IN-02/03/04 (test cleanup, docs, fingerprint-after-TTY-check). No fix weakened any check.

### Gaps Summary

None. The phase goal is achieved: the live WebTransport server runs full mutual SSH-key auth bound to the outer TLS session (RFC 9266 EKM) before any session/reattach frame; first-contact triggers a blocking SHA-256 TOFU prompt requiring "yes"; failures are uniform and fieldless; unauthorized client keys and declined/no-TTY TOFU both fail closed. The adversarial tests are proven non-vacuous by direct falsification.

---

_Verified: 2026-06-14_
_Verifier: Claude (gsd-verifier, opus)_
