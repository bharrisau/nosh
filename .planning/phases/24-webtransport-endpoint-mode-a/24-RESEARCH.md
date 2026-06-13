# Phase 24: WebTransport Endpoint + Mode A - Research

**Researched:** 2026-06-13
**Domain:** WebTransport-over-HTTP/3 endpoint (wtransport 0.7.1), NoshTransport wrapper, outer TLS wiring, downgrade protection, test-support auth stub
**Confidence:** HIGH — builder API verified against docs.rs 0.7.1; feature flags verified against crates.io features page; re-ask triggers resolved against live GitHub and crates.io sources

---

## Summary

Phase 24 adds a new `WtransportTransport` (and matching `WtransportSendStream` / `WtransportRecvStream`) that implements the Phase 23 `NoshTransport` trait, wires a `wtransport` listener into `nosh-server`, adds a matching dialer to `nosh-client`, and gates both behind a `webtransport` Cargo feature. A `test-support`-gated auth stub replaces inner SSH-key auth (Phase 25) so the integration test can prove the full shell pump works over WebTransport before auth is wired.

The main new knowledge for this phase (vs. what the CONTEXT.md decisions already locked) is:

1. Issue #311 is still open, no 0.7.2 published — the `time = "=0.3.47"` workspace pin is still required (D-02 confirmed).
2. The exact `wtransport` feature set is `default-features = false, features = ["self-signed", "ring"]`. There is no `runtime-tokio` feature; the runtime integration comes from using `tokio` directly.
3. `wtransport::Connection` does NOT expose `datagram_send_buffer_space`. The wrapper must reach through `quic_connection()` (enabled by the `quinn` feature) to get the underlying `quinn::Connection::datagram_send_buffer_space()`. This means the wrapper needs `features = ["self-signed", "ring", "quinn"]`.
4. `wtransport::SendStream::finish()` is ASYNC (unlike quinn 0.11 where `finish()` is sync). The `NoshSendStream::finish` wrapper body therefore calls `.finish().await` — the inverse of the quinn wrapper.
5. `wtransport::RecvStream::stop` takes `self` (consuming). `NoshRecvStream::stop` takes `&mut self`. The wrapper must use `Option<RecvStream>` with `.take()` to adapt this.
6. `wtransport::SendDatagramError` has three variants: `NotConnected`, `UnsupportedByPeer`, `TooLarge`. There is no `Disabled` variant — map `NotConnected` to `SendDatagramError::ConnectionLost`.
7. UDP/443 binding requires root or `setcap CAP_NET_BIND_SERVICE`. On this dev machine `ip_unprivileged_port_start = 1024`, so 443 is privileged. The `--port` flag default should be 4433 for dev; 443 requires explicit operator setup.

**Primary recommendation:** The wrapper is a near-mechanical exercise — `wtransport`'s API is a close match to quinn's. The three non-obvious implementation points are: (a) `datagram_send_buffer_space` via `quic_connection()`, (b) the async `finish()` adapter, and (c) the consuming `stop()` adapter via `Option<RecvStream>`.

---

<user_constraints>
## User Constraints (from CONTEXT.md)

### Locked Decisions

- **D-01:** Add `wtransport = { version = "0.7.1", default-features = false, features = ["self-signed", "ring"] }` — `default-features = false` + explicit `ring` prevents the rustls crypto-provider unification panic. `cargo tree -f "{p} {f}" | grep rustls` showing only `ring` is SC#4. (Research note: add `"quinn"` to features also — required for `quic_connection()` to access `datagram_send_buffer_space`.)
- **D-02:** Pin `time = "=0.3.47"` at the workspace level. wtransport issue #311 is still open as of 2026-06-13; no 0.7.2 published. Keep the pin.
- **D-03:** Datagram MTU uses `wtransport::Connection::max_datagram_size()` — NOT `max_datagram_payload_size()` (that method does not exist). `max_datagram_size()` already accounts for WebTransport capsule overhead; return value is directly usable as the payload budget.
- **D-04:** Outer TLS presents a real CA-signed certificate loaded from operator-configured PEM files. ACME is out of scope; cert acquisition is an external sidecar.
- **D-05:** Outer CA cert gives server identity; inner SSH-key handshake (Phase 25) is the authoritative end-to-end mutual auth. Client performs standard outer-TLS validation.
- **D-06:** Native QUIC mode is unchanged — keeps self-signed-cert SPKI-pinning.
- **D-07:** One transport per process. `--mode native|webtransport`. Server in `--mode webtransport` rejects raw-QUIC (non-WebTransport) connection attempts.
- **D-08:** Client gets matching `--webtransport` flag.

### Claude's Discretion

- Listen port: default UDP/443 with configurable `--port`. Document root / `setcap CAP_NET_BIND_SERVICE` requirement.
- Exact `ServerConfigBuilder` chain: `with_bind_address` + `with_custom_tls(rustls::ServerConfig)` — verified (see Standard Stack section).
- Test-only auth stub: gate behind `test-support` cargo feature, NOT `#[cfg(test)]`, following the v1.3 pattern (`#[cfg(any(test, feature = "test-support"))]`).
- `open_bi` double-await asymmetry absorbed inside the `WtransportTransport::open_bi` wrapper.

### Deferred Ideas (OUT OF SCOPE)

- ACME / automatic certificate management inside nosh.
- Mode B (Envoy-fronted proxy deployment).
</user_constraints>

<phase_requirements>
## Phase Requirements

| ID | Description | Research Support |
|----|-------------|------------------|
| WT-02 | Server binds wtransport listener on UDP/443 in Mode A, outer TLS from existing cert/key material | D-04 + verified builder chain: `ServerConfig::builder().with_bind_address(addr).with_custom_tls(rustls_server_config).build()` |
| WT-03 | Client connects, delivers interactive shell — datagram state-sync, predictive echo, control/scrollback channels all work over WebTransport | D-03 MTU method verified, open_bi double-await pattern confirmed, send_datagram/receive_datagram API confirmed |
| WT-05 | Transport selection explicit (CLI flag); Mode A server rejects raw-QUIC connections | D-07: wtransport's `accept()` loop only surfaces WebTransport sessions; non-WT QUIC clients get a connection error; `--mode webtransport` vs `--mode native` dispatch |
</phase_requirements>

---

## Architectural Responsibility Map

| Capability | Primary Tier | Secondary Tier | Rationale |
|------------|-------------|----------------|-----------|
| WebTransport session accept loop | API / Backend (nosh-server) | — | New `run_wt_accept_loop` parallel to `run_accept_loop`; same session pump underneath |
| Outer TLS config construction | API / Backend (nosh-server, nosh-auth) | — | `build_wt_server_config` reuses existing `nosh-auth` cert loading; `with_custom_tls(rustls_cfg)` injects it |
| NoshTransport impl for wtransport | nosh-server + nosh-client (new files) | nosh-proto (trait) | `WtransportTransport` wraps `wtransport::Connection`; stream wrappers in same files |
| CLI mode flag dispatch | Binary entry point (main.rs) | — | `--mode native|webtransport` selects accept loop; `--webtransport` on client |
| Downgrade protection (WT-05) | API / Backend (nosh-server) | — | wtransport's accept loop only yields WT sessions; non-WT clients fail at the HTTP/3 CONNECT upgrade; server in WT mode never calls quinn accept |
| Test-only auth stub | nosh-server feature-gated | nosh-client dev-dep | `#[cfg(any(test, feature = "test-support"))]` in `handle_wt_connection`; release builds reject connections without inner auth |
| Privilege documentation | Ops / Documentation | Binary (error message) | Document `setcap`/root requirement for port 443 |

---

## Standard Stack

### Core Addition

| Library | Version | Purpose | Why Standard |
|---------|---------|---------|--------------|
| `wtransport` | 0.7.1 | WebTransport-over-HTTP/3 session layer | Only mature, tokio-native Rust WebTransport impl; uses same quinn/rustls the workspace already pins; `with_custom_tls` escape hatch feeds existing rustls configs verbatim |

**Features required:** `{ version = "0.7.1", default-features = false, features = ["self-signed", "ring", "quinn"] }`

- `default-features = false`: prevents activating `aws-lc-rs` alongside `ring` (Pitfall WT-1)
- `ring`: selects ring as the rustls crypto provider, matching the workspace
- `self-signed`: pulls `rcgen` for cert generation (optional path; rcgen already in workspace)
- `quinn`: exposes `Connection::quic_connection() -> &quinn::Connection`, required for `datagram_send_buffer_space`

[VERIFIED: docs.rs/crate/wtransport/0.7.1/features] — feature flags confirmed against the features page.

### Workspace-Level Pin (still required)

```toml
# Workspace Cargo.toml — add/keep:
time = "=0.3.47"
```

[VERIFIED: github.com/BiagioFesta/wtransport/issues/311] — issue still open as of 2026-06-13; no 0.7.2 published; `time = "=0.3.47"` is the documented temporary fix.

### No New Crates Beyond wtransport

- `rcgen` (already in workspace) — used if operator cert loading path needs fallback test cert
- `tokio` (already in workspace) — wtransport uses `^1.28.1`, satisfied by workspace 1.52.x
- All auth crates (`ssh-key`, `ssh-agent-client-rs`, `ed25519-dalek`) — unchanged, reused in Phase 25

### Installation (workspace Cargo.toml additions)

```toml
[workspace.dependencies]
wtransport = { version = "0.7.1", default-features = false, features = ["self-signed", "ring", "quinn"] }
time = "=0.3.47"
```

```toml
# crates/nosh-server/Cargo.toml [features]
webtransport = ["dep:wtransport"]

# crates/nosh-server/Cargo.toml [dependencies]
wtransport = { workspace = true, optional = true }

# crates/nosh-client/Cargo.toml [features]
webtransport = ["dep:wtransport"]

# crates/nosh-client/Cargo.toml [dependencies]
wtransport = { workspace = true, optional = true }
```

**Version verification:**
```bash
cargo search wtransport   # → wtransport = "0.7.1"  (confirmed 2026-06-13)
```

---

## Package Legitimacy Audit

| Package | Registry | Age | Downloads | Source Repo | slopcheck | Disposition |
|---------|----------|-----|-----------|-------------|-----------|-------------|
| `wtransport` | crates.io | ~2 yrs (first release 2024) | Established | github.com/BiagioFesta/wtransport | [OK] | Approved |

**Packages removed due to slopcheck [SLOP] verdict:** none

**Packages flagged as suspicious [SUS]:** none

slopcheck was available and ran successfully. `wtransport` rated [OK]. Confirmed on crates.io registry (Rust ecosystem — not confused with npm).

---

## Architecture Patterns

### System Architecture Diagram

```
nosh-client (--webtransport)
    │
    │  QUIC / HTTP/3 / WebTransport (UDP/443)
    ▼
wtransport::Endpoint<Client>
    │  [outer TLS: CA-signed cert validation]
    ▼
wtransport::Connection
    │
    ├── send_datagram / receive_datagram  →  StateDiff (datagram channel)
    └── open_bi / accept_bi              →  bidi streams (control, shell I/O, scrollback)
                                              │
                                              ▼
                                     NoshTransport trait boundary
                                              │
                                              ▼
                              WtransportTransport impl
                                              │
                                              ▼
                              session pump (run_session / run_reattach_session)
                              [UNCHANGED — same pump as native QUIC mode]

─────────────────────────────────────────────────────────────────────────────

nosh-server (--mode webtransport)
    │
    ▼
wtransport::Endpoint<Server>
    │  .accept().await → IncomingSession
    │  .await          → SessionRequest
    │  .accept().await → wtransport::Connection
    │
    ├── [test-support: skip inner auth]
    └── [release: reject — Phase 25 wires real inner auth]
                │
                ▼
        WtransportTransport::new(conn)   ← impl NoshTransport
                │
                ▼
        Box<dyn NoshTransport>  →  handle_connection (unchanged entry point)
```

### Recommended Project Structure

```
crates/nosh-server/src/
├── wt_transport.rs     # WtransportTransport + WtransportSendStream + WtransportRecvStream
│                       # build_wt_server_config, run_wt_accept_loop
├── quinn_transport.rs  # (existing) QuinnTransport (unchanged)
└── server.rs           # (existing) gains --mode dispatch; no other changes

crates/nosh-client/src/
├── wt_transport.rs     # WtransportClientConnection (impl NoshTransport)
│                       # build_wt_client_config, connect_wt
└── client.rs           # gains --webtransport flag dispatch
```

### Pattern 1: ServerConfigBuilder chain (Mode A, real CA cert)

```rust
// Source: docs.rs/wtransport/0.7.1/wtransport/config/struct.ServerConfigBuilder.html
// [VERIFIED: docs.rs]

use wtransport::ServerConfig;

pub fn build_wt_server_config(
    bind_addr: SocketAddr,
    rustls_cfg: rustls::ServerConfig,   // loaded from operator cert + key PEM files
) -> anyhow::Result<wtransport::ServerConfig> {
    Ok(ServerConfig::builder()
        .with_bind_address(bind_addr)
        .with_custom_tls(rustls_cfg)    // TlsServerConfig = rustls::ServerConfig
        .build())
}
```

The `rustls::ServerConfig` passed here is built with the operator's CA cert/key (loaded from PEM via `rcgen` or `rustls-pemfile`), NOT the self-signed cert used in native QUIC mode. The existing `nosh-auth::signer` module loads keys from PEM; a new helper constructs the rustls config from PEM cert + key rather than a self-signed cert.

### Pattern 2: ClientConfigBuilder chain (outer TLS with native CA roots)

```rust
// Source: docs.rs/wtransport/0.7.1/wtransport/config/struct.ClientConfigBuilder.html
// [VERIFIED: docs.rs]

use wtransport::ClientConfig;

pub fn build_wt_client_config(
    // For Mode A direct: server has a CA-signed cert — use native roots.
    // For custom rustls config (SPKI pinning fallback): use with_custom_tls(rustls_cfg).
) -> anyhow::Result<wtransport::ClientConfig> {
    Ok(ClientConfig::builder()
        .with_bind_default()
        .with_native_certs()            // validates the CA-signed server cert
        .build())
}
```

For testing with a self-signed cert or custom verifier, use `.with_custom_tls(rustls_client_cfg)` instead.

### Pattern 3: WtransportTransport implementation (key API mappings)

```rust
// Source: docs.rs/wtransport/0.7.1/wtransport/connection/struct.Connection.html
// [VERIFIED: docs.rs]

use async_trait::async_trait;
use bytes::Bytes;
use wtransport::Connection;
use nosh_proto::transport_trait::{
    NoshTransport, NoshSendStream, NoshRecvStream, SendDatagramError,
};

pub struct WtransportTransport(pub Connection);

#[async_trait]
impl NoshTransport for WtransportTransport {
    fn send_datagram(&self, data: Bytes) -> Result<(), SendDatagramError> {
        // wtransport::SendDatagramError has 3 variants: TooLarge, UnsupportedByPeer, NotConnected
        self.0.send_datagram(data.as_ref()).map_err(|e| match e {
            wtransport::error::SendDatagramError::TooLarge => SendDatagramError::TooLarge,
            wtransport::error::SendDatagramError::UnsupportedByPeer => SendDatagramError::UnsupportedByPeer,
            wtransport::error::SendDatagramError::NotConnected => {
                SendDatagramError::ConnectionLost("not connected".to_string())
            }
        })
    }

    fn datagram_send_buffer_space(&self) -> usize {
        // wtransport::Connection has NO datagram_send_buffer_space().
        // Must reach through to quinn::Connection via quic_connection() (quinn feature).
        self.0.quic_connection().datagram_send_buffer_space()
    }

    fn max_datagram_size(&self) -> Option<usize> {
        // max_datagram_size() already subtracts WebTransport capsule overhead (D-03).
        // Returns None if datagrams unsupported/disabled.
        self.0.max_datagram_size()
    }

    async fn read_datagram(&self) -> anyhow::Result<Bytes> {
        // Returns wtransport::Datagram; Datagram implements Deref<Target=[u8]>
        let dg = self.0.receive_datagram().await?;
        Ok(Bytes::copy_from_slice(&dg))
    }

    async fn accept_bi(&self) -> anyhow::Result<(Box<dyn NoshSendStream>, Box<dyn NoshRecvStream>)> {
        let (s, r) = self.0.accept_bi().await?;
        Ok((Box::new(WtransportSendStream(s)), Box::new(WtransportRecvStream(Some(r)))))
    }

    async fn open_bi(&self) -> anyhow::Result<(Box<dyn NoshSendStream>, Box<dyn NoshRecvStream>)> {
        // DOUBLE AWAIT: open_bi().await? → OpeningBiStream; .await? → (SendStream, RecvStream)
        // Both awaits are hidden inside this single-await interface.
        let (s, r) = self.0.open_bi().await?.await?;
        Ok((Box::new(WtransportSendStream(s)), Box::new(WtransportRecvStream(Some(r)))))
    }

    fn remote_address(&self) -> std::net::SocketAddr {
        self.0.remote_address()
    }

    fn close(&self, _code: u32, reason: &[u8]) {
        // wtransport::Connection::close() does not take a VarInt application code.
        // It accepts a u32 code as VarInt internally.
        let reason_str = String::from_utf8_lossy(reason);
        self.0.close(wtransport::error::ConnectionError::ApplicationClosed(
            wtransport::VarInt::from_u32(_code), reason_str.as_bytes().to_vec()
        ));
        // NOTE: verify exact close() signature from docs.rs — may differ. [ASSUMED]
    }
}
```

**IMPORTANT note on `close()`:** The exact `Connection::close()` API signature for wtransport needs to be confirmed at implementation time — the docs list it as a method but the precise signature (VarInt, reason bytes) is not fully documented in the fetched content. Tag as [ASSUMED] pending impl-time verification.

### Pattern 4: WtransportSendStream — async finish, sync reset

```rust
// Key difference from QuinnSendStream: finish() is ASYNC in wtransport 0.7.1.
// [VERIFIED: docs.rs/wtransport/0.7.1/wtransport/stream/struct.SendStream.html]

pub struct WtransportSendStream(pub wtransport::SendStream);

#[async_trait]
impl NoshSendStream for WtransportSendStream {
    async fn write_all(&mut self, data: &[u8]) -> anyhow::Result<()> {
        Ok(self.0.write_all(data).await?)
    }

    async fn flush(&mut self) -> anyhow::Result<()> {
        // wtransport::SendStream implements AsyncWrite — flush via AsyncWriteExt
        use tokio::io::AsyncWriteExt;
        Ok(self.0.flush().await?)
    }

    async fn finish(&mut self) -> anyhow::Result<()> {
        // UNLIKE quinn, wtransport::SendStream::finish() IS ASYNC.
        // The wrapper body calls .await — this is the OPPOSITE of QuinnSendStream.
        Ok(self.0.finish().await?)
    }

    async fn stopped(&mut self) -> anyhow::Result<()> {
        let _result = self.0.stopped().await;
        Ok(())
    }

    fn reset(&mut self, _code: u32) {
        // wtransport::SendStream::reset takes VarInt; no return value to handle
        let _ = self.0.reset(wtransport::VarInt::from_u32(_code));
    }
}
```

### Pattern 5: WtransportRecvStream — consuming stop adapter

```rust
// KEY DIFFERENCE: wtransport::RecvStream::stop(self) takes ownership (consuming).
// NoshRecvStream::stop takes &mut self. Use Option<RecvStream> with .take().
// [VERIFIED: docs.rs/wtransport/0.7.1/wtransport/stream/struct.RecvStream.html]

pub struct WtransportRecvStream(pub Option<wtransport::RecvStream>);

#[async_trait]
impl NoshRecvStream for WtransportRecvStream {
    async fn read_exact(&mut self, buf: &mut [u8]) -> anyhow::Result<()> {
        match self.0.as_mut() {
            Some(s) => Ok(s.read_exact(buf).await?),
            None => Err(anyhow::anyhow!("stream already stopped")),
        }
    }

    async fn read(&mut self, buf: &mut [u8]) -> anyhow::Result<Option<usize>> {
        match self.0.as_mut() {
            Some(s) => Ok(s.read(buf).await?),
            None => Ok(None), // stopped → EOF
        }
    }

    fn stop(&mut self, code: u32) {
        // take() consumes the inner stream, satisfying stop(self) signature.
        if let Some(stream) = self.0.take() {
            stream.stop(wtransport::VarInt::from_u32(code));
        }
    }
}
```

### Pattern 6: Accept loop (Mode A, downgrade protection via WT-05)

```rust
// Source: docs.rs/wtransport/0.7.1/wtransport/endpoint/struct.Endpoint.html
// [VERIFIED: docs.rs]

pub async fn run_wt_accept_loop(
    endpoint: wtransport::Endpoint<wtransport::endpoint::endpoint_side::Server>,
    registry: Arc<SessionRegistry>,
    limits: AuthLimits,
    shell: Option<String>,
) -> anyhow::Result<()> {
    let sem = Arc::new(tokio::sync::Semaphore::new(limits.max_concurrent));
    loop {
        // Only WebTransport sessions surface here — non-WT QUIC clients attempting
        // a raw QUIC connection get a protocol error at the HTTP/3 CONNECT upgrade
        // stage; wtransport's accept() loop never yields them as sessions.
        // This satisfies WT-05: a server in --mode webtransport never processes
        // raw QUIC session data.
        let Some(incoming) = endpoint.accept().await else { break };

        // Pre-auth semaphore (replicates run_accept_loop's DoS cap — Pitfall WT-1 guard)
        let permit = match sem.clone().try_acquire_owned() {
            Ok(p) => p,
            Err(_) => { tracing::warn!("pre-auth cap reached, dropping incoming WT session"); continue; }
        };

        let auth_timeout = limits.auth_timeout;
        let registry = registry.clone();
        let shell = shell.clone();

        tokio::spawn(async move {
            let _permit = permit;
            let result = tokio::time::timeout(auth_timeout, async {
                let session_request = incoming.await?;
                // Optional: inspect session_request.path() to gate on URL path
                let conn: wtransport::Connection = session_request.accept().await?;
                let transport: Box<dyn NoshTransport> =
                    Box::new(WtransportTransport(conn));
                handle_connection(transport, registry, shell).await
            }).await;
            if let Err(_elapsed) = result {
                tracing::warn!("WT session auth timed out");
            }
        });
    }
    Ok(())
}
```

**WT-05 mechanism:** `wtransport`'s `Endpoint::accept()` only yields HTTP/3 WebTransport CONNECT upgrade requests. A raw QUIC client (without HTTP/3 upgrade) never produces an `IncomingSession` from this loop. The server's mode selection in `main.rs` dispatches to either `run_accept_loop` (quinn, native) or `run_wt_accept_loop` (wtransport), never both. Thus a server started `--mode webtransport` has no quinn accept loop running and cannot process any non-WT QUIC connection.

### Pattern 7: Test-only auth stub (test-support gate)

```rust
// In handle_wt_connection (inside run_wt_accept_loop's spawned task):
// Phase 25 will replace the stub with run_inner_auth_server().

#[cfg(any(test, feature = "test-support"))]
fn is_test_auth_bypass() -> bool { true }

#[cfg(not(any(test, feature = "test-support")))]
fn is_test_auth_bypass() -> bool { false }

// Inside handle_wt_connection, before processing SessionOpen:
if is_test_auth_bypass() {
    // Skip inner auth for integration tests — use a synthetic NoshPublicKey
    // derived from a test key provided via a secondary CLI arg or env var.
    tracing::warn!("INNER AUTH BYPASSED — test-support mode; MUST NOT be in release builds");
} else {
    // Phase 25 fills this in: run_inner_auth_server(...)
    // For Phase 24 release builds: reject connection immediately.
    transport.close(1, b"inner-auth-not-implemented");
    return Ok(());
}
```

The test integration (in `nosh-client/tests/`) uses `nosh-server` as a `dev-dependency` with `features = ["test-support"]`, exactly matching the existing pattern in `nosh-client/Cargo.toml`.

### Anti-Patterns to Avoid

- **Passing raw quinn MTU to datagram encoder:** `wtransport::Connection::max_datagram_size()` already subtracts capsule overhead; DO NOT call `quic_connection().max_datagram_size()` for payload sizing — the raw quinn value is too large.
- **Ignoring the double-await on `open_bi`:** `self.0.open_bi().await?` returns `OpeningBiStream`, not the stream pair. Must `await?` again. Skipping the second await compiles but never resolves the stream.
- **Using `#[cfg(test)]` for auth stub:** `#[cfg(test)]` is invisible to integration tests in other crates. Use `#[cfg(any(test, feature = "test-support"))]` so the nosh-client integration tests can enable the stub path via `features = ["test-support"]` in their dev-dependency.
- **Routing datagrams over streams:** Do not send `StateDiff` over a reliable stream as a fallback. Datagram loss is the feature; head-of-line blocking on the terminal state stream defeats predictive echo.
- **Not replicating the pre-auth semaphore:** `run_wt_accept_loop` must have the same `Semaphore`-based pre-auth cap as `run_accept_loop`. WebTransport sessions are not exempt from DoS hardening.
- **Using `finish()` synchronously in the WT wrapper:** wtransport's `finish()` is async. The wrapper body must `.await` it. The inverse error (omitting `.await` in the quinn wrapper) was explicitly documented in `quinn_transport.rs`; the WT wrapper has the opposite pattern.

---

## Don't Hand-Roll

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| WebTransport session accept | Custom HTTP/3 framing on top of quinn | `wtransport::Endpoint::accept()` | WebTransport framing (CONNECT upgrade, SETTINGS, Quarter Stream ID) is non-trivial; wtransport handles all of it |
| Datagram overhead accounting | Calculate Quarter-Stream-ID varint size manually | `wtransport::Connection::max_datagram_size()` | Already subtracts all overhead; manual calculation gets varint sizing wrong under high stream IDs |
| Outer TLS config for WebTransport | New TLS config construction | Reuse existing `nosh-auth` cert loading + `with_custom_tls(rustls_cfg)` | The existing `rustls::ServerConfig` construction in `nosh-auth` already produces a valid TLS 1.3 config; no new code needed |
| Pre-auth session state machine | Bespoke pre-auth object | Replicate the `Semaphore` + `auth_timeout` pattern from `run_accept_loop` | Copy exactly — any divergence risks the DoS protection gap |

**Key insight:** wtransport's value is not just the WebTransport protocol — it is the session-accept state machine (IncomingSession → SessionRequest → Connection) that handles URL-path routing, SETTINGS negotiation, and the HTTP/3 CONNECT upgrade before handing the application a clean Connection object. Building this manually from raw quinn would require implementing significant portions of RFC 9114 and the WebTransport draft spec.

---

## Runtime State Inventory

Step 2.5: SKIPPED — this is a greenfield feature addition (no rename/refactor/migration). No existing runtime state references WebTransport.

---

## Environment Availability Audit

| Dependency | Required By | Available | Version | Fallback |
|------------|------------|-----------|---------|----------|
| `cargo` / Rust toolchain | Build | ✓ | (workspace builds pass) | — |
| UDP/443 binding | Production server | ✗ for unprivileged user | `ip_unprivileged_port_start = 1024` on this machine | Use `--port 4433` for dev/CI; document `setcap CAP_NET_BIND_SERVICE` for production |
| `ssh-agent` | Test-support integration tests | ✓ | In PATH (existing test harness uses it) | Tests skip if unavailable (existing pattern) |
| `wtransport` crate | Phase 24 feature | Not yet in workspace | 0.7.1 on crates.io | — |

**Missing dependencies with no fallback:** none — the dev/CI path uses `--port 4433`.

**Missing dependencies with fallback:** UDP/443 binding — use high port for dev/CI; document privilege requirement for production.

**UDP/443 privilege note (planner must document in plan):** On this machine (`ip_unprivileged_port_start = 1024`), binding UDP/443 requires one of:
- Running `nosh-server` as root (not recommended for production)
- `sudo setcap 'cap_net_bind_service=+ep' $(which nosh-server)` after each build
- A sidecar (e.g. socat, iptables REDIRECT) forwarding 443 → high port
- Changing `sysctl net.ipv4.ip_unprivileged_port_start=443` (system-wide, requires root)

The `--port` flag default must remain 4433 for dev/CI. Only production deployments target 443.

---

## Common Pitfalls

### Pitfall 1: wtransport `datagram_send_buffer_space` not exposed (WT-specific)

**What goes wrong:** `WtransportTransport::datagram_send_buffer_space()` has no native method on `wtransport::Connection`. If the implementer calls a non-existent method, it fails to compile.

**Why it happens:** The wtransport docs list all Connection methods and `datagram_send_buffer_space` is not among them. Only `max_datagram_size` is directly exposed.

**How to avoid:** Use the `quinn` feature of wtransport to access `Connection::quic_connection() -> &quinn::Connection`, then call `quinn::Connection::datagram_send_buffer_space()` on the underlying connection. This requires adding `"quinn"` to the wtransport feature list.

**Warning signs:** Compile error `no method named datagram_send_buffer_space found for struct wtransport::Connection`.

### Pitfall 2: Crypto-provider feature unification panic (WT-1, from PITFALLS.md)

**What goes wrong:** If `wtransport` is added without `default-features = false`, its default feature set activates `ring` (which is already fine) — BUT if any other path activates `aws-lc-rs`, both providers register and rustls panics at the first TLS handshake.

**How to avoid:** `wtransport = { version = "0.7.1", default-features = false, features = ["self-signed", "ring", "quinn"] }`. Run `cargo tree -f "{p} {f}" | grep rustls` immediately after adding; confirm only `ring` appears.

**Current workspace status:** Current tree shows `rustls v0.23.40 ring,std` — ring only. Adding wtransport with explicit `ring` feature preserves this.

### Pitfall 3: Double-await on open_bi silently skipped

**What goes wrong:** Writing `let (s, r) = self.0.open_bi().await?;` compiles because `open_bi()` itself is async and the `?` unwraps the outer `Result<OpeningBiStream, _>`. But the result is discarded — the variable `(s, r)` never exists, and the code silently produces an `OpeningBiStream` that gets dropped.

**Correct pattern:** `let (s, r) = self.0.open_bi().await?.await?;` — both `await?` calls are required.

**Warning signs:** Streams opened by the WT client never receive data from the server; server `accept_bi` never yields; sessions appear to hang immediately after connect.

### Pitfall 4: finish() async/sync direction confusion

**What goes wrong:** `quinn::SendStream::finish()` is sync; `wtransport::SendStream::finish()` is async. The `QuinnSendStream` wrapper explicitly notes "DO NOT add `.await` inside this wrapper body." The `WtransportSendStream` wrapper is the OPPOSITE — the body MUST call `.finish().await`.

**How to avoid:** Add a comment to `WtransportSendStream::finish()` explicitly stating "ASYNC unlike quinn — call `.finish().await`." Mirror the quinn wrapper's warning comment but invert the instruction.

### Pitfall 5: RecvStream::stop() consumes the stream

**What goes wrong:** `wtransport::RecvStream::stop(self)` takes ownership. `NoshRecvStream::stop(&mut self)` does not. Direct delegation fails at compile time.

**How to avoid:** Wrap in `Option<wtransport::RecvStream>` and call `.take()` inside `stop()`. After stop, subsequent `read` / `read_exact` calls return EOF/error (via the `None` branch).

### Pitfall 6: #[cfg(test)] auth stub invisible to integration tests

**What goes wrong:** If the auth stub is gated with `#[cfg(test)]`, it is compiled only when running `cargo test` on `nosh-server` itself. Integration tests in `nosh-client` that depend on `nosh-server` do NOT see `#[cfg(test)]` symbols from nosh-server — they see the normal library. The stub never activates and the test fails to connect.

**How to avoid:** Gate with `#[cfg(any(test, feature = "test-support"))]` exactly as the existing channel mux stubs in `server.rs` (lines 830–840). Add `nosh-server = { path = "../nosh-server", features = ["test-support"] }` to `nosh-client/Cargo.toml`'s `[dev-dependencies]` — this entry already exists.

---

## Code Examples

### Minimal server accept + shell pump (Phase 24 win condition)

```rust
// Source: docs.rs/wtransport/0.7.1 + Phase 23 transport_trait.rs
// [CITED: docs.rs/wtransport/0.7.1]

let config = wtransport::ServerConfig::builder()
    .with_bind_address("0.0.0.0:4433".parse().unwrap())
    .with_custom_tls(rustls_server_cfg)
    .build();

let endpoint = wtransport::Endpoint::server(config)?;

while let Some(incoming) = endpoint.accept().await {
    tokio::spawn(async move {
        let session_req = incoming.await?;
        let conn = session_req.accept().await?;
        let transport: Box<dyn NoshTransport> = Box::new(WtransportTransport(conn));
        // handle_connection is unchanged — works with any Box<dyn NoshTransport>
        handle_connection(transport, registry, shell).await
    });
}
```

### Minimal client connect

```rust
// Source: docs.rs/wtransport/0.7.1
// [CITED: docs.rs/wtransport/0.7.1]

let config = wtransport::ClientConfig::builder()
    .with_bind_default()
    .with_native_certs()
    .build()?;

let endpoint = wtransport::Endpoint::client(config)?;
let conn = endpoint
    .connect(wtransport::ConnectOptions::new("https://server.example.com:443/nosh"))
    .await?;
let transport: Box<dyn NoshTransport> = Box::new(WtransportTransport(conn));
```

### Datagram MTU sizing (D-03 correct path)

```rust
// Correct: use wtransport's max_datagram_size() which accounts for capsule overhead.
// [VERIFIED: docs.rs/wtransport/0.7.1/wtransport/connection/struct.Connection.html]

fn max_datagram_size(&self) -> Option<usize> {
    self.0.max_datagram_size()   // ✓ capsule overhead already subtracted
}

// WRONG — do not use:
// fn max_datagram_size(&self) -> Option<usize> {
//     self.0.quic_connection().max_datagram_size()  // ✗ raw quinn value, too large
// }
```

---

## State of the Art

| Old Approach | Current Approach | When Changed | Impact |
|--------------|------------------|--------------|--------|
| Direct `quinn::Connection` everywhere | `Box<dyn NoshTransport>` seam | Phase 23 (just shipped) | WebTransport wrapper plugs into existing session pump unchanged |
| Self-signed cert pinning for all modes | CA-signed cert for outer WT TLS, SPKI pinning for native QUIC | Phase 24 (this phase) | WT outer TLS validates like a normal HTTPS server; inner auth (Phase 25) handles end-to-end identity |
| wtransport #311 open, time pin required | Same — #311 still open 2026-06-13 | No change | Keep `time = "=0.3.47"` workspace pin |

**Deprecated/outdated:**
- The `STATE.md` note at line 88: "Datagram MTU in WebTransport mode uses `wtransport::Connection::max_datagram_payload_size()`" — this method does NOT exist. The correct method is `max_datagram_size()`. D-03 in CONTEXT.md already corrects this. The planner must not reference the old STATE.md note.

---

## Re-Ask Triggers (RESOLVED)

### (RESOLVED) Re-ask 1: wtransport #311 / `time` pin status

**Finding:** Issue #311 is still OPEN as of 2026-06-13. No 0.7.2 has been published; the only published version remains 0.7.1 (2026-04-26). The temporary workaround documented in the issue is to pin `time = "=0.3.47"`.

**Recommendation:** Keep D-02 — pin `time = "=0.3.47"` at workspace level.

[VERIFIED: github.com/BiagioFesta/wtransport/issues/311 fetched 2026-06-13; crates.io/api/v1/crates/wtransport fetched 2026-06-13]

### (RESOLVED) Re-ask 2: Exact `ServerConfigBuilder` + `ClientConfigBuilder` API

**Finding — server:**
The builder chain is:
```
ServerConfig::builder()
  .with_bind_address(SocketAddr)     // or .with_bind_default(port)
  .with_custom_tls(TlsServerConfig)  // TlsServerConfig = rustls::ServerConfig
  .build()                           // → ServerConfig
```
Optional transport tuning via `.max_idle_timeout()`, `.keep_alive_interval()`, `.allow_migration()` between `.with_custom_tls(...)` and `.build()`.

**Finding — client:**
```
ClientConfig::builder()
  .with_bind_default()               // or .with_bind_address(SocketAddr)
  .with_native_certs()               // or .with_custom_tls(TlsClientConfig)
  .build()                           // → ClientConfig
```

**Finding — feature flags:**
- `default-features = false` is required to prevent aws-lc-rs activation.
- The correct feature set is `["self-signed", "ring", "quinn"]`.
- There is NO `runtime-tokio` feature in wtransport 0.7.1 (unlike quinn which has `runtime-tokio`). wtransport uses tokio directly.
- `ring` is a DEFAULT feature — so `default-features = false` + `features = ["ring"]` is the opt-in; `default-features = true` would also activate `ring` but would pull `self-signed` and potentially other features unconditionally.

[VERIFIED: docs.rs/crate/wtransport/0.7.1/features; docs.rs/wtransport/0.7.1/wtransport/config/struct.ServerConfigBuilder.html; docs.rs/wtransport/0.7.1/wtransport/config/struct.ClientConfigBuilder.html]

### (RESOLVED) Re-ask 3: Datagram MTU method name

**Finding:** `wtransport::Connection::max_datagram_size()` exists and returns `Option<usize>`. There is NO `max_datagram_payload_size()` method. `max_datagram_size()` already subtracts WebTransport capsule (Quarter Stream ID) overhead — the value is the directly-usable payload budget (D-03 confirmed).

[VERIFIED: docs.rs/wtransport/0.7.1/wtransport/connection/struct.Connection.html]

### (RESOLVED) Re-ask 4: `open_bi` double-await

**Finding:** `open_bi()` is async, returns `Result<OpeningBiStream, ConnectionError>`. `OpeningBiStream` implements `Future<Output = Result<(SendStream, RecvStream), StreamOpeningError>>`. The complete pattern is `conn.open_bi().await?.await?`. The full pattern is absorbed inside `WtransportTransport::open_bi()` so callers see a single-await interface.

[VERIFIED: docs.rs/wtransport/0.7.1/wtransport/connection/struct.Connection.html#method.open_bi + docs.rs/wtransport/0.7.1/wtransport/stream/struct.OpeningBiStream.html]

### (RESOLVED) Re-ask 5: Issue #285 unidirectional finish() hang

**Finding:** Issue #285 (finish() hangs on unidirectional streams) remains OPEN, labelled "Investigation." Confirmed to affect unidirectional streams only. nosh uses bidi streams for all channels — the issue is not relevant to Phase 24. Continue to avoid unidirectional stream `finish()` as CONTEXT.md already states.

[VERIFIED: github.com/BiagioFesta/wtransport/issues/285 fetched 2026-06-13]

### (RESOLVED) Re-ask 6: Mode flag / downgrade protection mechanism

**Finding:** The `wtransport::Endpoint<Server>::accept()` loop only yields `IncomingSession` objects, which are HTTP/3 WebTransport CONNECT upgrade requests. A raw QUIC client (using quinn directly, without HTTP/3) never generates an `IncomingSession`. The server's two code paths are mutually exclusive: `main.rs` dispatches to either `run_accept_loop` (quinn endpoint) or `run_wt_accept_loop` (wtransport endpoint) based on `--mode`. Running `--mode webtransport` starts only the wtransport endpoint; the quinn endpoint is never created. This is the correct mechanism for WT-05.

No explicit "reject" code is needed — the wtransport accept loop simply never surfaces non-WT connections. If a raw QUIC client attempts to connect, the HTTP/3 upgrade handshake fails at the transport layer before reaching application code.

[CITED: docs.rs/wtransport/0.7.1 accept loop pattern; ARCHITECTURE.md §WebTransport accept loop]

### (RESOLVED) Re-ask 7: UDP/443 privilege requirement

**Finding:** On this machine, `ip_unprivileged_port_start = 1024`. Binding UDP/443 requires root or `setcap CAP_NET_BIND_SERVICE`. The plan must document this and set the `--port` default to 4433 for dev/CI. The `--port` argument (Claude's Discretion) should default to 443 in documentation but to 4433 in the binary's `default_value` so CI/dev can run unprivileged.

---

## Open Questions

1. **`Connection::close()` exact signature** — The docs list `close()` as a method but the exact signature (code type, reason bytes) was not fully verified from the fetched docs.rs content. At implementation time, confirm whether it takes `VarInt + &[u8]` or a different error type.
   - What we know: it exists; it closes the connection
   - What's unclear: the precise error code + reason argument types
   - Recommendation: check at implementation start; use `quic_connection().close(code.into(), reason)` as fallback if the native API is awkward.

2. **`wtransport::Datagram` type** — `receive_datagram()` returns `Result<Datagram, ConnectionError>`. The wrapper converts to `Bytes::copy_from_slice(&dg)` assuming `Datagram: Deref<Target=[u8]>`. Confirm this at implementation time.
   - What we know: Datagram is a struct returned by receive_datagram
   - What's unclear: exact Deref impl or method to get payload bytes
   - Recommendation: check docs.rs/wtransport/0.7.1/wtransport/datagram/ at impl time; likely `dg.payload()` or `dg.as_ref()`.

3. **rustls-pemfile for CA cert loading** — Loading operator PEM cert + key for the outer TLS config requires parsing PEM files. The existing `nosh-auth` uses `rcgen`-generated certs, not PEM file loading. A new helper using `rustls-pemfile` (already a transitive dep via rustls) is needed.
   - What we know: `rustls-pemfile` is likely available as a transitive dep
   - What's unclear: whether it needs to be added as a direct dep or is already accessible
   - Recommendation: check `cargo tree | grep rustls-pemfile` at impl time; if absent, add as a workspace dep.

---

## Assumptions Log

| # | Claim | Section | Risk if Wrong |
|---|-------|---------|---------------|
| A1 | `Connection::close()` accepts VarInt code + reason bytes | Code Examples / Pattern 3 | Wrong call signature → compile error; fallback via `quic_connection().close()` |
| A2 | `wtransport::Datagram` is Deref to `[u8]` or has `.payload()` / `.as_ref()` | Pattern 3 (read_datagram) | Wrong method → compile error at impl time |
| A3 | `rustls-pemfile` is reachable as a transitive dep without adding it directly | Open Questions #3 | May need explicit dep addition |

---

## Sources

### Primary (HIGH confidence)

- [docs.rs/crate/wtransport/0.7.1/features](https://docs.rs/crate/wtransport/0.7.1/features) — feature flags confirmed: `ring` default, `self-signed` default, `quinn` non-default; no `runtime-tokio` feature
- [docs.rs/wtransport/0.7.1/wtransport/config/struct.ServerConfigBuilder.html](https://docs.rs/wtransport/0.7.1/wtransport/config/struct.ServerConfigBuilder.html) — full builder chain with method signatures
- [docs.rs/wtransport/0.7.1/wtransport/config/struct.ClientConfigBuilder.html](https://docs.rs/wtransport/0.7.1/wtransport/config/struct.ClientConfigBuilder.html) — full builder chain with method signatures
- [docs.rs/wtransport/0.7.1/wtransport/connection/struct.Connection.html](https://docs.rs/wtransport/0.7.1/wtransport/connection/struct.Connection.html) — all Connection methods: max_datagram_size, send_datagram, receive_datagram, open_bi (double-await), accept_bi, quic_connection, remote_address, close
- [docs.rs/wtransport/0.7.1/wtransport/stream/struct.SendStream.html](https://docs.rs/wtransport/0.7.1/wtransport/stream/struct.SendStream.html) — finish() is async, reset() is sync
- [docs.rs/wtransport/0.7.1/wtransport/stream/struct.RecvStream.html](https://docs.rs/wtransport/0.7.1/wtransport/stream/struct.RecvStream.html) — stop() takes self (consuming)
- [docs.rs/wtransport/0.7.1/wtransport/error/enum.SendDatagramError.html](https://docs.rs/wtransport/0.7.1/wtransport/error/enum.SendDatagramError.html) — 3 variants: TooLarge, UnsupportedByPeer, NotConnected
- [crates.io/api/v1/crates/wtransport](https://crates.io/api/v1/crates/wtransport) — latest version 0.7.1 published 2026-04-26; no 0.7.2
- [github.com/BiagioFesta/wtransport/issues/311](https://github.com/BiagioFesta/wtransport/issues/311) — still open 2026-06-13; time pin is the fix
- [github.com/BiagioFesta/wtransport/issues/285](https://github.com/BiagioFesta/wtransport/issues/285) — still open; affects unidirectional streams only
- crates/nosh-proto/src/transport_trait.rs — Phase 23 trait definitions (read directly)
- crates/nosh-server/src/quinn_transport.rs — quinn wrapper pattern to mirror (read directly)
- crates/nosh-auth/src/verifier.rs — existing cert/key handling to reuse (read directly)
- crates/nosh-server/src/server.rs — test-support feature gate pattern (lines 830–840)

### Secondary (MEDIUM confidence)

- .planning/research/STACK.md — wtransport 0.7.1 dep graph and API surface (researched 2026-06-13)
- .planning/research/PITFALLS.md — WT-1 through WT-5, WF-1 pitfall descriptions
- .planning/research/ARCHITECTURE.md — component responsibility map, build sequence, data flow diagrams

---

## Metadata

**Confidence breakdown:**
- Standard Stack: HIGH — feature flags and builder API verified against docs.rs 0.7.1
- Architecture patterns: HIGH — based on reading Phase 23 trait definitions and quinn_transport.rs analog
- Pitfalls: HIGH — grounded in verified API discrepancies (no datagram_send_buffer_space, async finish, consuming stop)
- Issue status: HIGH — fetched live from GitHub 2026-06-13

**Research date:** 2026-06-13
**Valid until:** 2026-07-13 (stable ecosystem; re-verify if wtransport 0.7.2 releases or #311 closes before planning)
