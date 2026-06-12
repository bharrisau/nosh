---
phase: 21-channel-multiplexing-foundation
verified: 2026-06-12T06:30:00Z
status: passed
score: 6/6 must-haves verified
overrides_applied: 0
re_verification:
  previous_status: none
---

# Phase 21: Channel Multiplexing Foundation Verification Report

**Phase Goal:** Logical channels are negotiated over a dedicated control stream using OPEN/ACCEPT/REJECT before any data stream is bound — the discriminant-stability enforcement test is the first commit, and the layer is proven with a simple echo channel before scrollback adds complexity.
**Verified:** 2026-06-12T06:30:00Z
**Status:** passed
**Re-verification:** No — initial verification

## Goal Achievement

### Observable Truths (ROADMAP Success Criteria 1–6)

| # | Truth (SC) | Status | Evidence |
|---|-----------|--------|----------|
| 1 | SC#1 / MUX-06 — `message_discriminant_order_is_stable` pins every variant byte; mux variants appended at 10–14; test is the first phase commit | ✓ VERIFIED | `codec.rs:276-306` pins discriminants 0-14 (SessionOpen=0 … TerminalControl=9, ChannelOpen=10 … ChannelClose=14). Git log: `d5f7e76 feat(21-01): append mux variants + discriminant test` precedes all server (`ca808cc`, `d4a65ba`) and client (`964cf53`, `9b99693`) impl commits. Test passes. |
| 2 | SC#2 / MUX-04 — client-EVEN / server-ODD parity; simultaneous-open cannot collide; MAX_OPEN_CHANNELS bounds half-open memory | ✓ VERIFIED | Client `EvenIdAllocator` starts 2, +2 (`client/channel.rs:51-68`). Server `next_server_channel_id` starts 1, +2, `debug_assert! % 2 != 0` (`server.rs:795,1274-1276`). `MAX_OPEN_CHANNELS = 64` checked on both client-open (`server.rs:1035`) and server-open (`server.rs:1278`) paths and on reattach (`server.rs:1809`). Server rejects odd-id client opens (`server.rs:1017`). Test `channel_simultaneous_open` passes. |
| 3 | MUX-01 — control-first OPEN→ACCEPT/REJECT before stream bind; REJECT opaque | ✓ VERIFIED | `open_channel` (`client.rs:760-783`) sends ChannelOpen, awaits Accept/Reject, then `open_bi`+varint prefix only on Accept. `ChannelReject` carries only `channel_id` (`messages.rs:213-216`); codec test asserts 2-byte encoding (`codec.rs:335-342`). Test `channel_open_accept_reject` asserts opaque 2-byte reject. |
| 4 | MUX-02 / SC#3 — one quinn stream per channel via varint prefix; accept_bi arm; channel tasks tokio::spawn'd (no HOL); median PTY latency < 5 ms under saturation | ✓ VERIFIED | accept_bi arm reads only the varint prefix, no payload (`server.rs:1199-1233`); tasks `tokio::spawn(run_channel_task(...))` (`server.rs:1108`). Test `channel_echo_roundtrip` saturates 256 KiB and asserts median over 5 samples `< Duration::from_millis(5)` (`channel_mux.rs:346-388`). Passes. |
| 5 | MUX-03 — 256 KiB credit window; CR-01 fix caps echo reads to remaining_credit (no byte discard at boundary) | ✓ VERIFIED | `INITIAL_CREDIT = 256*1024` (`server/channel.rs:46`). CR-01 fix present: `read_cap = remaining_credit.min(buf.len())` then `read(&mut buf[..read_cap])` (`server/channel.rs:229-241`). Verifier probe (768 KiB = 3× window) round-tripped byte-for-byte with zero loss across credit boundaries. Test `channel_flow_control_backpressure` passes. |
| 6 | MUX-05 / SC#5+SC#6 — cold-reattach re-establishes channels over control stream; REAL server-odd open concurrent with client-even | ✓ VERIFIED | `run_reattach_session` has full mux dispatch + accept_bi arm (`server.rs:1789-1932`); test `channel_reattach_reopen` orphans, reattaches, re-opens via fresh ChannelOpen (not byte-replay) and echoes. `channel_simultaneous_open` fires a REAL server-odd open via `server_open_tx` slot accessor concurrently with client-even open. Both pass. |

**Score:** 6/6 truths verified

### Required Artifacts

| Artifact | Expected | Status | Details |
|----------|----------|--------|---------|
| `crates/nosh-proto/src/messages.rs` | 5 mux variants + ChannelType appended | ✓ VERIFIED | ChannelOpen/Accept/Reject/Credit/Close at end; ChannelType{Echo,Scrollback,PortForward,AgentForward}; ChannelReject opaque (channel_id only) |
| `crates/nosh-proto/src/codec.rs` | discriminant-stability test | ✓ VERIFIED | `message_discriminant_order_is_stable` pins 0-14; `mux_variants_round_trip` asserts 2-byte opaque reject |
| `crates/nosh-server/src/channel.rs` | per-channel task + credit + varint reader; echo test-gated | ✓ VERIFIED | `run_channel_task`, `read_varint_u32` (no panic, 5-byte cap), CR-01 read-cap; echo loop `#[cfg(any(test, feature="test-support"))]` |
| `crates/nosh-server/src/server.rs` | post-auth accept_bi arm + dispatch + lifecycle | ✓ VERIFIED | accept_bi only after `drop(permit)` at `server.rs:539`; pre-auth `run_accept_loop` has no accept_bi; PFWD/AFWD unconditionally rejected; WR-01/WR-02/CR-02 fixes present |
| `crates/nosh-server/src/registry.rs` | server-open test seam | ✓ VERIFIED | `store/take_server_open_tx`, `first_active_slot` all `#[cfg(any(test, feature="test-support"))]` |
| `crates/nosh-client/src/channel.rs` | even-id allocator + drain task + credit | ✓ VERIFIED | `EvenIdAllocator` (2,4,6…), `run_channel_task` credit replenish in 128 KiB chunks, half-close |
| `crates/nosh-client/src/client.rs` | open_channel / await_channel_accept | ✓ VERIFIED | control-first ordering; WR-03 fix loops past interleaved PtyData (`client.rs:717-737`) |
| `crates/nosh-client/tests/channel_mux.rs` | 6-test integration suite | ✓ VERIFIED | All 6 tests substantive and passing |

### Key Link Verification

| From | To | Via | Status |
|------|----|----|--------|
| client `open_channel` | server control dispatch | ChannelOpen on control stream | ✓ WIRED |
| server `ChannelAccept` | client `await_channel_accept` | control stream, loops past PtyData | ✓ WIRED |
| client data stream | server channel task | varint prefix → accept_bi → `ChannelEvent::Stream` (send, not try_send) | ✓ WIRED |
| client drain | server echo credit | `ChannelCredit` via pump (single-writer A4) | ✓ WIRED |
| server-open seam | session pump | `server_open_tx` → `recv_or_pending` arm | ✓ WIRED (test-only) |

### Behavioral Spot-Checks

| Behavior | Command | Result | Status |
|----------|---------|--------|--------|
| Release build excludes test seams | `cargo build --release -p nosh-server` + `nm` rlib | clean; `store/take_server_open_tx`, `first_active_slot`, `run_echo_loop` symbols ABSENT | ✓ PASS |
| Mux integration suite | `cargo test -p nosh-client --test channel_mux` | 6 passed; 0 failed; no hang (0.11s) | ✓ PASS |
| Full workspace | `cargo test --workspace` | 0 failures across all suites; no hangs | ✓ PASS |
| CR-01 multi-window probe (768 KiB = 3× window) | verifier-authored probe | byte-exact round-trip, zero loss at credit boundaries | ✓ PASS |

### Requirements Coverage

| Requirement | Description | Status | Evidence |
|-------------|-------------|--------|----------|
| MUX-01 | Control-first OPEN/ACCEPT/REJECT, opaque REJECT | ✓ SATISFIED | Truth #3 |
| MUX-02 | Concurrent channels, own stream, no HOL | ✓ SATISFIED | Truth #4 |
| MUX-03 | Per-channel credit windows | ✓ SATISFIED | Truth #5 + probe |
| MUX-04 | Clean lifecycle, parity, no leak | ✓ SATISFIED | Truths #2, #6 + `channel_lifecycle_clean` |
| MUX-05 | Survives migration + cold-reattach re-open | ✓ SATISFIED | Truth #6 |
| MUX-06 | Append-only wire format, discriminant test first commit | ✓ SATISFIED | Truth #1 |

### Anti-Patterns Found

None. No TODO/FIXME/XXX/TBD/HACK/PLACEHOLDER markers in any phase-modified file. Only one benign dead-code warning (`next_ctrl_frame` unused test helper in `channel_mux.rs`) — non-blocking.

### Security Verification

- accept_bi appears ONLY in `handle_connection`/`run_session`/`run_reattach_session`, all AFTER `drop(permit)` (`server.rs:539`). Pre-auth `run_accept_loop` contains only `endpoint.accept()` — no stream acceptance pre-auth (Pitfall M-1 / T-21-03 satisfied).
- `ChannelType::PortForward | AgentForward` unconditionally rejected on fresh and reattach paths (`server.rs:1052-1058`, `1818`).
- `SSH_AUTH_SOCK` in the server env-strip denylist (`session.rs:39`); never forwarded by client (`client.rs:430`); session test asserts absence.
- Echo loop and all server-open/slot seams compiled OUT of release builds (cfg-gated; confirmed symbol-absent in release rlib).
- Review findings CR-01, CR-02, WR-01, WR-02, WR-03 independently confirmed present in code (not merely claimed in summaries).

### Gaps Summary

None. The phase goal is achieved: logical channels negotiate control-first (OPEN→ACCEPT/REJECT) before any data stream binds; the discriminant-stability test is the first phase commit; the layer is proven end-to-end with a test-gated echo channel; all six ROADMAP success criteria hold in the live codebase; all security invariants verified.

---

_Verified: 2026-06-12T06:30:00Z_
_Verifier: Claude (gsd-verifier)_
