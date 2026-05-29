---
plan_id: "03"
phase: 1
title: "nosh-client binary: connect, ALPN assert, stream echo + datagram round-trip + idle hold"
status: complete
requirements: [TRANS-01, TRANS-02, TRANS-03, TRANS-04, TRANS-05]
---

# Summary — Plan 03: nosh-client

## What was built
- `crates/nosh-client/src/client.rs`:
  - `build_client_config()` — installs ring provider, builds `rustls::ClientConfig` with `PlaceholderServerVerifier` (Phase 2 seam) and `with_no_client_auth()`, sets `alpn_protocols = [nosh/0]`, converts via `QuicClientConfig::try_from`, attaches `nosh_proto::transport_config(true)` (keep-alive ON — TRANS-05).
  - `make_endpoint()` — client endpoint on an ephemeral local port with the config as default.
  - `connect(endpoint, addr)` — connects to "localhost" and asserts negotiated ALPN == `nosh/0` (TRANS-01).
  - `stream_echo_roundtrip` (TRANS-02), `datagram_roundtrip` asserting `max_datagram_size().is_some()` + payload fits (TRANS-03), `concurrent_roundtrip` via `tokio::try_join!` (TRANS-04).
- `crates/nosh-client/src/lib.rs` re-exporting all helpers; `[lib]` + `[[bin]]` in Cargo.toml; dev-dependency on `nosh-server` for tests.
- `crates/nosh-client/src/main.rs` — clap `--addr`/`--port` (default 4433) + `--idle-hold-secs` (default 2); connects, runs all round-trips with assertions, holds idle, checks `conn.close_reason().is_none()`, closes cleanly and `wait_idle`.

## Verification
- `cargo build -p nosh-client` exits 0; `--help` shows the flags.
- **Live end-to-end demo** (server on :14433, client connecting): all five checks pass and the client exits 0:
  - ALPN nosh/0 verified (TRANS-01)
  - stream echo matched (TRANS-02)
  - datagram round-trip matched, `max_datagram_size = Some(1382)` (TRANS-03)
  - concurrent stream + datagram ok (TRANS-04)
  - connection survived idle (TRANS-05)

## Key files created
- crates/nosh-client/src/client.rs, crates/nosh-client/src/lib.rs
- crates/nosh-client/src/main.rs (replaced stub), crates/nosh-client/Cargo.toml (lib+bin+dev-deps)

## Notes / deviations
- `lib.rs` + dev-dependency on nosh-server added here (originally Plan 04) to avoid rework; the integration tests in Plan 04 import these helpers directly.

## Self-Check: PASSED
