---
phase: 24-webtransport-endpoint-mode-a
plan: "01"
subsystem: build/deps
tags: [wtransport, webtransport, cargo-features, crypto-provider, ring]
dependency_graph:
  requires: []
  provides: [wtransport-dep, webtransport-feature]
  affects: [Cargo.toml, crates/nosh-server/Cargo.toml, crates/nosh-client/Cargo.toml]
tech_stack:
  added: [wtransport 0.7.1]
  patterns: [workspace-dep, optional-feature-dep]
key_files:
  modified:
    - Cargo.toml
    - crates/nosh-server/Cargo.toml
    - crates/nosh-client/Cargo.toml
    - Cargo.lock
decisions:
  - "D-01: wtransport 0.7.1 with default-features=false and features=[self-signed,ring,quinn]; quinn feature required for quic_connection() access to datagram_send_buffer_space"
  - "D-02: time=0.3.47 workspace pin retained; wtransport issue #311 still open 2026-06-13; no 0.7.2 published (confirmed via cargo search)"
  - "SC#4 satisfied: cargo tree with webtransport features shows ring-only rustls, zero aws-lc-rs"
metrics:
  duration_minutes: 5
  completed_date: "2026-06-13"
  tasks_completed: 2
  tasks_total: 2
  files_modified: 4
---

# Phase 24 Plan 01: Workspace wtransport Dependency + webtransport Feature Gate Summary

wtransport 0.7.1 added to workspace with ring-only crypto provider gate; both nosh-server and nosh-client have off-by-default `webtransport` feature backed by optional workspace dep.

## What Was Built

**Task 1 — Workspace dep + time pin (commit ed8610e):**

Added `wtransport = { version = "0.7.1", default-features = false, features = ["self-signed", "ring", "quinn"] }` to `[workspace.dependencies]` in `Cargo.toml`. The `default-features = false` is load-bearing — it prevents the `aws-lc-rs` crypto provider from being activated alongside `ring`, which would panic at the first TLS handshake. The `quinn` feature is required (not optional) because `wtransport::Connection` does not expose `datagram_send_buffer_space` natively; it is accessed via `Connection::quic_connection() -> &quinn::Connection`.

Also added `time = "=0.3.47"` workspace pin to work around wtransport issue #311 (still open as of 2026-06-13; no 0.7.2 published — confirmed by `cargo search wtransport`).

**Task 2 — webtransport feature in both crates (commit c8c8b85):**

Both `crates/nosh-server/Cargo.toml` and `crates/nosh-client/Cargo.toml` gained:
- `[features]`: `webtransport = ["dep:wtransport"]`
- `[dependencies]`: `wtransport = { workspace = true, optional = true }`

SC#4 gate confirmed: `cargo tree --features nosh-server/webtransport` shows zero `aws-lc-rs` occurrences; rustls appears only with `ring,std` features. Existing `cargo test --workspace` (default features) passes unchanged.

## Commits

| Task | Commit | Description |
|------|--------|-------------|
| 1 | ed8610e | chore(24-01): add wtransport 0.7.1 + time pin to workspace dependencies |
| 2 | c8c8b85 | chore(24-01): add webtransport feature + optional wtransport dep to both crates |

## Deviations from Plan

None — plan executed exactly as written. The D-02 re-ask trigger (check for 0.7.2) was run first via `cargo search wtransport` and confirmed 0.7.1 is still latest; the time pin was added as specified.

## Threat Surface Scan

No new network endpoints, auth paths, or schema changes were introduced. This plan is purely a Cargo manifest change. The crypto-provider unification risk (T-24-01-T) was explicitly checked via SC#4 (`cargo tree` with webtransport features) and confirmed mitigated.

## Self-Check: PASSED

- [x] `Cargo.toml` contains `wtransport = { version = "0.7.1", default-features = false, features = ["self-signed", "ring", "quinn"] }`
- [x] `Cargo.toml` contains `time = "=0.3.47"`
- [x] `crates/nosh-server/Cargo.toml` has `webtransport = ["dep:wtransport"]` and `wtransport = { workspace = true, optional = true }`
- [x] `crates/nosh-client/Cargo.toml` has `webtransport = ["dep:wtransport"]` and `wtransport = { workspace = true, optional = true }`
- [x] `cargo build --features nosh-server/webtransport,nosh-client/webtransport` exits 0
- [x] SC#4: zero `aws-lc-rs` in `cargo tree` with webtransport features
- [x] `cargo test --workspace` (default features) passes
- [x] Commits ed8610e and c8c8b85 exist in git log
