---
phase: 24-webtransport-endpoint-mode-a
plan: "03"
subsystem: nosh-server
tags: [webtransport, transport, auth, dos-hardening]
dependency_graph:
  requires: ["24-01", "23-02"]
  provides: ["wt_transport.rs NoshTransport impl", "run_wt_accept_loop", "handle_connection_wt", "--mode webtransport dispatch"]
  affects: ["nosh-server main.rs", "nosh-server lib.rs", "nosh-server server.rs"]
tech_stack:
  added: ["wtransport::Connection wrapper", "wtransport::SendStream/RecvStream wrappers", "rustls PemObject PEM loading via pki_types"]
  patterns: ["async finish() adapter", "Option<RecvStream> consuming-stop adapter", "double-await open_bi()", "cfg(feature=webtransport) gating", "pre-auth semaphore cap + auth timeout", "cfg(any(test, feature=test-support)) auth stub"]
key_files:
  created:
    - crates/nosh-server/src/wt_transport.rs
  modified:
    - crates/nosh-server/src/lib.rs
    - crates/nosh-server/src/server.rs
    - crates/nosh-server/src/main.rs
decisions:
  - "D-03 enforced: max_datagram_size() from wtransport::Connection directly (capsule overhead already subtracted) — never via quic_connection()"
  - "D-05 enforced: outer TLS ServerConfig uses with_no_client_auth(); inner SSH-key auth is Phase 25"
  - "PEM loading via rustls::pki_types::PemObject (from_pem_slice) rather than rustls-pemfile — rustls-pemfile is not in the dep tree, pki_types alloc feature suffices"
  - "Datagram payload via Datagram::payload() → Bytes (zero-copy slice into existing buffer)"
  - "wtransport accept() returns IncomingSession (not Option) — loop{} not while-let-Some()"
  - "handle_connection_wt placed in wt_transport.rs with pub(crate) visibility on CLOSE_PROTOCOL, SessionOpenParams, run_session, run_reattach_session in server.rs"
  - "Synthetic test identity uses NoshPublicKey::from_raw([0u8; 32]) — no test_support::test_identity() exists in nosh-auth"
metrics:
  duration: "~25 minutes"
  completed: "2026-06-13"
  tasks_completed: 3
  files_changed: 4
---

# Phase 24 Plan 03: SERVER WebTransport endpoint Summary

WtransportTransport + accept loop + outer-TLS config + --mode dispatch over wtransport 0.7.1, adapting 6 API diffs from the quinn wrapper.

## Tasks Completed

| Task | Name | Commit | Files |
|------|------|--------|-------|
| 1 | WtransportTransport wrapper + outer-TLS config | 10869bf | wt_transport.rs (new), lib.rs, server.rs |
| 2 | WT accept loop + auth stub (in same file as Task 1) | 10869bf | wt_transport.rs |
| 3 | --mode / --cert / --key CLI flags and dispatch | c7d00f6 | main.rs |

## What Was Built

`crates/nosh-server/src/wt_transport.rs` (462 lines, gated behind `#[cfg(feature = "webtransport")]`) provides:

- `WtransportTransport(wtransport::Connection)` implementing `NoshTransport`
- `WtransportSendStream(wtransport::SendStream)` implementing `NoshSendStream`
- `WtransportRecvStream(Option<wtransport::RecvStream>)` implementing `NoshRecvStream`
- `build_ca_cert_rustls_config(cert_path, key_path)` — loads operator PEM cert/key, builds outer TLS `rustls::ServerConfig` with `with_no_client_auth()` and `h3` ALPN (D-05)
- `build_wt_server_config(bind_addr, rustls_cfg)` — wraps in `wtransport::ServerConfig` via `.with_custom_tls()`
- `make_wt_endpoint(addr, cert_path, key_path)` — builds `wtransport::Endpoint<Server>` with helpful bind-failure error on privileged ports
- `run_wt_accept_loop(endpoint, registry, limits, shell)` — infinite accept loop with semaphore DoS cap + auth timeout (parity with `run_accept_loop`)
- `handle_connection_wt(transport, registry, shell)` — inner-auth gate, then `accept_bi` + first-frame dispatch to `run_session`/`run_reattach_session`

`crates/nosh-server/src/main.rs` gains:
- `TransportMode { Native, Webtransport }` `ValueEnum`
- `--mode <MODE>` (default `native`), `--cert <PATH>`, `--key <PATH>` flags
- Dispatch: `Native` → existing quinn path; `Webtransport` → `make_wt_endpoint` + `run_wt_accept_loop` (cfg-gated; non-WT build gives a clear error message)

## Six API Diffs Applied (vs quinn wrapper)

1. `send_datagram`: 3-variant map (no `Disabled`); `NotConnected` → `ConnectionLost`
2. `datagram_send_buffer_space`: via `quic_connection()` (not on `wtransport::Connection`)
3. `max_datagram_size`: directly from `wtransport::Connection::max_datagram_size()` — capsule overhead already subtracted (D-03)
4. `read_datagram`: `receive_datagram().await?` then `dg.payload()` for zero-copy `Bytes`
5. `open_bi`: double-await `self.0.open_bi().await?.await?`
6. `finish()`: body calls `.finish().await` — ASYNC (opposite of quinn wrapper's sync)

Plus adapter: `WtransportRecvStream(Option<RecvStream>)` with `.take()` to adapt consuming `stop(self)` to `&mut self`.

## Security Properties

- T-24-03-D (DoS): `run_wt_accept_loop` replicates the `Semaphore(max_concurrent)` cap and `auth_timeout` from `run_accept_loop`; over-cap connections dropped (no `.refuse()` on WT IncomingSession)
- T-24-03-E (auth bypass): auth stub gated `#[cfg(any(test, feature = "test-support"))]`; release builds reject inner-auth-less connections via `transport.close(1, b"inner-auth-not-implemented")` before any session work
- T-24-03-S (downgrade): `--mode webtransport` dispatch arm only calls `make_wt_endpoint` — no quinn endpoint is ever created; raw-QUIC clients fail at HTTP/3 CONNECT upgrade before reaching application code (WT-05)
- T-24-03-I (outer TLS): `with_no_client_auth()` is correct — inner SSH-key auth (Phase 25) is the authoritative mutual auth layer (D-05)

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 1 - Bug] wtransport::Endpoint::accept() returns IncomingSession directly**
- Found during: Task 2 implementation
- Issue: Research/patterns showed `while let Some(incoming) = endpoint.accept().await` but the actual API signature is `pub async fn accept(&self) -> IncomingSession` (not `Option<IncomingSession>`) — it panics if the endpoint closes
- Fix: Changed to `loop { let incoming = endpoint.accept().await; ... }`
- Files: wt_transport.rs
- Commit: 10869bf

**2. [Rule 2 - API discovery] PEM loading via PemObject trait instead of rustls-pemfile**
- Found during: Task 1 implementation
- Issue: `rustls-pemfile` is not a transitive dep; RESEARCH noted it as "likely available" but it was not. The `rustls-pki-types` 1.14.1 crate (transitive via rustls 0.23) provides `PemObject::from_pem_slice` in the `alloc` feature (no `std` needed). Used `std::fs::read` + `CertificateDer::pem_slice_iter` / `PrivateKeyDer::from_pem_slice` instead.
- Fix: No new dep added; used existing rustls pki_types APIs
- Files: wt_transport.rs

**3. [Rule 1 - Bug] nosh_auth::test_support::test_identity() does not exist**
- Found during: Task 2 implementation
- Issue: The plan called for `nosh_auth::test_support::test_identity()` but no such function exists; `test_support` module has only `EphemeralAgent`
- Fix: Used `nosh_auth::NoshPublicKey::from_raw([0u8; 32])` for the synthetic test identity
- Files: wt_transport.rs
- Commit: 10869bf

**4. [Rule 2 - visibility] server.rs private items needed pub(crate)**
- Found during: Task 2 (handle_connection_wt needs access to server internals)
- Issue: `CLOSE_PROTOCOL`, `SessionOpenParams` (and its fields), `run_session`, `run_reattach_session` were all private in server.rs; wt_transport.rs needed to call them
- Fix: Made them `pub(crate)` in server.rs — no semantic change, same compilation unit
- Files: server.rs
- Commit: 10869bf

## Known Stubs

- `handle_connection_wt` inner-auth bypass: in `test-support` builds, the real inner SSH-key handshake is replaced by a synthetic `NoshPublicKey::from_raw([0u8; 32])`. This is intentional — Phase 25 fills in the real inner auth. The stub is correctly gated with `#[cfg(any(test, feature = "test-support"))]` and release builds reject connections before reaching any session code.

## Threat Flags

None — all network endpoints and auth paths are covered by the plan's threat model. No new network surface beyond what the plan defines.

## Self-Check: PASSED

Files created/modified:
- crates/nosh-server/src/wt_transport.rs: FOUND
- crates/nosh-server/src/lib.rs: FOUND
- crates/nosh-server/src/server.rs: FOUND
- crates/nosh-server/src/main.rs: FOUND

Commits:
- 10869bf: FOUND
- c7d00f6: FOUND

Build checks:
- `cargo build -p nosh-server`: PASS (default features, no webtransport)
- `cargo build -p nosh-server --features webtransport`: PASS
- `cargo build -p nosh-server --features "webtransport test-support"`: PASS
- `cargo test --workspace`: PASS
