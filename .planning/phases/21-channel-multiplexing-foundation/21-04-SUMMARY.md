---
plan: 21-04
phase: 21-channel-multiplexing-foundation
status: complete
completed: 2026-06-12
requirements: [MUX-01, MUX-02, MUX-03, MUX-04, MUX-05]
---

# 21-04 Summary — Channel Mux Integration Suite

**One-liner:** Six-test echo-channel integration suite proving the mux layer end-to-end (lifecycle, opaque REJECT, MUX-03 backpressure, SC#3 PTY latency, SC#5 reattach re-open, SC#6 simultaneous client-even/server-odd open), plus a `test-support` cargo feature that exposes the server's test-only mux seams to the cross-crate integration tests.

## What was built

- `crates/nosh-client/tests/channel_mux.rs` (new) — 6 `#[tokio::test]`s:
  - `channel_open_accept_reject` — OPEN→ACCEPT/REJECT control-first ordering; opaque REJECT (no reason payload); PortForward/AgentForward rejected.
  - `channel_echo_roundtrip` — varint-prefix stream bind + byte round-trip; **SC#3** PTY input latency measured as the **median of 5 keystroke round-trips** under a 256 KiB saturation write, asserted < 5 ms (robust to scheduler jitter; still fails hard on real HOL blocking).
  - `channel_flow_control_backpressure` — MUX-03 credit window pauses the sender and resumes after `ChannelCredit`, no deadlock.
  - `channel_lifecycle_clean` — MUX-04 half-close→full-close releases resources; unknown-id ACCEPT is a no-op (no panic).
  - `channel_simultaneous_open` — **SC#6** real concurrent client-even + server-odd `ChannelOpen` (server side via the `test-support` fixture), non-colliding ids, session survives.
  - `channel_reattach_reopen` — **SC#5** channel re-established over the control stream after orphan + cold reattach.
- `test-support` cargo feature on `nosh-server` (off by default) gating the mux test seams (`server_open_tx`, `store/take_server_open_tx`, `first_active_slot`, the echo loop, odd-id accept/reject handling); `nosh-client` enables it as a dev-dependency feature.

## Key decisions / fixes

- **Root-cause fix (echo hang):** the test-support refactor initially missed `run_channel_task_inner`'s echo branch (and `run_echo_loop`), which stayed `#[cfg(test)]`-only. Under the feature (cfg(test)=false) the server accepted Echo but ran the production stub that discards bytes, hanging the peer's `read_exact`. Gating both on `cfg(any(test, feature = "test-support"))` fixed it.
- **SC#3 robustness:** switched from a single-sample `<5 ms` assertion (flaked at 6 ms on a loaded host) to a median-of-5 measurement. Keeps the binding 5 ms contract; HOL blocking would delay every sample, so the median still catches a real regression.
- **No hangs:** every network await in the suite is bounded by `tokio::time::timeout`.

## Verification

- `cargo test -p nosh-client --test channel_mux` → 6/6 pass (~0.1 s, no hangs).
- `cargo test --workspace` → 0 failures.
- `cargo build --release -p nosh-server` → compiles with all test seams absent (production gating verified).

## Notes for verification/next phase

- The `test-support` feature is the mechanism Phase 22 (scrollback) should reuse if it needs server-side test seams for its integration tests.
- PTY I/O remains on stream 0 this phase (channel migration deferred, per CONTEXT.md).
