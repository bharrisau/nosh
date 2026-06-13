---
phase: 24-webtransport-endpoint-mode-a
plan: "04"
subsystem: nosh-client
tags: [webtransport, transport, client, quic, wtransport]
dependency_graph:
  requires: ["24-01", "24-02"]
  provides: ["connect_wt", "WtransportTransport", "--webtransport flag"]
  affects: ["24-05"]
tech_stack:
  added:
    - wtransport 0.7.1 (webtransport feature gate — already in workspace deps from 24-01/24-03)
  patterns:
    - NoshTransport trait impl for WebTransport connection wrapper
    - Option<RecvStream> adapter for consuming stop() API
    - async finish() adapter (inverse of quinn synchronous finish)
    - Box<dyn NoshTransport> fed into unchanged generic pump
key_files:
  created:
    - crates/nosh-client/src/wt_transport.rs
  modified:
    - crates/nosh-client/src/lib.rs
    - crates/nosh-client/src/main.rs
decisions:
  - "Datagram::payload() used for zero-copy Bytes (vs copy_from_slice(&dg) via Deref)"
  - "ConnectOptions not re-exported at wtransport crate root; pass url &str directly to endpoint.connect() (IntoConnectOptions via ToString blanket impl)"
  - "build_wt_client_config() returns ClientConfig directly (not Result) — build() is infallible"
  - "WT pump teardown: conn.close() via trait; no endpoint.wait_idle() (no quinn Endpoint in WT mode)"
  - "Non-feature runtime guard: #[cfg(not(feature=webtransport))] block emits clear error if --webtransport passed to a plain build"
metrics:
  duration_secs: 689
  completed: "2026-06-13"
  tasks_completed: 2
  tasks_total: 2
  files_created: 1
  files_modified: 2
---

# Phase 24 Plan 04: Client WebTransport Dialer Summary

Client-side WebTransport dialer: `WtransportTransport` wrapper (impl `NoshTransport`), `build_wt_client_config` / `connect_wt` helpers, and `--webtransport` / `--wt-path` CLI flags feeding the existing generic pump unchanged.

## What was built

**`crates/nosh-client/src/wt_transport.rs`** (292 lines, new, feature-gated)

Three structs implementing the `NoshTransport` / `NoshSendStream` / `NoshRecvStream` trait trifecta, plus two config builders and a connect helper:

- `WtransportTransport` — wraps `wtransport::Connection`, implements all nine `NoshTransport` methods including `rtt()` and `is_closed()` overrides
- `WtransportSendStream` — wraps `wtransport::SendStream`; `finish()` is ASYNC (opposite of quinn wrapper)
- `WtransportRecvStream` — wraps `Option<wtransport::RecvStream>` to adapt the consuming `stop(self)` API via `.take()`
- `build_wt_client_config()` — Mode A: `with_native_certs()`, returns `ClientConfig` (infallible)
- `build_wt_client_config_custom_tls(rustls::ClientConfig)` — test-only: injects a custom verifier for Plan 05 self-signed cert fixture
- `connect_wt(config, url)` — builds Endpoint, dials via `endpoint.connect(url)`, returns `Box<dyn NoshTransport>`

**`crates/nosh-client/src/main.rs`** (modified)

- Added `--webtransport: bool` and `--wt-path: String` (default `/nosh`) to `Args`
- Added `#[cfg(feature = "webtransport")]` WT connect branch at the top of the reconnect loop:
  - Constructs `https://{host}:{port}{wt_path}` URL
  - Calls `build_wt_client_config()` + `connect_wt()` → `Box<dyn NoshTransport>`
  - Passes to the SAME `fresh_session` / `reattach_session` pump as native mode
  - Backoff on dial failure flows through the existing supervisor logic
  - Teardown via `conn.close()` trait method only (no `endpoint.wait_idle()`)
- Added `#[cfg(not(feature = "webtransport"))]` runtime guard: emits a clear error if `--webtransport` is passed to a binary built without the feature
- Native QUIC path is byte-for-byte unchanged

## Six API diffs from QuinnTransport (per RESEARCH.md)

| Diff | Detail |
|------|--------|
| `send_datagram` | 3-variant `SendDatagramError` (no `Disabled`); `NotConnected` → `ConnectionLost` |
| `datagram_send_buffer_space` | No native method; routes through `quic_connection()` (quinn feature) |
| `max_datagram_size` | Same method name, different semantics: capsule overhead already subtracted (D-03) |
| `read_datagram` | `receive_datagram()` returns `Datagram`; `.payload()` extracts zero-copy `Bytes` |
| `finish()` | `wtransport::SendStream::finish()` IS async; wrapper body calls `.await` (OPPOSITE of quinn) |
| `stop()` | `wtransport::RecvStream::stop(self)` is consuming; adapter: `Option<RecvStream>` + `.take()` |

## Deviations from Plan

**1. [Rule 1 - Bug] `ConnectOptions::new` does not exist at crate root**

- **Found during:** Task 1 — `cargo build` reported `cannot find ConnectOptions in wtransport`
- **Issue:** The plan's RESEARCH.md references `wtransport::ConnectOptions::new(url)` but `ConnectOptions` lives at `wtransport::endpoint::ConnectOptions` and is not re-exported at the crate root
- **Fix:** Passed the URL `&str` directly to `endpoint.connect(url)` — `&str: ToString: IntoConnectOptions` via blanket impl, so no intermediate struct is needed
- **Files modified:** `crates/nosh-client/src/wt_transport.rs`
- **Commit:** 3f6e0bb

**2. [Rule 1 - Bug] `ClientConfigBuilder::build()` is infallible**

- **Found during:** Task 1 — `cargo build` reported `?` operator cannot be applied to `ClientConfig`
- **Issue:** `build()` returns `ClientConfig` directly (not `Result<ClientConfig, _>`); the RESEARCH pattern used `?` expecting it to be fallible
- **Fix:** Removed `?` and changed function return type from `anyhow::Result<wtransport::ClientConfig>` to `wtransport::ClientConfig`
- **Files modified:** `crates/nosh-client/src/wt_transport.rs`
- **Commit:** 3f6e0bb (combined with bug above — both found in same build pass)

## Known Stubs

None. All methods are fully implemented. The `build_wt_client_config_custom_tls` function is intentionally provided for Plan 05's integration test harness and is documented as test-only — this is by design, not a stub.

## Threat Flags

No new threat surface introduced beyond what the plan's threat model covers (T-24-04-S: CA cert validation in Mode A; T-24-04-I: async finish; T-24-04-D: backoff supervisor).

## Self-Check

### Files exist

- `crates/nosh-client/src/wt_transport.rs` — FOUND
- `crates/nosh-client/src/lib.rs` (modified) — FOUND
- `crates/nosh-client/src/main.rs` (modified) — FOUND

### Commits exist

- Task 1: `3f6e0bb` — FOUND
- Task 2: `9bec033` — FOUND

### Build verification

- `cargo build -p nosh-client --features webtransport` — PASSED
- `cargo build -p nosh-client` (default) — PASSED
- `cargo test --workspace` — PASSED (all test results ok)
- `--help` shows `--webtransport` and `--wt-path <WT_PATH>` — PASSED

## Self-Check: PASSED
