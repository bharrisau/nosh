---
phase: 21-channel-multiplexing-foundation
plan: "01"
subsystem: nosh-proto
tags: [mux, wire-protocol, discriminant-stability, channel-types]
dependency_graph:
  requires: []
  provides: [ChannelOpen, ChannelAccept, ChannelReject, ChannelCredit, ChannelClose, ChannelType, message_discriminant_order_is_stable]
  affects: [nosh-proto, nosh-server, nosh-client]
tech_stack:
  added: []
  patterns: [append-only-enum, discriminant-stability-test, opaque-reject]
key_files:
  created: []
  modified:
    - crates/nosh-proto/src/messages.rs
    - crates/nosh-proto/src/codec.rs
decisions:
  - "ChannelReject carries only channel_id (no reason field) — opaque per MUX-01 / T-21-02"
  - "ChannelType enum variants ordered Echo/Scrollback/PortForward/AgentForward; APPEND-ONLY"
  - "message_discriminant_order_is_stable is the gating first commit of Phase 21 per MUX-06"
metrics:
  duration_seconds: 120
  completed_date: "2026-06-11"
  tasks_completed: 1
  files_changed: 2
---

# Phase 21 Plan 01: Mux Message Variants and Discriminant-Stability Test Summary

Five channel-multiplexing `Message` variants (discriminants 10–14) and a `ChannelType` enum appended to `nosh-proto`, with an exhaustive byte-level discriminant-stability test pinning all 15 variants.

## What Was Built

**`crates/nosh-proto/src/messages.rs`**

Five new variants appended after `TerminalControl` (discriminant 9):

- `ChannelOpen { channel_id: u32, channel_type: ChannelType }` — discriminant 10 (client → server)
- `ChannelAccept { channel_id: u32 }` — discriminant 11 (server → client)
- `ChannelReject { channel_id: u32 }` — discriminant 12 (server → client, opaque)
- `ChannelCredit { channel_id: u32, bytes: u64 }` — discriminant 13 (either direction)
- `ChannelClose { channel_id: u32 }` — discriminant 14 (either direction)

New `ChannelType` enum with four variants in order: `Echo` (test-only), `Scrollback` (Phase 22 consumer), `PortForward` (declared/rejected), `AgentForward` (declared/rejected). Marked APPEND-ONLY in doc comment.

`variant_name()` extended with one arm per new variant (logging path, never exposes payload).

**`crates/nosh-proto/src/codec.rs`**

Two new tests in `mod tests`:

- `message_discriminant_order_is_stable` — non-async test; iterates 15 `(expected_disc, msg)` pairs and asserts `postcard::to_allocvec(msg).unwrap()[0] == expected_disc`. Covers all existing variants plus the five new ones. This is the gating first commit per MUX-06.
- `mux_variants_round_trip` — async test; round-trips all five new variants through `write_message` → `read_message` and additionally asserts `ChannelReject` encodes to exactly 2 bytes (discriminant + zero channel_id varint), confirming no reason field is present.

## Commits

| Task | Name | Commit | Files |
|------|------|--------|-------|
| 1 | Append mux variants + discriminant-stability test | d5f7e76 | messages.rs, codec.rs |

## Test Results

`cargo test -p nosh-proto`: **34/34 pass** (0 failed).

New tests:
- `codec::tests::message_discriminant_order_is_stable` — PASS
- `codec::tests::mux_variants_round_trip` — PASS

## Deviations from Plan

None — plan executed exactly as written.

## Known Stubs

None. The new types are complete wire definitions. No placeholder values or TODO markers.

## Threat Flags

None. No new network endpoints or trust boundaries introduced. The new variants ride the existing control stream and are covered by the existing TLS mutual-auth and encryption.

## Self-Check: PASSED

- `crates/nosh-proto/src/messages.rs` — FOUND, contains `ChannelOpen`
- `crates/nosh-proto/src/codec.rs` — FOUND, contains `message_discriminant_order_is_stable`
- Commit d5f7e76 — FOUND (`git log --oneline -1` → `d5f7e76 feat(21-01): append mux Message variants...`)
- All 34 nosh-proto tests pass
