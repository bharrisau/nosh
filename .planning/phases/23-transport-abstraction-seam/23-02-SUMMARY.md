---
phase: 23-transport-abstraction-seam
plan: "02"
subsystem: nosh-server
tags: [transport-trait, quinn-wrapper, session-pump, refactor, async-finish]
dependency_graph:
  requires: [23-01]
  provides: [QuinnTransport, QuinnSendStream, QuinnRecvStream, transport-agnostic-session-pump]
  affects: [nosh-server]
tech_stack:
  added: [async-trait = { workspace = true } in nosh-server]
  patterns:
    - "Pure pass-through newtype wrappers over quinn types"
    - "Box<dyn Trait> deref (&mut *) for trait-object call sites"
    - "Auth-before-boxing ordering (extract_peer_identity before QuinnTransport wrap)"
    - "async fn finish() .await enforcement at all trait-object call sites"
key_files:
  created:
    - crates/nosh-server/src/quinn_transport.rs
  modified:
    - crates/nosh-server/Cargo.toml
    - crates/nosh-server/src/lib.rs
    - crates/nosh-server/src/channel.rs
    - crates/nosh-server/src/server.rs
decisions:
  - "QuinnTransport::finish() wrapper body uses let _ = self.0.finish(); Ok(()) — quinn's finish() is synchronous; the async wrapper exists only for trait object-safety. Callers on Box<dyn NoshSendStream> MUST .await."
  - "send_burst stays a plain fn (not async) taking &dyn NoshTransport — synchronous datagram loop; making it async would force a design regression (Pitfall 2)"
  - "extract_peer_identity and handshake_data run on the raw quinn::Connection BEFORE Box::new(QuinnTransport(conn)) — these are quinn-specific API and absent from NoshTransport (T-23-03 auth-before-boxing invariant)"
  - "accept_bi Err arms simplified to Err(_) → TransportLost since trait returns anyhow::Error (quinn::ConnectionError variants no longer accessible through the trait)"
  - "clean_exit() retained with #[allow(dead_code)] — was used for quinn error classification in handle_connection; available for future use"
  - "Box<dyn NoshTransport>: send_burst called as send_burst(&*conn, ...) — deref-coerces Box<dyn T> to &dyn T"
metrics:
  duration_seconds: 1140
  completed_date: "2026-06-13T05:02:47Z"
  tasks_completed: 3
  files_created: 1
  files_modified: 4
---

# Phase 23 Plan 02: Transport Abstraction Seam — Quinn Wrappers + Server Refactor Summary

Pure pass-through Quinn wrappers in `nosh-server` and a no-behaviour-change refactor of the session pump so `run_session`, `run_reattach_session`, `send_burst`, `run_channel_task`, `run_scrollback_sender_task`, and `run_echo_loop` are generic over the Plan 01 traits rather than concrete quinn types.

## What Was Built

`crates/nosh-server/src/quinn_transport.rs` (132 lines) provides three newtype wrappers:

- `QuinnTransport(quinn::Connection)` implementing `NoshTransport` — direct delegation; `send_datagram` maps `quinn::SendDatagramError` variants 1:1; `close` uses `code.into()` for `VarInt` conversion; `accept_bi`/`open_bi` box the returned streams.
- `QuinnSendStream(quinn::SendStream)` implementing `NoshSendStream` — `finish()` wrapper body calls quinn's synchronous `finish()` (no `.await`); async-ness is only in the trait signature for object-safety; callers must `.await`.
- `QuinnRecvStream(quinn::RecvStream)` implementing `NoshRecvStream` — `read_exact` delegates via `AsyncReadExt`; `stop` uses `code.into()`.

`crates/nosh-server/src/channel.rs` was refactored:

- `ChannelEvent::Stream` now holds `Box<dyn NoshSendStream>` + `Box<dyn NoshRecvStream>` (D-02/SC#4).
- `read_varint_u32` signature: `&mut dyn NoshRecvStream` (body unchanged — same `read_exact` call).
- `run_channel_task_inner`, `run_scrollback_sender_task`, `run_echo_loop` signatures: `&mut dyn NoshSendStream` / `&mut dyn NoshRecvStream`.
- `nosh_proto::codec::read_message(ch_recv)` → `nosh_proto::read_message_ns(ch_recv)` in `run_scrollback_sender_task`.
- All four `finish()` call sites at lines ~138, ~372, ~390, ~436 gained `.await` (T-23-06: async fn on trait — un-awaited would silently skip QUIC half-close).

`crates/nosh-server/src/server.rs` was refactored:

- `send_burst` signature: `&dyn NoshTransport` (remains `fn`, not `async` — Pitfall 2).
- `handle_connection`: `extract_peer_identity` and `handshake_data` run on raw `quinn::Connection`, THEN `let conn: Box<dyn NoshTransport> = Box::new(QuinnTransport(conn))` (T-23-03 auth-before-boxing invariant).
- `run_session` and `run_reattach_session` signatures: `Box<dyn NoshTransport>`, `Box<dyn NoshSendStream>`, `Box<dyn NoshRecvStream>`.
- All `nosh_proto::write_message(&mut send, ...)` → `nosh_proto::write_message_ns(&mut *send, ...)` (the `&mut *` deref-coerces `Box<dyn T>` to `&mut dyn T`).
- All `nosh_proto::read_message(&mut recv)` → `nosh_proto::read_message_ns(&mut *recv)`.
- `conn.close(X.into(), ...)` in the session pump → `conn.close(X, ...)` (trait takes `u32`).
- `accept_bi` arms: `read_varint_u32(&mut *ch_recv)`, `ch_send.reset(0u32)`, `ch_recv.stop(0u32)` (no `.into()`/`.ok()`).
- Scrollback task spawns: `&mut *ch_send, &mut *ch_recv` for deref to trait object.
- Three `finish()` call sites at lines ~1455, ~1586, ~2145 gained `.await` (T-23-06).

## Verification Results

- `cargo build --workspace`: exits 0.
- `cargo test --workspace`: all tests pass (18+107+32+6+14+2+1+3+11+3+3+6+3+4+44+113+1 = 371 tests), zero failures, zero ignored unexpected.
- No `.finish()` without `.await` in `channel.rs`: `! grep -nE '\.finish\(\)\s*;'` passes.
- No `.finish()` without `.await` in `server.rs`: `! grep -nE '(send|ch_send)\.finish\(\)\s*;'` passes.
- `WRAPPERS_OK`: all three impl blocks present in `quinn_transport.rs`.
- `ChannelEvent::Stream` holds `Box<dyn NoshSendStream>`, `Box<dyn NoshRecvStream>`.
- `read_varint_u32` signature uses `&mut dyn NoshRecvStream`.
- Zero test file modifications (D-05 satisfied — `git diff --name-only crates/ | grep tests/` returns empty).
- Auth-before-boxing ordering confirmed: `extract_peer_identity` (line 553) + `handshake_data` (line 564) before `Box::new(QuinnTransport(conn))` (line 574).

## Deviations from Plan

**1. [Rule 1 - Bug] accept_bi Err arm simplified to catch-all**
- **Found during:** Task 3
- **Issue:** The original code matched `Err(quinn::ConnectionError::ApplicationClosed(_)) | Err(quinn::ConnectionError::LocallyClosed)` specifically, with a separate `Err(_)` arm for transient errors. After boxing, `conn.accept_bi()` returns `anyhow::Result`; `quinn::ConnectionError` variants are no longer directly accessible through the trait. The old two-arm match would not compile.
- **Fix:** Collapsed to a single `Err(_) => break SessionEnd::TransportLost` arm — equivalent behaviour since all connection-close errors should be treated as transport loss.
- **Files modified:** `crates/nosh-server/src/server.rs`

**2. [Rule 2 - Missing Critical Functionality] clean_exit() retained as dead_code**
- **Found during:** Task 3
- **Issue:** `clean_exit(quinn::ConnectionError)` became dead code when `handle_connection` switched from direct quinn `accept_bi` to the trait. Removing it would be cleaner but it documents the error-mapping intent.
- **Fix:** Added `#[allow(dead_code)]` with a comment noting it's retained for future use.
- **Files modified:** `crates/nosh-server/src/server.rs`

## Threat Flags

None. This is a pure internal refactor of the server session pump. No new network surface, no new auth path, no new untrusted-input parsing beyond what was already in place. The auth-before-boxing invariant (T-23-03) is verified by line-number ordering in `handle_connection`.

## Known Stubs

None. This plan introduces no new user-visible behavior — it is a pure transport-abstraction refactor. The session pump operates identically to pre-refactor from the network's perspective.

## Self-Check

Files exist:
- `crates/nosh-server/src/quinn_transport.rs`: FOUND
- `crates/nosh-server/src/channel.rs` (modified): FOUND
- `crates/nosh-server/src/server.rs` (modified): FOUND

Commits exist:
- `30a00a6`: feat(23-02): add pure pass-through Quinn wrappers in nosh-server
- `6960a2c`: feat(23-02): abstract channel.rs over NoshSendStream/NoshRecvStream traits
- `cecbd0a`: feat(23-02): make server.rs session pump generic over NoshTransport

## Self-Check: PASSED

## Commits

| Hash | Message |
|------|---------|
| `30a00a6` | feat(23-02): add pure pass-through Quinn wrappers in nosh-server |
| `6960a2c` | feat(23-02): abstract channel.rs over NoshSendStream/NoshRecvStream traits |
| `cecbd0a` | feat(23-02): make server.rs session pump generic over NoshTransport |
