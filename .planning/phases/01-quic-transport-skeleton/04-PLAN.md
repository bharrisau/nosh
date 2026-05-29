---
plan_id: "04"
phase: 1
wave: 3
title: "Integration tests (handshake/echo/datagram/coexist/60s-idle) + runnable demo wiring + README"
depends_on: ["02", "03"]
files_modified:
  - crates/nosh-server/src/lib.rs
  - crates/nosh-server/src/main.rs
  - crates/nosh-server/Cargo.toml
  - crates/nosh-client/src/lib.rs
  - crates/nosh-client/src/main.rs
  - crates/nosh-client/Cargo.toml
  - tests/transport.rs
  - Cargo.toml
  - README.md
autonomous: true
requirements: [TRANS-01, TRANS-02, TRANS-03, TRANS-04, TRANS-05]
must_haves:
  truths:
    - "D-08: automated integration tests are the verification gate (handshake+ALPN, stream echo, datagram round-trip, concurrent coexistence, 60s idle survival)"
    - "The 60s idle-survival test exists and is runnable (may be #[ignore] for CI speed); a fast idle-survival proxy test runs in the default suite"
    - "D-08: a runnable two-binary demo (nosh-server + nosh-client) is documented and works"
    - "Tests bind to 127.0.0.1:0 (ephemeral port) to avoid CI port collisions"
  artifacts:
    - "tests/transport.rs (or per-crate tests) exercising all five TRANS criteria in-process"
    - "README.md documents the two-binary demo run command"
---

# Plan 04 — Integration tests + demo + README

## Objective
Prove Phase 1 with both halves of D-08: (a) automated integration tests covering all five TRANS criteria as the verification gate, and (b) a documented, runnable two-binary demo. Expose the server/client connection logic as library targets so tests can drive them in-process.

## Context
- Depends on Plans 02 (server.rs) and 03 (client.rs).
- The binary crates currently expose their logic only through `mod` in main.rs; this plan adds `lib.rs` to each so an integration test crate can import `build_server_config`/`run_accept_loop` and the client helpers.

<task id="1" type="execute">
<title>Expose server + client logic as library targets</title>
<read_first>
- crates/nosh-server/src/main.rs
- crates/nosh-server/src/server.rs
- crates/nosh-client/src/main.rs
- crates/nosh-client/src/client.rs
- crates/nosh-server/Cargo.toml
- crates/nosh-client/Cargo.toml
</read_first>
<action>
For `nosh-server`: create `crates/nosh-server/src/lib.rs` with `pub mod server;` (and re-export the key fns: `pub use server::{build_server_config, make_endpoint, run_accept_loop};`). Update `crates/nosh-server/Cargo.toml` to declare both a `[lib]` (name `nosh_server`) and the existing `[[bin]]` (name `nosh-server`, path `src/main.rs`). Change `main.rs` to use the crate's library (`use nosh_server::server;` instead of `mod server;`).
For `nosh-client`: mirror this — create `crates/nosh-client/src/lib.rs` with `pub mod client;` and `pub use client::{build_client_config, make_endpoint, connect, stream_echo_roundtrip, datagram_roundtrip, concurrent_roundtrip};`; add a `[lib]` (name `nosh_client`) and keep `[[bin]]`; update `main.rs` to `use nosh_client::client;`.
Verify both crates still build as binaries and now also as libs.
</action>
<acceptance_criteria>
- `crates/nosh-server/src/lib.rs` and `crates/nosh-client/src/lib.rs` exist and re-export the connection helpers
- Both `Cargo.toml` files declare a `[lib]` and a `[[bin]]` target
- `cargo build` (whole workspace) exits 0 and `cargo build --bins` exits 0
</acceptance_criteria>
</task>

<task id="2" type="execute">
<title>Integration tests covering all five TRANS criteria</title>
<read_first>
- crates/nosh-server/src/lib.rs
- crates/nosh-client/src/lib.rs
- .planning/phases/01-quic-transport-skeleton/01-RESEARCH.md (Proof approach)
- .planning/REQUIREMENTS.md (TRANS-01..05 acceptance)
</read_first>
<action>
Add a workspace integration test. Easiest target: a dedicated test crate member, OR tests under `crates/nosh-client/tests/` depending on both `nosh-server` and `nosh-client`. Use a `nosh-client/tests/transport.rs` integration test (add `nosh-server = { path = "../nosh-server" }` as a `[dev-dependencies]` of nosh-client, plus `tokio` with `macros`+`rt-multi-thread`+`time`, `bytes`, `nosh-proto`). If a separate top-level `tests/` crate is cleaner, add it as a workspace member instead and document in README — pick one and keep it consistent.
Write these `#[tokio::test]` cases, each spawning an in-process server on `127.0.0.1:0` (read back the bound port via `endpoint.local_addr()`), spawning `run_accept_loop` as a background task, then connecting a client:
  1. `handshake_and_alpn`: connect succeeds and `connect()` already asserts ALPN == nosh/0 (TRANS-01).
  2. `stream_echo_intact`: `stream_echo_roundtrip(conn, b"the quick brown fox")` returns the identical bytes (TRANS-02).
  3. `datagram_roundtrip_enabled`: `conn.max_datagram_size().is_some()` AND a sent datagram echoes back identical; also assert the server side had datagrams enabled by the successful round-trip (TRANS-03).
  4. `stream_and_datagram_coexist`: `concurrent_roundtrip(conn)` completes with both payloads intact (TRANS-04).
  5. `idle_survival_fast`: after connecting, sleep ~3s (longer than nothing but fast) and assert `conn.close_reason().is_none()` and a subsequent stream echo still works — a fast proxy that the keep-alive/idle config keeps a quiet connection alive (default suite).
  6. `idle_survival_60s` marked `#[ignore]`: sleep 60s with no traffic, then assert `conn.close_reason().is_none()` and a stream echo still round-trips — the honest TRANS-05 proof, runnable via `cargo test -- --ignored`.
Use generous read limits (e.g. 64 KiB) for `read_to_end`. Ensure the server task is aborted/dropped at test end.
</action>
<acceptance_criteria>
- An integration test file exists with tests named for handshake/ALPN, stream echo, datagram round-trip, coexistence, and idle survival
- A 60s idle test exists and is annotated `#[ignore]`; a fast idle-survival test runs in the default suite
- Tests bind the server to `127.0.0.1:0` and discover the port via `local_addr()`
- `cargo test --workspace` exits 0 (default suite, excluding the ignored 60s test)
- `cargo test --workspace -- --ignored` runs the 60s test and exits 0
</acceptance_criteria>
</task>

<task id="3" type="execute">
<title>README documenting the runnable two-binary demo</title>
<read_first>
- crates/nosh-server/src/main.rs
- crates/nosh-client/src/main.rs
- .planning/phases/01-quic-transport-skeleton/01-SKELETON.md (capability + run command)
</read_first>
<action>
Create `README.md` at repo root with: a one-paragraph description of nosh (from PROJECT.md), a "Phase 1: QUIC transport skeleton" section, a "Run the demo" subsection with the exact commands — terminal 1: `cargo run -p nosh-server` (note default 127.0.0.1:4433, UDP/443 is the production target), terminal 2: `cargo run -p nosh-client` — and a note on what to expect in the tracing output (server logs accept + ALPN + echoed datagrams; client logs ALPN verified, stream echo matched, datagram round-trip matched, concurrent ok, survived idle). Add a "Run the tests" subsection: `cargo test --workspace` (fast suite) and `cargo test --workspace -- --ignored` (includes the 60s idle-survival test). Keep it concise.
</action>
<acceptance_criteria>
- `README.md` exists at repo root and contains `cargo run -p nosh-server` and `cargo run -p nosh-client`
- `README.md` documents both `cargo test --workspace` and the `-- --ignored` 60s test command
</acceptance_criteria>
</task>

## Verification
- `cargo build --workspace` and `cargo build --bins` succeed.
- `cargo test --workspace` passes (all TRANS criteria except the 60s ignored test).
- `cargo test --workspace -- --ignored` passes the 60s idle-survival test.
- Manual demo: `cargo run -p nosh-server` then `cargo run -p nosh-client` completes the round-trips and the client exits 0.
