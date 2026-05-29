---
plan_id: "02"
phase: 1
wave: 2
title: "nosh-server binary: QUIC endpoint, accept loop, stream echo + datagram echo"
depends_on: ["01"]
files_modified:
  - crates/nosh-server/src/main.rs
  - crates/nosh-server/src/server.rs
autonomous: true
requirements: [TRANS-01, TRANS-02, TRANS-03, TRANS-04]
must_haves:
  truths:
    - "Server builds a quinn Endpoint from an rcgen self-signed cert with ALPN nosh/0 and with_no_client_auth"
    - "Server attaches the shared nosh-proto transport_config (datagram buffers + finite idle timeout)"
    - "Server echoes bytes received on an accepted bidi stream back to the client (TRANS-02)"
    - "Server echoes datagrams received back to the client (TRANS-03/04)"
    - "D-06: bind address/port configurable via --addr/--port, default port 4433"
  artifacts:
    - "crates/nosh-server/src/server.rs exposes a reusable run_server / build_server_config used by both the binary and integration tests"
---

# Plan 02 — nosh-server binary

## Objective
Implement the `nosh-server` binary: build a quinn `Endpoint` (rcgen self-signed cert, ALPN `nosh/0`, shared transport config), run an accept loop, and for each connection echo bidi-stream bytes and echo datagrams. Structure the connection-handling logic as a library function (`server.rs`) so the integration tests in Plan 04 can drive it in-process.

## Context
- Depends on Plan 01 (nosh-proto ALPN/transport_config, workspace deps already in nosh-server/Cargo.toml).
- The server uses `with_no_client_auth()` this phase; client auth is Phase 2.
- Echo handlers prove TRANS-02 (stream) and TRANS-03/04 (datagram, coexistence).

<task id="1" type="execute">
<title>server.rs: build quinn server config and endpoint</title>
<read_first>
- crates/nosh-proto/src/lib.rs (ALPN, transport_config)
- .planning/phases/01-quic-transport-skeleton/01-RESEARCH.md (quinn↔rustls wiring, server side)
- .planning/research/STACK.md (QuicServerConfig::try_from wiring)
- .planning/research/PITFALLS.md (Pitfall 4 ALPN, Pitfall 1 datagrams)
</read_first>
<action>
Create `crates/nosh-server/src/server.rs`. Provide `pub fn build_server_config() -> anyhow::Result<quinn::ServerConfig>`:
  - Ensure the rustls ring CryptoProvider default is installed once (call `rustls::crypto::ring::default_provider().install_default().ok();`).
  - Generate an ephemeral self-signed cert with rcgen 0.14 for subject alt name `localhost` (rcgen `generate_simple_self_signed(vec!["localhost".into()])`), extract the cert DER and the private key DER.
  - Build `rustls::ServerConfig::builder().with_no_client_auth().with_single_cert(vec![cert_der], key_der)?`; set `rustls_cfg.alpn_protocols = vec![nosh_proto::ALPN.to_vec()];`.
  - Convert: `quinn::ServerConfig::with_crypto(Arc::new(quinn::crypto::rustls::QuicServerConfig::try_from(rustls_cfg)?))`, then `server_config.transport_config(Arc::new(nosh_proto::transport_config(false)))` (server does not set keep-alive; client does).
  - Return the quinn ServerConfig.
Provide `pub fn make_endpoint(addr: std::net::SocketAddr) -> anyhow::Result<quinn::Endpoint>` building `quinn::Endpoint::server(build_server_config()?, addr)`.
</action>
<acceptance_criteria>
- `crates/nosh-server/src/server.rs` contains `pub fn build_server_config` and `pub fn make_endpoint`
- The rustls server config sets `alpn_protocols` to `nosh_proto::ALPN.to_vec()`
- Server config uses `QuicServerConfig::try_from` and attaches `nosh_proto::transport_config`
- `cargo build -p nosh-server` exits 0
</acceptance_criteria>
</task>

<task id="2" type="execute">
<title>server.rs: accept loop + per-connection echo handlers</title>
<read_first>
- crates/nosh-server/src/server.rs (config/endpoint from task 1)
- .planning/phases/01-quic-transport-skeleton/01-RESEARCH.md (streams + datagrams coexist)
- .planning/research/PITFALLS.md (Pitfall 2 max_datagram_size before send)
</read_first>
<action>
In `server.rs` add `pub async fn run_accept_loop(endpoint: quinn::Endpoint) -> anyhow::Result<()>`: loop on `endpoint.accept()`, and for each incoming connection `tokio::spawn(handle_connection(conn))`.
Add `async fn handle_connection(incoming: quinn::Incoming) -> anyhow::Result<()>`: `let conn = incoming.await?;` then log (`tracing::info!`) the peer addr and the negotiated ALPN (read `conn.handshake_data()` downcast to `quinn::crypto::rustls::HandshakeData`, log `.protocol`). Then run two concurrent pumps with `tokio::join!` / `tokio::select!`:
  - Stream echo: loop `conn.accept_bi().await` → for each `(mut send, mut recv)` spawn a task that reads to end (`recv.read_to_end(limit).await`) and writes the same bytes back (`send.write_all(&buf).await; send.finish()`), echoing intact (TRANS-02).
  - Datagram echo: loop `conn.read_datagram().await` → echo the received `Bytes` straight back with `conn.send_datagram(bytes)` after checking `conn.max_datagram_size()` is `Some` and the payload fits (Pitfall 2). Log each datagram echoed.
Handle `ConnectionError::ApplicationClosed`/`LocallyClosed`/`TimedOut` as clean loop exits (not errors). Keep the function resilient: a single stream/datagram error logs and continues.
</action>
<acceptance_criteria>
- `server.rs` contains `pub async fn run_accept_loop` and a connection handler that calls both `accept_bi` and `read_datagram`
- Datagram echo path calls `conn.max_datagram_size()` before `send_datagram` (Pitfall 2)
- Stream echo reads the incoming stream and writes the same bytes back via a `SendStream`
- `cargo build -p nosh-server` exits 0
</acceptance_criteria>
</task>

<task id="3" type="execute">
<title>main.rs: CLI (--addr/--port default 4433), tracing init, run</title>
<read_first>
- crates/nosh-server/src/server.rs
- .planning/phases/01-quic-transport-skeleton/01-CONTEXT.md (D-06 port default)
</read_first>
<action>
Replace `crates/nosh-server/src/main.rs` stub with the real entrypoint. Add `mod server;`. Define a clap derive `Args` struct with `--addr` (default `127.0.0.1`, type `std::net::IpAddr`) and `--port` (default `4433`, `u16`). In `#[tokio::main] async fn main() -> anyhow::Result<()>`: init `tracing_subscriber` (env-filter, default info); parse args; build `SocketAddr` from addr+port; `tracing::info!` "nosh-server listening on {addr} (ALPN nosh/0)" plus a note that UDP/443 is the production target and 4433 is the unprivileged dev default; build the endpoint via `server::make_endpoint(addr)?` and run `server::run_accept_loop(endpoint).await`.
</action>
<acceptance_criteria>
- `crates/nosh-server/src/main.rs` contains a clap struct with `port` defaulting to `4433` and `addr` defaulting to a loopback IP
- `main` initializes `tracing_subscriber` and calls `server::run_accept_loop`
- `cargo build -p nosh-server` exits 0
- `cargo run -p nosh-server -- --help` exits 0 and shows `--addr` and `--port`
</acceptance_criteria>
</task>

## Verification
- `cargo build -p nosh-server` succeeds.
- `cargo run -p nosh-server -- --help` shows `--addr` and `--port` (default 4433).
- `server.rs` exposes `build_server_config`, `make_endpoint`, and `run_accept_loop` for reuse by Plan 04 tests.
