---
plan_id: "03"
phase: 1
wave: 2
title: "nosh-client binary: connect, ALPN assert, stream echo + datagram round-trip + idle hold"
depends_on: ["01"]
files_modified:
  - crates/nosh-client/src/main.rs
  - crates/nosh-client/src/client.rs
autonomous: true
requirements: [TRANS-01, TRANS-02, TRANS-03, TRANS-04, TRANS-05]
must_haves:
  truths:
    - "Client builds a quinn ClientConfig using the nosh-auth PlaceholderServerVerifier and ALPN nosh/0"
    - "Client transport config enables datagram buffers and sets keep_alive_interval (TRANS-05)"
    - "Client asserts negotiated ALPN equals nosh/0 after handshake (TRANS-01)"
    - "Client performs a reliable bidi-stream echo round-trip and verifies bytes returned intact (TRANS-02)"
    - "Client round-trips a datagram and checks max_datagram_size is Some on its side (TRANS-03)"
    - "Client runs the stream echo and datagram round-trip concurrently without interference (TRANS-04)"
  artifacts:
    - "crates/nosh-client/src/client.rs exposes build_client_config / connect / round-trip helpers reusable by integration tests"
---

# Plan 03 — nosh-client binary

## Objective
Implement the `nosh-client` binary: build a quinn `ClientConfig` (placeholder verifier from `nosh-auth`, ALPN `nosh/0`, shared transport config with keep-alive), connect to the server, assert ALPN, then run a reliable-stream echo round-trip and a datagram round-trip — concurrently — verifying both succeed without interference. Structure the logic in `client.rs` for reuse by Plan 04 integration tests.

## Context
- Depends on Plan 01 (nosh-proto ALPN/transport_config, nosh-auth PlaceholderServerVerifier; workspace deps already in nosh-client/Cargo.toml).
- The client sets `keep_alive_interval` (it is the keep-alive side per quinn docs) — this is what proves TRANS-05.

<task id="1" type="execute">
<title>client.rs: build quinn client config with placeholder verifier</title>
<read_first>
- crates/nosh-proto/src/lib.rs (ALPN, transport_config)
- crates/nosh-auth/src/verifier.rs (PlaceholderServerVerifier)
- .planning/phases/01-quic-transport-skeleton/01-RESEARCH.md (quinn↔rustls wiring, client side; TLS verifier seam)
- .planning/research/STACK.md (QuicClientConfig::try_from)
</read_first>
<action>
Create `crates/nosh-client/src/client.rs`. Provide `pub fn build_client_config() -> anyhow::Result<quinn::ClientConfig>`:
  - Install the ring CryptoProvider default once (`rustls::crypto::ring::default_provider().install_default().ok();`) and obtain the process default provider via `rustls::crypto::CryptoProvider::get_default()` (Arc) to hand to the verifier.
  - Build `rustls::ClientConfig::builder().dangerous().with_custom_certificate_verifier(Arc::new(nosh_auth::verifier::PlaceholderServerVerifier::new(provider))).with_no_client_auth();` then set `rustls_cfg.alpn_protocols = vec![nosh_proto::ALPN.to_vec()];`.
  - Convert: `quinn::ClientConfig::new(Arc::new(quinn::crypto::rustls::QuicClientConfig::try_from(rustls_cfg)?))`, then `client_config.transport_config(Arc::new(nosh_proto::transport_config(true)))` (true = enable keep-alive — TRANS-05).
Provide `pub fn make_endpoint() -> anyhow::Result<quinn::Endpoint>` that builds a client `quinn::Endpoint::client("0.0.0.0:0".parse()?)` and sets its default client config.
Provide `pub async fn connect(endpoint: &quinn::Endpoint, server_addr: SocketAddr) -> anyhow::Result<quinn::Connection>` calling `endpoint.connect(server_addr, "localhost")?.await?` and then asserting the negotiated ALPN: read `conn.handshake_data()` → downcast `quinn::crypto::rustls::HandshakeData` → `anyhow::ensure!(hd.protocol.as_deref() == Some(nosh_proto::ALPN), "ALPN mismatch")` (TRANS-01). Return the connection.
</action>
<acceptance_criteria>
- `crates/nosh-client/src/client.rs` contains `build_client_config`, `make_endpoint`, `connect`
- Client config uses `PlaceholderServerVerifier` and sets `alpn_protocols` to `nosh_proto::ALPN.to_vec()`
- Client transport config calls `nosh_proto::transport_config(true)` (keep-alive enabled)
- `connect` asserts the negotiated ALPN equals `nosh_proto::ALPN`
- `cargo build -p nosh-client` exits 0
</acceptance_criteria>
</task>

<task id="2" type="execute">
<title>client.rs: stream echo + datagram round-trip helpers (concurrent)</title>
<read_first>
- crates/nosh-client/src/client.rs (connect from task 1)
- .planning/phases/01-quic-transport-skeleton/01-RESEARCH.md (datagram send/recv; coexistence)
- .planning/research/PITFALLS.md (Pitfall 1 max_datagram_size Some, Pitfall 2 size check)
</read_first>
<action>
In `client.rs` add:
  - `pub async fn stream_echo_roundtrip(conn: &quinn::Connection, payload: &[u8]) -> anyhow::Result<Vec<u8>>`: `conn.open_bi().await` → `send.write_all(payload).await; send.finish()?;` → `recv.read_to_end(limit).await` → return the echoed bytes. The caller asserts they equal `payload` (TRANS-02).
  - `pub async fn datagram_roundtrip(conn: &quinn::Connection, payload: Bytes) -> anyhow::Result<Bytes>`: `anyhow::ensure!(conn.max_datagram_size().is_some(), "datagrams not enabled")` (TRANS-03 proof), ensure payload fits `max_datagram_size()` (Pitfall 2), `conn.send_datagram(payload)?`, then `conn.read_datagram().await` for the echo and return it (caller asserts equality, TRANS-04).
  - `pub async fn concurrent_roundtrip(conn: &quinn::Connection) -> anyhow::Result<()>`: run `stream_echo_roundtrip` and `datagram_roundtrip` concurrently via `tokio::try_join!`, assert each result matches its sent payload — proving streams and datagrams coexist without interference (TRANS-04).
</action>
<acceptance_criteria>
- `client.rs` contains `stream_echo_roundtrip`, `datagram_roundtrip`, `concurrent_roundtrip`
- `datagram_roundtrip` asserts `conn.max_datagram_size().is_some()` (TRANS-03)
- `concurrent_roundtrip` uses `tokio::try_join!` to run stream + datagram together (TRANS-04)
- `cargo build -p nosh-client` exits 0
</acceptance_criteria>
</task>

<task id="3" type="execute">
<title>main.rs: CLI, connect, run round-trips, brief idle hold, exit</title>
<read_first>
- crates/nosh-client/src/client.rs
- .planning/phases/01-quic-transport-skeleton/01-CONTEXT.md (D-06 port default; D-08 demo)
</read_first>
<action>
Replace `crates/nosh-client/src/main.rs` stub with the real entrypoint. Add `mod client;`. Define a clap derive `Args` with `--addr` (default `127.0.0.1`, `IpAddr`), `--port` (default `4433`, u16), and `--idle-hold-secs` (default `2`, u64 — short by default so the demo is quick; document that a real 60s idle survival is exercised by the integration test). In `#[tokio::main] async fn main() -> anyhow::Result<()>`: init tracing; parse args; build server `SocketAddr`; `client::make_endpoint()?`; `client::connect(&endpoint, server_addr).await?`; log "connected, ALPN nosh/0 verified". Then: run `client::stream_echo_roundtrip` with a known payload (e.g. b"hello-nosh") and assert/log the echo matches; run `client::datagram_roundtrip` with a small `Bytes` payload and assert/log; run `client::concurrent_roundtrip` and log success. Then `tokio::time::sleep(Duration::from_secs(args.idle_hold_secs)).await` and confirm the connection is still alive (`conn.close_reason().is_none()`), logging "connection survived {n}s idle". Close the connection cleanly with `conn.close(0u32.into(), b"done")` and `endpoint.wait_idle().await`. Exit 0 on full success, non-zero on any failure.
</action>
<acceptance_criteria>
- `crates/nosh-client/src/main.rs` contains a clap struct with `port` default `4433` and an idle-hold option
- `main` calls `connect`, `stream_echo_roundtrip`, `datagram_roundtrip`, and `concurrent_roundtrip`
- `main` performs an idle sleep then checks `conn.close_reason().is_none()`
- `cargo build -p nosh-client` exits 0
- `cargo run -p nosh-client -- --help` exits 0 and shows `--addr`, `--port`
</acceptance_criteria>
</task>

## Verification
- `cargo build -p nosh-client` succeeds.
- `cargo run -p nosh-client -- --help` shows the flags.
- `client.rs` exposes `build_client_config`, `make_endpoint`, `connect`, and the round-trip helpers for reuse by Plan 04 tests.
