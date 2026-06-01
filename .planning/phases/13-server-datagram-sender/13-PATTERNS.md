# Phase 13: Server Datagram Sender - Pattern Map

**Mapped:** 2026-06-02
**Files analyzed:** 4
**Analogs found:** 4 / 4

---

## File Classification

| New/Modified File | Role | Data Flow | Closest Analog | Match Quality |
|-------------------|------|-----------|----------------|---------------|
| `crates/nosh-proto/src/datagram.rs` | protocol/codec | transform | itself (additive: new tag + encode/decode pair) | exact — same file, same tag/decode pattern |
| `crates/nosh-server/src/server.rs` | service/pump | event-driven | itself (additive: new `select!` arm) | exact — same file, `migration_poll` arm is the template |
| `crates/nosh-server/src/registry.rs` | service/model | CRUD | itself (additive: `with_terminal_state` delegate) | exact — same file, existing delegate pattern |
| `crates/nosh-client/tests/sync.rs` | test | request-response | `crates/nosh-client/tests/reattach.rs` + `transport.rs` | role-match — same test harness, similar in-process pattern |

---

## Pattern Assignments

### `crates/nosh-proto/src/datagram.rs` — additive: TAG_CLIENT_EPOCH + ClientEpoch + decode_epoch_ack/encode_epoch_ack

**Analog:** Same file. The existing `TAG_STATE_DIFF` / `decode_datagram` pair is the exact template.

**Tag constant pattern** (lines 21-22 — reserved comment to activate):
```rust
const TAG_STATE_DIFF: u8 = 0x01;
// const TAG_CLIENT_EPOCH: u8 = 0x02; // reserved for Phase 13 (ClientEpoch, client → server)
```
Phase 13 UNCOMMENTS `TAG_CLIENT_EPOCH = 0x02`.

**Imports pattern** (lines 14-17 — already present, no new imports needed):
```rust
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use crate::codec::ProtoError;
```

**Wire type pattern** — copy `StateDiff` shape for the new `ClientEpoch` type (lines 49-75):
```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateDiff {
    pub epoch: u64,
    // ...
}
```
`ClientEpoch` follows the same shape: a `#[derive(Debug, Clone, Copy, Serialize, Deserialize)]` struct with a single `pub epoch: u64` field.

**Decode function pattern** (lines 377-394 — `decode_datagram` is the exact template):
```rust
pub fn decode_datagram(bytes: &[u8]) -> Result<StateDiff, ProtoError> {
    let (tag, body) = bytes.split_first().ok_or_else(|| {
        ProtoError::Postcard(postcard::Error::DeserializeUnexpectedEnd)
    })?;
    if *tag != TAG_STATE_DIFF {
        return Err(ProtoError::Postcard(
            postcard::Error::DeserializeBadEncoding,
        ));
    }
    let diff: StateDiff = postcard::from_bytes(body).map_err(ProtoError::Postcard)?;
    // (MAX_RUNS guard is StateDiff-specific; ClientEpoch has no runs vec so no guard needed)
    Ok(diff)
}
```
`decode_epoch_ack` mirrors this: split tag, check `TAG_CLIENT_EPOCH`, deserialize `ClientEpoch`, return `Ok(ce.epoch)`.

**Encode function pattern** (lines 361-364 — final payload assembly in `encode_datagram`):
```rust
let mut payload = Vec::with_capacity(1 + body.len());
payload.push(TAG_STATE_DIFF);
payload.extend_from_slice(&body);
Ok((Bytes::from(payload), deferred_runs))
```
`encode_epoch_ack` follows the same tag-prefix pattern with `TAG_CLIENT_EPOCH`.

---

### `crates/nosh-server/src/server.rs` — additive: diff_interval arm in run_session + run_reattach_session; epoch-ack read_datagram arm; ResumeComplete gate

**Analog:** Same file. The `migration_poll` interval arm (lines 398-445) is the load-bearing template.

**Imports already present** (lines 14-27) — no new imports needed except `Duration` and `MissedTickBehavior` which are already used:
```rust
use std::time::Duration;
// tokio::time::interval and MissedTickBehavior::Skip are already used via migration_poll
```

**Interval initialization pattern** (lines 398-399 — exact template for diff_interval):
```rust
let mut migration_poll = tokio::time::interval(Duration::from_millis(500));
migration_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
```
`diff_interval` initialization copies this shape:
```rust
let mut diff_interval = tokio::time::interval(Duration::from_millis(16));
diff_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
```

**select! interval arm pattern** (lines 434-445 — migration_poll arm):
```rust
_ = migration_poll.tick() => {
    let cur = conn.remote_address();
    if cur != last_seen_addr {
        tracing::info!(
            session_id = %session_id,
            old = %last_seen_addr,
            new = %cur,
            "connection migrated"
        );
        last_seen_addr = cur;
    }
}
```
`diff_interval` arm copies this `_ = interval.tick() => { ... }` shape, with the ResumeComplete guard at the top.

**TransportLost break pattern** (lines 799-801 — in run_reattach_session loop):
```rust
if nosh_proto::write_message(&mut send, &Message::PtyData { data })
    .await
    .is_err()
{
    break SessionEnd::TransportLost;
}
```
`conn.send_datagram(payload)` error handling uses the same break pattern for `ConnectionLost` and `UnsupportedByPeer`/`Disabled`.

**push_output_and_parse callsite** (line 416 in run_session, line 795 in run_reattach_session):
```rust
slot.push_output_and_parse(&data);
```
The epoch increment in Phase 13 belongs in the PTY output arm alongside (or just after) this call.

**ResumeComplete gate for run_reattach_session** — the replay completion site (line 719-723):
```rust
tracing::info!(
    replaying_from_seq,
    chunks = chunks.len(),
    truncated,
    "replay complete"
);
```
A plain `bool resume_complete = false` declared before the loop is set `true` immediately after this log line. Both the replay code and the `select!` loop are in the same async function (`run_reattach_session`), so no shared primitive is needed.

**ResumeComplete gate for run_session** — set `let resume_complete = true;` before the loop begins (no replay in run_session).

**send_datagram error handling pattern** — from `crates/nosh-client/src/client.rs` line 249:
```rust
conn.send_datagram(payload).context("send_datagram")?;
```
Server side uses explicit match (not `?`) to distinguish `ConnectionLost` (break `SessionEnd::TransportLost`) from `TooLarge` (skip tick, not fatal) from `UnsupportedByPeer`/`Disabled` (break — configuration error):
```rust
match e {
    quinn::SendDatagramError::TooLarge => { /* encode_datagram guarantees this doesn't fire */ }
    quinn::SendDatagramError::UnsupportedByPeer
    | quinn::SendDatagramError::Disabled => break SessionEnd::TransportLost,
    quinn::SendDatagramError::ConnectionLost(_) => break SessionEnd::TransportLost,
}
```

**read_datagram in select! pattern** — from `crates/nosh-client/src/client.rs` line 250:
```rust
let echoed = conn.read_datagram().await.context("read_datagram")?;
```
Server side uses a named select! arm:
```rust
datagram = conn.read_datagram() => {
    match datagram {
        Ok(bytes) => { /* decode epoch-ack */ }
        Err(_) => break SessionEnd::TransportLost,
    }
}
```

---

### `crates/nosh-server/src/registry.rs` — additive: with_terminal_state delegate on SessionSlot

**Analog:** Same file. Existing delegate pattern on `SessionSlot` (lines 369-383 for `push_output_and_parse`, lines 434-436 for `replay_from`).

**Delegate method pattern** (lines 434-436 — `replay_from` delegate):
```rust
pub fn replay_from(&self, last_acked_seq: u64) -> (Vec<(u64, Bytes)>, u64, bool) {
    self.output_buf.lock().unwrap().replay_from(last_acked_seq)
}
```

**Mutex lock discipline** (lines 378-382 in `push_output_and_parse`) — poison-recovery pattern:
```rust
self.terminal_state
    .lock()
    .unwrap_or_else(|e| e.into_inner())
    .advance(chunk);
```

**`with_terminal_state` delegate shape** — the new method follows the same pattern. It accepts a closure `F: FnOnce(&TerminalState) -> R`, locks briefly, applies the closure, and returns:
```rust
/// Read-only access to the terminal state via a closure.
/// The lock is held only for the duration of `f` — NEVER across `.await`.
/// (Anti-Pattern #2: no lock held across await points.)
pub fn with_terminal_state<F, R>(&self, f: F) -> R
where
    F: FnOnce(&TerminalState) -> R,
{
    let ts = self.terminal_state.lock().unwrap_or_else(|e| e.into_inner());
    f(&ts)
}
```
This matches the lock-discipline pattern established by `push_output_and_parse` and `resize`.

**Lock order comment** (lines 241-244 — doc comment on `terminal_state` field):
```rust
/// Lock order: always acquire `output_buf` lock before `terminal_state` lock.
/// Never hold either lock across `.await` (Anti-Pattern #2).
terminal_state: Mutex<TerminalState>,
```
The `with_terminal_state` method doc must repeat the "NEVER across `.await`" invariant.

---

### `crates/nosh-client/tests/sync.rs` — NEW integration test file (SYNC-03)

**Analog:** `crates/nosh-client/tests/reattach.rs` (reattach test pattern) + `crates/nosh-client/tests/transport.rs` (datagram test pattern).

**File-level module boilerplate** (reattach.rs lines 1-23):
```rust
//! Phase 6 cold-reattach integration tests ...

use std::sync::Arc;
use std::time::Duration;

use nosh_client::client::{self, ReattachOutcome};
use nosh_server::registry::SessionRegistry;

mod common;
use common::{spawn_server_with_registry, TestKey, HOST};

const SH: &str = "/bin/sh";

fn have_sh() -> bool {
    std::path::Path::new(SH).exists()
}
```
`sync.rs` uses the same boilerplate with different imports:
```rust
//! Phase 13 SYNC-03 integration tests — server datagram sender.

use std::time::Duration;

use bytes::Bytes;
use nosh_client::client;
use nosh_proto::datagram::{decode_datagram, encode_epoch_ack};
use nosh_server::registry::SessionRegistry;
use nosh_server::server::AuthLimits;

mod common;
use common::{TestKey, HOST};

const SH: &str = "/bin/sh";

fn have_sh() -> bool {
    std::path::Path::new(SH).exists()
}
```

**Server spawn pattern** (reattach.rs lines 30-43 — `server_with_key`):
```rust
async fn server_with_key(
    registry: Arc<SessionRegistry>,
    client_key: &TestKey,
) -> common::TestServer {
    let host_key = TestKey::generate();
    spawn_server_with_registry(
        &host_key,
        &[&client_key.public],
        nosh_server::server::AuthLimits::default(),
        Some(SH.to_string()),
        registry,
    )
    .await
}
```
`sync.rs` uses the same shape (or `spawn_server_with_shell` from `transport.rs` lines 27-42 if registry access is not needed).

**Client endpoint construction** (reattach.rs lines 46-51):
```rust
fn client_endpoint_for(key: &TestKey) -> (quinn::Endpoint, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let kh = dir.path().join("known_hosts");
    let ep = common::client_endpoint(key.client_identity(), kh).unwrap();
    (ep, dir)
}
```

**Test function skeleton** (transport.rs lines 56-62 — `#[tokio::test]` form):
```rust
#[tokio::test]
async fn datagram_enabled() {
    let h = spawn_server().await;
    let (_ep, conn) = connect(&h).await;
    assert!(
        conn.max_datagram_size().is_some(),
        "datagrams must be enabled/negotiated (max_datagram_size Some)"
    );
}
```

**Session interaction pattern** — `common::session_marker_usable` (common/mod.rs lines 233-244) shows how to drive a session by sending a shell script and collecting output. For `sync.rs`, test clients must:
1. Open a session via `client::run_session_open`
2. Send input via the reliable stream
3. Race `conn.read_datagram()` against a timeout to assert datagrams arrive

**Datagram read pattern** (client.rs lines 249-251):
```rust
conn.send_datagram(payload).context("send_datagram")?;
let echoed = conn.read_datagram().await.context("read_datagram")?;
```
In `sync.rs`, the test client reads datagrams directly with a `tokio::time::timeout` wrapper:
```rust
let bytes = tokio::time::timeout(
    Duration::from_secs(5),
    conn.read_datagram(),
)
.await
.expect("datagram arrived within 5s")
.expect("no connection error");
let diff = decode_datagram(&bytes).expect("valid StateDiff datagram");
assert!(!diff.runs.is_empty());
```

**Epoch-ack emission pattern** (unique to sync.rs — using the new `encode_epoch_ack`):
```rust
let ack = encode_epoch_ack(diff.epoch);
conn.send_datagram(ack).expect("send epoch-ack");
```

**`have_sh()` skip pattern** (reattach.rs line 26 + inline at top of each test):
```rust
if !have_sh() {
    eprintln!("skipping test_name: {SH} not available");
    return;
}
```

---

## Shared Patterns

### Mutex Lock Discipline (Anti-Pattern #2)
**Source:** `crates/nosh-server/src/registry.rs` lines 236-244, 369-383
**Apply to:** All places in server.rs that acquire `slot.terminal_state` via `with_terminal_state`
```rust
// CORRECT: snapshot under lock, then drop lock before any .await
let (cols, rows, cursor, cells) = slot.with_terminal_state(|ts| {
    let (cols, rows) = ts.size();
    let cursor = ts.cursor();
    let cells: Vec<Vec<Cell>> = ts.viewport_rows()
        .map(|(_, row)| row.to_vec())
        .collect();
    (cols, rows, cursor, cells)
});
// Lock is now released. Safe to .await below.
```

### Postcard tag-prefix encode/decode
**Source:** `crates/nosh-proto/src/datagram.rs` lines 377-394 (decode) and 360-364 (encode assembly)
**Apply to:** `decode_epoch_ack` and `encode_epoch_ack` in datagram.rs
```rust
// Decode: split_first → check tag → postcard::from_bytes
let (tag, body) = bytes.split_first().ok_or_else(|| ProtoError::Postcard(...))?;
if *tag != TAG_CLIENT_EPOCH { return Err(...); }
let val: ClientEpoch = postcard::from_bytes(body).map_err(ProtoError::Postcard)?;

// Encode: postcard::to_allocvec → prepend tag → Bytes::from
let body = postcard::to_allocvec(&val).map_err(ProtoError::Postcard)?;
let mut payload = Vec::with_capacity(1 + body.len());
payload.push(TAG_CLIENT_EPOCH);
payload.extend_from_slice(&body);
Bytes::from(payload)
```

### SessionEnd::TransportLost break pattern
**Source:** `crates/nosh-server/src/server.rs` line 800
**Apply to:** `send_datagram` error arm and `read_datagram` error arm in both session loops
```rust
Err(_) => break SessionEnd::TransportLost,
```

### tokio::time::interval + MissedTickBehavior::Skip
**Source:** `crates/nosh-server/src/server.rs` lines 398-399
**Apply to:** `diff_interval` initialization in both `run_session` and `run_reattach_session`
```rust
let mut diff_interval = tokio::time::interval(Duration::from_millis(16));
diff_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
```

### unwrap_or_else(|e| e.into_inner()) for Mutex poison recovery
**Source:** `crates/nosh-server/src/registry.rs` lines 378-381
**Apply to:** All `terminal_state.lock()` calls (via `with_terminal_state` delegate)
```rust
self.terminal_state
    .lock()
    .unwrap_or_else(|e| e.into_inner())
```

---

## Key Structural Observations for Planner

1. **`terminal_state` field is private** in `SessionSlot` (line 244 of registry.rs — no `pub`/`pub(crate)` modifier). The server.rs connection task cannot access it directly. A `pub fn with_terminal_state<F, R>(&self, f: F) -> R` delegate must be added to `SessionSlot`. This is the only new method needed on `SessionSlot`.

2. **`CursorPos` import in terminal.rs** (line 38): `use nosh_proto::datagram::{CellStyle, CursorPos};` — `CursorPos` is already imported from `nosh_proto::datagram` into `nosh-server`, so Phase 13 code in server.rs can use it directly after `use nosh_proto::datagram::CursorPos` in the server.rs imports.

3. **`Cell` type import path**: `crate::terminal::Cell` (nosh-server crate). Phase 13 adds `use crate::terminal::Cell;` to server.rs imports when declaring `last_acked_snapshot: Vec<Vec<Cell>>`.

4. **encode_epoch_ack returns `Bytes`** — `conn.send_datagram` accepts `Bytes` directly. No conversion needed.

5. **Both `run_session` and `run_reattach_session` need identical `diff_interval` arm logic** — the only difference is `resume_complete` initialization (`true` for `run_session`, `false` for `run_reattach_session` until after replay).

6. **`MAX_RUNS` is already `pub` in datagram.rs** (line 26) — the deferred-queue cap `pending_deferred.len() > MAX_RUNS` can reference it directly from server.rs as `nosh_proto::datagram::MAX_RUNS`.

7. **The `encode_epoch_ack` function can be infallible** — `postcard::to_allocvec` on a 9-byte struct (`u8` tag + up to 8 bytes for a `u64` varint) cannot realistically fail. Use `.expect("postcard")` as the existing tests do (see datagram.rs `tag_encode` test helper at line 477-483).

---

## No Analog Found

All four files have direct analogs or are additive to existing files. No file requires patterns from RESEARCH.md only.

---

## Metadata

**Analog search scope:** `crates/nosh-proto/src/`, `crates/nosh-server/src/`, `crates/nosh-client/src/`, `crates/nosh-client/tests/`
**Files scanned:** 8 (datagram.rs, registry.rs, server.rs, terminal.rs, client.rs, tests/common/mod.rs, tests/transport.rs, tests/reattach.rs)
**Pattern extraction date:** 2026-06-02
