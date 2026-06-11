# Phase 21: Channel Multiplexing Foundation - Pattern Map

**Mapped:** 2026-06-11
**Files analysed:** 6 new/modified files
**Analogs found:** 6 / 6

## File Classification

| New/Modified File | Role | Data Flow | Closest Analog | Match Quality |
|-------------------|------|-----------|----------------|---------------|
| `crates/nosh-proto/src/messages.rs` | model | request-response | `crates/nosh-proto/src/messages.rs` (self — append to) | exact |
| `crates/nosh-proto/src/codec.rs` | test | request-response | `crates/nosh-proto/src/codec.rs` (existing discriminant-stability test, self — add test) | exact |
| `crates/nosh-server/src/server.rs` | service | event-driven | `crates/nosh-server/src/server.rs` (run_session select! loop — extend) | exact |
| `crates/nosh-server/src/channel.rs` | service | event-driven | `crates/nosh-server/src/server.rs` (run_session mpsc + tokio::spawn pattern) | role-match |
| `crates/nosh-client/src/client.rs` | service | request-response | `crates/nosh-client/src/client.rs` (open_bi + write_message pattern — extend) | exact |
| `crates/nosh-client/src/channel.rs` | service | event-driven | `crates/nosh-client/src/client.rs` (reattach_collect + collect_until_close) | role-match |
| `crates/nosh-client/tests/channel_mux.rs` | test | request-response | `crates/nosh-client/tests/reattach.rs` + `common/mod.rs` | exact |

---

## Pattern Assignments

### `crates/nosh-proto/src/messages.rs` — append new mux variants

**Analog:** self (append-only; existing file is the direct target)

**Imports pattern** (lines 1–11):
```rust
//! Wire message types for the nosh control protocol.
// ... (module doc unchanged)
use serde::{Deserialize, Serialize};
```

**Append-only comment pattern** (lines 56–62 — copy this comment block for the Phase 21 section header):
```rust
// ── Phase 21: Channel Multiplexing Foundation ────────────────────────────────
//
// These variants are appended AFTER `TerminalControl` to preserve the
// postcard discriminant order of all existing variants. Inserting or
// reordering is NOT backward-compatible. The discriminant-stability test in
// codec.rs (message_discriminant_order_is_stable) enforces this invariant.
// APPEND-ONLY from here.
```

**Existing enum tail to append after** (lines 154–175):
```rust
    // ── Phase 16: Out-of-band terminal control passthrough ───────────────────
    // ...
    TerminalControl(TerminalControlPayload),
}
// ↑ Phase 21 mux variants go AFTER this closing brace — i.e., BEFORE the `}`
// that closes the enum. New variants: ChannelOpen, ChannelAccept, ChannelReject,
// ChannelCredit, ChannelClose (discriminants 10–14).
```

**`variant_name` match extension pattern** (lines 219–231 — add arms for every new variant):
```rust
    pub fn variant_name(&self) -> &'static str {
        match self {
            // ... existing arms ...
            Message::TerminalControl(_) => "TerminalControl",
            // Phase 21 — add one arm per new variant:
            Message::ChannelOpen { .. } => "ChannelOpen",
            Message::ChannelAccept { .. } => "ChannelAccept",
            Message::ChannelReject { .. } => "ChannelReject",
            Message::ChannelCredit { .. } => "ChannelCredit",
            Message::ChannelClose { .. } => "ChannelClose",
        }
    }
```

**New type to add in the same file — `ChannelType` enum** (model the `TerminalControlPayload` shape at lines 182–211):
```rust
/// The logical channel type requested in a [`Message::ChannelOpen`] frame.
///
/// APPEND-ONLY: adding a variant is safe; removing or reordering corrupts
/// deployed connections (postcard encodes by source-order position).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChannelType {
    /// Test-only echo channel (NOT a production channel type).
    /// Accepted by the server only in #[cfg(test)] builds.
    Echo,
    /// Scrollback sync channel (Phase 22 consumer).
    Scrollback,
    /// Port-forward channel — declared but REJECTed by v1.3 peers (FWD-01 deferred).
    PortForward,
    /// Agent-forward channel — declared but REJECTed by v1.3 peers (FWD-02 deferred).
    AgentForward,
}
```

---

### `crates/nosh-proto/src/codec.rs` — discriminant-stability test (first commit)

**Analog:** self; the gating test goes in the existing `#[cfg(test)] mod tests` block (lines 80–262).

**Existing discriminant-stability precedent** (lines 246–260 — this is the pattern to copy and extend):
```rust
// 6. DISCRIMINANT STABILITY: encode a SessionClose (existing variant,
//    discriminant 3 in the original enum) and verify it still decodes as
//    SessionClose after the five new variants were appended. Appending to
//    the END must not shift existing discriminants.
{
    let sc = Message::SessionClose {
        exit_code: 99,
        reason: "discriminant-stability-check".to_string(),
    };
    let mut buf: Vec<u8> = Vec::new();
    write_message(&mut buf, &sc).await.expect("write SessionClose");
    let mut cursor = std::io::Cursor::new(buf);
    let got = read_message(&mut cursor).await.expect("read SessionClose after extension");
    assert_eq!(sc, got, "SessionClose discriminant must not shift after appending new variants");
}
```

**New test to add — exhaustive byte-level pin** (goes in `mod tests` AFTER `reattach_variants_round_trip`):
```rust
/// MUX-06 / Phase 21: every `Message` variant must encode with its EXACT
/// expected postcard discriminant byte. This test MUST be the first commit
/// of Phase 21 so that any reordering is caught before new variants are added.
///
/// postcard encodes enum variants as a leading varint equal to the variant's
/// 0-based source-order index. For discriminants 0..127 this is a single byte.
///
/// Count (0-based): SessionOpen=0, PtyData=1, Resize=2, SessionClose=3,
/// SessionOpened=4, Reattach=5, ReattachOk=6, ReattachErr=7, Ack=8,
/// TerminalControl=9. First Phase-21 variant = 10.
#[test]
fn message_discriminant_order_is_stable() {
    use crate::messages::{ChannelType, TerminalControlPayload};
    let cases: &[(u8, Message)] = &[
        (0, Message::SessionOpen { term: "xterm".into(), cols: 80, rows: 24, env: vec![] }),
        (1, Message::PtyData { data: vec![0x41] }),
        (2, Message::Resize { cols: 80, rows: 24 }),
        (3, Message::SessionClose { exit_code: 0, reason: String::new() }),
        (4, Message::SessionOpened { token: [0u8; 16] }),
        (5, Message::Reattach { token: [0u8; 16], last_acked_seq: 0 }),
        (6, Message::ReattachOk { new_token: [0u8; 16], replaying_from_seq: 0, truncated: false }),
        (7, Message::ReattachErr),
        (8, Message::Ack { seq: 0 }),
        (9, Message::TerminalControl(TerminalControlPayload::Title { title: String::new() })),
        // Phase 21 mux variants — discriminants 10–14 (append-only):
        (10, Message::ChannelOpen { channel_id: 2, channel_type: ChannelType::Echo }),
        (11, Message::ChannelAccept { channel_id: 2 }),
        (12, Message::ChannelReject { channel_id: 2 }),
        (13, Message::ChannelCredit { channel_id: 2, bytes: 256 * 1024 }),
        (14, Message::ChannelClose { channel_id: 2 }),
    ];
    for (expected_disc, msg) in cases {
        let encoded = postcard::to_allocvec(msg).expect("encode");
        assert_eq!(
            encoded[0], *expected_disc,
            "Message::{} must encode with discriminant {}; encoded[0] = {}",
            msg.variant_name(), expected_disc, encoded[0]
        );
    }
}
```

**Existing round-trip test to model new mux round-trip tests on** (lines 107–157 `session_variants_round_trip`):
```rust
#[tokio::test]
async fn session_variants_round_trip() {
    let msgs = [ /* variants... */ ];
    for msg in msgs {
        let mut buf: Vec<u8> = Vec::new();
        write_message(&mut buf, &msg).await.expect("write");
        let mut cursor = std::io::Cursor::new(buf);
        let got = read_message(&mut cursor).await.expect("read");
        assert_eq!(msg, got, "session variant must round-trip exactly");
    }
}
```

---

### `crates/nosh-server/src/server.rs` — extend session select! loop + add accept_bi arm

**Analog:** self (run_session and run_reattach_session — extend existing select! loops)

**Existing imports that new channel types will join** (lines 14–33):
```rust
use std::collections::VecDeque;
use std::net::SocketAddr;
// ...
use nosh_proto::{Message, TerminalControlPayload};
// Add: use std::collections::HashMap;
// Add: use crate::channel::{ChannelEvent, run_channel_task};
use quinn::crypto::rustls::{HandshakeData, QuicServerConfig};
use tokio::sync::mpsc;
```

**The one accept_bi call that exists today** (lines 557–560 — control stream accept, NOT to be moved):
```rust
// The client opens exactly one bidi stream and sends SessionOpen first.
let (send, mut recv) = match conn.accept_bi().await {
    Ok(pair) => pair,
    Err(e) => return clean_exit(e),
};
```
Note: a SECOND `accept_bi` arm for channel data streams goes inside `run_session` / `run_reattach_session` ONLY, after `drop(permit)` — never in `run_accept_loop`.

**Existing select! loop structure to add the new arms to** (lines 743–944 — the run_session loop):
```rust
let session_end: SessionEnd = loop {
    tokio::select! {
        res = &mut wait_task => { /* shell exit */ }
        chunk = out_rx.recv() => { /* PTY output */ }
        _ = migration_poll.tick() => { /* OBS-01 */ }
        _ = diff_interval.tick() => { /* SYNC-03 diff tick */ }
        datagram = conn.read_datagram() => { /* epoch ack */ }
        msg = nosh_proto::read_message(&mut recv) => {
            match msg {
                Ok(Message::PtyData { data }) => { ... }
                Ok(Message::Resize { cols, rows }) => { ... }
                Ok(Message::SessionClose { .. }) | Ok(Message::SessionOpen { .. }) => {
                    break SessionEnd::ClientClosed;
                }
                Ok(Message::Ack { seq }) => { ... }
                Ok(Message::SessionOpened { .. })
                | Ok(Message::Reattach { .. })
                | Ok(Message::ReattachOk { .. })
                | Ok(Message::ReattachErr)
                | Ok(Message::TerminalControl(_)) => {
                    break SessionEnd::ClientClosed;
                }
                Err(_) => { break SessionEnd::TransportLost; }
            }
        }
        // Phase 21: add two new arms here —
        // 1. incoming_stream = conn.accept_bi() => { read varint prefix, dispatch to channel task }
        // 2. Phase 21 mux variants added to the msg match block above.
    }
};
```

**match msg arms to add for Phase 21** (alongside existing arms at lines 898–941):
```rust
Ok(Message::ChannelOpen { channel_id, channel_type }) => {
    // Parity check: client-initiated ids must be even.
    if channel_id % 2 != 0 {
        tracing::warn!(channel_id, "client sent ChannelOpen with odd id (server parity); ignoring");
        continue;
    }
    if channel_map.contains_key(&channel_id) {
        let _ = nosh_proto::write_message(&mut send, &Message::ChannelReject { channel_id }).await;
        continue;
    }
    // Accept or reject based on channel_type.
    // ...
}
Ok(Message::ChannelCredit { channel_id, bytes }) => {
    if let Some(task_tx) = channel_map.get(&channel_id) {
        let _ = task_tx.try_send(ChannelEvent::Credit(bytes));
    }
    // Unknown id: log + ignore, never panic.
}
Ok(Message::ChannelClose { channel_id }) => {
    channel_map.remove(&channel_id);
}
// ChannelAccept / ChannelReject are server→client only; receiving them here
// is a protocol error — treat the same as other unexpected frames:
Ok(Message::ChannelAccept { .. }) | Ok(Message::ChannelReject { .. }) => {
    break SessionEnd::ClientClosed;
}
```

**mpsc channel spawn pattern to copy** (lines 685–710 — the existing in_tx / out_rx pattern):
```rust
let (out_tx, mut out_rx) = mpsc::channel::<Vec<u8>>(64);
let mut reader_handle = crate::pty_io::start_interruptible_reader(master_raw_fd, reader, out_tx)
    .expect("start interruptible PTY reader");

let (in_tx, mut in_rx) = mpsc::channel::<Vec<u8>>(64);
let writer_for_task = slot.take_pty_writer().expect("writer was just stored in slot");
let slot_for_writer = slot.clone();
let mut input_writer = tokio::task::spawn_blocking(move || {
    // ...
});
```
Channel tasks follow the same `mpsc::channel` + `tokio::spawn` shape; the channel map stores the `tx` half keyed by `channel_id`.

**send.finish() + send.stopped() half-close pattern** (lines 976–982):
```rust
let _ = send.finish();
let _ = tokio::time::timeout(Duration::from_secs(2), send.stopped()).await;
```

---

### `crates/nosh-server/src/channel.rs` — new file: per-channel server task

**Analog:** `crates/nosh-server/src/server.rs` run_session's mpsc + tokio::spawn pattern (lines 685–710, 743–944)

**File skeleton pattern** (model on the module header style of `crates/nosh-server/src/pty_io.rs`):
```rust
//! Per-logical-channel task for the server side (Phase 21 MUX-02/MUX-03).
//!
//! Each channel runs as a separate `tokio::spawn` task so it cannot block
//! the main session pump (anti-pattern: inline channel reads cause HOL blocking).

use tokio::sync::mpsc;
use nosh_proto::Message;  // for ChannelClose sent back via control_tx
```

**ChannelEvent enum** (internal type, models the existing `SessionEnd` enum pattern):
```rust
pub enum ChannelEvent {
    /// A new (SendStream, RecvStream) pair has been bound to this channel.
    Stream(quinn::SendStream, quinn::RecvStream),
    /// The peer has granted additional send credit (bytes).
    Credit(u64),
    /// Peer or local side initiated close.
    Close,
}
```

**Task function signature** (model on run_session signature at lines 608–616):
```rust
pub async fn run_channel_task(
    conn: quinn::Connection,
    channel_id: u32,
    events: mpsc::Receiver<ChannelEvent>,
    // control_tx: used to send ChannelClose back to the session pump for cleanup.
    control_tx: mpsc::Sender<...>,
)
```

---

### `crates/nosh-client/src/client.rs` — extend with ChannelOpen send + open_bi prefix write

**Analog:** self (lines 562–607 `reattach_collect` / `open_session` — follow same open_bi + write_message shape)

**open_bi + write pattern to replicate** (lines 594–607 `open_session`):
```rust
pub async fn open_session(
    conn: &quinn::Connection,
    term: String,
    cols: u16,
    rows: u16,
    env: Vec<(String, String)>,
) -> anyhow::Result<(quinn::SendStream, quinn::RecvStream)> {
    let (mut send, recv) = conn.open_bi().await.context("open session stream")?;
    nosh_proto::write_message(
        &mut send,
        &Message::SessionOpen { term, cols, rows, env },
    )
    .await
    .context("send SessionOpen")?;
    Ok((send, recv))
}
```
New `open_channel` function follows this shape: `open_bi().await`, write `ChannelOpen` on the CONTROL stream (not the new stream), await `ChannelAccept` on the control stream recv, THEN write varint channel-id prefix on the new bidi stream.

**read_message await-reply pattern to replicate** (lines 536–551 `await_reattach_reply`):
```rust
pub async fn await_reattach_reply(recv: &mut quinn::RecvStream) -> anyhow::Result<ReattachOutcome> {
    match nosh_proto::read_message(recv).await {
        Ok(Message::ReattachOk { new_token, replaying_from_seq, truncated }) =>
            Ok(ReattachOutcome::Ok { new_token, replaying_from_seq, truncated }),
        Ok(Message::ReattachErr) => Ok(ReattachOutcome::Err),
        Ok(other) => anyhow::bail!("unexpected reply to Reattach: {}", other.variant_name()),
        Err(e) => anyhow::bail!("failed to read reattach reply: {e}"),
    }
}
```
`await_channel_accept` follows this exactly: match on `ChannelAccept` / `ChannelReject`, use `variant_name()` in bail! (never Debug).

**send_reattach function to replicate for send_channel_open** (lines 515–522):
```rust
pub async fn send_reattach(send: &mut quinn::SendStream, token: [u8; 16], last_acked_seq: u64)
-> anyhow::Result<()> {
    nosh_proto::write_message(send, &Message::Reattach { token, last_acked_seq })
        .await
        .context("send Reattach")
}
```
`send_channel_open` is the same shape: write `ChannelOpen { channel_id, channel_type }` on the CONTROL stream's SendStream.

---

### `crates/nosh-client/src/channel.rs` — new file: per-channel client task

**Analog:** `crates/nosh-client/src/client.rs` `collect_until_close` (lines 635–670 approx) + reattach_collect mpsc pattern

**collect_until_close shape to model** (lines ~635–670 area — read_message loop with timeout):
```rust
pub async fn run_session_collect(
    conn: &quinn::Connection,
    // ...
) -> anyhow::Result<(Vec<u8>, i32)> {
    // ... open_bi, then:
    loop {
        match nosh_proto::read_message(recv).await {
            Ok(Message::PtyData { data }) => { output.extend_from_slice(&data); }
            Ok(Message::SessionClose { exit_code, .. }) => { return Ok((output, exit_code)); }
            Ok(_) => {}
            Err(_) => { return Ok((output, -1)); }
        }
    }
}
```
Client channel task is the same: a loop that reads frames from the channel's RecvStream, sends credit back via an mpsc sender when the buffer is drained, and exits on EOF.

---

### `crates/nosh-client/tests/channel_mux.rs` — new integration test file

**Analog:** `crates/nosh-client/tests/reattach.rs` (full file above) + `common/mod.rs` (full file above)

**Test file header pattern** (reattach.rs lines 1–23):
```rust
//! Phase 21 channel-multiplexing integration tests — Roadmap success criteria
//! SC#1–SC#6 for MUX-01…MUX-06.

use std::sync::Arc;
use std::time::Duration;

use nosh_client::client::{self};
use nosh_server::registry::SessionRegistry;

mod common;
use common::{spawn_server_with_registry, TestKey, HOST};

const SH: &str = "/bin/sh";

fn have_sh() -> bool {
    std::path::Path::new(SH).exists()
}
```

**Server + client setup pattern to reuse** (reattach.rs lines 30–51 — `server_with_key` + `client_endpoint_for`):
```rust
async fn server_with_key(registry: Arc<SessionRegistry>, client_key: &TestKey) -> common::TestServer {
    let host_key = TestKey::generate();
    spawn_server_with_registry(
        &host_key,
        &[&client_key.public],
        nosh_server::server::AuthLimits::default(),
        Some(SH.to_string()),
        registry,
    ).await
}

fn client_endpoint_for(key: &TestKey) -> (quinn::Endpoint, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let kh = dir.path().join("known_hosts");
    let ep = common::client_endpoint(key.client_identity(), kh).unwrap();
    (ep, dir)
}
```

**Orphan-wait pattern to reuse** (reattach.rs lines 216–226):
```rust
let orphan_deadline = std::time::Instant::now() + Duration::from_secs(5);
loop {
    if registry.total_orphans() >= 1 { break; }
    if std::time::Instant::now() > orphan_deadline {
        panic!("server did not orphan within 5s (cycle {cycle})");
    }
    tokio::time::sleep(Duration::from_millis(25)).await;
}
```

**drain loop (ignoring non-target frames) pattern** (reattach.rs lines 167–196 `drain_n`):
```rust
match tokio::time::timeout(Duration::from_millis(400), nosh_proto::read_message(recv)).await {
    Ok(Ok(nosh_proto::Message::PtyData { data })) => { /* handle */ }
    Ok(Ok(_)) => { /* ignore non-PtyData control frames */ }
    Ok(Err(_)) => return false, // stream closed
    Err(_) => { /* idle timeout */ }
}
```

**open_bi reattach sequence to model channel-reattach test on** (reattach.rs lines 232–248):
```rust
let (mut send2, mut recv2) = conn2.open_bi().await.expect("open bi");
client::send_reattach(&mut send2, token, counter.last_acked_seq())
    .await
    .expect("send reattach");
let outcome = client::await_reattach_reply(&mut recv2)
    .await
    .expect("await_reattach_reply");
```
Channel-reattach test (MUX-05) follows this shape: after `ReattachOk`, send `ChannelOpen`, await `ChannelAccept`.

---

## Shared Patterns

### Append-only enum variant rule
**Source:** `crates/nosh-proto/src/messages.rs` lines 56–62 comment block
**Apply to:** `messages.rs` (new variants), `codec.rs` (stability test)
```rust
// These variants are appended AFTER `<previous last variant>` to preserve the
// postcard discriminant order of all existing variants. Inserting or
// reordering is NOT backward-compatible.
```

### Never log message payload — use `variant_name()`
**Source:** `crates/nosh-client/src/client.rs` lines 503, 549 and `crates/nosh-server/src/server.rs` lines 576
**Apply to:** all new `match msg` arms in server.rs, client.rs, channel.rs
```rust
// W3 / D-07: never Debug a frame (could carry a token) — use the variant name:
Ok(other) => anyhow::bail!("unexpected frame: {}", other.variant_name()),
tracing::warn!(%peer, frame = other.variant_name(), "unexpected frame");
```

### No-panic on unknown/unexpected ids
**Source:** `crates/nosh-server/src/server.rs` lines 888–890
**Apply to:** `ChannelAccept`, `ChannelReject`, `ChannelCredit`, `ChannelClose` handlers in server.rs and channel.rs
```rust
Ok(_) => {} // ignore unexpected frames (log + continue, never panic or unwrap)
Err(_) => { break SessionEnd::TransportLost; }
```

### tokio::select! arm shape — break vs continue
**Source:** `crates/nosh-server/src/server.rs` lines 744–944
**Apply to:** run_session and run_reattach_session extended select! loops
```rust
// Use `break SessionEnd::Variant` to exit the loop (fatal conditions).
// Use `continue` to skip to the next iteration (non-fatal: unknown frames, log+skip).
// The new accept_bi arm reads ONLY the varint prefix, never channel payload inline.
```

### send.finish() + send.stopped() stream close
**Source:** `crates/nosh-server/src/server.rs` lines 976–982
**Apply to:** `crates/nosh-server/src/channel.rs` channel task cleanup, `crates/nosh-client/src/channel.rs` channel close
```rust
let _ = send.finish();
let _ = tokio::time::timeout(Duration::from_secs(2), send.stopped()).await;
```

### mpsc channel for cross-task communication (not shared Mutex)
**Source:** `crates/nosh-server/src/server.rs` lines 685–710
**Apply to:** `channel.rs` (both client and server) — channel tasks communicate with the session pump via mpsc, never via shared state
```rust
let (out_tx, mut out_rx) = mpsc::channel::<Vec<u8>>(64);
// task holds rx; session pump holds tx (or vice versa).
// Channel map: HashMap<u32, mpsc::Sender<ChannelEvent>> — no Mutex needed.
```

---

## No Analog Found

All files have direct analogs in the codebase. No entries.

---

## Metadata

**Analog search scope:** `crates/nosh-proto/src/`, `crates/nosh-server/src/`, `crates/nosh-client/src/`, `crates/nosh-client/tests/`
**Files scanned:** 34 Rust source files
**Pattern extraction date:** 2026-06-11
