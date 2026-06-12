---
phase: 21-channel-multiplexing-foundation
plan: "02"
subsystem: nosh-server
tags: [mux, channel-lifecycle, flow-control, server-session, cfg-test, sc6]
dependency_graph:
  requires: [21-01]
  provides:
    - channel.rs (ChannelEvent, run_channel_task, read_varint_u32, INITIAL_CREDIT)
    - server mux dispatch (ChannelOpen/Credit/Close in run_session + run_reattach_session)
    - accept_bi arm for channel data streams (post-auth only)
    - cfg(test) server-initiated odd-id open path (SC#6 evidence)
    - SessionSlot.server_open_tx + store/take accessors (test builds only)
    - SessionRegistry.first_active_slot (test builds only)
  affects: [nosh-server, nosh-client (plan 21-03/21-04 consumers)]
tech_stack:
  added: []
  patterns:
    - recv_or_pending() wrapper for conditional select! arms (cfg(test)-safe)
    - HashMap<u32, mpsc::Sender<ChannelEvent>> channel map (pump-local, no Mutex)
    - mpsc control_tx single-writer invariant (A4 / Pitfall M-6)
    - tokio::spawn(run_channel_task) per accepted channel (MUX-02, no HOL blocking)
    - 256 KiB byte-credit window per channel (MUX-03)
    - odd-id allocator (1, 3, 5, …) for server-initiated channels under #[cfg(test)]
key_files:
  created:
    - crates/nosh-server/src/channel.rs
  modified:
    - crates/nosh-server/src/server.rs
    - crates/nosh-server/src/registry.rs
    - crates/nosh-server/src/lib.rs
decisions:
  - "recv_or_pending() helper wraps Option<Receiver> so cfg(test)-conditional arms
    stay syntactically present in tokio::select! but semantically absent in production
    (tokio::select! does not accept #[cfg()] attributes on individual arms)"
  - "server_open_rx_opt is Some(rx) in test builds, None in production; recv_or_pending
    returns pending() when None — the arm never wakes in production (T-21-10)"
  - "ChannelAccept/Reject split into separate match arms to allow independent handling
    of the test-only odd-id reply path (Reject → map removal + Close event)"
  - "try_recv approach rejected in favour of select! arm via recv_or_pending for
    correct async responsiveness (not opportunistic on control_rx wakeup)"
  - "store_server_open_tx + take_server_open_tx on SessionSlot, first_active_slot
    on SessionRegistry — minimal #[cfg(test)] accessors for plan 21-04 harness"
metrics:
  duration_seconds: 3300
  completed_date: "2026-06-11"
  tasks_completed: 3
  files_changed: 4
---

# Phase 21 Plan 02: Server Mux Layer Summary

Server-side channel multiplexing layer: control-stream dispatch for OPEN/ACCEPT/REJECT/CREDIT/CLOSE, a secondary `accept_bi` arm that binds data streams to channel tasks, 256 KiB per-channel credit windows, lifecycle teardown, and a `#[cfg(test)]`-gated server-initiated odd-id path providing SC#6 evidence for plan 21-04.

## What Was Built

### `crates/nosh-server/src/channel.rs` (new)

Per-logical-channel server task module.

**`ChannelEvent` enum** — three variants:
- `Stream(quinn::SendStream, quinn::RecvStream)`: binds a QUIC stream pair to the task
- `Credit(u64)`: replenishes the 256 KiB send-credit window (MUX-03)
- `Close`: signals the task to finish and release resources

**`INITIAL_CREDIT: u64 = 256 * 1024`** — per-channel byte-credit constant (MUX-03).

**`read_varint_u32(recv: &mut quinn::RecvStream) -> anyhow::Result<u32>`** — reads a postcard/LEB128 u32 varint (at most 5 bytes) from a QUIC RecvStream. Returns `Err` on truncation or overflow; never panics (T-21-06 / V5 input validation).

**`run_channel_task(channel_id, events, control_tx)`** — the per-channel async task:
- Awaits `ChannelEvent::Stream` before doing any I/O (handles Close before stream arrives gracefully)
- In `#[cfg(test)]`: runs `run_echo_loop` — reads bytes from RecvStream, echoes back on SendStream, tracks `remaining_credit`, pauses at zero, resumes on `ChannelEvent::Credit`
- In production: drains RecvStream to EOF and handles Close events (no production channel type accepted yet)
- On exit: calls `send.finish()` + bounded `send.stopped()` then sends `Message::ChannelClose { channel_id }` through `control_tx` — the pump removes the map entry
- NEVER calls `write_message` on the control stream directly (A4 / Pitfall M-6 single-writer invariant)

### `crates/nosh-server/src/server.rs` (extended)

**New imports**: `HashMap`, `ChannelType`, `ChannelEvent`, `run_channel_task`, `MAX_OPEN_CHANNELS = 64`.

**`recv_or_pending<T>()` helper** — wraps `Option<Receiver<T>>`, returning `std::future::pending()` when `None`. This makes the server-open trigger arm always syntactically present in `tokio::select!` but semantically absent in production builds.

**`run_session` additions**:
- `channel_map: HashMap<u32, mpsc::Sender<ChannelEvent>>` — pump-local (no Mutex, no shared state)
- `(channel_ctrl_tx, channel_ctrl_rx)` — single-writer mpsc path for channel tasks to send outbound control frames
- `server_open_rx_opt: Option<Receiver<ChannelType>>` — `Some` in test, `None` in production
- `next_server_channel_id: u32 = 1` — odd-id allocator (advances by 2)
- In `#[cfg(test)]`: stores `server_open_tx` in `slot` via `slot.store_server_open_tx()`

**New select! arms in `run_session`**:
1. `incoming_stream = conn.accept_bi()` — reads only the varint prefix via `read_varint_u32`, dispatches `ChannelEvent::Stream` to the matching channel task; malformed varint or unknown id = logged no-op, never panic (T-21-06 / T-21-07)
2. `Some(ctrl_msg) = channel_ctrl_rx.recv()` — the sole writer for outbound control frames; on `ChannelClose` removes map entry before writing to the client
3. `server_open_req = recv_or_pending(&mut server_open_rx_opt)` — allocates odd id, spawns task, sends `Message::ChannelOpen` via `send` (single-writer; this arm runs on the same pump task that owns `send`)

**Extended `match msg` block**:
- `ChannelOpen`: parity check (odd = ignore), duplicate check, cap check, type gating (PortForward/AgentForward → always reject T-21-08; Scrollback → reject Phase 22; Echo → `#[cfg(test)]` only), then ChannelAccept + spawn
- `ChannelCredit`: forward to task; unknown id = logged no-op (T-21-07)
- `ChannelClose`: signal task, remove from map; unknown id = logged no-op
- `ChannelAccept` (odd id, `#[cfg(test)]`): client accepted server-initiated channel — continue
- `ChannelReject` (odd id, `#[cfg(test)]`): client rejected — drop map entry + Close event to task
- `ChannelAccept`/`ChannelReject` (even id or production): protocol error → `ClientClosed`

**`run_reattach_session`**: same channel infrastructure as `run_session` (channel_map, ctrl pair, accept_bi arm, ctrl_rx drain arm, ChannelOpen/Credit/Close dispatch). No `server_open_rx` arm on reattach (test-only path is fresh-session only per the plan).

### `crates/nosh-server/src/registry.rs` (extended, test builds only)

**`SessionSlot.server_open_tx`** — `#[cfg(test)]` field: `Mutex<Option<mpsc::Sender<ChannelType>>>`, initialised to `None` in `SessionSlot::new`.

**`SessionSlot::store_server_open_tx(tx)`** — `#[cfg(test)]` method: stores the sender for integration tests to retrieve.

**`SessionSlot::take_server_open_tx()`** — `#[cfg(test)]` method: takes the sender (returns `None` if not yet stored or already taken). Tests poll briefly after connecting before calling this.

**`SessionRegistry::first_active_slot()`** — `#[cfg(test)]` method: returns the first Active slot in the registry. Integration tests use `registry.first_active_slot()?.take_server_open_tx()` to get the sender and trigger a server-initiated ChannelOpen (plan 21-04 SC#6 test harness).

### `crates/nosh-server/src/lib.rs`

Added `pub mod channel;`.

## Test-Accessor Documentation (for Plan 21-04)

**How to reach a live server session's `server_open_tx` handle from an integration test:**

```rust
// 1. Obtain the registry (from TestServer returned by spawn_server_with_registry).
let registry: Arc<SessionRegistry> = test_server.registry.clone();

// 2. Wait until the session pump has stored the sender (brief poll loop).
let slot = loop {
    if let Some(slot) = registry.first_active_slot() {
        break slot;
    }
    tokio::time::sleep(Duration::from_millis(25)).await;
};

// 3. Take the sender (one-shot; store it before it's consumed).
let server_open_tx = slot.take_server_open_tx()
    .expect("server pump must have stored server_open_tx");

// 4. Send a ChannelType to trigger a server-initiated ChannelOpen (odd id).
server_open_tx.send(ChannelType::Echo).await
    .expect("server pump must still be running");

// The server session pump will allocate an odd channel id (1, 3, 5, …),
// spawn a run_channel_task, and write Message::ChannelOpen { channel_id, channel_type: Echo }
// on the control stream to the client. The client reads it as a server-initiated open.
```

**Module path**: `nosh_server::registry::SessionRegistry::first_active_slot` and `nosh_server::registry::SessionSlot::take_server_open_tx`.

**Types**: `ChannelType` is at `nosh_proto::messages::ChannelType` (not re-exported at `nosh_proto` crate root as of plan 21-01).

## Commits

| Task | Name | Commit | Files |
|------|------|--------|-------|
| 1 | channel.rs — ChannelEvent, run_channel_task, read_varint_u32 | ca808cc | channel.rs, lib.rs |
| 2 | Wire mux dispatch + accept_bi arm into run_session/run_reattach_session | d4a65ba | server.rs, channel.rs |
| 3 | cfg(test) server-initiated odd-id ChannelOpen path (SC#6) | d4fa4de | server.rs, registry.rs |

## Test Results

`cargo test -p nosh-server -- --test-threads=1`: **109/109 pass** (0 failed).
`cargo build --release -p nosh-server`: success — test-only symbols absent from production binary.

## Deviations from Plan

**recv_or_pending() pattern for cfg(test)-conditional select! arm**

The plan called for a `#[cfg(test)]` select! arm for the server-open trigger. `tokio::select!` does not accept `#[cfg()]` attribute macros on individual arms. The deviation introduces a `recv_or_pending()` helper that wraps `Option<Receiver<T>>`, returning `std::future::pending()` when `None`. In production `server_open_rx_opt` is `None`; in test builds it is `Some(rx)`. The arm is always syntactically present but semantically inert in production — same security and binary properties as the plan required (T-21-10). This is a Rule 3 auto-fix: the `#[cfg()]`-on-arm approach would not compile.

**`store_server_open_tx` / `take_server_open_tx` on `SessionSlot` (registry.rs)**

The plan left open how to expose `server_open_tx` to the test harness. Added `#[cfg(test)]` field + two accessor methods on `SessionSlot` and `first_active_slot()` on `SessionRegistry`. This is the minimal accessor called for by the plan ("add a minimal `#[cfg(test)]` accessor rather than widening any production API").

**`run_session` only for server-open path (not run_reattach_session)**

Plan 21-02 Task 3 specifies "in run_session (the fresh-session loop only)". The `server_open_rx_opt` and `next_server_channel_id` infrastructure lives only in `run_session`. Confirmed: `run_reattach_session` has no server-open arm.

## Known Stubs

None. All implemented functionality is complete. No placeholder values or TODO markers.

## Threat Flags

None. No new network endpoints or trust boundaries beyond those in the plan's threat model.

## Self-Check: PASSED

- `crates/nosh-server/src/channel.rs` — FOUND, contains `run_channel_task`
- `crates/nosh-server/src/server.rs` — FOUND, contains `ChannelOpen` dispatch
- `crates/nosh-server/src/registry.rs` — FOUND, contains `store_server_open_tx`
- Commit ca808cc — FOUND (Task 1)
- Commit d4a65ba — FOUND (Task 2)
- Commit d4fa4de — FOUND (Task 3)
- All 109 nosh-server tests pass
- Release build: no test-only symbols
- `accept_bi` only in `handle_connection` (control stream) + `run_session` + `run_reattach_session` — never in `run_accept_loop`
