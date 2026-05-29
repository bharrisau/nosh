---
phase: 1
phase_name: quic-transport-skeleton
date: 2026-05-29
depth: standard
status: clean
files_reviewed: 13
findings:
  critical: 0
  warning: 0
  info: 1
  total: 1
---

# Code Review — Phase 1: QUIC Transport Skeleton

Reviewed the Rust source for the four Phase 1 crates (`nosh-proto`, `nosh-auth`,
`nosh-server`, `nosh-client`) plus the integration test suite. Review depth:
standard (per-file correctness, security, and quality analysis).

## Files reviewed

- crates/nosh-proto/src/lib.rs
- crates/nosh-proto/src/codec.rs
- crates/nosh-proto/src/messages.rs
- crates/nosh-proto/src/transport.rs
- crates/nosh-auth/src/lib.rs
- crates/nosh-auth/src/verifier.rs
- crates/nosh-server/src/lib.rs
- crates/nosh-server/src/main.rs
- crates/nosh-server/src/server.rs
- crates/nosh-client/src/lib.rs
- crates/nosh-client/src/main.rs
- crates/nosh-client/src/client.rs
- crates/nosh-client/tests/transport.rs

## Result: CLEAN

No critical or warning-level findings. The skeleton is correct on every axis the
phase brief called out.

### Security checks (all pass)

- **TLS verifier is NOT a no-op (PITFALL 5).** `PlaceholderServerVerifier`
  (crates/nosh-auth/src/verifier.rs) implements `verify_tls12_signature` and
  `verify_tls13_signature` by delegating to the rustls `CryptoProvider`'s real
  `verify_tls12_signature`/`verify_tls13_signature` with the provider's
  `signature_verification_algorithms`. Only `verify_server_cert` accepts any cert
  — the intended Phase 2 pinning seam, clearly documented. There is no MITM hole.
- **Datagram size guards present on both sides.** Server `datagram_echo_loop`
  checks `conn.max_datagram_size()` and drops oversized/disabled datagrams
  (PITFALL 2); client `datagram_roundtrip` asserts `max_datagram_size().is_some()`
  and that the payload fits before sending.
- **Codec length-prefix guard.** `read_message` validates `len > MAX_FRAME_LEN`
  (16 MiB) *before* allocating `vec![0u8; len]`, preventing unbounded allocation
  from a hostile/corrupt length prefix. `encode` enforces the same cap.
- **Idle timeout is finite (PITFALL 3).** `transport_config` sets a 300s
  `max_idle_timeout` (never `None`) and 15s client keep-alive.
- **ALPN enforced (PITFALL 4).** `client::connect` asserts the negotiated
  protocol equals `nosh/0` and both endpoints set `alpn_protocols`.

### Correctness / panics

- No panics on the happy path. The only `unwrap()`/`expect()` calls are on
  compile-time-constant inputs (`"0.0.0.0:0".parse()`, a 300s `Duration`
  conversion) that cannot fail at runtime.
- `clean_exit` correctly maps orderly QUIC teardown variants
  (`ApplicationClosed`/`LocallyClosed`/`ConnectionClosed`/`TimedOut`) to `Ok(())`
  so the accept/echo loops exit cleanly rather than logging spurious errors.
- Integration tests cover all five TRANS criteria, including an `#[ignore]`d
  honest 60s idle-survival proof alongside a fast proxy.

### Info-level (non-blocking)

- **CR-01 (info):** `server::stream_echo_loop` bounds the echo read with
  `read_to_end(nosh_proto::codec::MAX_FRAME_LEN)` (16 MiB), reusing the codec's
  *frame* constant for a raw byte echo that is not actually a framed `Message`.
  It is correctly bounded and harmless, but the constant is semantically a frame
  cap, not a stream-echo cap; the client side uses a separate 64 KiB `READ_LIMIT`.
  Consider a dedicated echo-limit constant when the real shell I/O path lands in
  a later phase. No action required for Phase 1.

## Build / test / clippy

- `cargo build --workspace --all-targets`: pass
- `cargo test --workspace`: pass (3 unit + 5 integration, 1 ignored slow test)
- `cargo clippy --workspace --all-targets`: pass (no warnings)
