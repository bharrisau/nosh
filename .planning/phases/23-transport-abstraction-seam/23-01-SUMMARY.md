---
phase: 23-transport-abstraction-seam
plan: "01"
subsystem: nosh-proto
tags: [transport-trait, object-safety, async-trait, codec, quic-abstraction]
dependency_graph:
  requires: []
  provides: [NoshTransport, NoshSendStream, NoshRecvStream, SendDatagramError, write_message_ns, read_message_ns]
  affects: [nosh-proto]
tech_stack:
  added: [async-trait = "0.1.89"]
  patterns: [async_trait macro for object-safe async traits, TDD RED/GREEN/REFACTOR]
key_files:
  created:
    - crates/nosh-proto/src/transport_trait.rs
    - crates/nosh-proto/src/transport_trait_tests.rs
  modified:
    - Cargo.toml
    - crates/nosh-proto/Cargo.toml
    - crates/nosh-proto/src/lib.rs
decisions:
  - "async-trait 0.1.89 used over native AFIT: native async fn in trait desugars to impl Future which is NOT object-safe; D-02 mandates Box<dyn NoshSendStream> so async-trait is mandatory"
  - "send_datagram/datagram_send_buffer_space/max_datagram_size kept synchronous: send_burst calls them in a non-async tight loop; making them async would force send_burst to become async (design regression)"
  - "max_datagram_size returns Option<usize> not usize: None means datagrams not negotiated; callers match on Some(c) to gate send_burst"
  - "codec helpers write_message_ns/read_message_ns added to transport_trait.rs rather than codec.rs: Box<dyn NoshSendStream> does not implement AsyncWrite+Unpin; helpers delegate to the trait methods"
  - "tests in transport_trait_tests.rs: separate test module file referenced from lib.rs under #[cfg(test)]; keeps trait file clean"
metrics:
  duration_seconds: 493
  completed_date: "2026-06-13T04:39:28Z"
  tasks_completed: 2
  files_created: 2
  files_modified: 3
---

# Phase 23 Plan 01: Transport Abstraction Seam — Trait Definitions Summary

Three object-safe traits (`NoshTransport`, `NoshSendStream`, `NoshRecvStream`), `SendDatagramError`, and the `write_message_ns`/`read_message_ns` codec helpers authored in `nosh-proto` using `async-trait 0.1.89` for dyn-compatibility.

## What Was Built

`crates/nosh-proto/src/transport_trait.rs` (244 lines) is the thin I/O boundary that decouples the session pump from the concrete transport. The trait surface covers every connection-level method called in the server session pump (`send_datagram`, `datagram_send_buffer_space`, `max_datagram_size`, `read_datagram`, `accept_bi`, `open_bi`, `remote_address`, `close`) and both stream directions (`write_all`, `flush`, `finish`, `stopped`, `reset` on send; `read_exact`, `read`, `stop` on recv). The codec helpers `write_message_ns`/`read_message_ns` parallel the existing `codec::write_message`/`read_message` but accept `&mut dyn NoshSendStream`/`&mut dyn NoshRecvStream` instead of the `AsyncWrite + Unpin`/`AsyncRead + Unpin` generics that `Box<dyn ...>` cannot satisfy.

## TDD Gate Compliance

- RED commit `17b7398`: failing test module added — compile error (unresolved `transport_trait` module and missing `async_trait`/`anyhow` crates).
- GREEN commit `acb96a4`: traits implemented, 40 tests pass (35 original + 5 new object-safety tests).
- GREEN commit `8a4b9cd`: codec helper tests added, 44 tests pass (4 new round-trip and MAX_FRAME_LEN enforcement tests).
- REFACTOR: removed dead `SliceRecvStream` struct from test file; no behaviour change.

## Verification Results

- `cargo build -p nosh-proto`: exits 0.
- `cargo test -p nosh-proto`: 44 passed, 0 failed (including 9 new tests for this plan).
- `cargo test --workspace`: all test suites green, zero test modifications (D-05 satisfied).
- `#[async_trait]` appears 7 times in `transport_trait.rs` (>= 3 required — 3 for trait definitions, 4 for the wrapper impls in the test mocks).
- `send_datagram`, `datagram_send_buffer_space`, `max_datagram_size` are plain `fn` (confirmed by grep — no `async` prefix).
- `max_datagram_size` returns `-> Option<usize>`.
- `async-trait = "0.1.89"` in workspace `[workspace.dependencies]`; `crates/nosh-proto/Cargo.toml` has both `async-trait = { workspace = true }` and `anyhow = { workspace = true }`.
- `transport_trait.rs` is 244 lines (>= 80 required).
- `read_message_ns` references `crate::codec::MAX_FRAME_LEN` (3 occurrences — definition and T-23-01 DoS mitigation guard).

## Deviations from Plan

None — plan executed exactly as written.

## Threat Flags

None. No new network surface, no new auth path, no new untrusted-input parsing beyond the existing `codec::decode` (reused via `read_message_ns` which preserves the `MAX_FRAME_LEN` guard identically).

## Known Stubs

None. The traits are pure trait definitions with no data; no stubs that flow to UI rendering or user-visible behaviour.

## Commits

| Hash | Message |
|------|---------|
| `17b7398` | test(23-01): add failing tests for NoshTransport/NoshSendStream/NoshRecvStream object-safety |
| `acb96a4` | feat(23-01): add NoshTransport/NoshSendStream/NoshRecvStream traits and SendDatagramError |
| `8a4b9cd` | feat(23-01): add write_message_ns/read_message_ns codec helpers + re-export trait surface |
