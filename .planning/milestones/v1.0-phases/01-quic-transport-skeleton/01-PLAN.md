---
plan_id: "01"
phase: 1
wave: 1
title: "Workspace scaffold + nosh-proto codec + nosh-auth placeholder verifier + shared transport config"
depends_on: []
files_modified:
  - Cargo.toml
  - crates/nosh-proto/Cargo.toml
  - crates/nosh-proto/src/lib.rs
  - crates/nosh-proto/src/messages.rs
  - crates/nosh-proto/src/codec.rs
  - crates/nosh-proto/src/transport.rs
  - crates/nosh-auth/Cargo.toml
  - crates/nosh-auth/src/lib.rs
  - crates/nosh-auth/src/verifier.rs
  - crates/nosh-server/Cargo.toml
  - crates/nosh-server/src/main.rs
  - crates/nosh-client/Cargo.toml
  - crates/nosh-client/src/main.rs
  - .gitignore
autonomous: true
requirements: [TRANS-01, TRANS-03, TRANS-05]
must_haves:
  truths:
    - "D-01: Cargo workspace with crates nosh-proto, nosh-auth, nosh-server, nosh-client under crates/"
    - "D-02/D-03: postcard Message codec isolated in a single nosh-proto::codec module"
    - "D-04: the single-module codec isolation makes the documented postcard→prost migration path a one-file swap (no prost work this phase; cap'n proto rejected)"
    - "D-05: Message enum exists with a SessionClose { exit_code, reason } variant"
    - "ALPN constant b\"nosh/0\" defined once in nosh-proto and exported"
    - "Shared transport-config builder sets datagram_receive_buffer_size Some, keep_alive_interval, finite max_idle_timeout"
    - "D-07: placeholder ServerCertVerifier delegates verify_tls13_signature to the CryptoProvider (not stubbed)"
  artifacts:
    - "crates/nosh-proto/src/codec.rs encodes/decodes Message via postcard with u32 length framing"
    - "crates/nosh-auth/src/verifier.rs PlaceholderServerVerifier implementing rustls ServerCertVerifier"
---

# Plan 01 — Foundation: workspace, proto codec, placeholder verifier, transport config

## Objective
Stand up the full multi-crate Cargo workspace (D-01) and the two shared libraries every other plan depends on: `nosh-proto` (ALPN constant, `Message` enum, postcard codec, shared quinn `TransportConfig` builder) and `nosh-auth` (honest placeholder TLS verifier — the Phase 2 seam). No binaries yet.

## Context
- This is Wave 1; Plans 02 (server) and 03 (client) depend on the libraries built here.
- Greenfield repo: no Cargo.toml exists yet.
- Crate versions are pinned in `.planning/research/STACK.md` and `01-RESEARCH.md`.

<task id="1" type="execute">
<title>Create workspace manifest and .gitignore</title>
<read_first>
- INIT.md (project scope)
- .planning/phases/01-quic-transport-skeleton/01-RESEARCH.md (verified stack table + workspace layout)
- .planning/research/ARCHITECTURE.md (Workspace / Crate Layout section)
</read_first>
<action>
Create the root `Cargo.toml` as a virtual workspace (`[workspace]`, `resolver = "2"`, `members = ["crates/nosh-proto", "crates/nosh-auth", "crates/nosh-server", "crates/nosh-client"]`). Add a `[workspace.dependencies]` table declaring shared versions once: quinn = "0.11.9" with features ["runtime-tokio","rustls-ring"], rustls = "0.23" with default features (ring provider), rcgen = "0.14", tokio = "1" with features ["rt-multi-thread","macros","net","time","io-util","sync"], postcard = "1" (features ["alloc"]), serde = "1" (features ["derive"]), bytes = "1", tracing = "0.1", tracing-subscriber = "0.3" (features ["env-filter"]), clap = "4" (features ["derive"]), anyhow = "1", thiserror = "1". Add a `[workspace.package]` with edition = "2021".
Create `.gitignore` containing at least `/target` and `Cargo.lock` left tracked is fine (binary workspace) — add `/target`.
</action>
<acceptance_criteria>
- `Cargo.toml` at repo root contains `[workspace]` and lists all four crates in `members`
- `Cargo.toml` contains a `[workspace.dependencies]` table with `quinn`, `rustls`, `rcgen`, `tokio`, `postcard`, `serde`, `bytes`, `tracing`, `clap`, `anyhow`, `thiserror`
- `.gitignore` contains `/target`
</acceptance_criteria>
</task>

<task id="2" type="execute">
<title>nosh-proto crate: ALPN, Message enum, postcard codec</title>
<read_first>
- Cargo.toml (workspace deps just created)
- .planning/phases/01-quic-transport-skeleton/01-RESEARCH.md (Message codec + ALPN sections)
- .planning/research/PITFALLS.md (Pitfall 4 ALPN)
</read_first>
<action>
Create `crates/nosh-proto/Cargo.toml` (package name `nosh-proto`, edition from workspace) depending on `serde`, `postcard`, `bytes`, `thiserror`, `quinn`, `tokio` via `workspace = true`.
Create `crates/nosh-proto/src/lib.rs` exporting modules `messages`, `codec`, `transport`, and the constant `pub const ALPN: &[u8] = b"nosh/0";` with a doc comment noting it is the single canonical ALPN identifier set on both client and server.
Create `crates/nosh-proto/src/messages.rs` defining `#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)] pub enum Message { SessionClose { exit_code: i32, reason: String } }` with a doc comment stating future control frames (resize, etc.) plug in here (D-05).
Create `crates/nosh-proto/src/codec.rs` as the single isolated codec module (D-03): a `ProtoError` thiserror enum (variants for postcard serialize/deserialize, frame too large, io), `pub fn encode(msg: &Message) -> Result<Vec<u8>, ProtoError>` (postcard `to_allocvec` body, prefixed with the body length as a `u32` big-endian → returns the full framed bytes), `pub fn decode(frame: &[u8]) -> Result<Message, ProtoError>` (postcard `from_bytes` on the body), and async helpers `pub async fn write_message<W: tokio::io::AsyncWrite + Unpin>(w: &mut W, msg: &Message) -> Result<(), ProtoError>` and `pub async fn read_message<R: tokio::io::AsyncRead + Unpin>(r: &mut R) -> Result<Message, ProtoError>` that read the u32 BE length prefix then the body. Cap accepted frame length (e.g. 16 MiB) returning the too-large error. Add a `#[cfg(test)]` unit test that round-trips a `SessionClose { exit_code: 42, reason: "bye".into() }` through encode→decode and asserts equality.
</action>
<acceptance_criteria>
- `crates/nosh-proto/src/lib.rs` contains `pub const ALPN: &[u8] = b"nosh/0";`
- `crates/nosh-proto/src/messages.rs` contains `enum Message` with a `SessionClose` variant having `exit_code` and `reason` fields
- `crates/nosh-proto/src/codec.rs` contains `pub fn encode(` and `pub fn decode(` and uses `postcard`
- The codec round-trip unit test exists and `cargo test -p nosh-proto` exits 0
</acceptance_criteria>
</task>

<task id="3" type="execute">
<title>nosh-proto: shared quinn TransportConfig builder</title>
<read_first>
- crates/nosh-proto/src/lib.rs
- .planning/phases/01-quic-transport-skeleton/01-RESEARCH.md (Datagrams + Idle survival sections)
- .planning/research/PITFALLS.md (Pitfall 1 datagram buffer, Pitfall 3 idle/keepalive)
</read_first>
<action>
Create `crates/nosh-proto/src/transport.rs` with `pub fn transport_config(enable_keep_alive: bool) -> quinn::TransportConfig`. The builder MUST: set `datagram_receive_buffer_size(Some(1 << 20))` and `datagram_send_buffer_size(1 << 20)` (Pitfall 1 — datagrams silently disabled otherwise); set `max_idle_timeout(Some(Duration::from_secs(300).try_into().expect("valid idle timeout")))` (finite, never None — Pitfall 3); and when `enable_keep_alive` is true set `keep_alive_interval(Some(Duration::from_secs(15)))` (client sets keep-alive; 15s is comfortably below both the 30s default and the 300s idle timeout, satisfying TRANS-05's 60s-idle requirement). Document each setting with the requirement it satisfies. Re-export from `lib.rs`.
</action>
<acceptance_criteria>
- `crates/nosh-proto/src/transport.rs` contains `datagram_receive_buffer_size(Some(`
- `transport.rs` sets a finite `max_idle_timeout` (Some, not None) and a `keep_alive_interval` under the keep-alive branch
- `pub fn transport_config` is re-exported and `cargo build -p nosh-proto` exits 0
</acceptance_criteria>
</task>

<task id="4" type="execute">
<title>nosh-auth crate: honest placeholder ServerCertVerifier (Phase 2 seam)</title>
<read_first>
- .planning/phases/01-quic-transport-skeleton/01-RESEARCH.md (TLS verifier seam section)
- .planning/research/PITFALLS.md (Pitfall 5 — do NOT no-op verify_tls13_signature)
- crates/nosh-proto/Cargo.toml (dependency style)
</read_first>
<action>
Create `crates/nosh-auth/Cargo.toml` (package `nosh-auth`) depending on `rustls` (workspace = true). Create `crates/nosh-auth/src/lib.rs` exporting a `verifier` module and a module-level doc comment: "Phase 1 ships only a placeholder server verifier. Phase 2 fills this crate with real SSH-key cert-pinning verifiers + ssh-agent signing."
Create `crates/nosh-auth/src/verifier.rs` defining `#[derive(Debug)] pub struct PlaceholderServerVerifier { provider: std::sync::Arc<rustls::crypto::CryptoProvider> }` with `pub fn new(provider: Arc<CryptoProvider>) -> Self`. Implement `rustls::client::danger::ServerCertVerifier` for it:
  - `verify_server_cert(...)` returns `Ok(rustls::client::danger::ServerCertVerified::assertion())` — a doc comment MUST state "PLACEHOLDER: accepts any cert; Phase 2 replaces this with SSH-key SPKI pinning. Signature verification below is REAL so Phase 2's swap is minimal."
  - `verify_tls12_signature(...)` delegates to `rustls::crypto::verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)`
  - `verify_tls13_signature(...)` delegates to `rustls::crypto::verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)` — this MUST NOT be a stub returning assertion() (Pitfall 5)
  - `supported_verify_schemes(...)` returns `self.provider.signature_verification_algorithms.supported_schemes()`
Name the struct so its placeholder nature is unmistakable and add a `// TODO(phase-2):` comment at the swap point.
</action>
<acceptance_criteria>
- `crates/nosh-auth/src/verifier.rs` contains `impl rustls::client::danger::ServerCertVerifier for PlaceholderServerVerifier`
- `verify_tls13_signature` body calls `rustls::crypto::verify_tls13_signature(` (delegated, not stubbed)
- `verify_tls12_signature` body calls `rustls::crypto::verify_tls12_signature(`
- A `TODO(phase-2)` comment marks the verify_server_cert seam
- `cargo build -p nosh-auth` exits 0
</acceptance_criteria>
</task>

<task id="5" type="execute">
<title>Create minimal binary crate stubs so the whole workspace builds green after Wave 1</title>
<read_first>
- Cargo.toml (workspace members + deps)
- crates/nosh-proto/Cargo.toml (dependency style)
</read_first>
<action>
Create `crates/nosh-server/Cargo.toml` (package `nosh-server`, a `[[bin]]` named `nosh-server`) and `crates/nosh-client/Cargo.toml` (package `nosh-client`, `[[bin]]` named `nosh-client`). For now each depends on `nosh-proto = { path = "../nosh-proto" }`, `nosh-auth = { path = "../nosh-auth" }`, and workspace `quinn`, `rustls`, `rcgen`, `tokio` (features incl. `rt-multi-thread`,`macros`), `clap`, `anyhow`, `tracing`, `tracing-subscriber`, `bytes`. Create `crates/nosh-server/src/main.rs` and `crates/nosh-client/src/main.rs` as minimal stubs: `fn main() { println!("nosh-server stub"); }` (Plans 02/03 replace these with the real binaries). Keep the dependency list complete now so Plans 02/03 only edit `main.rs`.
</action>
<acceptance_criteria>
- `crates/nosh-server/Cargo.toml` and `crates/nosh-client/Cargo.toml` exist with their `[[bin]]` names
- `crates/nosh-server/src/main.rs` and `crates/nosh-client/src/main.rs` each contain a `fn main(`
- `cargo build` (whole workspace) exits 0
</acceptance_criteria>
</task>

## Verification
- `cargo build` succeeds for the WHOLE workspace (4 crates: 2 libs + 2 stub binaries).
- `cargo test -p nosh-proto` passes (codec round-trip).
- `cargo clippy --workspace` has no errors (warnings acceptable).

## Notes
- Plans 02 and 03 (Wave 2) replace the stub `main.rs` files with the real server/client; they do not change the `Cargo.toml` dependency lists created here.
