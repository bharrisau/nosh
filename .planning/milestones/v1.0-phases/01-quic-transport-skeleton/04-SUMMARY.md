---
plan_id: "04"
phase: 1
title: "Integration tests + runnable demo wiring + README"
status: complete
requirements: [TRANS-01, TRANS-02, TRANS-03, TRANS-04, TRANS-05]
---

# Summary — Plan 04: Integration tests + demo + README

## What was built
- **Library targets** for server/client were already added in waves 2 (`nosh_server` / `nosh_client` libs re-exporting connection helpers), so this plan only added the tests and README.
- `crates/nosh-client/tests/transport.rs` — integration tests, each spinning an in-process server on `127.0.0.1:0` (ephemeral port, discovered via `local_addr()`) with the accept loop on a background task:
  - `handshake_and_alpn` (TRANS-01) — connect succeeds; `client::connect` asserts ALPN == nosh/0.
  - `stream_echo_intact` (TRANS-02) — 43-byte payload echoes back byte-identical.
  - `datagram_roundtrip_enabled` (TRANS-03) — `max_datagram_size().is_some()` and datagram echoes intact.
  - `stream_and_datagram_coexist` (TRANS-04) — `concurrent_roundtrip` via `try_join!`.
  - `idle_survival_fast` (TRANS-05 fast proxy) — 3s idle, still alive and usable.
  - `idle_survival_60s` (TRANS-05 honest, `#[ignore]`) — 60s idle, connection survives and a stream still round-trips.
- `README.md` — project description, Phase 1 crate table, "Run the demo" (two-binary commands, expected tracing output) and "Run the tests" (`cargo test --workspace` + `-- --ignored`).

## Verification
- `cargo build --workspace` and `cargo build --bins` exit 0.
- `cargo test --workspace` — all unit + integration tests pass: 5 transport tests pass, `idle_survival_60s` ignored (3 codec unit tests also pass).
- `cargo test --workspace -- --ignored` — `idle_survival_60s` passes in 60.01s (honest TRANS-05 proof).
- `cargo clippy --workspace --all-targets` — no warnings, no errors.
- Live two-binary demo confirmed in Plan 03 (client exits 0 with all five checks logged).

## Key files created
- crates/nosh-client/tests/transport.rs
- README.md

## Notes / deviations
- The lib targets + nosh-server dev-dependency this plan was scheduled to create were brought forward into plans 02/03 to avoid rework; the net result matches the plan.

## Self-Check: PASSED
