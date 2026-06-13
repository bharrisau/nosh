# Phase 24: WebTransport Endpoint + Mode A — Pattern Map

**Mapped:** 2026-06-13
**Files analysed:** 8 new/modified files
**Analogs found:** 8 / 8

---

## File Classification

| New/Modified File | Role | Data Flow | Closest Analog | Match Quality |
|---|---|---|---|---|
| `crates/nosh-server/src/wt_transport.rs` | service (transport wrapper) | request-response | `crates/nosh-server/src/quinn_transport.rs` | exact |
| `crates/nosh-client/src/wt_transport.rs` | service (transport wrapper) | request-response | `crates/nosh-server/src/quinn_transport.rs` | exact |
| `crates/nosh-server/src/server.rs` (modify) | controller (accept loop) | request-response | `crates/nosh-server/src/server.rs` lines 64–172 | self |
| `crates/nosh-server/src/main.rs` (modify) | config / binary entry | request-response | `crates/nosh-server/src/main.rs` lines 17–60 | self |
| `crates/nosh-client/src/main.rs` (modify) | config / binary entry | request-response | `crates/nosh-client/src/main.rs` lines 1114–1175 | self |
| `crates/nosh-client/src/client.rs` (modify) | service (dialer) | request-response | `crates/nosh-client/src/client.rs` lines 91–130 | role-match |
| `Cargo.toml` (workspace, modify) | config | — | `Cargo.toml` lines 20–36 | self |
| `crates/nosh-server/Cargo.toml` (modify) | config | — | `crates/nosh-server/Cargo.toml` lines 7–13 | self |

---

## Pattern Assignments

### `crates/nosh-server/src/wt_transport.rs` (service, request-response)

**Analog:** `crates/nosh-server/src/quinn_transport.rs`

This is the primary new file. Every method maps one-for-one from `QuinnTransport` / `QuinnSendStream` / `QuinnRecvStream` to `WtransportTransport` / `WtransportSendStream` / `WtransportRecvStream`, with five documented API differences. Copy the entire quinn_transport.rs structure, then apply the diffs below.

**Imports pattern** (analog lines 30–36):
```rust
use async_trait::async_trait;
use bytes::Bytes;
use std::net::SocketAddr;
use tokio::io::AsyncWriteExt as _;
use nosh_proto::transport_trait::{
    NoshTransport, NoshSendStream, NoshRecvStream, SendDatagramError,
};
```
Add for wt_transport.rs:
```rust
use wtransport::{Connection, SendStream, RecvStream, VarInt};
use wtransport::error::SendDatagramError as WtSendDatagramError;
```

**Core transport struct** (analog lines 40–88):
```rust
// ANALOG: QuinnTransport(pub quinn::Connection)
pub struct WtransportTransport(pub Connection);
```

**send_datagram diff** — three variants (NOT four like quinn). Map `NotConnected` → `ConnectionLost`, not `Disabled` (no such variant in wtransport):
```rust
// QUINN (analog lines 48–57):
fn send_datagram(&self, data: Bytes) -> Result<(), SendDatagramError> {
    self.0.send_datagram(data).map_err(|e| match e {
        quinn::SendDatagramError::TooLarge => SendDatagramError::TooLarge,
        quinn::SendDatagramError::UnsupportedByPeer => SendDatagramError::UnsupportedByPeer,
        quinn::SendDatagramError::Disabled => SendDatagramError::Disabled,
        quinn::SendDatagramError::ConnectionLost(e) => {
            SendDatagramError::ConnectionLost(e.to_string())
        }
    })
}

// WTRANSPORT replacement:
fn send_datagram(&self, data: Bytes) -> Result<(), SendDatagramError> {
    self.0.send_datagram(data.as_ref()).map_err(|e| match e {
        WtSendDatagramError::TooLarge => SendDatagramError::TooLarge,
        WtSendDatagramError::UnsupportedByPeer => SendDatagramError::UnsupportedByPeer,
        WtSendDatagramError::NotConnected => {
            SendDatagramError::ConnectionLost("not connected".to_string())
        }
        // wtransport has NO Disabled variant — the match is exhaustive with 3 arms.
    })
}
```

**datagram_send_buffer_space diff** — must go through `quic_connection()` (quinn feature required):
```rust
// QUINN (analog lines 59–61):
fn datagram_send_buffer_space(&self) -> usize {
    self.0.datagram_send_buffer_space()
}

// WTRANSPORT replacement — wtransport::Connection has NO datagram_send_buffer_space().
// The `quinn` feature on wtransport exposes quic_connection() -> &quinn::Connection:
fn datagram_send_buffer_space(&self) -> usize {
    self.0.quic_connection().datagram_send_buffer_space()
}
```

**max_datagram_size diff** — method name is the same but semantics differ: wtransport's value already subtracts WebTransport capsule overhead (D-03). Do NOT use `quic_connection().max_datagram_size()` — that raw quinn value is too large:
```rust
// QUINN (analog lines 63–65):
fn max_datagram_size(&self) -> Option<usize> {
    self.0.max_datagram_size()
}

// WTRANSPORT replacement — same call, different semantics (capsule overhead subtracted):
fn max_datagram_size(&self) -> Option<usize> {
    // DO NOT use: self.0.quic_connection().max_datagram_size() — raw quinn value is too large.
    self.0.max_datagram_size()
}
```

**read_datagram diff** — wtransport returns `Datagram` not `Bytes`. Convert via `as_ref()` or `payload()` (verify at impl time — see RESEARCH Open Question 2):
```rust
// QUINN (analog lines 67–69):
async fn read_datagram(&self) -> anyhow::Result<Bytes> {
    Ok(self.0.read_datagram().await?)
}

// WTRANSPORT replacement:
async fn read_datagram(&self) -> anyhow::Result<Bytes> {
    let dg = self.0.receive_datagram().await?;
    // Verify at impl: Datagram may be dg.payload() or &*dg or dg.as_ref().
    Ok(Bytes::copy_from_slice(&dg))
}
```

**open_bi diff** — DOUBLE AWAIT (critical, Pitfall 3 in RESEARCH):
```rust
// QUINN (analog lines 76–79):
async fn open_bi(&self) -> anyhow::Result<(Box<dyn NoshSendStream>, Box<dyn NoshRecvStream>)> {
    let (s, r) = self.0.open_bi().await?;
    Ok((Box::new(QuinnSendStream(s)), Box::new(QuinnRecvStream(r))))
}

// WTRANSPORT replacement — open_bi().await? yields OpeningBiStream, not the pair.
// Second .await? on OpeningBiStream yields (SendStream, RecvStream). BOTH needed:
async fn open_bi(&self) -> anyhow::Result<(Box<dyn NoshSendStream>, Box<dyn NoshRecvStream>)> {
    let (s, r) = self.0.open_bi().await?.await?;
    Ok((Box::new(WtransportSendStream(s)), Box::new(WtransportRecvStream(Some(r)))))
}
```

**accept_bi** — single await (same as quinn), but RecvStream wraps in `Option`:
```rust
// QUINN (analog lines 71–74):
async fn accept_bi(&self) -> anyhow::Result<(Box<dyn NoshSendStream>, Box<dyn NoshRecvStream>)> {
    let (s, r) = self.0.accept_bi().await?;
    Ok((Box::new(QuinnSendStream(s)), Box::new(QuinnRecvStream(r))))
}

// WTRANSPORT replacement:
async fn accept_bi(&self) -> anyhow::Result<(Box<dyn NoshSendStream>, Box<dyn NoshRecvStream>)> {
    let (s, r) = self.0.accept_bi().await?;
    Ok((Box::new(WtransportSendStream(s)), Box::new(WtransportRecvStream(Some(r)))))
}
```

**close diff** — verify exact wtransport::Connection::close() signature at impl time (RESEARCH Open Question 1):
```rust
// QUINN (analog lines 85–87):
fn close(&self, code: u32, reason: &[u8]) {
    self.0.close(code.into(), reason) // quinn::VarInt: From<u32>
}

// WTRANSPORT placeholder — verify signature from docs.rs at impl:
fn close(&self, code: u32, reason: &[u8]) {
    // [ASSUMED] — confirm exact args at impl time. Fallback:
    self.0.quic_connection().close(VarInt::from_u32(code), reason)
}
```

**WtransportSendStream diff** — `finish()` is ASYNC (Pitfall 4). Opposite of quinn wrapper. RecvStream wraps in `Option`:

```rust
// QUINN finish (analog lines 107–112):
async fn finish(&mut self) -> anyhow::Result<()> {
    // quinn 0.11: SendStream::finish() is SYNCHRONOUS. DO NOT .await here.
    let _ = self.0.finish();
    Ok(())
}

// WTRANSPORT replacement — finish() IS ASYNC. Must .await:
async fn finish(&mut self) -> anyhow::Result<()> {
    // UNLIKE quinn: wtransport::SendStream::finish() IS ASYNC. MUST call .await.
    // The quinn wrapper doc says "DO NOT .await" — the OPPOSITE is true here.
    Ok(self.0.finish().await?)
}
```

**WtransportRecvStream wrapper** — consuming `stop()` adapter (Pitfall 5). No analog in quinn — new pattern:
```rust
// QUINN (analog lines 130–147):
pub struct QuinnRecvStream(pub quinn::RecvStream);
// stop() delegates directly:
fn stop(&mut self, code: u32) {
    let _ = self.0.stop(code.into());
}

// WTRANSPORT replacement — stop(self) is consuming in wtransport.
// Use Option<RecvStream> with .take() to adapt &mut self → consuming call:
pub struct WtransportRecvStream(pub Option<RecvStream>);

fn stop(&mut self, code: u32) {
    // take() satisfies the consuming stop(self) requirement.
    if let Some(stream) = self.0.take() {
        stream.stop(VarInt::from_u32(code));
    }
}

// read_exact and read must check for None after a prior stop():
async fn read_exact(&mut self, buf: &mut [u8]) -> anyhow::Result<()> {
    match self.0.as_mut() {
        Some(s) => Ok(s.read_exact(buf).await?),
        None => Err(anyhow::anyhow!("stream already stopped")),
    }
}

async fn read(&mut self, buf: &mut [u8]) -> anyhow::Result<Option<usize>> {
    match self.0.as_mut() {
        Some(s) => Ok(s.read(buf).await?),
        None => Ok(None),
    }
}
```

**build_wt_server_config and run_wt_accept_loop** live in the same file. Analog for `run_wt_accept_loop` is `run_accept_loop` (analog lines 135–172). Key structural differences:

```rust
// QUINN accept loop (analog lines 144–170):
let permits = Arc::new(tokio::sync::Semaphore::new(limits.max_concurrent));
while let Some(incoming) = endpoint.accept().await {
    let permit = match permits.clone().try_acquire_owned() {
        Ok(p) => p,
        Err(_) => { incoming.refuse(); continue; }
    };
    // ... spawn handle_connection(incoming, timeout, permit, shell, registry)
}

// WTRANSPORT replacement — endpoint.accept() yields IncomingSession not quinn::Incoming.
// No .refuse() on IncomingSession; just drop it. Three-step accept (not two):
while let Some(incoming) = endpoint.accept().await {
    let permit = match permits.clone().try_acquire_owned() {
        Ok(p) => p,
        Err(_) => {
            tracing::warn!("pre-auth cap reached, dropping incoming WT session");
            continue; // drop incoming — no .refuse() method
        }
    };
    tokio::spawn(async move {
        let _permit = permit;
        let result = tokio::time::timeout(auth_timeout, async {
            let session_request = incoming.await?;         // IncomingSession → SessionRequest
            let conn: Connection = session_request.accept().await?;  // SessionRequest → Connection
            // test-support gate here (before handle_connection)
            let transport: Box<dyn NoshTransport> = Box::new(WtransportTransport(conn));
            handle_connection_wt(transport, registry, shell).await
        }).await;
        // ...
    });
}
```

**Test-support auth stub pattern** — copy from server.rs lines 830–841:
```rust
// EXISTING PATTERN (server.rs lines 830–841):
#[cfg(any(test, feature = "test-support"))]
slot.store_server_open_tx(server_open_tx);
#[cfg(any(test, feature = "test-support"))]
let mut server_open_rx_opt: Option<...> = Some(server_open_rx_inner);
#[cfg(not(any(test, feature = "test-support")))]
{
    let _ = (...);
}
#[cfg(not(any(test, feature = "test-support")))]
let mut server_open_rx_opt: Option<...> = None;

// WTRANSPORT auth stub — same gate, different body:
#[cfg(any(test, feature = "test-support"))]
let skip_inner_auth = true;
#[cfg(not(any(test, feature = "test-support")))]
let skip_inner_auth = false;

if !skip_inner_auth {
    // Phase 25 fills this in. For now: reject.
    transport.close(1, b"inner-auth-not-implemented");
    return Ok(());
}
// test builds continue past this point
```

---

### `crates/nosh-client/src/wt_transport.rs` (service, request-response)

**Analog:** `crates/nosh-server/src/quinn_transport.rs` (same structure as server wt_transport)

The client-side WebTransport wrapper implements `NoshTransport` identically to the server wrapper — same struct, same method bodies. It also contains `build_wt_client_config` and `connect_wt`.

**build_wt_client_config** — analog is `build_client_config` in `crates/nosh-client/src/client.rs` lines 94–130:
```rust
// QUINN analog (client.rs lines 94–130):
pub fn build_client_config(
    identity: &ClientIdentity,
    known_hosts: PathBuf,
    host: impl Into<String>,
) -> anyhow::Result<quinn::ClientConfig> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let provider = rustls::crypto::CryptoProvider::get_default()...
    // ... builds rustls::ClientConfig with HostKeyVerifier + cert resolver
    let quic_crypto = QuicClientConfig::try_from(rustls_cfg)...
    let mut client_config = quinn::ClientConfig::new(Arc::new(quic_crypto));
    ...
}

// WTRANSPORT replacement — uses with_native_certs() (D-05: outer CA cert validated normally):
pub fn build_wt_client_config() -> anyhow::Result<wtransport::ClientConfig> {
    Ok(wtransport::ClientConfig::builder()
        .with_bind_default()
        .with_native_certs()   // validates CA-signed server cert; standard HTTPS validation
        .build()?)
    // For test builds using self-signed cert:
    // .with_custom_tls(rustls_client_config)  instead of .with_native_certs()
}
```

**connect_wt** — analog is `client::connect` in client.rs:
```rust
// QUINN analog (client.rs — connect function):
// endpoint.connect(addr, host)?.await

// WTRANSPORT replacement:
pub async fn connect_wt(
    config: wtransport::ClientConfig,
    url: &str,
) -> anyhow::Result<Box<dyn NoshTransport>> {
    let endpoint = wtransport::Endpoint::client(config)?;
    let conn = endpoint
        .connect(wtransport::ConnectOptions::new(url))
        .await?;
    Ok(Box::new(WtransportTransport(conn)))
}
```

The `WtransportTransport`, `WtransportSendStream`, `WtransportRecvStream` structs and impls are identical to the server-side wt_transport.rs — copy them verbatim.

---

### `crates/nosh-server/src/server.rs` (modify — accept loop dispatch + `handle_connection_wt`)

**Analog:** Self — `build_server_config` / `make_endpoint` / `run_accept_loop` (lines 64–172).

**Mode dispatch pattern** — new `--mode` flag selects the accept loop. Analog `run_accept_loop` (lines 135–172) is called from `main.rs`; a parallel `run_wt_accept_loop` lives in `wt_transport.rs` and is called from `main.rs` in the same dispatch:

```rust
// EXISTING (server.rs lines 115–126):
pub fn make_endpoint(
    addr: SocketAddr,
    host_key_path: &Path,
    authorized_keys_path: &Path,
) -> anyhow::Result<quinn::Endpoint> {
    let endpoint = quinn::Endpoint::server(
        build_server_config(host_key_path, authorized_keys_path)?,
        addr,
    )
    ...
}

// NEW parallel function in wt_transport.rs:
pub fn make_wt_endpoint(
    addr: SocketAddr,
    cert_path: &Path,
    key_path: &Path,
) -> anyhow::Result<wtransport::Endpoint<...>> {
    let rustls_cfg = build_ca_cert_rustls_config(cert_path, key_path)?;
    let wt_config = build_wt_server_config(addr, rustls_cfg)?;
    wtransport::Endpoint::server(wt_config)
}
```

**`handle_connection_wt`** is the WebTransport-specific version of `handle_connection` (lines 519–613). Key difference: no quinn-specific `extract_peer_identity` / `handshake_data` call (those are `quinn::Connection`-specific, T-23-03). The boxing step happens before accepting the first stream:

```rust
// EXISTING (server.rs lines 519–613 abbreviated):
async fn handle_connection(incoming, auth_timeout, permit, shell_override, registry) {
    let conn = tokio::time::timeout(auth_timeout, incoming).await?...;
    drop(permit);
    // quinn-specific: extract peer identity BEFORE boxing
    let peer_identity = match extract_peer_identity(&conn) { ... };
    let conn: Box<dyn NoshTransport> = Box::new(QuinnTransport(conn));
    // ... accept_bi, read first frame, dispatch to run_session
}

// NEW handle_connection_wt (in wt_transport.rs or server.rs):
// conn is already Box<dyn NoshTransport> on entry (WtransportTransport was boxed in accept loop).
// No extract_peer_identity (quinn-specific). Auth stub in test-support builds.
// Then: identical accept_bi + first-frame dispatch to run_session.
```

The `run_session` and `run_reattach_session` functions are UNCHANGED — they already accept `Box<dyn NoshTransport>` (Phase 23 seam).

---

### `crates/nosh-server/src/main.rs` (modify — `--mode` flag)

**Analog:** Self — `Args` struct and `main()` (lines 17–117).

**`--mode` flag addition** — follows existing clap arg pattern (lines 17–60):
```rust
// EXISTING pattern (main.rs lines 17–60):
#[derive(Parser, Debug)]
#[command(name = "nosh-server", about, version)]
struct Args {
    #[arg(long, default_value = "127.0.0.1")]
    addr: IpAddr,

    #[arg(long, default_value_t = 4433)]
    port: u16,

    #[arg(long)]
    host_key: Option<PathBuf>,
    // ...
}

// ADDITIONS:
    /// Transport mode. `native` uses direct QUIC (existing behaviour); `webtransport`
    /// wraps QUIC in WebTransport over HTTP/3.
    /// Port 443 requires root or `setcap CAP_NET_BIND_SERVICE`.
    #[arg(long, default_value = "native")]
    mode: TransportMode,

    /// PEM certificate file for WebTransport outer TLS (--mode webtransport only).
    #[arg(long)]
    cert: Option<PathBuf>,

    /// PEM private key file for WebTransport outer TLS (--mode webtransport only).
    #[arg(long)]
    key: Option<PathBuf>,

// NEW enum (derive-able, matches clap's value_enum):
#[derive(Clone, Debug, clap::ValueEnum)]
enum TransportMode {
    Native,
    Webtransport,
}
```

**`main()` dispatch** — analog lines 72–117:
```rust
// EXISTING (main.rs lines 115–117):
let endpoint = server::make_endpoint(addr, &host_key, &authorized_keys)?;
server::run_accept_loop(endpoint, registry, limits, args.shell).await

// NEW dispatch:
match args.mode {
    TransportMode::Native => {
        let endpoint = server::make_endpoint(addr, &host_key, &authorized_keys)?;
        server::run_accept_loop(endpoint, registry, limits, args.shell).await
    }
    TransportMode::Webtransport => {
        let cert = args.cert.context("--cert required for --mode webtransport")?;
        let key = args.key.context("--key required for --mode webtransport")?;
        // bind 443 error message: "failed to bind; port 443 requires root or setcap"
        let endpoint = wt_transport::make_wt_endpoint(addr, &cert, &key)?;
        wt_transport::run_wt_accept_loop(endpoint, registry, limits, args.shell).await
    }
}
```

---

### `crates/nosh-client/src/main.rs` (modify — `--webtransport` flag)

**Analog:** Self — `Args` struct (lines 1114–1175).

**`--webtransport` flag** — follows existing bool flag pattern (`--status` at line 1172):
```rust
// EXISTING bool flag pattern (main.rs lines 1170–1174):
    /// Surface measured RTT (SRTT) in the terminal title via OSC 0/2 (QOL-04).
    #[arg(long)]
    status: bool,

// ADDITION:
    /// Connect using WebTransport over HTTP/3 instead of raw QUIC.
    /// The server must be started with `--mode webtransport`.
    #[arg(long)]
    webtransport: bool,

    /// WebTransport URL path (used with --webtransport; default "/nosh").
    #[arg(long, default_value = "/nosh")]
    wt_path: String,
```

**`main()` dispatch** — analog is the `connect` call that follows `Args::parse()`:
```rust
// Existing connect path uses client::connect / client::make_endpoint.
// When --webtransport:
//   build_wt_client_config() → connect_wt(url) → Box<dyn NoshTransport>
// When native:
//   existing path unchanged
```

---

### `crates/nosh-client/src/client.rs` (modify — `build_ca_cert_rustls_client_config`)

**Analog:** `build_client_config` (lines 94–130) for the outer TLS config construction pattern.

No direct analog for PEM file loading — the existing code uses `rcgen`-generated in-memory certs. A new helper is needed that loads from PEM via `rustls-pemfile` (verify it is a transitive dep at impl; if not, add as workspace dep):

```rust
// EXISTING pattern (client.rs lines 94–130 abbreviated):
pub fn build_client_config(
    identity: &ClientIdentity,
    known_hosts: PathBuf,
    host: impl Into<String>,
) -> anyhow::Result<quinn::ClientConfig> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let provider = rustls::crypto::CryptoProvider::get_default()
        .cloned()
        .unwrap_or_else(|| Arc::new(rustls::crypto::ring::default_provider()));
    // ... builds rustls::ClientConfig with pinning verifier
}

// NEW helper for WebTransport outer TLS (CA cert validation, NOT pinning):
// This is the "with_native_certs" path — no custom verifier needed.
// The wtransport ClientConfigBuilder handles this via .with_native_certs().
// No Rust code needed in client.rs for Mode A (CA cert validated by OS trust store).
```

**NOTE:** For Mode A (D-04, CA-signed cert), the client uses `.with_native_certs()` on the wtransport `ClientConfigBuilder`. No custom `rustls::ClientConfig` is required — the existing nosh-auth verifier machinery is bypassed for the outer TLS layer. The inner SSH-key auth (Phase 25) will add a separate layer.

For test builds using a self-signed cert, a custom `rustls::ClientConfig` with a bypassing verifier is needed. The test pattern from `build_client_config` (skipping signature checks in the verifier) provides the template.

---

### `Cargo.toml` (workspace, modify)

**Analog:** Self — `[workspace.dependencies]` (lines 20–36).

**Additions** — follow existing workspace dep pattern:
```toml
# EXISTING pattern (Cargo.toml lines 21–24):
quinn = { version = "0.11.9", default-features = false, features = ["runtime-tokio", "rustls-ring"] }
rustls = { version = "0.23", default-features = false, features = ["ring", "std"] }

# ADD at end of [workspace.dependencies]:
wtransport = { version = "0.7.1", default-features = false, features = ["self-signed", "ring", "quinn"] }
time = "=0.3.47"
```

`default-features = false` prevents aws-lc-rs activation (Pitfall WT-1 from RESEARCH). The `quinn` feature is required for `quic_connection()` access to `datagram_send_buffer_space`. The `time` pin is required until wtransport issue #311 is resolved.

---

### `crates/nosh-server/Cargo.toml` (modify)

**Analog:** Self — `[features]` (lines 7–13) and `[dependencies]` (lines 23–57).

**Features addition** — follow existing `test-support` feature pattern (lines 7–13):
```toml
# EXISTING (Cargo.toml lines 7–13):
[features]
test-support = []

# ADD:
webtransport = ["dep:wtransport"]
```

**Dependency addition** — follow existing optional dep convention:
```toml
# ADD to [dependencies]:
wtransport = { workspace = true, optional = true }
```

**nosh-client/Cargo.toml** follows identical pattern for `webtransport = ["dep:wtransport"]` feature and optional dep. The existing `dev-dependencies` entry `nosh-server = { path = "../nosh-server", features = ["test-support"] }` (line 51) already enables test-support; the planner should add `"webtransport"` to that feature list for the integration test:
```toml
# EXISTING (nosh-client/Cargo.toml line 51):
nosh-server = { path = "../nosh-server", features = ["test-support"] }

# MODIFIED for integration test:
nosh-server = { path = "../nosh-server", features = ["test-support", "webtransport"] }
```

---

## Shared Patterns

### test-support feature gate
**Source:** `crates/nosh-server/src/server.rs` lines 830–841
**Apply to:** `crates/nosh-server/src/wt_transport.rs` (auth stub), `crates/nosh-client/tests/` harness
```rust
#[cfg(any(test, feature = "test-support"))]
slot.store_server_open_tx(server_open_tx);
#[cfg(not(any(test, feature = "test-support")))]
{
    let _ = (next_server_channel_id, server_open_tx, server_open_rx_inner);
}
#[cfg(not(any(test, feature = "test-support")))]
let mut server_open_rx_opt: Option<tokio::sync::mpsc::Receiver<ChannelType>> = None;
```
The WebTransport auth stub must use this EXACT gate — never `#[cfg(test)]` alone. `#[cfg(test)]` on the nosh-server crate is invisible to nosh-client integration tests (RESEARCH Pitfall 6, lines 580–583).

### Pre-auth semaphore / DoS cap
**Source:** `crates/nosh-server/src/server.rs` lines 144–158
**Apply to:** `crates/nosh-server/src/wt_transport.rs` (`run_wt_accept_loop`)
```rust
let permits = Arc::new(tokio::sync::Semaphore::new(limits.max_concurrent));
while let Some(incoming) = endpoint.accept().await {
    let permit = match permits.clone().try_acquire_owned() {
        Ok(p) => p,
        Err(_) => {
            tracing::warn!(
                "pre-auth connection cap ({}) reached; refusing connection",
                limits.max_concurrent
            );
            incoming.refuse();  // quinn — wtransport: just `continue` (no .refuse())
            continue;
        }
    };
    // permit held until handshake resolves (D-13)
```
`run_wt_accept_loop` MUST replicate this cap — WebTransport sessions are not exempt from DoS hardening (RESEARCH anti-patterns).

### Auth timeout wrapping
**Source:** `crates/nosh-server/src/server.rs` lines 162–169
**Apply to:** `crates/nosh-server/src/wt_transport.rs` (`run_wt_accept_loop` spawned task)
```rust
tokio::spawn(async move {
    if let Err(e) = handle_connection(incoming, timeout, permit, shell, registry).await {
        tracing::warn!("connection handler ended: {e:#}");
    }
});
```
The WT version wraps the three-step accept + handle_connection_wt in `tokio::time::timeout(auth_timeout, async { ... })`.

### Box<dyn NoshTransport> dispatch seam
**Source:** `crates/nosh-server/src/server.rs` line 574
**Apply to:** `crates/nosh-server/src/wt_transport.rs`
```rust
// T-23-03: box AFTER all transport-specific operations (no quinn-specific calls after this):
let conn: Box<dyn NoshTransport> = Box::new(QuinnTransport(conn));
```
For WebTransport: box immediately when the `Connection` is obtained from `session_request.accept().await?` — there are no wtransport-specific operations needed after boxing (no equivalent of `extract_peer_identity`).

### VarInt conversion
**Source:** `crates/nosh-server/src/quinn_transport.rs` lines 86, 121, 145
**Apply to:** All three wt_transport wrapper types
```rust
// QUINN: code.into()  (quinn::VarInt: From<u32>)
// WTRANSPORT: VarInt::from_u32(code)
// wtransport::VarInt does NOT impl From<u32>; use the explicit constructor.
```

### Clap `--mode` / `--webtransport` bool flag pattern
**Source:** `crates/nosh-server/src/main.rs` lines 17–60; `crates/nosh-client/src/main.rs` lines 1114–1175
**Apply to:** both main.rs modifications
```rust
// Bool flag (client, analog lines 1170–1174):
#[arg(long)]
status: bool,

// Enum flag (server, new pattern — clap ValueEnum):
#[derive(Clone, Debug, clap::ValueEnum)]
enum TransportMode { Native, Webtransport }
#[arg(long, default_value = "native")]
mode: TransportMode,
```

---

## No Analog Found

All files have close analogs. The following implementation points have no direct codebase analog and must use RESEARCH.md patterns:

| Implementation point | Reason |
|---|---|
| `build_wt_server_config` (`ServerConfig::builder().with_bind_address(...).with_custom_tls(rustls_cfg).build()`) | No WebTransport config construction exists yet; use RESEARCH Pattern 1 (lines 218–234) |
| `build_ca_cert_rustls_server_config` (PEM cert + key → `rustls::ServerConfig`) | Existing signer.rs only builds from `rcgen`-generated in-memory certs. Need `rustls-pemfile` loading — verify it is a transitive dep at impl time (RESEARCH Open Question 3) |
| `wtransport::Datagram` payload extraction (`receive_datagram()` return value) | No existing datagram receive code uses wtransport; verify `.payload()` / `&*dg` / `.as_ref()` at impl time (RESEARCH Open Question 2) |
| `wtransport::Connection::close()` exact signature | Docs incomplete — verify at impl; fallback: `quic_connection().close(VarInt::from_u32(code), reason)` (RESEARCH Open Question 1) |

---

## Metadata

**Analog search scope:** `crates/nosh-server/src/`, `crates/nosh-client/src/`, `crates/nosh-auth/src/`, `crates/nosh-client/tests/`, `Cargo.toml`, `crates/*/Cargo.toml`
**Files scanned:** 22 source files + 6 Cargo.toml files
**Pattern extraction date:** 2026-06-13
