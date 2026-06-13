# Phase 23: Transport Abstraction Seam - Pattern Map

**Mapped:** 13/06/2026
**Files analysed:** 9 new/modified files
**Analogs found:** 9 / 9 (all have direct source analogs or first-of-kind with full excerpt)

---

## File Classification

| New/Modified File | Role | Data Flow | Closest Analog | Match Quality |
|-------------------|------|-----------|----------------|---------------|
| `crates/nosh-proto/src/transport_trait.rs` | trait definition | request-response | `crates/nosh-proto/src/transport.rs` (same crate, structurally similar thin module) | first-of-kind; analog for module shape only |
| `crates/nosh-proto/src/lib.rs` | config / re-export | — | itself (existing) | exact — add `pub mod transport_trait` + re-export |
| `crates/nosh-server/src/quinn_transport.rs` | service / wrapper | request-response | `crates/nosh-proto/src/transport.rs` (pass-through pattern); `quinn::Connection` usage in `server.rs` | role-match |
| `crates/nosh-client/src/quinn_transport.rs` | service / wrapper | request-response | `crates/nosh-server/src/quinn_transport.rs` (identical pattern, different crate) | exact mirror |
| `crates/nosh-server/src/server.rs` | controller | request-response + streaming | itself (existing) | exact — replace concrete types with trait |
| `crates/nosh-server/src/channel.rs` | controller | streaming | itself (existing) | exact — replace concrete types with trait |
| `crates/nosh-client/src/client.rs` | service / helper | request-response | itself (existing) | exact — replace concrete types with trait |
| `crates/nosh-client/src/channel.rs` | controller | streaming | `crates/nosh-server/src/channel.rs` | role-match |
| `Cargo.toml` (workspace) + 3 crate `Cargo.toml`s | config | — | existing `Cargo.toml` workspace.dependencies pattern | exact |

---

## Pattern Assignments

### `crates/nosh-proto/src/transport_trait.rs` (NEW — trait definition)

**Analog for module shape:** `crates/nosh-proto/src/transport.rs`

**Imports pattern** (transport.rs lines 1–6 — copy crate-root import style):
```rust
// transport.rs structure to mirror: one pub fn, no re-exports needed in the file.
// transport_trait.rs adds pub types/traits instead.
use std::time::Duration;
use quinn::TransportConfig;
```

For `transport_trait.rs`, the equivalent is:
```rust
use async_trait::async_trait;
use bytes::Bytes;
use std::net::SocketAddr;
```

**Module registration pattern** (`lib.rs` lines 9–17 — copy exactly for the new module):
```rust
// lib.rs existing pattern:
pub mod codec;
pub mod datagram;
pub mod messages;
pub mod transport;

pub use codec::{decode, encode, read_message, write_message, ProtoError};
// ...
pub use transport::transport_config;
```
New line to add to `lib.rs`:
```rust
pub mod transport_trait;
pub use transport_trait::{NoshTransport, NoshSendStream, NoshRecvStream, SendDatagramError,
                          write_message_ns, read_message_ns};
```

**The full trait surface to author** (from RESEARCH.md — no existing analog, this is the first trait):
```rust
// crates/nosh-proto/src/transport_trait.rs (NEW)
use async_trait::async_trait;
use bytes::Bytes;
use std::net::SocketAddr;

/// Error returned by NoshTransport::send_datagram.
/// Maps 1:1 to quinn::SendDatagramError for the Quinn wrapper.
#[derive(Debug, thiserror::Error)]
pub enum SendDatagramError {
    #[error("datagram too large for current path MTU")]
    TooLarge,
    #[error("peer does not support datagrams")]
    UnsupportedByPeer,
    #[error("datagrams disabled on this connection")]
    Disabled,
    #[error("connection lost: {0}")]
    ConnectionLost(String),
}

#[async_trait]
pub trait NoshTransport: Send + Sync + 'static {
    fn send_datagram(&self, data: Bytes) -> Result<(), SendDatagramError>;
    fn datagram_send_buffer_space(&self) -> usize;
    fn max_datagram_size(&self) -> Option<usize>;
    async fn read_datagram(&self) -> anyhow::Result<Bytes>;
    async fn accept_bi(&self) -> anyhow::Result<(Box<dyn NoshSendStream>, Box<dyn NoshRecvStream>)>;
    async fn open_bi(&self) -> anyhow::Result<(Box<dyn NoshSendStream>, Box<dyn NoshRecvStream>)>;
    fn remote_address(&self) -> SocketAddr;
    fn close(&self, code: u32, reason: &[u8]);
}

#[async_trait]
pub trait NoshSendStream: Send + 'static {
    async fn write_all(&mut self, data: &[u8]) -> anyhow::Result<()>;
    async fn flush(&mut self) -> anyhow::Result<()>;
    async fn finish(&mut self) -> anyhow::Result<()>;
    async fn stopped(&mut self) -> anyhow::Result<()>;
    fn reset(&mut self, code: u32);
}

#[async_trait]
pub trait NoshRecvStream: Send + 'static {
    async fn read_exact(&mut self, buf: &mut [u8]) -> anyhow::Result<()>;
    async fn read(&mut self, buf: &mut [u8]) -> anyhow::Result<Option<usize>>;
    fn stop(&mut self, code: u32);
}
```

**Key design constraint — synchronous methods MUST NOT become async:**

From `server.rs` lines 436–515 (`send_burst`), the datagram send loop:
```rust
// server.rs lines 437–440, 477–479 — the SYNCHRONOUS-only datagram loop
fn send_burst(
    conn: &quinn::Connection,     // becomes &dyn NoshTransport
    result: DiffTickResult,
    cap: usize,
) -> (Vec<DiffRun>, bool) {
    if let Err(e) = conn.send_datagram(result.payload) { ... }  // NO .await
    // ...
    while !deferred.is_empty()
        && burst_count < BURST_CAP
        && conn.datagram_send_buffer_space() >= cap  // NO .await
    { ... }
}
```
`send_datagram` and `datagram_send_buffer_space` must be plain `fn` (not `async fn`) in the trait.

**Codec helpers for trait objects** (new additions to `transport_trait.rs`):

The existing codec functions (`codec.rs` lines 56–77) take `AsyncWrite + Unpin` / `AsyncRead + Unpin`, which `Box<dyn NoshSendStream>` does not implement. Add these two helpers:
```rust
// Parallel to codec::write_message but works with NoshSendStream trait objects.
pub async fn write_message_ns(
    stream: &mut dyn NoshSendStream,
    msg: &crate::messages::Message,
) -> Result<(), crate::codec::ProtoError> {
    let frame = crate::codec::encode(msg)?;
    stream.write_all(&frame).await.map_err(|e| {
        crate::codec::ProtoError::Io(std::io::Error::new(std::io::ErrorKind::BrokenPipe, e))
    })?;
    stream.flush().await.map_err(|e| {
        crate::codec::ProtoError::Io(std::io::Error::new(std::io::ErrorKind::BrokenPipe, e))
    })?;
    Ok(())
}

// Parallel to codec::read_message but works with NoshRecvStream trait objects.
pub async fn read_message_ns(
    stream: &mut dyn NoshRecvStream,
) -> Result<crate::messages::Message, crate::codec::ProtoError> {
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await.map_err(|e| {
        crate::codec::ProtoError::Io(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, e))
    })?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > crate::codec::MAX_FRAME_LEN {
        return Err(crate::codec::ProtoError::FrameTooLarge(len));
    }
    let mut body = vec![0u8; len];
    stream.read_exact(&mut body).await.map_err(|e| {
        crate::codec::ProtoError::Io(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, e))
    })?;
    crate::codec::decode(&body)
}
```

---

### `crates/nosh-server/src/quinn_transport.rs` (NEW — Quinn wrapper)

**Analog:** `crates/nosh-proto/src/transport.rs` (thin module, one logical unit) + direct calls in `server.rs`

**Imports pattern** (mirror `server.rs` import style for quinn types):
```rust
// server.rs lines 14–37 import style:
use bytes::Bytes;
use nosh_proto::transport_trait::{NoshTransport, NoshSendStream, NoshRecvStream, SendDatagramError};
```

**QuinnTransport wrapper** (derived from every `conn.*` call site in `server.rs`):

Connection-level method call sites confirmed at `server.rs`:
- `conn.send_datagram(result.payload)` — line 442, inside `send_burst`
- `conn.datagram_send_buffer_space()` — line 479, inside `send_burst` while-guard
- `conn.max_datagram_size()` — lines 913, 1796
- `conn.read_datagram()` — lines 952, 1829 (select! arm)
- `conn.accept_bi()` — lines 566, 1285, 2061
- `conn.remote_address()` — lines 540, 743
- `conn.close(CLOSE_AUTH.into(), ...)` — lines 554, 569, 586, 591, 1444, 1458, 1570

```rust
// crates/nosh-server/src/quinn_transport.rs (NEW)
use async_trait::async_trait;
use bytes::Bytes;
use std::net::SocketAddr;
use nosh_proto::transport_trait::{
    NoshTransport, NoshSendStream, NoshRecvStream, SendDatagramError,
};

pub struct QuinnTransport(pub quinn::Connection);

#[async_trait]
impl NoshTransport for QuinnTransport {
    fn send_datagram(&self, data: Bytes) -> Result<(), SendDatagramError> {
        self.0.send_datagram(data).map_err(|e| match e {
            quinn::SendDatagramError::TooLarge => SendDatagramError::TooLarge,
            quinn::SendDatagramError::UnsupportedByPeer => SendDatagramError::UnsupportedByPeer,
            quinn::SendDatagramError::Disabled => SendDatagramError::Disabled,
            quinn::SendDatagramError::ConnectionLost(e) => SendDatagramError::ConnectionLost(e.to_string()),
        })
    }
    fn datagram_send_buffer_space(&self) -> usize {
        self.0.datagram_send_buffer_space()
    }
    fn max_datagram_size(&self) -> Option<usize> {
        self.0.max_datagram_size()
    }
    async fn read_datagram(&self) -> anyhow::Result<Bytes> {
        Ok(self.0.read_datagram().await?)
    }
    async fn accept_bi(&self) -> anyhow::Result<(Box<dyn NoshSendStream>, Box<dyn NoshRecvStream>)> {
        let (s, r) = self.0.accept_bi().await?;
        Ok((Box::new(QuinnSendStream(s)), Box::new(QuinnRecvStream(r))))
    }
    async fn open_bi(&self) -> anyhow::Result<(Box<dyn NoshSendStream>, Box<dyn NoshRecvStream>)> {
        let (s, r) = self.0.open_bi().await?;
        Ok((Box::new(QuinnSendStream(s)), Box::new(QuinnRecvStream(r))))
    }
    fn remote_address(&self) -> SocketAddr {
        self.0.remote_address()
    }
    fn close(&self, code: u32, reason: &[u8]) {
        self.0.close(code.into(), reason)   // quinn::VarInt: From<u32>
    }
}
```

**QuinnSendStream wrapper** (derived from every `ch_send.*` call site in `channel.rs` and `server.rs`):

Stream-level send call sites confirmed at `channel.rs`:
- `ch_send.finish()` — lines 138, 372, 390, 436 (synchronous in quinn 0.11 — returns `()`)
- `ch_send.stopped()` — lines 139, 373, 391, 437 (async)
- `ch_send.write_all(&encoded)` — line 402 (via `AsyncWrite`)
- `ch_send.reset(0u32.into())` — lines 1311, 2080 in `server.rs` (synchronous)

```rust
pub struct QuinnSendStream(pub quinn::SendStream);

#[async_trait]
impl NoshSendStream for QuinnSendStream {
    async fn write_all(&mut self, data: &[u8]) -> anyhow::Result<()> {
        use tokio::io::AsyncWriteExt;
        Ok(self.0.write_all(data).await?)
    }
    async fn flush(&mut self) -> anyhow::Result<()> {
        use tokio::io::AsyncWriteExt;
        Ok(self.0.flush().await?)
    }
    async fn finish(&mut self) -> anyhow::Result<()> {
        // quinn 0.11: finish() is synchronous (fn finish(&mut self) -> ()).
        // Confirmed from channel.rs: `let _ = ch_send.finish()` — no .await.
        self.0.finish();
        Ok(())
    }
    async fn stopped(&mut self) -> anyhow::Result<()> {
        self.0.stopped().await?;
        Ok(())
    }
    fn reset(&mut self, code: u32) {
        let _ = self.0.reset(code.into());   // quinn::VarInt: From<u32>
    }
}
```

**QuinnRecvStream wrapper** (derived from every `ch_recv.*` / `recv.*` call site):

Stream-level recv call sites confirmed:
- `recv.read_exact(&mut buf)` — `channel.rs` line 75 in `read_varint_u32`; codec via `AsyncRead`
- `ch_recv.read(&mut buf)` — `channel.rs` lines 179, 488
- `ch_recv.stop(0u32.into())` — `server.rs` lines 1312, 2081 (synchronous; returns `Result` that is ignored)

```rust
pub struct QuinnRecvStream(pub quinn::RecvStream);

#[async_trait]
impl NoshRecvStream for QuinnRecvStream {
    async fn read_exact(&mut self, buf: &mut [u8]) -> anyhow::Result<()> {
        use tokio::io::AsyncReadExt;
        Ok(self.0.read_exact(buf).await.map(|_| ())?)
    }
    async fn read(&mut self, buf: &mut [u8]) -> anyhow::Result<Option<usize>> {
        Ok(self.0.read(buf).await?)
    }
    fn stop(&mut self, code: u32) {
        let _ = self.0.stop(code.into());    // quinn::VarInt: From<u32>
    }
}
```

---

### `crates/nosh-client/src/quinn_transport.rs` (NEW — client-side Quinn wrapper)

**Analog:** `crates/nosh-server/src/quinn_transport.rs` (identical struct definitions, different crate)

The client wrapper is structurally identical to the server wrapper. Both crates use the same `quinn::Connection`, `quinn::SendStream`, `quinn::RecvStream` types. Copy the three structs and their impls verbatim from the server wrapper.

The only difference is the `open_channel` helper in `client.rs` (line 772) calls `conn.open_bi()`, not `accept_bi()` — both are on the trait and both delegate identically.

---

### `crates/nosh-server/src/server.rs` (MODIFIED — generics over NoshTransport)

**Analog:** itself (existing)

**`send_burst` signature change** (current lines 436–440 → after refactor):
```rust
// BEFORE (line 437):
fn send_burst(
    conn: &quinn::Connection,
    result: DiffTickResult,
    cap: usize,
) -> (Vec<DiffRun>, bool) {

// AFTER:
fn send_burst(
    conn: &dyn NoshTransport,      // trait object ref — no cloning, no heap alloc
    result: DiffTickResult,
    cap: usize,
) -> (Vec<DiffRun>, bool) {
```

All call sites within the function body are unchanged (`conn.send_datagram(...)`, `conn.datagram_send_buffer_space()`) because those method names are the same on the trait.

**`handle_connection` boxing pattern** (current lines 520–594):

The critical ordering: `extract_peer_identity` must be called on the raw `quinn::Connection` BEFORE boxing. Both `conn.peer_identity()` (line 547) and `conn.handshake_data()` (line 558) are quinn-specific and absent from `NoshTransport`. The boxing step comes after these calls:
```rust
// Current flow (lines 530–594 simplified):
let conn = /* await incoming */;
drop(permit);                                  // 1. release auth permit
let peer = conn.remote_address();              // 2. still quinn::Connection
let peer_identity = extract_peer_identity(&conn);  // 3. quinn-specific — NOT on trait
let alpn = conn.handshake_data()...;           // 4. quinn-specific — NOT on trait

// After refactor: wrap HERE, after all quinn-specific calls:
let conn: Box<dyn NoshTransport> = Box::new(QuinnTransport(conn));

let (send, mut recv) = match conn.accept_bi().await { ... };   // 5. via trait
```

**`run_session` signature change** (current lines 632–639):
```rust
// BEFORE:
async fn run_session(
    conn: quinn::Connection,
    peer: SocketAddr,
    identity: nosh_auth::NoshPublicKey,
    mut send: quinn::SendStream,
    mut recv: quinn::RecvStream,
    params: SessionOpenParams,
    registry: Arc<SessionRegistry>,
) -> anyhow::Result<()> {

// AFTER (option A — generic, preferred for stack-local conn):
async fn run_session<T: NoshTransport>(
    conn: T,          // or Box<dyn NoshTransport> passed from handle_connection
    peer: SocketAddr,
    identity: nosh_auth::NoshPublicKey,
    mut send: Box<dyn NoshSendStream>,
    mut recv: Box<dyn NoshRecvStream>,
    params: SessionOpenParams,
    registry: Arc<SessionRegistry>,
) -> anyhow::Result<()> {
```

**`run_reattach_session` signature change** (current lines 1546–1553):
```rust
// BEFORE:
async fn run_reattach_session(
    conn: quinn::Connection,
    peer: SocketAddr,
    identity: nosh_auth::NoshPublicKey,
    mut send: quinn::SendStream,
    mut recv: quinn::RecvStream,
    reattach_params: ([u8; 16], u64),
    registry: Arc<crate::registry::SessionRegistry>,
) -> anyhow::Result<()> {

// AFTER (same pattern as run_session):
async fn run_reattach_session<T: NoshTransport>(
    conn: T,
    peer: SocketAddr,
    identity: nosh_auth::NoshPublicKey,
    mut send: Box<dyn NoshSendStream>,
    mut recv: Box<dyn NoshRecvStream>,
    reattach_params: ([u8; 16], u64),
    registry: Arc<crate::registry::SessionRegistry>,
) -> anyhow::Result<()> {
```

**`nosh_proto::read_message` / `write_message` call sites** (current: take `&mut quinn::SendStream` / `&mut quinn::RecvStream`):

The existing codec functions `write_message<W: AsyncWrite + Unpin>` and `read_message<R: AsyncRead + Unpin>` (codec.rs lines 56–77) currently work because `quinn::SendStream: AsyncWrite + Unpin` and `quinn::RecvStream: AsyncRead + Unpin`. After the refactor, the streams are `Box<dyn NoshSendStream>` which does NOT implement `AsyncWrite`.

All call sites in `server.rs` (e.g., lines 671, 847, 983, 1343) must be changed to use `nosh_proto::write_message_ns` / `nosh_proto::read_message_ns`:
```rust
// BEFORE (server.rs line 671):
if nosh_proto::write_message(&mut send, &Message::SessionOpened { token: initial_token })
    .await.is_err()

// AFTER:
if nosh_proto::write_message_ns(&mut *send, &Message::SessionOpened { token: initial_token })
    .await.is_err()
```
Note the `&mut *send` dereference: `send` is `Box<dyn NoshSendStream>`, so `*send` is `dyn NoshSendStream`, and `&mut *send` is `&mut dyn NoshSendStream`.

**`conn.accept_bi()` result type change in select! arm** (lines 1285–1329 and 2061–2096):
```rust
// BEFORE (line 1287):
incoming_stream = conn.accept_bi() => {
    match incoming_stream {
        Ok((ch_send, mut ch_recv)) => {
            // ch_send: quinn::SendStream, ch_recv: quinn::RecvStream

// AFTER:
incoming_stream = conn.accept_bi() => {
    match incoming_stream {
        Ok((ch_send, mut ch_recv)) => {
            // ch_send: Box<dyn NoshSendStream>, ch_recv: Box<dyn NoshRecvStream>
            // read_varint_u32 takes &mut dyn NoshRecvStream:
            match crate::channel::read_varint_u32(&mut *ch_recv).await {
```

**`ChannelEvent::Stream` send (lines 1298, 2069):**
```rust
// BEFORE:
task_tx.send(ChannelEvent::Stream(ch_send, ch_recv)).await

// AFTER (ch_send/ch_recv are already Box<dyn ...> from accept_bi):
task_tx.send(ChannelEvent::Stream(ch_send, ch_recv)).await   // unchanged if enum updated
```

**Reset/stop on unknown channel stream (lines 1310–1312):**
```rust
// BEFORE:
let mut ch_send = ch_send;   // ch_send: quinn::SendStream
let _ = ch_send.reset(0u32.into());
ch_recv.stop(0u32.into()).ok();

// AFTER:
let _ = ch_send.reset(0);    // NoshSendStream::reset takes u32 directly
ch_recv.stop(0);             // NoshRecvStream::stop takes u32 directly
```

**Migration detection (line 743 and 890):**
```rust
// No change needed — remote_address() is on the trait:
let mut last_seen_addr: SocketAddr = conn.remote_address();   // unchanged
// ...
let cur = conn.remote_address();   // unchanged
```

**Datagram ack arm in select! (lines 952, 1829):**
```rust
// BEFORE:
datagram = conn.read_datagram() => { ... }

// AFTER: unchanged — read_datagram() is on the trait
datagram = conn.read_datagram() => { ... }
```

**`conn.close(...)` call sites (lines 554, 570, 586–592, 1444, 1458, 1570):**
```rust
// BEFORE:
conn.close(CLOSE_AUTH.into(), b"peer identity extraction failed");
// AFTER: u32 → the trait takes u32 directly:
conn.close(CLOSE_AUTH, b"peer identity extraction failed");
```

---

### `crates/nosh-server/src/channel.rs` (MODIFIED — ChannelEvent + stream types)

**Analog:** itself (existing)

**`ChannelEvent::Stream` type change** (current lines 35–46):
```rust
// BEFORE (channel.rs lines 35–46):
pub enum ChannelEvent {
    Stream(quinn::SendStream, quinn::RecvStream),
    Credit(u64),
    Close,
}

// AFTER:
use nosh_proto::transport_trait::{NoshSendStream, NoshRecvStream};

pub enum ChannelEvent {
    Stream(Box<dyn NoshSendStream>, Box<dyn NoshRecvStream>),
    Credit(u64),
    Close,
}
```

**`read_varint_u32` signature change** (current lines 69–88):
```rust
// BEFORE (channel.rs line 69):
pub async fn read_varint_u32(recv: &mut quinn::RecvStream) -> anyhow::Result<u32> {
    // body calls: recv.read_exact(&mut buf).await?
    // read_exact is already on NoshRecvStream, so body is unchanged.

// AFTER:
pub async fn read_varint_u32(recv: &mut dyn NoshRecvStream) -> anyhow::Result<u32> {
    // body unchanged — recv.read_exact(&mut buf).await? works via the trait
```

**`run_channel_task_inner` signature change** (current lines 151–155):
```rust
// BEFORE:
async fn run_channel_task_inner(
    channel_id: u32,
    ch_send: &mut quinn::SendStream,
    ch_recv: &mut quinn::RecvStream,
    events: &mut mpsc::Receiver<ChannelEvent>,
    _control_tx: &mpsc::Sender<Message>,
)

// AFTER:
async fn run_channel_task_inner(
    channel_id: u32,
    ch_send: &mut dyn NoshSendStream,
    ch_recv: &mut dyn NoshRecvStream,
    events: &mut mpsc::Receiver<ChannelEvent>,
    _control_tx: &mpsc::Sender<Message>,
)
```

**`run_scrollback_sender_task` signature change** (current lines 240–248):
```rust
// BEFORE:
pub async fn run_scrollback_sender_task(
    channel_id: u32,
    slot: Arc<crate::registry::SessionSlot>,
    ch_send: &mut quinn::SendStream,
    ch_recv: &mut quinn::RecvStream,
    events: &mut mpsc::Receiver<ChannelEvent>,
    control_tx: &mpsc::Sender<Message>,
    epoch_src: Arc<std::sync::atomic::AtomicU64>,
)

// AFTER:
pub async fn run_scrollback_sender_task(
    channel_id: u32,
    slot: Arc<crate::registry::SessionSlot>,
    ch_send: &mut dyn NoshSendStream,
    ch_recv: &mut dyn NoshRecvStream,
    events: &mut mpsc::Receiver<ChannelEvent>,
    control_tx: &mpsc::Sender<Message>,
    epoch_src: Arc<std::sync::atomic::AtomicU64>,
)
```

**`nosh_proto::codec::read_message` call in `run_scrollback_sender_task`** (line 270):
```rust
// BEFORE (channel.rs line 270):
msg = nosh_proto::codec::read_message(ch_recv) => {

// AFTER:
msg = nosh_proto::read_message_ns(ch_recv) => {
```

**`ch_send.write_all(...)` call in `run_scrollback_sender_task`** (line 402):
```rust
// BEFORE (channel.rs line 402):
if ch_send.write_all(&encoded).await.is_err() { break; }

// AFTER: unchanged — write_all is on NoshSendStream
if ch_send.write_all(&encoded).await.is_err() { break; }
```

**`ch_send.finish()` / `ch_send.stopped()` pattern** (channel.rs lines 138–139, 372–377, 390–396, 436–437):
```rust
// BEFORE (quinn::SendStream — finish() is SYNCHRONOUS):
let _ = ch_send.finish();
let _ = tokio::time::timeout(Duration::from_secs(2), ch_send.stopped()).await;

// AFTER (NoshSendStream — finish() is ASYNC; MUST .await or the half-close silently no-ops):
let _ = ch_send.finish().await;   // async fn in trait — must .await
let _ = tokio::time::timeout(Duration::from_secs(2), ch_send.stopped()).await;
```
CRITICAL: `NoshSendStream::finish` is `async fn` (the trait is object-safe via `#[async_trait]`). On a `Box<dyn NoshSendStream>` / `&mut dyn NoshSendStream`, `let _ = ch_send.finish();` (no `.await`) builds a `Pin<Box<dyn Future>>` and DROPS IT UNPOLLED — the QUIC half-close never runs, but `let _ =` suppresses the must_use warning so it compiles and tests may still pass. Every `finish()` call site at lines 138, 372, 390, 436 (and the server.rs sites 1437, 1568, 2127) MUST become `.finish().await`. `stopped`, `write_all`, `read`, `read_exact` are already awaited at their call sites and stay textually unchanged. Only the `QuinnSendStream::finish` WRAPPER body stays sync (`let _ = self.0.finish(); Ok(())`) because quinn's `finish()` is synchronous — it is the trait-object CALLERS that need `.await`.

**Scrollback task spawn in `run_session` / `run_reattach_session`** — the `ChannelEvent::Stream(s, r)` match arm inside the task spawn (server.rs lines 1134–1146, 1957–1970):
```rust
// BEFORE:
Some(ChannelEvent::Stream(s, r)) => break (s, r),
// ch_send type: quinn::SendStream, ch_recv type: quinn::RecvStream
let (mut ch_send, mut ch_recv) = ...; // from the break
run_scrollback_sender_task(..., &mut ch_send, &mut ch_recv, ...).await;

// AFTER:
Some(ChannelEvent::Stream(s, r)) => break (s, r),
// ch_send type: Box<dyn NoshSendStream>, ch_recv type: Box<dyn NoshRecvStream>
let (mut ch_send, mut ch_recv) = ...;
run_scrollback_sender_task(..., &mut *ch_send, &mut *ch_recv, ...).await;
```
The `&mut *` dereference unboxes `Box<dyn Trait>` to `&mut dyn Trait` for the function parameter.

**`run_echo_loop` signature change** (lines 453–458, `#[cfg(any(test, feature = "test-support"))]`):
```rust
// BEFORE:
async fn run_echo_loop(
    _channel_id: u32,
    ch_send: &mut quinn::SendStream,
    ch_recv: &mut quinn::RecvStream,
    events: &mut mpsc::Receiver<ChannelEvent>,
)

// AFTER:
async fn run_echo_loop(
    _channel_id: u32,
    ch_send: &mut dyn NoshSendStream,
    ch_recv: &mut dyn NoshRecvStream,
    events: &mut mpsc::Receiver<ChannelEvent>,
)
```

---

### `crates/nosh-client/src/client.rs` (MODIFIED — stream helper signatures)

**Analog:** itself (existing)

**`stream_echo_roundtrip`** (lines 225–237):
```rust
// BEFORE:
pub async fn stream_echo_roundtrip(
    conn: &quinn::Connection,
    payload: &[u8],
) -> anyhow::Result<Vec<u8>> {
    let (mut send, mut recv) = conn.open_bi().await.context("open_bi")?;
    send.write_all(payload).await.context("stream write")?;
    send.finish().context("stream finish")?;
    let echoed = recv.read_to_end(READ_LIMIT).await.context("stream read_to_end")?;
    Ok(echoed)
}
```

Note: `recv.read_to_end(READ_LIMIT)` uses `AsyncReadExt::read_to_end` from `tokio::io` which is NOT on `NoshRecvStream`. The helper must either be kept taking `&quinn::Connection` (not abstracted), or `read_to_end` must be implemented as a loop using `NoshRecvStream::read`. This is a callout for the planner.

**`datagram_roundtrip`** (lines 241–253): accesses `conn.max_datagram_size()`, `conn.send_datagram(...)`, `conn.read_datagram()` — all on the trait; trivially updated.

**`open_session`** (lines 588–608) — returns concrete `(quinn::SendStream, quinn::RecvStream)`:
```rust
// BEFORE:
pub async fn open_session(
    conn: &quinn::Connection,
    term: String,
    cols: u16,
    rows: u16,
    env: Vec<(String, String)>,
) -> anyhow::Result<(quinn::SendStream, quinn::RecvStream)> {
    let (mut send, recv) = conn.open_bi().await.context("open session stream")?;
    nosh_proto::write_message(&mut send, ...).await.context("send SessionOpen")?;
    Ok((send, recv))
}

// AFTER:
pub async fn open_session(
    conn: &dyn NoshTransport,
    term: String,
    cols: u16,
    rows: u16,
    env: Vec<(String, String)>,
) -> anyhow::Result<(Box<dyn NoshSendStream>, Box<dyn NoshRecvStream>)> {
    let (mut send, recv) = conn.open_bi().await.context("open session stream")?;
    nosh_proto::write_message_ns(&mut *send, &Message::SessionOpen { ... })
        .await.context("send SessionOpen")?;
    Ok((send, recv))
}
```

**`send_reattach`** (lines 516–524) — takes `&mut quinn::SendStream`:
```rust
// BEFORE:
pub async fn send_reattach(
    send: &mut quinn::SendStream,
    token: [u8; 16],
    last_acked_seq: u64,
) -> anyhow::Result<()> {
    nosh_proto::write_message(send, &Message::Reattach { token, last_acked_seq })

// AFTER:
pub async fn send_reattach(
    send: &mut dyn NoshSendStream,
    token: [u8; 16],
    last_acked_seq: u64,
) -> anyhow::Result<()> {
    nosh_proto::write_message_ns(send, &Message::Reattach { token, last_acked_seq })
```

**`send_ack`** (line 529), **`send_input`** (line 611), **`send_resize`** (line 623) — same pattern as `send_reattach`: replace `&mut quinn::SendStream` with `&mut dyn NoshSendStream`, replace `nosh_proto::write_message` with `nosh_proto::write_message_ns`.

**`await_reattach_reply`** (line 537), **`collect_until_close`** (line 653) — take `&mut quinn::RecvStream`:
```rust
// BEFORE:
pub async fn await_reattach_reply(recv: &mut quinn::RecvStream) -> anyhow::Result<ReattachOutcome> {
    match nosh_proto::read_message(recv).await {

// AFTER:
pub async fn await_reattach_reply(recv: &mut dyn NoshRecvStream) -> anyhow::Result<ReattachOutcome> {
    match nosh_proto::read_message_ns(recv).await {
```

**`reattach_collect`** (lines 566–585) and **`open_session_with_token`** (lines 491–507) — both call `conn.open_bi()` returning `(quinn::SendStream, quinn::RecvStream)` and then pass them to `send_reattach` / `nosh_proto::read_message`. After refactor, `open_bi()` on the trait returns `Box<dyn NoshSendStream>` / `Box<dyn NoshRecvStream>`.

**`open_channel`** (lines 760–783) — the critical `conn.open_bi()` at line 772 and the `send.write_all(&prefix)` at line 778:
```rust
// BEFORE:
pub async fn open_channel(
    conn: &quinn::Connection,
    control_send: &mut quinn::SendStream,
    control_recv: &mut quinn::RecvStream,
    ...
) -> anyhow::Result<Option<(quinn::SendStream, quinn::RecvStream)>> {
    ...
    let (mut send, recv) = conn.open_bi().await.context("open channel bidi stream")?;
    send.write_all(&prefix).await.context("write channel-id varint prefix")?;
    Ok(Some((send, recv)))

// AFTER:
pub async fn open_channel(
    conn: &dyn NoshTransport,
    control_send: &mut dyn NoshSendStream,
    control_recv: &mut dyn NoshRecvStream,
    ...
) -> anyhow::Result<Option<(Box<dyn NoshSendStream>, Box<dyn NoshRecvStream>)>> {
    ...
    let (mut send, recv) = conn.open_bi().await.context("open channel bidi stream")?;
    send.write_all(&prefix).await.context("write channel-id varint prefix")?;
    Ok(Some((send, recv)))
```

**`send_channel_open`** (lines 681–695) and **`await_channel_accept`** (lines 713–746) — same pattern: replace stream parameter types with trait objects.

---

### `crates/nosh-client/src/channel.rs` (MODIFIED — concrete stream types to trait objects)

**Analog:** `crates/nosh-server/src/channel.rs` (identical role, mirrored pattern)

**`run_channel_task`** (lines 97–163) — takes owned concrete streams:
```rust
// BEFORE:
pub async fn run_channel_task(
    channel_id: u32,
    mut ch_recv: quinn::RecvStream,
    mut ch_send: quinn::SendStream,
    control_tx: mpsc::Sender<Message>,
)

// AFTER:
pub async fn run_channel_task(
    channel_id: u32,
    mut ch_recv: Box<dyn NoshRecvStream>,
    mut ch_send: Box<dyn NoshSendStream>,
    control_tx: mpsc::Sender<Message>,
)
```
Body: `ch_recv.read(&mut buf)` — unchanged; `ch_send.finish()` / `ch_send.stopped()` — unchanged. All methods are on the traits.

**`run_scrollback_drain_task`** (lines 204–345) — same pattern:
```rust
// BEFORE:
pub async fn run_scrollback_drain_task(
    channel_id: u32,
    mut ch_recv: quinn::RecvStream,
    mut ch_send: quinn::SendStream,
    control_tx: mpsc::Sender<Message>,
    page_tx: mpsc::Sender<Message>,
    mut req_rx: mpsc::Receiver<Message>,
)

// AFTER:
pub async fn run_scrollback_drain_task(
    channel_id: u32,
    mut ch_recv: Box<dyn NoshRecvStream>,
    mut ch_send: Box<dyn NoshSendStream>,
    control_tx: mpsc::Sender<Message>,
    page_tx: mpsc::Sender<Message>,
    mut req_rx: mpsc::Receiver<Message>,
)
```

Body: `nosh_proto::write_message(&mut ch_send, &msg)` (line 223) → `nosh_proto::write_message_ns(&mut *ch_send, &msg)`.
Body: `nosh_proto::read_message(&mut ch_recv)` (line 236) → `nosh_proto::read_message_ns(&mut *ch_recv)`.

---

### `Cargo.toml` files (MODIFIED — add `async-trait` dependency)

**Analog:** existing `workspace.dependencies` in `Cargo.toml` lines 20–34.

**Pattern** (copy existing workspace dep entry style):
```toml
# workspace Cargo.toml [workspace.dependencies] — add:
async-trait = "0.1.89"

# crates/nosh-proto/Cargo.toml [dependencies] — add:
async-trait = { workspace = true }

# crates/nosh-server/Cargo.toml [dependencies] — add:
async-trait = { workspace = true }

# crates/nosh-client/Cargo.toml [dependencies] — add:
async-trait = { workspace = true }
```

---

## Shared Patterns

### `#[async_trait]` attribute placement
**Source:** All trait definitions and all `impl Trait for Type` blocks must carry `#[async_trait]`.
**Apply to:** `NoshTransport` trait definition, `NoshSendStream` trait definition, `NoshRecvStream` trait definition, `QuinnTransport` impl, `QuinnSendStream` impl, `QuinnRecvStream` impl (in both server and client wrappers).
```rust
use async_trait::async_trait;

#[async_trait]
pub trait NoshTransport: Send + Sync + 'static { ... }

#[async_trait]
impl NoshTransport for QuinnTransport { ... }
```

### `&mut *box` dereference pattern for `Box<dyn Trait>` parameters
**Apply to:** Every site where a `Box<dyn NoshSendStream>` or `Box<dyn NoshRecvStream>` is passed to a function expecting `&mut dyn NoshSendStream` or `&mut dyn NoshRecvStream`.
```rust
// Pattern: deref-coerce Box<dyn T> to &mut dyn T
run_scrollback_sender_task(..., &mut *ch_send, &mut *ch_recv, ...).await;
nosh_proto::write_message_ns(&mut *send, &msg).await
```

### quinn `VarInt` from `u32` conversion
**Source:** `server.rs` lines 554, 1311, 2080 (`0u32.into()`, `CLOSE_AUTH.into()`).
**Apply to:** `QuinnTransport::close`, `QuinnSendStream::reset`, `QuinnRecvStream::stop` wrapper impls.
```rust
// Pattern: quinn takes VarInt, trait takes u32; From<u32> is impl'd on VarInt:
self.0.close(code.into(), reason)
let _ = self.0.reset(code.into());
let _ = self.0.stop(code.into());
```

### `nosh_proto::write_message` → `nosh_proto::write_message_ns` substitution
**Apply to:** Every call site in `server.rs`, `channel.rs` (server), `client.rs`, `channel.rs` (client) that currently calls `nosh_proto::write_message(&mut send, ...)` or `nosh_proto::codec::write_message(...)` where `send` is a `Box<dyn NoshSendStream>` or `&mut dyn NoshSendStream`.

Confirmed call sites:
- `server.rs` lines 671, 847, 855–858, 862–868, 1041, 1055, 1097, 1107, 1343, 1381, 1429–1436, 1565, 1603, 1624, 2104, 2120 (and symmetrical sites in `run_reattach_session`)
- `channel.rs` (server) line 270 (`nosh_proto::codec::read_message(ch_recv)`)
- `client.rs` lines 502, 521, 530, 596–605, 612–618, 623–627, 685–694, 718, 778
- `channel.rs` (client) lines 117, 154, 223, 295, 336

### D-05 zero-test-modification gate
**Source:** `23-CONTEXT.md` decision D-05.
**Apply to:** All planning tasks. If any test (in `crates/nosh-client/tests/`) requires modification to compile after the refactor, that is evidence the refactor changed behaviour. Stop and reconsider the approach rather than patching the test.

The open question in RESEARCH.md (§"Open Questions" item 2) must be resolved before finalising the plan: audit every test file for calls on returned stream pairs that use quinn-specific methods (e.g. `recv.read_to_end(...)`) not present on `NoshRecvStream`. `stream_echo_roundtrip` (client.rs line 234) uses `recv.read_to_end(READ_LIMIT)` which is NOT on `NoshRecvStream` — this function may need to remain taking `&quinn::Connection` OR the trait must gain a `read_to_end` method.

---

## No Analog Found

No files are entirely without analog. All new files either:
- follow the module shape of `nosh-proto/src/transport.rs` (for `transport_trait.rs`), or
- directly wrap existing concrete-type call sites documented above (for the wrapper files).

The single "truly new" design element — `#[async_trait]` for object-safe async traits — has no existing analog in this codebase. Follow the RESEARCH.md guidance and the full trait surface documented in the "Pattern Assignments" section above.

---

## Metadata

**Analog search scope:** `crates/nosh-server/src/`, `crates/nosh-client/src/`, `crates/nosh-proto/src/`
**Files read:** `server.rs` (2505 lines, read in two passes), `channel.rs` (server, 587 lines), `channel.rs` (client, 447 lines), `client.rs` (784 lines), `codec.rs` (392 lines), `transport.rs` (57 lines), `lib.rs` (25 lines), `Cargo.toml` (workspace + 3 crates)
**Pattern extraction date:** 13/06/2026
