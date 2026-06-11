---
phase: 21-channel-multiplexing-foundation
plan: "03"
subsystem: nosh-client
tags: [mux, client-channel, flow-control, even-id, credit, varint-prefix]
dependency_graph:
  requires: [21-01, 21-02]
  provides:
    - send_channel_open (control stream ChannelOpen writer)
    - await_channel_accept (ChannelAccept/ChannelReject reader)
    - open_channel (control-first open_bi + varint prefix)
    - ChannelAcceptOutcome enum
    - channel.rs (EvenIdAllocator, run_channel_task, INITIAL_CREDIT)
  affects: [nosh-client, nosh-client/tests (plan 21-04 consumer)]
tech_stack:
  added: [postcard (direct dep in nosh-client)]
  patterns:
    - control-first open (ACCEPT awaited before open_bi, MUX-01)
    - postcard varint channel-id prefix on new bidi stream (MUX-02)
    - even-id allocator (2, 4, 6, … — client-EVEN parity, MUX-04/ROADMAP SC#2)
    - mpsc control_tx single-writer invariant (A4 / Pitfall M-6)
    - tokio::spawn(run_channel_task) per opened channel (MUX-02/MUX-03)
    - 256 KiB byte-credit window, replenished in 128 KiB chunks (MUX-03)
    - send.finish() + bounded send.stopped() half-close on exit
key_files:
  created:
    - crates/nosh-client/src/channel.rs
  modified:
    - crates/nosh-client/src/client.rs
    - crates/nosh-client/src/lib.rs
    - crates/nosh-client/Cargo.toml
decisions:
  - "open_channel awaits ChannelAccept/ChannelReject on the CONTROL stream before calling open_bi — control-first ordering (MUX-01)"
  - "varint prefix written to the new bidi stream before any payload so stream→channel mapping survives QUIC connection migration (MUX-02)"
  - "ChannelAcceptOutcome::Rejected returns Ok(None) — caller discards the attempt cleanly"
  - "EvenIdAllocator starts at 2; wraps to 2 past u32::MAX - 1 (id 0 reserved, client-EVEN per locked ROADMAP SC#2)"
  - "Credit replenished every 128 KiB (half window) to avoid leaving the server waiting for the full 256 KiB window to drain before resuming sends"
  - "postcard added as a direct nosh-client dependency (was previously only a transitive dep via nosh-proto) — needed for to_allocvec varint encoding in open_channel"
metrics:
  duration_seconds: 226
  completed_date: "2026-06-11"
  tasks_completed: 2
  files_changed: 4
---

# Phase 21 Plan 03: Client Mux Layer Summary

Client-side channel-multiplexing layer: control-first `open_channel` (send ChannelOpen, await Accept/Reject, then open_bi with varint prefix), an even-id allocator for client-initiated channels, a per-channel drain task that grants ChannelCredit via the pump's control_tx mpsc, and the helpers needed for the MUX-05 reattach re-open path (proved in plan 21-04).

## What Was Built

### `crates/nosh-client/src/client.rs` (extended)

**New import:** `nosh_proto::messages::ChannelType` added alongside existing `nosh_proto::Message`.

**`ChannelAcceptOutcome` enum** — two variants:
- `Accepted`: server accepted the channel; caller may proceed to open_bi.
- `Rejected`: server rejected; channel id is free to reuse, returns `Ok(None)` from `open_channel`.

**`send_channel_open(control_send, channel_id, channel_type)`** — writes `Message::ChannelOpen { channel_id, channel_type }` on the CONTROL stream's `SendStream`. Modelled on `send_reattach`. Never opens a new stream.

**`await_channel_accept(control_recv, expected_id)`** — reads the next frame from the CONTROL stream:
- `ChannelAccept { channel_id }` where `channel_id == expected_id` → `Ok(Accepted)`
- `ChannelAccept` for a different id → `bail!` (protocol error, no debug output)
- `ChannelReject { .. }` → `Ok(Rejected)`
- Any other frame → `bail!` using `variant_name()` (W3/D-07; never Debug)

**`open_channel(conn, control_send, control_recv, channel_id, channel_type)`** — the composed entry point:
1. Calls `send_channel_open` on the control stream.
2. Calls `await_channel_accept` on the control stream.
3. On `Rejected`: returns `Ok(None)` — no bidi stream opened.
4. On `Accepted`: calls `conn.open_bi()`, encodes `channel_id` as a postcard varint with `postcard::to_allocvec`, writes the varint bytes to the new `SendStream` before any payload, returns `Ok(Some((send, recv)))`.

### `crates/nosh-client/src/channel.rs` (new)

**`INITIAL_CREDIT: u64 = 256 * 1024`** — per-channel byte-credit window constant (MUX-03).

**`CREDIT_REPLENISH_CHUNK: u64 = 128 * 1024`** — internal threshold; credit is replenished to the server every time this many bytes are drained from the receive buffer.

**`EvenIdAllocator`** — allocates even channel ids for client-initiated channels (MUX-04):
- `new()` → first id returned is 2
- `next_id(&mut self) -> u32` → returns 2, 4, 6, … incrementing by 2; wraps past `u32::MAX - 1` back to 2

**`run_channel_task(channel_id, ch_recv, ch_send, control_tx)`** — the per-channel async task:
- Reads bytes from `ch_recv` (server → client channel data) in a loop.
- Accumulates `drained_since_replenish`; on reaching `CREDIT_REPLENISH_CHUNK`, sends `Message::ChannelCredit { channel_id, bytes }` through `control_tx` (NEVER `write_message` on the stream directly — A4/Pitfall M-6).
- If `control_tx` is closed, logs and exits gracefully.
- On `RecvStream::EOF` or read error: flushes any remaining unsent credit, then calls `ch_send.finish()` + bounded `ch_send.stopped()` (2 s timeout, half-close pattern from server-side analog).
- Sends `Message::ChannelClose { channel_id }` through `control_tx` for pump map cleanup.
- NEVER panics; unknown/closed-id conditions are handled by the caller (tokio::spawn task exits).

**Unit tests (4 tests, all pass):**
- `even_id_allocator_yields_even_ids` — first four ids are 2, 4, 6, 8
- `even_id_allocator_all_even_nonzero` — 100 iterations, all even, all non-zero
- `initial_credit_is_256_kib` — INITIAL_CREDIT == 256 * 1024
- `credit_replenish_chunk_is_half_window` — CREDIT_REPLENISH_CHUNK == INITIAL_CREDIT / 2

### `crates/nosh-client/src/lib.rs`

Added `pub mod channel;` (declared before `pub mod client` for alphabetical order).

### `crates/nosh-client/Cargo.toml`

Added `postcard = { workspace = true }` to `[dependencies]` — needed by `open_channel` for `postcard::to_allocvec(&channel_id)` varint encoding.

## Commits

| Task | Name | Commit | Files |
|------|------|--------|-------|
| 1 | open_channel, send_channel_open, await_channel_accept in client.rs | 964cf53 | client.rs, Cargo.toml, Cargo.lock |
| 2 | channel.rs — EvenIdAllocator, run_channel_task, credit replenishment | 9b99693 | channel.rs, lib.rs |

## Test Results

`cargo build -p nosh-client`: **clean** (0 errors, 0 warnings).
`cargo test -p nosh-client --lib`: **104/104 pass** (0 failed).

New tests (4):
- `channel::tests::even_id_allocator_yields_even_ids` — PASS
- `channel::tests::even_id_allocator_all_even_nonzero` — PASS
- `channel::tests::initial_credit_is_256_kib` — PASS
- `channel::tests::credit_replenish_chunk_is_half_window` — PASS

## Deviations from Plan

**postcard added as direct nosh-client dependency**

The plan's `open_channel` action calls `postcard::to_allocvec(&channel_id)` to encode the varint prefix. `postcard` was previously only a transitive dependency (via `nosh-proto`); it was not in `nosh-client/Cargo.toml`. Added as a direct dependency using the workspace version. This is a Rule 3 auto-fix (missing dependency for a compile to succeed). No version pin change — the workspace version is used.

No other deviations. Plan executed as written.

## Known Stubs

None. All implemented functionality is complete. No placeholder values or TODO markers.

The `run_channel_task` drain loop correctly wires credit replenishment and half-close. The `open_channel` flow is complete end-to-end. The reattach re-open path (MUX-05) is served by calling `open_channel` after `ReattachOk` is received on the new control stream — the helpers are sufficient for plan 21-04 to exercise this.

## Threat Flags

None. No new network endpoints or trust boundaries beyond those in the plan's threat model. The `open_channel` path rides the existing mutual-TLS-authenticated QUIC connection; the control stream is the same one established at session open.

## Self-Check: PASSED

- `crates/nosh-client/src/channel.rs` — FOUND, contains `run_channel_task`
- `crates/nosh-client/src/client.rs` — FOUND, contains `open_channel`
- `crates/nosh-client/src/lib.rs` — FOUND, contains `pub mod channel`
- Commit 964cf53 — FOUND (Task 1)
- Commit 9b99693 — FOUND (Task 2)
- All 104 nosh-client lib tests pass
- No `write_message` call in channel.rs (only in comment)
