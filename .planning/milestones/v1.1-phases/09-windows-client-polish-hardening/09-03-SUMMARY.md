---
phase: 09-windows-client-polish-hardening
plan: "03"
subsystem: nosh-server
tags: [observability, migration, roaming, logging]
dependency_graph:
  requires: []
  provides: [OBS-01]
  affects: [crates/nosh-server]
tech_stack:
  added: []
  patterns: [tokio-interval-poll, select-arm-observer]
key_files:
  created: []
  modified:
    - crates/nosh-server/src/server.rs
decisions:
  - "Poll cadence 500 ms with MissedTickBehavior::Skip — responsive to human roaming events, bounds log rate to ~2/s max (T-09-08)"
  - "poll-based detection chosen because quinn 0.11 has no migration callback API"
  - "Select arm does not break the loop — purely observational"
  - "Placed only in run_session (not run_reattach_session) per plan constraint: keep change surface minimal"
metrics:
  duration_minutes: 10
  completed_date: "2026-05-30T07:21:14Z"
  tasks_completed: 1
  files_changed: 1
---

# Phase 9 Plan 03: Server Connection Migration Logging (OBS-01)

Server-side INFO log when a session's peer address changes (QUIC connection migration), closing the observability gap found in live validation.

## Task Completed

### Task 1: log connection migration in the session loop

In `run_session` (`crates/nosh-server/src/server.rs`):

- Added `last_seen_addr: SocketAddr = conn.remote_address()` baseline (sampled at loop entry, not at connection open time, to compare like-for-like).
- Added `migration_poll = tokio::time::interval(Duration::from_millis(500))` with `MissedTickBehavior::Skip`.
- New `tokio::select!` arm polls `conn.remote_address()` on each tick; when it differs from `last_seen_addr`, emits:
  ```
  INFO session_id=<id> old=<prev_addr> new=<cur_addr> connection migrated
  ```
  and updates `last_seen_addr = cur`.
- Arm never breaks the loop. All existing arms (wait_task, out_rx, read_message) and SessionEnd semantics are unchanged.

Root cause: ROAM-01 migration was validated working on a real Windows client with an actual network change, but the server emitted no log — OBS-01 closes this gap.

## Verification

### Validated on Linux
- `grep -n "connection migrated\|remote_address\|migration_poll" crates/nosh-server/src/server.rs`: all three present
- `cargo build -p nosh-server`: passes
- `cargo test --workspace`: all pass — no regression to session, reattach, migration, persistence tests

### No Windows Host Needed
The migration logging is platform-agnostic; the poll arm only reads `remote_address()` (no platform-specific code). Functional verification (confirm the log fires on a real address change) can be done on Linux by the existing Phase 7 migration test, which exercises `Endpoint::rebind()` mid-session.

## Deviations from Plan

None — plan executed exactly as written. `run_reattach_session` not modified (plan specified this is optional and not required for OBS-01).

## Self-Check: PASSED
- `crates/nosh-server/src/server.rs`: FOUND, contains "connection migrated" and migration_poll
- Commit 2bf6c9d: FOUND
