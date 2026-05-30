---
phase: 07-connection-migration-validation
plan: 01
subsystem: server-config, test-harness, transport
tags: [migration, qlog, transport-config, test-infrastructure]
dependency_graph:
  requires: [06-cold-reattach-protocol]
  provides: [migration-flag, qlog-infrastructure, rebind-helper]
  affects: [crates/nosh-server, crates/nosh-client, crates/nosh-proto]
tech_stack:
  added: [quinn-qlog-feature]
  patterns: [qlog-endpoint-builder, transport-rebind-helper]
key_files:
  created: [crates/nosh-client/tests/common/mod.rs (extended)]
  modified:
    - crates/nosh-server/src/server.rs
    - crates/nosh-client/Cargo.toml
    - crates/nosh-proto/src/transport.rs
    - crates/nosh-client/src/client.rs
    - crates/nosh-client/tests/common/mod.rs
decisions:
  - "Enabled qlog via dev-dependency feature unification (approach B) to avoid polluting the production binary"
  - "Used local QlogConfig variable (not method chaining) because into_stream() takes self by value"
metrics:
  duration: "~20 minutes"
  completed: "2026-05-30"
---

# Phase 7 Plan 01: Migration Infrastructure Summary

**One-liner:** Explicit `ServerConfig::migration(true)` with D-01 comment, quinn qlog dev feature, and qlog-enabled endpoint builder + rebind helper in the test harness.

## What Was Built

### Task 1: ServerConfig::migration(true) — D-01

Added an explicit `server_config.migration(true)` call in `build_server_config` in `crates/nosh-server/src/server.rs`, immediately after the transport config line. Preceded by a comment citing D-01 / Pitfall #1 / ROAM-01 explaining that this is intentional (not relying on the quinn default) so a future default change cannot silently disable connection migration.

### Task 2: quinn qlog feature + Pitfall #4 transport documentation

Added `quinn = { workspace = true, features = ["qlog"] }` to `[dev-dependencies]` in `crates/nosh-client/Cargo.toml` (approach B — feature unification; test build sees qlog, production binary does not require it). Added a Pitfall #4 / ROAM-01 comment in `transport.rs` near the `KEEP_ALIVE` and `MAX_IDLE_TIMEOUT` constants documenting that these values are intentionally unchanged for migration (300s idle >> any path-validation window; 15s keep-alive keeps the new path warm). Constants remain at 15s / 300s.

### Task 3: qlog-enabled endpoint builder + rebind helper

Added `make_endpoint_with_transport` to `crates/nosh-client/src/client.rs` — builds a client endpoint with a caller-supplied `TransportConfig` (all other auth/ALPN unchanged). Added three functions to `crates/nosh-client/tests/common/mod.rs`:
- `client_endpoint_with_qlog(identity, known_hosts, qlog_path)` — builds a client endpoint with a `QlogStream` writing to `qlog_path`; degrades gracefully on any qlog setup failure.
- `fresh_loopback_socket()` — `UdpSocket::bind("127.0.0.1:0")`.
- `rebind_client(endpoint)` — binds a fresh 127.0.0.1:0 socket and calls `endpoint.rebind()`, returning the new local addr.

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 1 - Bug] QlogConfig method chaining cannot call into_stream() directly**

- **Found during:** Task 3 compilation
- **Issue:** `QlogConfig::into_stream()` takes `self` by value, but the chained builder methods return `&mut Self`. Cannot call `.into_stream()` on the result of a chain; compiler error E0507.
- **Fix:** Assigned `QlogConfig::default()` to a local `mut qlog_cfg` variable and called `.writer()` / `.title()` as separate statements before calling `.into_stream()`.
- **Files modified:** `crates/nosh-client/tests/common/mod.rs`
- **Commit:** 11f6504

## Test Results

```
cargo test --workspace: ALL PASS
  nosh-auth: 11 passed, 0 failed (1 ignored — ssh-agent)
  nosh-client auth: 6 passed, 0 failed
  nosh-client persistence: 3 passed, 0 failed
  nosh-client reattach: 3 passed, 0 failed
  nosh-client session: 6 passed, 0 failed
  nosh-client transport: 4 passed, 0 failed (1 ignored — slow)
  nosh-proto: 6 passed, 0 failed
  nosh-server: 23+1 passed, 0 failed
```

## Commits

| Task | Message | Hash |
|------|---------|------|
| 1 | feat(07-01): set ServerConfig::migration(true) explicitly with intent comment (D-01) | 9094042 |
| 2 | feat(07-01): enable quinn qlog feature for test build; document Pitfall #4 transport settings (D-05) | c1e8e1d |
| 3 | feat(07-01): add qlog-enabled client endpoint builder and rebind helper to test harness | 11f6504 |

## Known Stubs

None — this plan is pure infrastructure (no user-visible behavior).

## Threat Flags

None — changes are additive test infrastructure + one production config line. No new network endpoints, auth paths, or schema changes.

## Self-Check: PASSED

- `crates/nosh-server/src/server.rs` contains `migration(true)` with intent comment: CONFIRMED
- `crates/nosh-client/Cargo.toml` contains `qlog` feature in dev-dependencies: CONFIRMED
- `crates/nosh-proto/src/transport.rs` contains Pitfall #4 comment; constants unchanged: CONFIRMED
- `crates/nosh-client/tests/common/mod.rs` exports `client_endpoint_with_qlog`, `fresh_loopback_socket`, `rebind_client`: CONFIRMED
- All commits exist: 9094042, c1e8e1d, 11f6504: CONFIRMED
- `cargo build --workspace --tests` succeeds: CONFIRMED
- `cargo test --workspace` passes: CONFIRMED
