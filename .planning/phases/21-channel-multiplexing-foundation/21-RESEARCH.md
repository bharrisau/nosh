# Phase 21: Channel Multiplexing Foundation - Research

**Researched:** 2026-06-11
**Domain:** QUIC channel multiplexing, postcard wire stability, per-channel flow control, quinn stream lifecycle
**Confidence:** HIGH

---

<user_constraints>
## User Constraints (from CONTEXT.md)

### Locked Decisions

**Channel-id allocation (MUX-04) — disjoint parity spaces (client-even / server-odd)**
Each side owns a disjoint id space so simultaneous opens from both ends cannot collide. Direction is fixed by ROADMAP success criterion 2: client-initiated channels use EVEN ids, server-initiated channels use ODD ids. Channel id 0 is reserved for the control channel (the existing session control stream). OPEN/ACCEPT/REJECT are new appended `Message` variants on it, NOT a new stream.

**Stream binding (MUX-02) — Channel-id varint prefix on the new stream**
After the control-channel handshake (OPEN → ACCEPT), the opener writes the channel-id as a varint at the very start of the freshly opened QUIC bidi stream; the receiver maps stream→channel on first read, before any channel payload. Channel state must never be keyed on the transport stream id (they change semantics across migration).

**Per-channel flow control (MUX-03) — 256 KiB byte-credit window**
Credit-based windows counted in bytes. Initial window 256 KiB per channel; credits replenish as the consumer drains its buffer. Byte-granular (not message-count).

**Echo channel (proving fixture) — Test-only, not a shipped channel type**
The echo channel lives only in integration tests. Not registered as a production channel type.

**Discriminant stability (MUX-06) — gating first commit**
The discriminant-stability enforcement test for the `Message` enum is the FIRST commit of the phase. New mux variants are appended AFTER `TerminalControl`. Postcard encodes variants by positional discriminant index (0-based).

**REJECT is opaque (MUX-01)**
REJECT carries no reason code.

### Claude's Discretion

None declared in CONTEXT.md.

### Deferred Ideas (OUT OF SCOPE)

- Port forwarding (FWD-01), agent forwarding (FWD-02), file transfer (XFER-01): declared/REJECTed this milestone, not implemented.
- Server-initiated channels (odd-id space): reserved by parity scheme, not exercised until a feature needs them.
</user_constraints>

<phase_requirements>
## Phase Requirements

| ID | Description | Research Support |
|----|-------------|------------------|
| MUX-01 | Logical channels negotiated control-first — id 0 carries OPEN/ACCEPT/REJECT before a data stream is bound; REJECT is opaque | New `ChannelOpen`/`ChannelAccept`/`ChannelReject` Message variants on existing control stream; varint read-ahead on new streams |
| MUX-02 | Multiple logical channels run concurrently over single QUIC connection, each on its own stream, no HOL blocking | `conn.open_bi()` / `conn.accept_bi()` per channel; separate tokio tasks per channel; stream priority on control vs data |
| MUX-03 | Per-channel application-level flow control (credit-based) so a slow consumer cannot stall the connection | 256 KiB byte-credit window; `ChannelCredit` control message; channel send task checks credit before writing |
| MUX-04 | Clean channel lifecycle — half-close, full-close release resources; rejected/closed leaks nothing; id parity prevents simultaneous-open collisions | `SendStream::finish()` + `RecvStream` read-to-EOF; `HashMap<u32, ChannelState>` cleaned on REJECT receipt; even/odd parity |
| MUX-05 | Channels survive QUIC migration transparently; cold reattach re-establishes via control channel (not byte-replayed) | QUIC migration transparent at transport; on cold reattach, client re-sends OPEN after ReattachOk |
| MUX-06 | Wire format stable — `Message` discriminant order is append-only, enforced by discriminant-stability test as first commit | postcard 0-indexed discriminant; TerminalControl = discriminant 9; new variants start at 10; test encodes each variant and asserts expected byte |
</phase_requirements>

---

## Summary

Phase 21 introduces a control-first logical channel layer over the single QUIC connection. The work is net-new (no existing channel infrastructure to refactor) but integrates tightly with the existing `Message` enum, the session control stream, and the cold-reattach machinery.

The dominant technical risk is discriminant corruption: postcard serialises `Message` variants by their 0-based source-order position. `TerminalControl` is currently at position 10 (discriminant **9**). New mux variants appended after it take discriminants 10, 11, 12. Inserting anywhere before `TerminalControl` silently corrupts every deployed connection. The first commit of the phase is a test that pins this.

The second risk is HOL blocking. Each logical channel must run on its own QUIC bidi stream and its own tokio task. Putting channel data reads into the main session `select!` loop would reintroduce application-level HOL blocking — if a scrollback consumer is slow, it would stall PTY output. The architecture is: control channel runs on the existing control-stream arm of `select!`; each data channel is a separate `tokio::spawn` task.

Per-channel flow control is an application-layer concern because QUIC's `stream_receive_window` is set uniformly at endpoint build time (not per stream instance). A 256 KiB byte-credit window sent via `ChannelCredit` frames on the control stream paces slow consumers without touching QUIC's transport-level window.

Channel re-establishment on cold reattach is straightforward: channels are ephemeral per-connection. `SequencedOutputBuffer` never replays `ChannelOpen`/`ChannelAccept` frames. After `ReattachOk`, the client re-sends `ChannelOpen` for any channel it wants.

**Primary recommendation:** Commit the discriminant-stability test first, then wire OPEN/ACCEPT/REJECT, then per-stream channel binding, then credit flow control, then the echo-channel integration test. This order keeps each commit independently verifiable.

---

## Architectural Responsibility Map

| Capability | Primary Tier | Secondary Tier | Rationale |
|------------|-------------|----------------|-----------|
| Channel negotiation (OPEN/ACCEPT/REJECT) | Server session task | Client session task | Control messages ride the existing bidirectional session control stream; both ends process them in their respective `read_message` arms |
| Per-channel data transport | Separate per-channel tokio task (server + client) | — | Each channel has its own `open_bi`/`accept_bi` pair; running data I/O inline in the main loop causes HOL blocking (Pitfall M-2) |
| Channel-id parity enforcement | Both endpoints independently | — | Client allocates from even counter; server from odd counter; no shared state needed |
| Credit flow control | Server channel sender task | Client control stream | Server tracks credit per channel; client sends `ChannelCredit` frames on the control stream to replenish |
| Stream lifecycle (half-close/full-close) | Owning channel task | — | `SendStream::finish()` for half-close; drop/EOF for full-close; resources released in channel map on close |
| Channel re-establishment after cold reattach | Client (initiates re-open) | Server (accepts) | Client sends `ChannelOpen` after `ReattachOk`; server's channel state was cleared on orphan transition |
| QUIC migration (IP change) | QUIC transport layer | — | All open streams migrate transparently; zero application-layer work needed for MUX-05's migration sub-requirement |
| Discriminant stability enforcement | nosh-proto test suite | CI | `message_discriminant_order_is_stable` in `crates/nosh-proto/src/messages.rs` |

---

## Standard Stack

### Core

No new crates are needed for this phase. All required APIs exist in the current workspace dependencies.

| Library | Version | Purpose | Why Standard |
|---------|---------|---------|--------------|
| `quinn` | 0.11.9 (workspace) | `open_bi`, `accept_bi`, `SendStream::finish`, `RecvStream` EOF, `set_priority` | Already the QUIC transport; multistream is native |
| `postcard` | 1.x (workspace) | Serialise `Message` enum variants including new mux variants | Existing codec; discriminant ordering is the stability invariant |
| `tokio` | 1.52.x (workspace) | Per-channel tasks via `tokio::spawn`; `mpsc::channel` for channel data routing | Already the async runtime |
| `bytes` | 1.x (workspace) | Varint encoding/decoding for channel-id prefix on new streams | Already a quinn transitive dep |

### Supporting

| Library | Version | Purpose | When to Use |
|---------|---------|---------|-------------|
| `tracing` | 0.1.x (workspace) | Log channel open/accept/reject/close events with channel_id | Already in use throughout the codebase |

### Alternatives Considered

| Instead of | Could Use | Tradeoff |
|------------|-----------|----------|
| Application-level channel-id varint prefix | Raw QUIC stream IDs for channel identification | QUIC stream IDs are connection-local and change meaning across connection migration. Application-level channel-id prefix is stable across migration (locked decision). |
| New `Message` variants on control stream | Separate control stream for mux | Locked against: the existing control stream IS channel 0. Adding a second stream just for mux control adds complexity without benefit. |
| Byte-credit flow control | Message-count credit | Byte credits handle variable-size payloads (scrollback pages) cleanly. Message counts would require estimating message size. |

**No new installation is required.** All dependencies are already in the workspace.

---

## Package Legitimacy Audit

No new packages are installed in this phase. All dependencies are existing workspace members.

**Packages removed due to slopcheck [SLOP] verdict:** none
**Packages flagged as suspicious [SUS]:** none

---

## Architecture Patterns

### System Architecture Diagram

```
Client                              Server
  |                                   |
  |  QUIC Connection (single)         |
  |-----------------------------------|
  |  Control stream (bidi, ch 0)      |
  |  [Message: ChannelOpen]  -------> |  session select! loop
  |  <------ [Message: ChannelAccept] |
  |                                   |
  |  New bidi stream: varint(ch_id)   |
  |  + channel payload  ---------->  |  tokio::spawn(channel_task)
  |  <---------- channel payload      |  (per channel, independent)
  |                                   |
  |  [Message: ChannelCredit]  -----> |  replenishes send credit
  |                                   |
  |  [Message: ChannelReject]  <----- |  (opaque, no reason code)
  |                                   |
  |  SendStream::finish() ----------> |  half-close
  |  <------- read-to-EOF            |  full-close
  |                                   |
  | QUIC datagrams (StateDiff)        |  unchanged, independent
  |  <-------------------------->     |
```

### Recommended Project Structure

No new crates or top-level directories are needed. Changes are confined to:

```
crates/nosh-proto/src/
├── messages.rs       # New ChannelOpen/ChannelAccept/ChannelReject/ChannelCredit variants (append-only)
└── lib.rs            # Export new types

crates/nosh-server/src/
├── server.rs         # accept_bi loop in select!; channel map; channel task spawning
└── channel.rs        # (new) per-channel task logic, credit tracking

crates/nosh-client/src/
├── client.rs         # open_bi + varint prefix write; ChannelOpen send; re-open on reattach
└── channel.rs        # (new) client channel task logic

crates/nosh-client/tests/
└── channel_mux.rs    # (new) integration tests: echo channel, simultaneous open, reattach
```

### Pattern 1: Discriminant-Stability Test

**What:** Encode each `Message` variant with postcard and assert the first byte of the encoded body matches the expected discriminant value.

**When to use:** This is the gating first commit. It must cover both existing variants (to pin them) and new mux variants (to confirm their position).

**Example:**
```rust
// Source: [ASSUMED] — based on postcard crate encoding semantics + existing codebase pattern
// in crates/nosh-proto/src/codec.rs (reattach_variants_round_trip discriminant check at line 247).
// postcard encodes enum variants as a leading varint equal to the variant's 0-based
// source-order index. For variants 0..127, this is a single byte.
#[test]
fn message_discriminant_order_is_stable() {
    use postcard::to_allocvec;
    // Variant → (discriminant, representative value)
    let cases: &[(u8, Message)] = &[
        (0,  Message::SessionOpen { term: "xterm".into(), cols: 80, rows: 24, env: vec![] }),
        (1,  Message::PtyData { data: vec![0x41] }),
        (2,  Message::Resize { cols: 80, rows: 24 }),
        (3,  Message::SessionClose { exit_code: 0, reason: String::new() }),
        (4,  Message::SessionOpened { token: [0u8; 16] }),
        (5,  Message::Reattach { token: [0u8; 16], last_acked_seq: 0 }),
        (6,  Message::ReattachOk { new_token: [0u8; 16], replaying_from_seq: 0, truncated: false }),
        (7,  Message::ReattachErr),
        (8,  Message::Ack { seq: 0 }),
        (9,  Message::TerminalControl(TerminalControlPayload::Title { title: String::new() })),
        // Phase 21: mux variants — appended after TerminalControl (discriminant 9).
        (10, Message::ChannelOpen { channel_id: 2, channel_type: ChannelType::Echo }),
        (11, Message::ChannelAccept { channel_id: 2 }),
        (12, Message::ChannelReject { channel_id: 2 }),
        (13, Message::ChannelCredit { channel_id: 2, bytes: 256 * 1024 }),
        (14, Message::ChannelClose { channel_id: 2 }),
    ];
    for (expected_disc, msg) in cases {
        let encoded = to_allocvec(msg).expect("encode");
        assert_eq!(
            encoded[0], *expected_disc,
            "Message::{} must encode with discriminant {}; got {}",
            msg.variant_name(), expected_disc, encoded[0]
        );
    }
}
```

**Note:** The exact variant names and `ChannelType` definition are implementation decisions for the planner. What matters is the pattern: hardcode the expected discriminant byte for every variant in the enum, both old and new.

### Pattern 2: Control Stream Message Dispatch

**What:** The existing `select!` loop reads `Message` frames from the control stream. New mux variant arms are added to the existing `match msg` block.

**When to use:** Handling `ChannelOpen`, `ChannelAccept`, `ChannelReject`, `ChannelCredit`, and `ChannelClose` on the server.

**Example:**
```rust
// Source: [ASSUMED] — extending the existing server.rs select! pattern (lines 896–944)
// in crates/nosh-server/src/server.rs
Ok(Message::ChannelOpen { channel_id, channel_type }) => {
    // Validate id parity: client-initiated = even.
    if channel_id % 2 != 0 {
        // Server-initiated ids are odd; client sending an odd id is a protocol error.
        // Log and ignore rather than panic (Pitfall M-4).
        tracing::warn!(channel_id, "client sent ChannelOpen with odd channel_id (server parity); ignoring");
        continue;
    }
    if channel_map.contains_key(&channel_id) {
        // Duplicate open: reject.
        let _ = nosh_proto::write_message(&mut send, &Message::ChannelReject { channel_id }).await;
        continue;
    }
    // Verify channel type is supported.
    match channel_type {
        ChannelType::Scrollback => { /* Phase 22 */ }
        ChannelType::Echo => {
            // Test-only: accept in integration tests only; reject in production builds.
            #[cfg(not(test))]
            {
                let _ = nosh_proto::write_message(&mut send, &Message::ChannelReject { channel_id }).await;
                continue;
            }
        }
        ChannelType::PortForward | ChannelType::AgentForward => {
            // Declared but rejected by v1.3 peers (CONTEXT.md / ROADMAP security note).
            let _ = nosh_proto::write_message(&mut send, &Message::ChannelReject { channel_id }).await;
            continue;
        }
    }
    // Send accept, record in channel map, spawn task.
    if nosh_proto::write_message(&mut send, &Message::ChannelAccept { channel_id }).await.is_ok() {
        // Channel task: tokio::spawn — NEVER inline in the select! loop (Pitfall M-2).
        let (to_channel_tx, to_channel_rx) = mpsc::channel(64);
        channel_map.insert(channel_id, to_channel_tx);
        tokio::spawn(run_channel_task(conn.clone(), channel_id, to_channel_rx));
    }
}
Ok(Message::ChannelCredit { channel_id, bytes }) => {
    // Replenish send credit for the channel sender task.
    if let Some(task_tx) = channel_map.get(&channel_id) {
        let _ = task_tx.try_send(ChannelEvent::Credit(bytes));
    }
}
Ok(Message::ChannelClose { channel_id }) => {
    channel_map.remove(&channel_id);
}
```

### Pattern 3: Varint Channel-ID Prefix on New Streams

**What:** After the control-channel handshake, the opener writes the channel-id as a QUIC varint at the head of the new bidi stream. The acceptor reads it before treating the rest of the stream as channel payload.

**When to use:** Binding a freshly opened QUIC stream to a channel (opener side) and identifying it (acceptor side).

**Example:**
```rust
// Source: [ASSUMED] — derived from quinn 0.11 SendStream/RecvStream API + postcard varint encoding
// The channel-id prefix is a postcard-encoded u32 varint (1–5 bytes for ids 0..u32::MAX).
// Using postcard::to_io() or manual varint write.

// --- Opener (client side, after ChannelAccept received) ---
let (mut ch_send, ch_recv) = conn.open_bi().await?;
// Write channel-id prefix: postcard varint of the channel id.
let id_bytes = postcard::to_allocvec(&channel_id)?;
ch_send.write_all(&id_bytes).await?;
// Stream is now ready for channel payload.

// --- Acceptor (server secondary accept loop) ---
// A separate task / select! arm that calls conn.accept_bi().
// After accept, read the channel-id prefix before handing off to channel logic.
let (ch_send, mut ch_recv) = conn.accept_bi().await?;
// Read varint: postcard varint for u32 is 1-5 bytes.
// Simplest: read a small buffer and deserialize.
let mut prefix_buf = [0u8; 5];
// Read just enough bytes for a varint (1–5).
// In practice, read 1 byte; if top bit is set, read more (varint continuation).
let channel_id: u32 = read_varint_u32(&mut ch_recv).await?;
// Look up channel_id in the pending-accept map.
```

**Note on varint encoding:** `postcard`'s varint encoding of a `u32` uses the least-significant 7 bits per byte with the high bit as a continuation flag (same as protobuf LEB128 encoding). For ids 0–127 (the realistic range in v1.3) this is a single byte. A `read_varint_u32` helper reads at most 5 bytes. [ASSUMED]

### Pattern 4: Secondary Stream Accept Loop

**What:** The server needs to accept incoming bidi streams from the client (for channel data) concurrently with the existing control-stream read loop. This is a second `accept_bi` arm, not a blocking loop.

**When to use:** Server-side stream acceptance, running as a separate select! arm or a dedicated tokio task.

**Example:**
```rust
// Source: [ASSUMED] — extending server.rs select! or spawning a separate accept task.
// CRITICAL: this arm MUST NOT run before authentication completes (Pitfall: auth gate).
// The existing handle_connection() already drops the pre-auth permit before dispatching
// to run_session(); the accept_bi arm belongs inside run_session() or run_reattach_session(),
// AFTER authentication has completed.

// Option A: add to the existing select! loop in run_session():
incoming_stream = conn.accept_bi() => {
    match incoming_stream {
        Ok((ch_send, mut ch_recv)) => {
            // Read channel-id varint prefix.
            match read_varint_u32(&mut ch_recv).await {
                Ok(channel_id) => {
                    // Hand to the appropriate pending channel task.
                    // The channel task was spawned on ChannelOpen ACCEPT above.
                    if let Some(task_tx) = channel_map.get(&channel_id) {
                        let _ = task_tx.try_send(ChannelEvent::Stream(ch_send, ch_recv));
                    }
                    // If no pending entry: the stream arrived before the ACCEPT was
                    // processed, or the channel was already closed — ignore/close stream.
                }
                Err(_) => {} // Malformed varint: ignore, don't panic (Pitfall M-4).
            }
        }
        Err(quinn::ConnectionError::ApplicationClosed(_) | quinn::ConnectionError::LocallyClosed) => {
            // Connection is closing: break the loop.
            break SessionEnd::TransportLost;
        }
        Err(_) => {} // Transient error: ignore.
    }
}
```

### Pattern 5: Cold Reattach Channel Re-Establishment

**What:** On cold reattach, all channel state is cleared. After `ReattachOk`, the client re-sends `ChannelOpen` for each channel it wants. The server's channel map is empty after the `Reconnecting → Active` transition.

**When to use:** MUX-05 re-establishment.

**Key sequence:**
```
Client                              Server
  | --- Reattach { token, seq } --> |
  | <-- ReattachOk { ... }          |  (channel_map cleared on orphan transition)
  | --- ChannelOpen { id: 2, ... -> |  re-open channels from scratch
  | <-- ChannelAccept { id: 2 } --- |
  | (channel operates normally)     |
```

The important constraint: the replay of `SequencedOutputBuffer` chunks (PtyData frames) occurs BEFORE the client sends ChannelOpen. The existing `replaying_from_seq` / `ReattachOk` machinery is unchanged. Channel re-establishment is a post-replay action.

### Anti-Patterns to Avoid

- **Inline channel data reads in the main select! loop:** Any `recv.read_buf()` or `read_message()` from a non-control stream inside the main session `select!` loop will stall all other arms when that stream is empty or its peer is slow. Per Pitfall M-2, each data channel runs in a dedicated `tokio::spawn` task.
- **Keying channel state on QUIC stream IDs:** Quinn does not expose stable application-visible stream IDs across migration. Always key on the application-level `channel_id` varint, not on any internal `StreamId`. The varint-prefix binding is how we resolve stream → channel without relying on QUIC internals.
- **Accumulating `ChannelOpen`/`ChannelAccept` frames in `SequencedOutputBuffer`:** Control frames are not PTY data. The `push_output_and_parse` path is for raw PTY bytes only. Mux control frames on the control stream must never be fed to the sequenced buffer.
- **Accepting streams before authentication completes:** The secondary `accept_bi` arm belongs inside `run_session` / `run_reattach_session`, after `drop(permit)`. The pre-auth accept loop in `run_accept_loop` opens exactly one stream (the control stream); all secondary stream acceptance happens after that.
- **Panicking on unknown or already-closed channel IDs:** `ChannelAccept`/`ChannelReject`/`ChannelCredit` for an unknown ID must be logged and ignored, never panicked (Pitfall M-4).

---

## Don't Hand-Roll

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| Varint encoding for channel-id prefix | Custom byte-packing | `postcard::to_allocvec(&channel_id_u32)` | postcard's varint is already used throughout the codebase; consistent, tested |
| Stream flow control | Custom byte-window state machine | Per-channel `remaining_credit: u64` counter, decremented on send, replenished by `ChannelCredit` messages | Simple and sufficient; QUIC's transport-layer window handles the lower-level byte throttle |
| Channel map concurrency | A `Mutex<HashMap>` shared across tasks | `tokio::sync::mpsc::channel` per channel task (task-local state) | The channel map lives in the single session task; each channel task communicates via its own mpsc sender. No shared mutable state needed. |
| Stream lifecycle coordination | Custom state machine with multiple handles | `SendStream::finish()` for initiating half-close; `RecvStream` EOF detection | Quinn's stream API already implements RFC 9000 half-close/full-close; no custom machinery needed. |

**Key insight:** QUIC already provides isolated streams with no application-level HOL blocking. The job is to wire channel negotiation cleanly and keep channel data tasks separate — not to build a transport.

---

## Common Pitfalls

### Pitfall 1: Discriminant Index Off-by-One (TerminalControl is Discriminant 9, Not 10)

**What goes wrong:** The STATE.md TODO says "confirm `TerminalControl` is currently discriminant 10 (not 9)". Counting the `Message` variants in source order (0-indexed): SessionOpen=0, PtyData=1, Resize=2, SessionClose=3, SessionOpened=4, Reattach=5, ReattachOk=6, ReattachErr=7, Ack=8, TerminalControl=9. `TerminalControl` is discriminant **9** (0-based). The first mux variant is discriminant **10**. Writing the stability test with the wrong expected value silently passes when the discriminants are wrong and then fails on first real use.

**Why it happens:** Off-by-one confusion between "10th variant in the enum" and "discriminant value 10". Position 10 in 1-based counting = discriminant 9 in 0-based encoding.

**How to avoid:** Count variants in the source file manually (as done in this research), or encode the enum in a test before writing any new variant and observe the actual first byte.

**Warning signs:** `message_discriminant_order_is_stable` test asserts `TerminalControl` has discriminant `10` but postcard actually encodes it as `9`. The test will fail before any new variants are added.

### Pitfall 2: Auth Bypass via Secondary Stream Accept

**What goes wrong:** A second `accept_bi` arm that runs in `run_accept_loop` (before authentication) could allow an unauthenticated client to open data channels. This would bypass the `AuthLimits` semaphore and potentially exhaust server resources with unauthenticated connections.

**Why it happens:** Copying the accept pattern from `run_session` into an earlier code path.

**How to avoid:** The secondary stream accept arm belongs exclusively inside `run_session` / `run_reattach_session`, after `drop(permit)`. The `handle_connection` function already enforces the auth gate; never add stream accepts before that gate. The ROADMAP security note is explicit: "The secondary stream accept loop must NOT open streams before authentication completes and must NOT bypass the AuthLimits semaphore."

**Warning signs:** A `conn.accept_bi()` call appearing before `run_session` is entered; or a `conn.accept_bi()` in `run_accept_loop`.

### Pitfall 3: HOL Blocking from Inline Channel Data Reads (Pitfall M-2)

**What goes wrong:** A `conn.accept_bi()` arm in the main `select!` loop that then reads channel payload inline stalls every other select arm (PTY output, diff tick, datagram ack) when the channel stream is empty. The 16 ms diff tick starts missing. PTY input latency exceeds 5 ms under a saturated channel (SC#3 violation).

**Why it happens:** Convenience — it looks natural to handle all stream I/O in one select! loop.

**How to avoid:** The `accept_bi` arm reads only the channel-id varint prefix (a handful of bytes), then hands the `(SendStream, RecvStream)` pair to a pre-spawned channel task via an mpsc message. All subsequent data I/O happens in that task.

**Warning signs:** `conn.accept_bi().await` resolves inside the main session loop and then reads channel data inline before returning to the select!. Any `loop` or streaming read on a non-control channel inside the session pump.

### Pitfall 4: Replaying Channel Control Frames on Reattach (Pitfall M-5)

**What goes wrong:** After a cold reattach, the `SequencedOutputBuffer` replays `PtyData` frames from `last_acked_seq`. If `ChannelOpen`/`ChannelAccept` frames were accidentally interleaved with `PtyData` in the buffer and replayed, the client would misparse them as shell output (wrong frame type) or open duplicate channels (double-open).

**Why it happens:** Confusion about what lives in `SequencedOutputBuffer`. It stores only raw PTY byte chunks (the shell's stdout). Control stream messages (`Message` enum frames) are never buffered in the sequenced output buffer.

**How to avoid:** `SequencedOutputBuffer::push_output_and_parse` is only ever called with raw PTY bytes. Channel control messages go through `nosh_proto::write_message` on the control stream's `SendStream` and are not stored in the sequenced buffer. On cold reattach, the server clears all channel state (channel map becomes empty) when the slot transitions to `Reconnecting`. The client re-opens channels after `ReattachOk`.

**Warning signs:** Any code that calls `slot.push_output_and_parse` with a `Message`-encoded frame; or channel-id state that survives across a `registry.orphan()` call.

### Pitfall 5: Channel Map Grows Without Bound (Pitfall M-4)

**What goes wrong:** A `ChannelAccept` or `ChannelReject` arrives for a channel-id that has already been removed from the pending map (e.g. the initiator timed out and closed the channel client-side). If the code panics on `channel_map.get(&id).unwrap()`, the session terminates. If it inserts a new entry without first receiving `ChannelOpen`, the map grows indefinitely.

**Why it happens:** Incorrect assumption that the protocol guarantees strict ordering of Accept/Reject after Open on the reliable stream. Within a single reliable stream, ordering is guaranteed (a QUIC bidi stream is ordered). The issue is that the *application* state machine may have already removed the entry before the reply arrives (e.g. a local timeout).

**How to avoid:** `ChannelAccept`/`ChannelReject` for unknown IDs are no-ops (log + continue). Never panic. Never insert a channel entry on ACCEPT if no corresponding OPEN is in the pending map.

**Warning signs:** `channel_map.get(&channel_id).expect(...)` or `.unwrap()` anywhere that handles Accept/Reject messages from the peer.

### Pitfall 6: Credit Window Exhaustion Deadlock

**What goes wrong:** The server sends data up to the 256 KiB credit window and then blocks, waiting for the client to send a `ChannelCredit` message. The client is processing data from the channel stream in a task that does not return to the control-stream read loop until the task drains the data. If the control-stream read loop is also blocked waiting for channel data (anti-pattern: inline HOL blocking), neither side makes progress.

**Why it happens:** The client's channel drain loop and the control-stream read loop are on the same task, so draining the channel blocks the credit replenishment path.

**How to avoid:** The client's channel task runs independently (`tokio::spawn`); it sends `ChannelCredit` frames back through the control stream via its own `mpsc` channel to the main control-stream write path. The control-stream write path is non-blocking (mpsc channel with backpressure, not blocking write). This keeps the credit replenishment path active regardless of channel data drain rate.

**Warning signs:** `ChannelCredit` messages sent inline inside a `while let Some(data) = ch_recv.read()` loop inside the session pump. Or a channel task that calls `write_message` on the control stream directly (creates a second writer for the control stream, which would corrupt framing).

---

## Code Examples

### Postcard Discriminant Encoding Pattern

```rust
// Source: [VERIFIED: crates/nosh-proto/src/codec.rs reattach_variants_round_trip]
// The existing discriminant check (lines ~247-260) encodes SessionClose and asserts it
// decodes correctly after new variants were appended. The mux stability test follows
// the same pattern but asserts the raw discriminant byte explicitly.

// The first byte of a postcard-encoded enum is the varint discriminant.
// For discriminants 0..127, this is a single byte equal to the discriminant value.
let encoded = postcard::to_allocvec(&msg).unwrap();
let discriminant_byte = encoded[0];
assert_eq!(discriminant_byte, expected, "discriminant must not shift");
```

### quinn SendStream Half-Close

```rust
// Source: [VERIFIED: crates/nosh-server/src/server.rs lines 976-982]
// Existing usage of send.finish() + send.stopped() for clean half-close.
let _ = send.finish();
// Wait up to 2s for peer to consume the finished stream (optional but recommended for clean teardown).
let _ = tokio::time::timeout(Duration::from_secs(2), send.stopped()).await;
```

### quinn accept_bi Pattern

```rust
// Source: [VERIFIED: crates/nosh-server/src/server.rs line 557]
// Existing usage: conn.accept_bi().await returns (SendStream, RecvStream)
let (send, mut recv) = match conn.accept_bi().await {
    Ok(pair) => pair,
    Err(e) => return clean_exit(e),
};
```

### quinn open_bi Pattern

```rust
// Source: [VERIFIED: crates/nosh-client/src/client.rs]
// Existing usage: conn.open_bi().await
let (mut send, mut recv) = conn.open_bi().await.context("open_bi")?;
```

### Channel Re-Establishment Sequence After Reattach

```rust
// Source: [ASSUMED] — derived from existing run_reattach_session() pattern in server.rs
// After ReattachOk is sent and replay is complete (resume_complete = true),
// the server's channel_map starts empty (cleared on slot.orphan()).
// The client sends ChannelOpen after receiving ReattachOk:
//
// Client-side:
let mut channel_map = HashMap::new(); // cleared on new connection
// After reading ReattachOk:
for ch_type in channels_to_restore {
    let id = next_even_channel_id();
    write_message(&mut control_send, &Message::ChannelOpen { channel_id: id, channel_type: ch_type }).await?;
    // Wait for ChannelAccept before opening the QUIC stream.
}
```

---

## Runtime State Inventory

> Skip: this is a greenfield feature phase, not a rename/refactor/migration.

---

## State of the Art

| Old Approach | Current Approach | When Changed | Impact |
|--------------|------------------|--------------|--------|
| Single bidi stream for all session traffic | Multiple streams per channel type (one per logical channel) | Phase 21 (this phase) | Enables scrollback sync, future agent/port forwarding without HOL blocking |
| Flat `Message` enum (10 variants, discriminant 0–9) | Extended enum with mux control variants (discriminants 10+) | Phase 21 (this phase) | New variants are append-only to preserve wire stability of deployed v1.2 sessions |
| No per-channel flow control | 256 KiB byte-credit window per channel | Phase 21 (this phase) | Slow scrollback consumer cannot stall shell output |

**Deprecated/outdated:**
- None in this phase. The change is strictly additive.

---

## Assumptions Log

| # | Claim | Section | Risk if Wrong |
|---|-------|---------|---------------|
| A1 | Postcard varint encoding of u32 uses a single byte for values 0–127 (LEB128-style) | Pattern 3, varint section | Channel-id prefix reader must handle multi-byte varints; a hardcoded 1-byte read would break for ids > 127 (unlikely in v1.3 but the reader should be correct) |
| A2 | `TerminalControl` is discriminant 9 (position 10 in 1-based, position 9 in 0-based). First mux variant is discriminant 10 | Pitfall 1, SC#1 analysis | Discriminant stability test would assert wrong values, either failing immediately or silently accepting a wrong ordering |
| A3 | The existing `run_session` / `run_reattach_session` select! loop can accommodate a `conn.accept_bi()` arm without architectural changes to the select! macro invocation | Pattern 4 | tokio select! with 6+ arms is fine; no practical limit |
| A4 | The channel task communicates with the main control-stream write path via mpsc, not by directly calling write_message | Pattern 2, Pitfall 6 | A channel task calling write_message directly on the control-stream SendStream from a separate task would cause concurrent writes and corrupt framing; the mpsc design is correct but must be enforced at review |
| A5 | `postcard` encodes struct variants with named fields (e.g. `ChannelOpen { channel_id: u32, channel_type: ChannelType }`) as: discriminant varint + field values in declaration order | Pattern 1, wire format | Wrong encoding would mean the stability test does not catch field-order changes; but postcard's serde-derive does use declaration order |

---

## Open Questions

1. **Exact `ChannelType` enum variants for v1.3**
   - What we know: Echo (test-only), Scrollback (Phase 22 consumer), PortForward (declared/rejected), AgentForward (declared/rejected) are the v1.3 variants.
   - What's unclear: Should `ChannelType` be defined in `nosh-proto` (wire types) or `nosh-server`/`nosh-client`? Since it is part of the `ChannelOpen` wire message, it belongs in `nosh-proto`.
   - Recommendation: Define `ChannelType` in `nosh-proto/src/messages.rs` alongside `Message`. Add a `// APPEND-ONLY` comment.

2. **Who sends `ChannelCredit` and on what stream?**
   - What we know: Credit is application-level flow control. The client grants the server permission to send more bytes on a given channel.
   - What's unclear: Is `ChannelCredit` a `Message` variant on the control stream (same as OPEN/ACCEPT/REJECT), or does it go on the channel's own bidi stream as in-band framing?
   - Recommendation: Put `ChannelCredit` on the control stream (same as OPEN/ACCEPT/REJECT). This keeps all channel management on one stream and avoids in-band framing complexity on data streams.

3. **Who re-opens the TTY channel on reattach?**
   - What we know: The STATE.md Pending Todos entry says "decide exact who-reopens-TTY-channel-on-reattach contract" — client sends `ChannelOpen { type: Tty }` after `ReattachOk`.
   - Recommendation: Client initiates — this is consistent with the client-even-id parity rule (client-initiated channels use even ids). The server does not proactively re-open. After `ReattachOk`, the client sends `ChannelOpen { type: Pty }` to re-bind PTY input/output to the new connection's data stream.

4. **Does the PTY data channel move to a separate bidi stream in this phase?**
   - What we know: SUMMARY.md says "PTY data moves to a second bidi stream"; ROADMAP says "new quinn streams per channel (not in-stream framing)" and lists the control stream as the first bidi stream.
   - What's unclear: Phase 21 scope says the echo channel proves the layer. Whether PTY I/O moves to its own channel in Phase 21 or remains on stream 0 alongside control messages is a scoping question with significant implementation impact.
   - Recommendation: Leave PTY I/O on the existing stream in Phase 21 (the echo channel proves the mux, not PTY migration). Migrating PTY to its own channel is Phase 22 prep and can be done as part of the scrollback phase when a second data channel is needed anyway. This keeps Phase 21 bounded.

5. **`TerminalControl` discriminant — confirm before writing the test**
   - Research finding: Counting variants in source order: `TerminalControl` is the 10th variant (1-based) = discriminant **9** (0-based). The STATE.md Pending Todo says "confirm... discriminant 10 (not 9)". Research has verified it is discriminant 9.
   - Recommendation: The discriminant-stability test must assert `TerminalControl` encodes with discriminant byte `9u8`, and the first new mux variant encodes with discriminant byte `10u8`. Confirm by running `postcard::to_allocvec(&Message::TerminalControl(...)).unwrap()[0]` before writing the assertions.

---

## Environment Availability

> Step 2.6: SKIPPED — no external dependencies beyond the project's own crates. All required APIs (`quinn` 0.11.9, `postcard` 1.x, `tokio` 1.x) are already compiled into the workspace.

---

## Validation Architecture

### Test Framework

| Property | Value |
|----------|-------|
| Framework | `cargo test` + `cargo nextest` (where available) |
| Config file | none (workspace default) |
| Quick run command | `cargo test -p nosh-proto && cargo test -p nosh-server` |
| Full suite command | `cargo test --workspace` |

### Phase Requirements → Test Map

| Req ID | Behaviour | Test Type | Automated Command | File Exists? |
|--------|-----------|-----------|-------------------|-------------|
| MUX-06 | Every `Message` variant encodes with the expected discriminant byte | unit | `cargo test -p nosh-proto message_discriminant_order_is_stable` | ❌ Wave 0 |
| MUX-01 | `ChannelOpen` on control stream yields `ChannelAccept` or `ChannelReject` before any data stream is bound; REJECT is opaque | integration | `cargo test -p nosh-client channel_open_accept_reject` | ❌ Wave 0 |
| MUX-02 | Echo channel delivers data end-to-end; PTY input latency < 5 ms during channel saturation | integration | `cargo test -p nosh-client channel_echo_roundtrip` | ❌ Wave 0 |
| MUX-03 | Credit window exhaustion blocks sender; `ChannelCredit` replenishes; PTY unaffected | integration | `cargo test -p nosh-client channel_flow_control_backpressure` | ❌ Wave 0 |
| MUX-04 | Rejected/closed channels release resources; `ChannelAccept` for unknown id is no-op | integration | `cargo test -p nosh-client channel_lifecycle_clean` | ❌ Wave 0 |
| MUX-04 SC#6 | Client-open and server-open fired simultaneously: no collision, session survives | integration | `cargo test -p nosh-client channel_simultaneous_open` | ❌ Wave 0 |
| MUX-05 | After cold reattach, channel re-opened via control stream and delivers data | integration | `cargo test -p nosh-client channel_reattach_reopen` | ❌ Wave 0 |

### Sampling Rate

- Per task commit: `cargo test -p nosh-proto && cargo test -p nosh-server -- --test-threads=1`
- Per wave merge: `cargo test --workspace`
- Phase gate: `cargo test --workspace` green before `/gsd:verify-work`

### Wave 0 Gaps

- [ ] `crates/nosh-client/tests/channel_mux.rs` — covers MUX-01, MUX-02, MUX-04 (echo channel lifecycle)
- [ ] `crates/nosh-proto/src/messages.rs` — `message_discriminant_order_is_stable` test (MUX-06); this is the first commit
- [ ] `crates/nosh-server/src/channel.rs` — channel task module (new file)
- [ ] `crates/nosh-client/src/channel.rs` — client channel task module (new file)

---

## Security Domain

### Applicable ASVS Categories

| ASVS Category | Applies | Standard Control |
|---------------|---------|-----------------|
| V2 Authentication | no | Auth pre-exists; this phase adds no new auth surface |
| V3 Session Management | yes | Channel state cleared on orphan/reattach; no channel state leaks across sessions |
| V4 Access Control | yes | Declared-but-rejected channel types (FWD-01, FWD-02); secondary stream accept after auth only |
| V5 Input Validation | yes | Unknown channel IDs, malformed varint prefix, oversized ChannelType enum values — all must be no-ops, not panics |
| V6 Cryptography | no | QUIC TLS 1.3 already handles channel data encryption; no new crypto |

### Known Threat Patterns for This Stack

| Pattern | STRIDE | Standard Mitigation |
|---------|--------|---------------------|
| Unauthenticated channel open (secondary accept before auth) | Elevation of Privilege | `accept_bi` arm only inside `run_session`/`run_reattach_session`, after `drop(permit)` |
| Channel-id exhaustion (open many channels without closing) | Denial of Service | Enforce a max open channel count (e.g. 64); reject `ChannelOpen` when at limit |
| `SSH_AUTH_SOCK` forwarded via new channel env vars | Information Disclosure | No new channel type forwards agent socket; FWD-02 is declared-rejected in v1.3 |
| Malformed varint in channel-id prefix | Tampering / Crash | `read_varint_u32` returns `Err` on malformed input; stream is closed, not panicked |
| `ChannelOpen` with unknown `ChannelType` variant | Tampering | Treat unknown enum variant as a `REJECT` case; postcard's serde-derive returns `Err` on unknown discriminant; handle gracefully |
| Port forward channel type accepted before feature is complete | Elevation of Privilege | `ChannelType::PortForward` and `ChannelType::AgentForward` unconditionally rejected by v1.3 peers; enforced with `ChannelReject` in control message handler |

---

## Project Constraints (from CLAUDE.md)

| Directive | Impact on This Phase |
|-----------|---------------------|
| Rust locked as sole implementation language | All channel infrastructure in Rust; no C/FFI |
| Transport: QUIC over UDP/443, one connection per session | New channels are additional streams on the existing connection; no new connections |
| Environment sanitization on every shell/exec | No new exec paths in this phase |
| `SSH_AUTH_SOCK` never forwarded via environment | FWD-02 declared-rejected; no env var forwarding in any new channel type |
| Security baked in from M2: AuthLimits semaphore not bypassed | Secondary stream accept exclusively post-auth |
| Platform: Linux only this milestone | ConPTY / Windows channel concerns are M6 |
| No AI-cliché prose, Australian English | Code comments and docs to follow project style |
| GSD workflow enforcement: no direct edits outside GSD | Changes delivered via plan → execute → verify cycle |

---

## Sources

### Primary (HIGH confidence)

- `crates/nosh-proto/src/messages.rs` (this session, direct read) — `Message` enum variant count, TerminalControl position (discriminant 9), existing discriminant stability check pattern in codec.rs
- `crates/nosh-server/src/server.rs` (this session, direct read) — `handle_connection`, `run_session`, `run_reattach_session`, `accept_bi` usage, `AuthLimits` semaphore, orphan/reattach lifecycle
- `.planning/research/PITFALLS.md` (this session, direct read) — Pitfalls M-1 through M-6 (verbatim codebase-grounded analysis)
- `.planning/research/SUMMARY.md` (this session, direct read) — load-bearing decisions, mux design, build order rationale
- `.planning/research/STACK.md` (this session, direct read) — quinn 0.11.9 multistream API surface (`open_bi`/`accept_bi`/`set_priority`/stream flow control)
- `.planning/phases/21-channel-multiplexing-foundation/21-CONTEXT.md` (this session, direct read) — all locked decisions
- `.planning/ROADMAP.md` Phase 21 section (this session, direct read) — success criteria SC#1–SC#6
- `.planning/REQUIREMENTS.md` MUX-01–MUX-06 (this session, direct read)

### Secondary (MEDIUM confidence)

- `.planning/milestones/v1.2-research/PITFALLS.md` (this session, direct read) — Pitfall 6 (PTY reader zombie) and Pitfall 7 (datagram/reattach race); context for understanding the session loop invariants
- `Cargo.toml` (this session, direct read) — confirmed quinn 0.11.9, postcard 1.x, tokio 1.x, bytes 1.x are workspace deps

### Tertiary (LOW confidence)

- None. All claims are grounded in direct codebase reading.

---

## Metadata

**Confidence breakdown:**
- Standard stack: HIGH — no new crates; all APIs verified in the codebase
- Architecture: HIGH — discriminant count verified by source inspection; quinn API patterns verified from existing usage in server.rs and client.rs
- Pitfalls: HIGH — M-1 through M-6 sourced directly from `.planning/research/PITFALLS.md` which was itself written from direct codebase inspection

**Research date:** 2026-06-11
**Valid until:** 2026-07-11 (30 days; stable crate versions; quinn/postcard API surface is not fast-moving)
