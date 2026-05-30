---
phase: 07-connection-migration-validation
verified: 2026-05-30T00:00:00Z
status: human_needed
score: 4/4 must-haves verified (3 automated VERIFIED + 1 human-pending live check)
overrides_applied: 0
human_verification:
  - test: "Wi-Fi→cellular real-network live check (D-06 / ROAM-01 SC#4)"
    expected: "A live nosh session continues across a real Wi-Fi→cellular switch with no re-auth prompt, no reconnect/error message, no line loss/dup, same session state, and only a brief (~1-2s) anti-amplification pause. Recorded as PASSED in the phase completion notes."
    why_human: "Requires a physical device that can switch from Wi-Fi to cellular against a publicly reachable server; cannot be exercised by loopback CI. The headless rebind test proves the same-connection migration mechanism; only a human can confirm the real multi-homed network path. D-06 explicitly marks this as NON-BLOCKING for autonomous completion."
---

# Phase 7: Connection Migration Validation — Verification Report

**Phase Goal:** A live nosh session survives a client IP/path change with no re-handshake and no application-visible interruption, confirmed by headless CI and a human live check.
**Verified:** 2026-05-30
**Status:** human_needed
**Re-verification:** No — initial verification

## Goal Achievement

The three automated success criteria are genuinely achieved in the codebase, confirmed by reading the actual code AND running the tests independently (not by trusting SUMMARY/REVIEW prose). The fourth criterion is a deliberately non-blocking human live check (D-06) that has not yet been recorded as PASSED — this routes the phase to `human_needed`, which is the planned terminal state for this phase.

### Observable Truths (ROADMAP Success Criteria)

| # | Truth | Status | Evidence |
|---|-------|--------|----------|
| 1 | `ServerConfig::migration(true)` set explicitly with documenting comment (not implicit default) | ✓ VERIFIED | `crates/nosh-server/src/server.rs:87-92` — explicit `server_config.migration(true);` preceded by a 5-line comment citing D-01 / Pitfall #1 / ROAM-01 and explaining intent (future quinn default change cannot silently disable roaming). |
| 2 | Headless integration test performs `Endpoint::rebind()` mid-session; active reliable stream continues with no `ConnectionError` and no message loss | ✓ VERIFIED | `crates/nosh-client/tests/migration.rs` runs a real rebind onto a fresh `127.0.0.1:0` socket (`rebind_client`, common/mod.rs:213-218 binds a NEW ephemeral-port socket and calls `endpoint.rebind`). Test asserts contiguous `LINE:0..79` (no gap/dup/reorder, lines 280-314), hard-fails on any `ConnectionError` (line 160 `.expect`, lines 251-265 transport-error panic). Live run: `D-03 PASS: received 80 lines (0..79)`, new local addr `127.0.0.1:52337`. |
| 3 | qlog/headless test confirms CID rotation on path change (RFC 9000 §9.5) | ✓ VERIFIED | Binding proof is `Connection::stats()` FrameStats deltas (correct: quinn 0.11.9 qlog records no PATH_CHALLENGE/CID — documented in 07-RESEARCH.md §1 and code comments). Test asserts `path_challenge` increased (0→2) AND `new/retire_connection_id` increased (10→16) across rebind (lines 350-392). qlog artifact also validated as present/non-empty/parseable JSON-seq (218 records, 32993 bytes). |
| 4 | Human Wi-Fi→cellular live check recorded as PASSED in phase completion notes | ? HUMAN-PENDING | `docs/migration-live-check.md` provides the full operator procedure + PASS checklist + RESULT block (D-06). The live run itself has NOT been recorded as PASSED. D-06 declares this non-blocking; phase is correctly `human_needed`. |

**Score:** 3/4 automated truths VERIFIED; truth #4 is the non-blocking human live check (pending).

### Adversarial Probe (anti-trivial-pass check)

To confirm the D-05 CID/path-validation assertions are NOT trivially satisfiable, I neutralized the rebind call (replaced `rebind_client(&endpoint)` with `endpoint.local_addr()` — no new socket, no path change) and re-ran the test:

```
[migration] D-03 PASS: received 80 lines (0..79)   ← data still flows (correct; not the migration proof)
[migration] D-04: migration stall: 54.489ms (~12.6x RTT)
D-05 FAIL: path_challenge FrameStats did not increase across rebind (pre=0, post=0);
           PATH_CHALLENGE/PATH_RESPONSE did not run — the path change was not validated
test migration_survives_path_change ... FAILED
```

This proves the D-05 FrameStats assertion is the binding proof that migration actually occurred — it FAILS when no rebind happens and PASSES (0→2 path_challenge) only when a real path change triggers QUIC path validation. The original file was restored after the probe.

### Code-Review Fixes — Independently Re-Verified

The REVIEW found 1 blocker + 3 warnings; REVIEW-FIX claimed all 4 fixed. I confirmed each in the actual code:

| Finding | Claimed Fix | Verified |
|---------|-------------|----------|
| CR-01 (BLOCKER): dropped first frame when not SessionOpened | `pending_first_frame: Option<Message>` re-injects into main loop | ✓ migration.rs:96-113 stores non-SessionOpened first frame; lines 151-161 drain it before reading network. No silent discard. |
| WR-01: DONE detection no-op | `done_received` flag + `break` | ✓ migration.rs:145, 210-213 (inner break), 246-248 (outer break). |
| WR-02: stall measurement race | `just_rebound` flag guards `t_first_post` | ✓ migration.rs:142, 207, 222 (`!just_rebound` guard), 241 (reset). Inner-loop `t_first_post` capture removed (lines 181-183 comment). |
| WR-03: rcgen unused production dep | Removed from `[dependencies]` | ✓ `crates/nosh-client/Cargo.toml` — no `rcgen` in `[dependencies]`. |

None of these fixes mask a real failure or cause a trivial pass — the probe above confirms the binding assertion still genuinely depends on migration occurring. IN-01 (info-only dead `pre_stats` write at line 127) was excluded from fix scope; it is overwritten at line 188 before its read at line 358 — harmless, not a goal blocker.

### Required Artifacts

| Artifact | Expected | Status | Details |
|----------|----------|--------|---------|
| `crates/nosh-server/src/server.rs` | explicit `migration(true)` + intent comment | ✓ VERIFIED | Lines 87-92. |
| `crates/nosh-proto/src/transport.rs` | Pitfall #4 comment, KEEP_ALIVE=15s / MAX_IDLE_TIMEOUT=300s unchanged | ✓ VERIFIED | Comment at lines 13-19; constants 15s/300s at lines 20-23. |
| `crates/nosh-client/tests/common/mod.rs` | `client_endpoint_with_qlog`, `fresh_loopback_socket`, `rebind_client` | ✓ VERIFIED | Lines 164-218; rebind binds a real fresh socket. |
| `crates/nosh-client/tests/migration.rs` | headless migration test ≥90 lines | ✓ VERIFIED | 473 lines; runs and passes. |
| `crates/nosh-client/src/client.rs` | `make_endpoint_with_transport` (additive) | ✓ VERIFIED | Line 125; existing `make_endpoint` (line 107) preserved. |
| `crates/nosh-client/Cargo.toml` | quinn `qlog` dev-feature | ✓ VERIFIED | `[dev-dependencies] quinn = { features = ["qlog"] }`; `serde_json` for parse. |
| `docs/migration-live-check.md` | Wi-Fi→cellular procedure + checklist + RESULT (D-06) | ✓ VERIFIED | Full doc with all sections; 12 "Wi-Fi" occurrences. |

### Key Link Verification

| From | To | Via | Status |
|------|----|----|--------|
| migration.rs | common/mod.rs | `rebind_client` + `client_endpoint_with_qlog` + `spawn_server_with_shell` | ✓ WIRED |
| migration.rs | `quinn::Connection::stats()` | FrameStats path_challenge / new/retire_connection_id deltas | ✓ WIRED (probe-confirmed binding) |
| server.rs | `quinn::ServerConfig` | `migration(true)` | ✓ WIRED |
| common/mod.rs | `quinn::Endpoint::rebind` | fresh 127.0.0.1:0 socket | ✓ WIRED |

### Behavioral Spot-Checks

| Behavior | Command | Result | Status |
|----------|---------|--------|--------|
| Migration test passes | `cargo test -p nosh-client --test migration -- --nocapture` | `1 passed`; D-03/D-04/D-05 all PASS; stall 0.1x RTT; path_challenge 0→2; CID 10→16 | ✓ PASS |
| Migration test fails without rebind (adversarial) | probe (rebind neutralized) | `D-05 FAIL: path_challenge ... pre=0, post=0` | ✓ PASS (assertion is non-trivial) |
| Full workspace green | `cargo test --workspace` | nosh-auth 11, client(auth 6/migration 1/persistence 3/reattach 3/session 6/transport 4), nosh-proto 6, nosh-server 23+1 — 0 failed | ✓ PASS |
| No compiler warnings | `cargo build -p nosh-client --tests` | clean | ✓ PASS |

### Requirements Coverage

| Requirement | Source Plan | Description | Status | Evidence |
|-------------|-------------|-------------|--------|----------|
| ROAM-01 | 07-01, 07-02 | Session survives IP/path change via migration, no re-handshake, validated headless + human live check | ✓ SATISFIED (automated) / human-pending (live check) | SC#1-3 verified above; SC#4 is the non-blocking D-06 live check. |

### Anti-Patterns Found

None. No TBD/FIXME/XXX/HACK/PLACEHOLDER markers in any phase-modified file. No stubs (live-check doc is a complete operator procedure, not a stub). No unaddressed debt markers.

### Human Verification Required

**1. Wi-Fi→cellular real-network live check (D-06 / ROAM-01 SC#4)**

- **Test:** Follow `docs/migration-live-check.md` — connect a nosh session over Wi-Fi to a publicly reachable server, start a numbered continuous output, switch the client from Wi-Fi to cellular, observe the session.
- **Expected:** No re-auth prompt, no reconnect/error message, output resumes after a brief (~1-2s) anti-amplification pause, no line loss/dup, same session state. Record PASS/FAIL in the phase completion notes.
- **Why human:** Requires a physical multi-homed device against a real server; loopback CI cannot exercise a real source-IP change. The headless test proves the same-connection migration mechanism; only the human check confirms the real-world roaming path. **Non-blocking per D-06 — autonomous completion does not require it.**

### Gaps Summary

No blocking gaps. All three automatable success criteria are genuinely achieved and independently confirmed by running the test and an adversarial probe that proves the migration assertions are not trivially satisfiable. The workspace is fully green. The only outstanding item is the D-06 human Wi-Fi→cellular live check, which is by design non-blocking and is the reason this phase terminates at `human_needed` rather than `passed`. The operator should run the documented procedure and record the PASS in the phase completion notes when convenient.

---

_Verified: 2026-05-30_
_Verifier: Claude (gsd-verifier, opus, adversarial pass)_

---

## Post-Verification Addendum (flake discovered + fixed)

During Phase 8 work, the migration validation test `migration_survives_path_change` was found to be **flaky (~50% failure)** — the initial opus verification above passed on lucky runs. Failure mode: `D-03 FAIL: sequence must start at LINE:0, got LINE:1` (LINE:0 intermittently dropped).

**Root cause:** a test-side parsing bug, NOT a client/protocol defect. PTY output is an unframed byte stream; on ~50% of runs the shell prompt coalesced with the first output into a single chunk `"$ LINE:0"` with no trailing newline, and the old `text.lines()` + `strip_prefix("LINE:")` parser silently dropped it.

**Fix:** commit `f7bd80a` — buffer partial lines across chunks and locate the `LINE:` token with `rfind`. Assertion NOT weakened; no retries/sleeps added; D-03/D-04/D-05 guarantees preserved.

**Re-verification:** 20/20 (fixer) + 15/15 (independent orchestrator run) = **35/35 PASS, 0 FAIL**. The migration validation test is now stable. SC#1–SC#3 remain VERIFIED on a now-reliable test; SC#4 (Wi-Fi→cellular live check, D-06) remains the non-blocking human item. Status unchanged: human_needed.
