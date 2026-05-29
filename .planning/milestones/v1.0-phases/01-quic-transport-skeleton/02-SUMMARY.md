---
plan_id: "02"
phase: 1
title: "nosh-server binary: QUIC endpoint, accept loop, stream echo + datagram echo"
status: complete
requirements: [TRANS-01, TRANS-02, TRANS-03, TRANS-04]
---

# Summary — Plan 02: nosh-server

## What was built
- `crates/nosh-server/src/server.rs`:
  - `build_server_config()` — installs the ring CryptoProvider, generates an ephemeral rcgen self-signed cert for `localhost`, builds `rustls::ServerConfig` with `with_no_client_auth().with_single_cert(..)`, sets `alpn_protocols = [nosh/0]`, converts via `QuicServerConfig::try_from`, and attaches `nosh_proto::transport_config(false)`.
  - `make_endpoint(addr)` — `quinn::Endpoint::server`.
  - `run_accept_loop(endpoint)` — accept loop spawning `handle_connection` per connection.
  - `handle_connection` — logs peer + negotiated ALPN, then runs `stream_echo_loop` and `datagram_echo_loop` concurrently via `tokio::select!`.
  - Stream echo (TRANS-02): `accept_bi` → read to end → write same bytes back → `finish`.
  - Datagram echo (TRANS-03/04): `read_datagram` → check `max_datagram_size()` (PITFALL 2) → `send_datagram` echo.
  - `clean_exit` maps orderly teardown (ApplicationClosed/LocallyClosed/ConnectionClosed/TimedOut) to `Ok(())`.
- `crates/nosh-server/src/lib.rs` re-exporting `build_server_config`, `make_endpoint`, `run_accept_loop`; `[lib]` + `[[bin]]` targets in Cargo.toml.
- `crates/nosh-server/src/main.rs` — clap `--addr` (default 127.0.0.1) / `--port` (default 4433, D-06), tracing init, runs the accept loop.

## Verification
- `cargo build -p nosh-server` exits 0.
- `cargo run -p nosh-server -- --help` shows `--addr` and `--port` (default 4433).
- Live end-to-end: server accepts the client connection, logs ALPN `nosh/0`, echoes stream + datagram (confirmed via the client smoke test in Plan 03).

## Key files created
- crates/nosh-server/src/server.rs, crates/nosh-server/src/lib.rs
- crates/nosh-server/src/main.rs (replaced stub), crates/nosh-server/Cargo.toml (lib+bin)

## Notes / deviations
- `lib.rs` was added in this plan (Plan 04 originally scheduled it) so `main.rs` uses the library from the start and the integration tests reuse it with zero rework.

## Self-Check: PASSED
