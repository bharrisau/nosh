---
phase: 25-inner-ssh-key-handshake-tofu-prompt
plan: "02"
subsystem: auth
tags: [inner-auth, webtransport, quic, ekm, channel-binding, ssh-key, ed25519, spawn-blocking]

# Dependency graph
requires:
  - phase: 25-inner-ssh-key-handshake-tofu-prompt
    plan: "01"
    provides: "InnerAuth{Challenge,Response,Complete,Fail} message variants, INNER_AUTH_EKM_LABEL/CONTEXT/LABEL_CLIENT/LABEL_SERVER constants, NoshTransport::export_keying_material default-Err trait method, check_authorized_key + verify_ed25519_spki in nosh-auth"
provides:
  - "run_inner_auth_server: Unauthenticated→ChallengeExchanged→Authenticated state machine with EKM-bound transcripts"
  - "WtransportTransport::export_keying_material: delegates to quic_connection().export_keying_material() (D-01 channel binding)"
  - "InnerAuthMode { Required, TestBypass }: replaces Phase-24 compile-time cfg! skip with explicit per-call opt-in"
  - "run_wt_accept_loop + handle_connection_wt: new authorized/host_signer/auth_mode params; Required is the default"
  - "main.rs WT arm: loads host_key + authorized_keys, builds InProcessEd25519Signer, passes InnerAuthMode::Required"
  - "common/mod.rs spawn_wt_server: explicit TestBypass for Phase-24 datagram-pump tests"
affects: [25-03-client-inner-auth, 25-04-adversarial-tests, plan04-adversarial]

# Tech tracking
tech-stack:
  added:
    - "sha2 = \"0.10\" (transcript SHA-256 hashing in nosh-server)"
    - "getrandom = \"0.2\" (CSPRNG nonce in nosh-server)"
    - "ed25519-dalek = \"2.2\" (direct dep in nosh-server for test key generation)"
  patterns:
    - "D-01 channel binding via EKM: export_keying_material folded into both signed transcripts so proxy-relay fails"
    - "D-04 no-oracle: fieldless InnerAuthFail on every failure; sig verified before authorized_keys lookup (Pitfall 5)"
    - "D-05 state machine: accept_bi() FIRST; auth gate before any session frame dispatch (MH-1)"
    - "D-06/WT-4: CSPRNG nonce via getrandom::getrandom, generated fresh per call"
    - "Pitfall 7 / T-25-02-DOS: host key signing in tokio::task::spawn_blocking"
    - "InnerAuthMode enum: security-test integrity — real auth runs by default even in test builds"

key-files:
  created:
    - "crates/nosh-server/src/inner_auth.rs"
  modified:
    - "crates/nosh-server/src/lib.rs"
    - "crates/nosh-server/src/wt_transport.rs"
    - "crates/nosh-server/src/main.rs"
    - "crates/nosh-server/Cargo.toml"
    - "crates/nosh-client/tests/common/mod.rs"

key-decisions:
  - "accept_bi() is called BEFORE the inner-auth gate so both auth and session frames share the SAME control stream — the client open_bi() matches this accept; no second stream needed"
  - "Signature is verified before authorized_keys lookup (Pitfall 5 fixed-order) to prevent timing oracle distinguishing key-not-found from bad-sig"
  - "InnerAuthMode replaces the Phase-24 cfg!(any(test, feature=test-support)) skip; Required is the default even in test builds; TestBypass is explicit opt-in only for datagram-pump tests"
  - "wt01_live_shell conversion to real inner auth is deferred to Plan 04 (Plan 04 Task 1 introduces run_inner_auth_client on the client side; wt01 currently passes TestBypass)"
  - "host_signer and authorized are threaded through run_wt_accept_loop as Arc<> clones so each spawned task holds its own ref"

patterns-established:
  - "Pattern: inner-auth state machine always runs before session frame dispatch (MH-1 invariant)"
  - "Pattern: on auth failure, send InnerAuthFail, close with opaque code, return Ok(()) — never propagate Err from handle_connection_wt for auth failures"

requirements-completed: [WT-04]

# Metrics
duration: 45min
completed: 2026-06-13
---

# Phase 25 Plan 02: SERVER inner SSH-key auth + InnerAuthMode gate Summary

**Server-side inner SSH-key mutual auth with EKM-bound transcripts, fieldless-fail no-oracle, and an explicit InnerAuthMode enum replacing the Phase-24 compile-time bypass**

## Performance

- **Duration:** ~45 min
- **Started:** 2026-06-13T08:10:00Z
- **Completed:** 2026-06-13T08:58:56Z
- **Tasks:** 2
- **Files modified:** 6 (1 created, 5 modified)

## Accomplishments

- Implemented `run_inner_auth_server` in `crates/nosh-server/src/inner_auth.rs`: full Unauthenticated → ChallengeExchanged → Authenticated state machine (D-05) with EKM channel binding (D-01/WT-3), CSPRNG nonce (D-06/WT-4), fixed verification order (Pitfall 5), spawn_blocking signing (Pitfall 7/T-25-02-DOS), and fieldless InnerAuthFail on every failure path (D-04 no-oracle)
- Implemented `WtransportTransport::export_keying_material` delegating to `conn.quic_connection().export_keying_material()` (D-01)
- Defined `InnerAuthMode { Required, TestBypass }` and removed the Phase-24 `cfg!(any(test, feature="test-support"))` compile-time skip — `Required` is the default even in test builds; `TestBypass` is an explicit per-call argument
- Wired `main.rs` WT arm: loads host key + authorized_keys, builds `InProcessEd25519Signer`, passes `InnerAuthMode::Required` to `run_wt_accept_loop`
- Updated `common/mod.rs spawn_wt_server` to pass explicit `InnerAuthMode::TestBypass` so Phase-24 shell-pump tests (wt01/wt02/wt03) continue to compile and pass
- 7 unit tests in `inner_auth.rs` proving: transcript determinism, EKM sensitivity (WT-3 foundation), client/server label separation (Pitfall 3), field sensitivity, nonce uniqueness (D-06), sign/verify roundtrip

## Task Commits

1. **Task 1: Implement run_inner_auth_server + WtransportTransport::export_keying_material** - `bd0cb7a` (feat)
2. **Task 2: Replace compile-time bypass with explicit InnerAuthMode; wire main.rs WT arm** - `bba1ff2` (feat)

## Files Created/Modified

- `crates/nosh-server/src/inner_auth.rs` — new; `run_inner_auth_server`, transcript helpers, unit tests
- `crates/nosh-server/src/lib.rs` — added `pub mod inner_auth`
- `crates/nosh-server/src/wt_transport.rs` — added `InnerAuthMode` enum, `export_keying_material` override, rewrote `handle_connection_wt`/`run_wt_accept_loop` with new params
- `crates/nosh-server/src/main.rs` — WT arm now loads host key + authorized_keys, passes `InnerAuthMode::Required`
- `crates/nosh-server/Cargo.toml` — added `sha2 = "0.10"`, `getrandom = "0.2"`, `ed25519-dalek = "2.2"`
- `crates/nosh-client/tests/common/mod.rs` — `spawn_wt_server` updated with explicit `InnerAuthMode::TestBypass` + dummy host_signer/authorized

## Decisions Made

The control stream is `accept_bi()`-ed FIRST (before the auth gate) so the same stream carries both inner-auth frames and the post-auth session open frame — matching the client-side `open_bi()` in Plan 03. No second stream is needed (D-05/MH-1 invariant preserved).

Client signature is verified BEFORE the `authorized_keys` lookup (Pitfall 5 fixed-order) to prevent a timing oracle that would reveal whether the client key is in the authorized set.

## Deviations from Plan

None — plan executed exactly as written.

## Real-auth test seam (InnerAuthMode): known deferral

The plan permitted leaving `wt01_live_shell` on `TestBypass` if the client-side `run_inner_auth_client` (Plan 03) is not yet available. This is the actual state: `wt01`, `wt02`, and `wt03` all use `TestBypass` in the updated `common/mod.rs`. Plan 04 Task 1 is the designated owner of converting `wt01` to real end-to-end inner auth once the client side lands.

This deferral is intentional and documented — it does NOT defeat the security property. The `InnerAuthMode::Required` path in `handle_connection_wt` and `run_wt_accept_loop` is live and exercises the real `run_inner_auth_server` gate. The Plan 04 adversarial tests will target `Required` explicitly.

## Issues Encountered

- `InProcessEd25519Signer::generate()` is `#[cfg(test)]`-gated in nosh-auth, so it is not callable from nosh-server's `#[cfg(test)]` code. Fixed by adding `ed25519-dalek = "2.2"` as a direct dependency and generating the key manually from a getrandom seed in the test helper.
- `quinn::Connection::export_keying_material` takes `&mut [u8]` (unsized slice), not `&mut [u8; 32]`. The `NoshTransport` trait uses the fixed-size form; coerced via `output as &mut [u8]` at the call site.

## Next Phase Readiness

- Plan 03 (client-side inner auth) can now be executed: the server gate is live, the transcript layout constants are shared from nosh-proto, and the `InnerAuthMode` seam is explicit
- Plan 04 (adversarial tests) can target `InnerAuthMode::Required` directly; the gate will not be vacuous
- Plan 02 deliverables satisfy the WT-04 requirement gate

---
*Phase: 25-inner-ssh-key-handshake-tofu-prompt*
*Completed: 2026-06-13*
