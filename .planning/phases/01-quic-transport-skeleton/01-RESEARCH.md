# Phase 1: QUIC Transport Skeleton — Research

**Researched:** 2026-05-29
**Phase:** 01-quic-transport-skeleton
**Requirements:** TRANS-01, TRANS-02, TRANS-03, TRANS-04, TRANS-05

> Synthesizes the verified stack/pitfall research already in `.planning/research/{STACK,ARCHITECTURE,PITFALLS}.md` into a phase-focused implementation guide. All crate versions/APIs below are marked HIGH confidence in STACK.md (verified against docs.rs at research time).

## Question this answers
"What do I need to know to PLAN Phase 1 well — establish a single QUIC connection (quinn + rustls, TLS 1.3, shared ALPN), echo over a reliable bidi stream, round-trip a datagram, prove stream+datagram coexistence, and survive 60s idle?"

## Verified Stack (pins for this phase)

| Crate | Version | Role in Phase 1 |
|-------|---------|-----------------|
| `quinn` | 0.11.9 (`runtime-tokio`, `rustls-ring`) | QUIC endpoint, streams, datagrams |
| `rustls` | 0.23.x (`ring` provider) | TLS 1.3 under quinn |
| `rcgen` | 0.14.x | ephemeral self-signed cert (dev placeholder) |
| `tokio` | 1.x (`rt-multi-thread`, `macros`, `net`, `time`, `io-util`) | async runtime |
| `postcard` | 1.x (`alloc`) | `Message` codec (serde binary) |
| `serde` | 1.x (`derive`) | derive for `Message` |
| `bytes` | 1.x | datagram payloads (quinn transitive) |
| `tracing` + `tracing-subscriber` | 0.1 / 0.3 | structured logging |
| `clap` | 4.x (`derive`) | CLI `--addr`/`--port` |
| `anyhow` | 1.x | binary error handling |
| `thiserror` | 1.x | proto codec error type |

## Critical wiring patterns (from STACK.md, HIGH confidence)

**quinn↔rustls conversion path (mandatory order):**
- Server: `rustls::ServerConfig::builder().with_no_client_auth().with_single_cert(certs, key)` → `quinn::crypto::rustls::QuicServerConfig::try_from(rustls_cfg)?` → `quinn::ServerConfig::with_crypto(Arc::new(..))` → attach `transport_config(Arc::new(transport))`.
- Client: `rustls::ClientConfig::builder().dangerous().with_custom_certificate_verifier(Arc::new(verifier)).with_no_client_auth()` → `quinn::crypto::rustls::QuicClientConfig::try_from(rustls_cfg)?` → `quinn::ClientConfig::new(Arc::new(..))` → `.transport_config(Arc::new(transport))`.
- rustls requires a process-wide default `CryptoProvider`; install `rustls::crypto::ring::default_provider().install_default()` once at startup (or build configs with `ClientConfig::builder_with_provider`).

**ALPN (Pitfall 4 — QUIC mandates it; mismatch = error 0x178):**
- Single constant in `nosh-proto`: `pub const ALPN: &[u8] = b"nosh/0";`
- Set on BOTH sides: `rustls_cfg.alpn_protocols = vec![ALPN.to_vec()];`
- Assert post-handshake: `connection.handshake_data()` → downcast to `quinn::crypto::rustls::HandshakeData`, check `.protocol == Some(ALPN.to_vec())`.

**Datagrams (Pitfall 1 — `datagram_receive_buffer_size` defaults to `None` = silently disabled):**
- On BOTH endpoints' `TransportConfig`: `transport.datagram_receive_buffer_size(Some(1 << 20));` (1 MiB) and `transport.datagram_send_buffer_size(1 << 20);`
- Verify enabled: `connection.max_datagram_size()` returns `Some(_)` on both sides — this is the explicit TRANS-03 proof.
- Send: `conn.send_datagram(Bytes)` (non-blocking) or `send_datagram_wait(..).await`. Receive: `conn.read_datagram().await`.
- Pitfall 2: clamp payload to `max_datagram_size()`; loopback tests use tiny payloads so safe, but check anyway.

**Idle survival (Pitfall 3 — default `max_idle_timeout` 30s, `keep_alive_interval` None):**
- Client transport: `transport.keep_alive_interval(Some(Duration::from_secs(15)));` (one side suffices per quinn docs).
- Both transports: `transport.max_idle_timeout(Some(Duration::from_secs(300).try_into()?));` (never `None` — hung-future risk).
- Keep-alive 15s comfortably below 300s idle and below the bare 30s default → satisfies "60s idle does not drop" (TRANS-05).

**Streams + datagrams coexist (TRANS-04):** same `quinn::Connection` multiplexes both; `conn.open_bi()` for the reliable stream, `send_datagram`/`read_datagram` for datagrams — no extra config beyond the buffers above.

## TLS verifier seam (D-07 — keep the skeleton honest)
- Phase 1 ships a **placeholder client-side `ServerCertVerifier`** named to flag it temporary (e.g. `PlaceholderServerVerifier` / dev verifier), living in `nosh-auth` so Phase 2 swaps it in place.
- It MUST delegate real signature verification: implement `verify_tls13_signature` / `verify_tls12_signature` by calling `rustls::crypto::verify_tls13_signature(message, cert, dss, &provider.signature_verification_algorithms)` (and the tls12 equivalent), NOT a stubbed `Ok(assertion())` (Pitfall 5 — MITM hole). `verify_server_cert` may accept any cert this phase (no pinning yet — that is Phase 2), but signature/scheme machinery is real.
- `supported_verify_schemes()` delegates to the provider's `signature_verification_algorithms.supported_schemes()`.
- Server uses `with_no_client_auth()` this phase (client auth is Phase 2).

## Message codec (D-02..D-05)
- `nosh-proto` defines `pub enum Message { SessionClose { exit_code: i32, reason: String }, /* room for future control frames */ }` deriving `Serialize, Deserialize`.
- One codec module: postcard `to_allocvec` / `from_bytes`, wrapped in length-delimited framing — `u32` big-endian length prefix + postcard body. `encode(&Message) -> Vec<u8>` and `decode(&[u8]) -> Result<Message, ProtoError>`, plus async stream read/write helpers usable over quinn `SendStream`/`RecvStream`.
- Format isolated to this one module so postcard→prost (D-04) is a one-file swap.
- For the echo proof, the bidi stream may send raw bytes OR a `Message`; the round-trip test should exercise the codec at least once (encode a `SessionClose`, send, decode, assert equality) to prove the codec works end to end.

## Workspace layout (D-01 — `crates/` subdir per ARCHITECTURE.md)
```
nosh/
├── Cargo.toml                 # [workspace] members + shared [workspace.dependencies]
├── crates/
│   ├── nosh-proto/  (lib)     # ALPN const, Message enum, codec module
│   ├── nosh-auth/   (lib)     # PlaceholderServerVerifier (honest seam); Phase-2 stub otherwise
│   ├── nosh-server/ (bin)     # endpoint setup, accept loop, echo + datagram echo handler
│   └── nosh-client/ (bin)     # connect, stream echo round-trip, datagram round-trip, idle hold
└── tests or per-crate integration tests
```
- Shared `[workspace.dependencies]` so versions are declared once.
- Common transport-config builder (datagram buffers, idle timeout) can live in `nosh-proto` or a small shared helper so client and server stay consistent.

## Proof approach (D-08 — both tests AND demo)
- **Integration tests** (gate): in-process client+server on `127.0.0.1:0` (ephemeral port) exercising: (1) handshake + ALPN assertion, (2) stream echo round-trip intact, (3) datagram round-trip + `max_datagram_size().is_some()` both sides, (4) concurrent stream-echo + datagram without interference, (5) 60s idle survival. The 60s test is real-time and slow — mark it `#[ignore]` by default with a fast keep-alive variant (e.g. assert connection alive after an interval exceeding the 30s default but using a shortened timeout/keepalive config) so CI stays fast while the honest 60s test is runnable on demand. Provide BOTH: a fast proxy test in the default suite and the literal 60s `#[ignore]`d test.
- **Demo:** `nosh-server` binds `--addr`/`--port` (default 4433); `nosh-client` connects, runs the echo + datagram round-trip, prints results, holds idle briefly. A human runs the two binaries and eyeballs `tracing` output.

## Pitfall checklist (must all be handled)
1. Datagram buffer `Some(_)` on both sides — else datagrams silently dead.
2. ALPN constant identical both sides, asserted post-handshake.
3. `keep_alive_interval` set (client) + finite `max_idle_timeout` — 60s idle survives.
4. Verifier delegates `verify_tls13_signature` to the CryptoProvider — no all-stub verifier.
5. Install default rustls CryptoProvider once before building configs.
6. `max_datagram_size()` checked before send (clamp/skip if exceeded).
7. Bind tests to port 0 to avoid CI port collisions; default binary port 4433 (unprivileged).

## Validation Architecture
Not applicable — `workflow.nyquist_validation` is disabled for this project. Proof is the integration-test gate + demo per D-08.

## Sources
All patterns/versions verified in `.planning/research/STACK.md` (HIGH confidence) and `.planning/research/PITFALLS.md` (Pitfalls 1–5). quinn 0.11 docs, rustls 0.23 docs, rcgen 0.14 docs as cited there.
