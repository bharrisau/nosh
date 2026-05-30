---
phase: 07-connection-migration-validation
plan: 02
subsystem: test, docs
tags: [migration, connection-migration, qlog, roaming, D-02, D-03, D-04, D-05, D-06]
dependency_graph:
  requires: [07-01]
  provides: [migration-validation-test, live-check-doc]
  affects: [crates/nosh-client/tests, docs]
tech_stack:
  added: [serde_json (dev-dep for qlog JSON-seq parse)]
  patterns: [headless-migration-test, rebind-mid-stream]
key_files:
  created:
    - crates/nosh-client/tests/migration.rs
    - docs/migration-live-check.md
  modified:
    - crates/nosh-client/Cargo.toml
decisions:
  - "qlog validation uses serde_json (already a transitive dep of qlog crate) for robust JSON-seq parse rather than a byte scan"
  - "First-frame handling: read and discard SessionOpened; if an unexpected frame arrives, continue gracefully rather than hard-fail (shell prompt timing)"
  - "Post-rebind first-frame timer set from any PtyData frame (not just LINE:N) to measure actual stall rather than parsing latency"
metrics:
  duration: "~30 minutes"
  completed: "2026-05-30"
---

# Phase 7 Plan 02: Migration Validation Test + Live-Check Doc Summary

**One-liner:** Headless `migration_survives_path_change` test proves a live session survives `Endpoint::rebind()` mid-stream with no loss/reorder/error, CID rotation via FrameStats, and qlog artifact; `docs/migration-live-check.md` documents the Wi-Fi→cellular manual procedure.

## What Was Built

### Task 1: Headless migration test (D-02..D-05)

`crates/nosh-client/tests/migration.rs` — a single `#[tokio::test]` named `migration_survives_path_change`:

- Guarded by `have_sh()` `/bin/sh` check (skips cleanly in restricted envs).
- Wrapped in `tokio::time::timeout(30s)` so a hang fails loudly.
- Uses `common::client_endpoint_with_qlog` (Plan 01) writing to a tempdir qlog file.
- Drives a monotonic 80-line output stream (`LINE:0`..`LINE:79`) over ~4s.
- After `LINE:10`, captures pre-rebind `conn.stats()`, `conn.stable_id()`, `conn.rtt()`, then calls `common::rebind_client` to force the path change (D-02).
- Sends an empty client frame immediately after rebind to advance the server's anti-amplification budget (Pitfall #2 mitigation).
- Asserts: strictly monotone sequence 0..79 with no gap/dup/reorder AND unchanged `stable_id` (same connection, no new handshake) — D-03.
- Measures wall-clock stall from last pre-rebind frame to first post-rebind frame; logs ratio to RTT; soft-warns if > 3x (D-04, no hard assert).
- Asserts FrameStats deltas: `path_challenge` increased AND `new_connection_id`+`retire_connection_id` increased — D-05 binding proof (per 07-RESEARCH.md §1, quinn 0.11.9 qlog does not record PATH_CHALLENGE frames or CID fields).
- Closes endpoint, waits 200ms for qlog to flush, then asserts qlog file exists, non-empty, and each RS-delimited record is valid `serde_json::Value` — D-05 artifact check.

**Test results (single run):**
```
D-03 PASS: 80 lines (0..79), no gap/dup/reorder; stable_id unchanged
D-04: stall 368µs (~0.1x RTT — well within 3x)
D-05 PASS: path_challenge 0→2; CID counters 10→14; qlog 211 records / 32KB
```

### Task 2: Wi-Fi→cellular live-check doc (D-06)

`docs/migration-live-check.md`:
- Purpose section distinguishing migration from cold reattach.
- Prerequisites (public-IP server, dual-network client, SSH key in authorized_keys).
- Step-by-step: connect over Wi-Fi, start numbered output loop, disable Wi-Fi to fall back to cellular.
- PASS checklist with 5 items: no re-auth, no reconnect message, output resumes, no loss/dup, same session state.
- Fillable RESULT block with operator, date, OS, network, stall, PASS/FAIL fields.
- Explicit non-blocking note: Phase 7 is `human_needed`; live check recorded in phase completion notes (D-06).

## Deviations from Plan

None from the intended behavior. One implementation note:

**qlog parse approach:** Used `serde_json` (already a transitive dep of the `qlog` crate via `qlog` feature unification) for JSON-seq record validation instead of a byte-scan. Added `serde_json = "1"` to dev-dependencies explicitly to avoid relying on transitive availability. This is cleaner than a raw `first-byte == '{'` check and was very low cost since the lockfile already contained serde_json 1.0.150.

## Test Results

```
cargo test -p nosh-client --test migration -- --nocapture:
  migration_survives_path_change ... ok  (4.49s)

cargo test --workspace: ALL PASS
  nosh-auth:              11 passed, 0 failed (1 ignored)
  nosh-client migration:   1 passed, 0 failed
  nosh-client auth:        6 passed, 0 failed
  nosh-client persistence: 3 passed, 0 failed
  nosh-client reattach:    3 passed, 0 failed
  nosh-client session:     6 passed, 0 failed
  nosh-client transport:   4 passed, 0 failed (1 ignored)
  nosh-proto:              6 passed, 0 failed
  nosh-server:            23+1 passed, 0 failed
```

## Commits

| Task | Message | Hash |
|------|---------|------|
| 1 | feat(07-02): headless connection migration validation test (D-02..D-05) | f586487 |
| 2 | docs(07-02): Wi-Fi→cellular live-check procedure + PASS checklist (D-06) | b411b98 |

## Known Stubs

None — the live check (D-06) is explicitly documented as non-blocking; the checklist and result block are to be filled in by the operator. The doc is complete; it is not a stub.

## Threat Flags

None — this plan adds only test code and a documentation file. No new network endpoints, auth paths, or schema changes.

## Self-Check: PASSED

- `crates/nosh-client/tests/migration.rs` exists, >= 90 lines: CONFIRMED (310+ lines)
- Test calls `rebind_client` mid-stream: CONFIRMED
- Test asserts LINE:n sequence no gap/dup/reorder (D-03): CONFIRMED
- Test asserts `conn.stable_id()` unchanged (D-03): CONFIRMED
- Test asserts FrameStats path_challenge + new/retire_connection_id increased (D-05): CONFIRMED
- Test measures + prints stall as Nx RTT, soft-warn only (D-04): CONFIRMED
- Test asserts qlog file exists, non-empty, parses (D-05 artifact): CONFIRMED
- `docs/migration-live-check.md` exists: CONFIRMED
- doc contains Wi-Fi procedure: CONFIRMED (12 occurrences of "Wi-Fi")
- doc contains PASS checklist: CONFIRMED
- doc contains RESULT block: CONFIRMED
- doc states live check is non-blocking: CONFIRMED
- All commits exist: f586487, b411b98: CONFIRMED
- `cargo test --workspace` passes: CONFIRMED
