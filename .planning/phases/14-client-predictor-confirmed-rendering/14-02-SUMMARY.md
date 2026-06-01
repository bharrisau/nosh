---
phase: 14-client-predictor-confirmed-rendering
plan: "02"
subsystem: nosh-client
tags: [datagram, display, ClientScreen, epoch-ack, run_pump, security]
dependency_graph:
  requires:
    - nosh_client::screen::ClientScreen (from 14-01)
    - nosh_proto::datagram::decode_datagram
    - nosh_proto::datagram::encode_epoch_ack
    - quinn::Connection::read_datagram
    - quinn::Connection::send_datagram
  provides:
    - run_pump datagram arm (apply + render + epoch-ack)
    - single display path via ClientScreen::render_to_stdout
    - PtyData display removal (D-14-02)
    - datagram epoch-ack on datagram channel (D-14-03a)
  affects:
    - crates/nosh-client/src/main.rs
tech_stack:
  added: []
  patterns:
    - "D-14-02: datagram-only display; PtyData arm advance-counter-only"
    - "D-14-03: highest_applied counter preserved in PtyData arm for cold-reattach Ack"
    - "D-14-03a: epoch-ack on datagram channel (encode_epoch_ack), distinct from reliable-stream Ack{seq}"
    - "T-14-06: monotonic epoch gate diff.epoch > last_applied_epoch before apply"
    - "T-14-08: terminal injection blocked — PtyData stdout write removed"
    - "Pitfall 1 pattern: render_to_stdout buffers to Vec<u8>, then async flush to tokio stdout"
    - "Pitfall 3 guard: epoch-ack via conn.send_datagram, NOT on reliable stream"
key_files:
  created: []
  modified:
    - crates/nosh-client/src/main.rs
decisions:
  - "conn + cols/rows threaded into run_pump (Task 1); fresh screen per invocation IS the reattach repaint reset — no need to hoist screen or call reset_physical explicitly"
  - "reattach_session re-reads crossterm::terminal::size() for its run_pump call to pick up any resize that happened during reconnect"
  - "Datagram transport Err returns PumpOutcome::TransportDrop matching reliable-stream behavior"
  - "Stale epoch and non-StateDiff datagrams silently discarded (not errors); only transport-layer Err triggers drop"
metrics:
  duration: "~20 minutes"
  completed: "2026-06-02"
  tasks_completed: 2
  files_changed: 1
---

# Phase 14 Plan 02: run_pump Datagram Arm — Confirmed Rendering Summary

**One-liner:** `run_pump` select! loop wired to `ClientScreen` via datagram arm: decode StateDiff, apply, render via single display path, emit datagram epoch-ack; PtyData arm reduced to highest_applied counter only.

## What Was Built

`crates/nosh-client/src/main.rs` modified to implement the complete datagram display path:

**Task 1 — Thread conn into run_pump; instantiate ClientScreen; reset_physical on reattach:**
- `run_pump` signature extended: `conn: &quinn::Connection` as first param, `cols: u16, rows: u16` added (needed to size `ClientScreen`)
- Both callers updated: `fresh_session` at line 551 and `reattach_session` at line 600
- `reattach_session` re-reads `crossterm::terminal::size()` before calling `run_pump` to pick up terminal dimensions current at reconnect time
- `let mut screen = nosh_client::screen::ClientScreen::new(cols, rows)` instantiated before the select! loop
- A per-run_pump fresh screen naturally gives a blank physical grid — this is the reattach full-repaint reset (symmetric with D-13-01b server baseline). Comment documents the invariant: reset_physical() is only needed if screen is hoisted above run_pump scope

**Task 2 — Datagram arm (apply + render + epoch-ack) and PtyData arm display removal:**
- New `datagram = conn.read_datagram()` arm added to select!, AFTER the reliable-stream arm
- On `Ok(bytes)`: `decode_datagram(&bytes)` → on `Ok(diff)`: monotonic epoch gate (`diff.epoch > screen.last_applied_epoch()`) → `screen.apply(&diff)` → buffer to `Vec<u8>` via `render_to_stdout` → async flush to tokio stdout → emit `conn.send_datagram(encode_epoch_ack(diff.epoch))`
- On `Err(_)`: returns `PumpOutcome::TransportDrop`
- Stale epoch and decode errors silently discarded
- `PtyData` arm: `stdout.write_all(&data)` and `stdout.flush()` removed (D-14-02); `highest_applied.saturating_add(1)` preserved (D-14-03)
- Reliable-stream `ack_interval.tick()` arm unchanged (D-14-03a distinction)
- Module doc comment updated to document D-14-02/D-14-03 invariants

## Security Invariants Verified

| Threat | Mitigation | Verified |
|--------|-----------|---------|
| T-14-05: malformed datagram | `decode_datagram` validates framing, caps MAX_RUNS=4096 → Err on bad input → silently discarded | YES |
| T-14-06: stale-epoch replay | `diff.epoch > screen.last_applied_epoch()` gate before apply | YES |
| T-14-07: epoch-ack on wrong channel | `conn.send_datagram(encode_epoch_ack(...))` — datagram channel only, not reliable stream | YES |
| T-14-08: terminal injection via PtyData | `stdout.write_all(&data)` removed from PtyData arm; grep confirms 0 stdout writes in that arm | YES |

## Commits

| Task | Commit | Files |
|------|--------|-------|
| Task 1: conn+dims into run_pump; ClientScreen instantiation | `e20cba3` | `crates/nosh-client/src/main.rs` |
| Task 2: datagram arm + PtyData display removal | `7267307` | `crates/nosh-client/src/main.rs` |

## Deviations from Plan

**1. [Rule 2 - Missing functionality] reattach_session re-reads terminal size**

- **Found during:** Task 1 implementation
- **Issue:** The plan says to pass `cols/rows` from `fresh_session`'s startup `crossterm::terminal::size()` call, and notes the `reattach_session` path also needs to pass dims. The plan does not specify which dims to use for reattach. Using the startup dims (captured before the reconnect supervisor loop) risks stale dims if the terminal was resized during reconnection.
- **Fix:** `reattach_session` calls `crossterm::terminal::size().unwrap_or((80, 24))` to get current dims at reconnect time before calling `run_pump`. This ensures the ClientScreen is sized to the actual terminal at reconnect, which is correct behavior.
- **Files modified:** `crates/nosh-client/src/main.rs` (line 599)
- **Commit:** `e20cba3`

## Known Stubs

None introduced in Plan 02. The `ConnectionLossOverlay` stub (Plan 01) is documented in 14-01-SUMMARY.md and remains intentional.

## Threat Flags

No new security surface beyond what the plan's threat_model covers. All four threat mitigations (T-14-05, T-14-06, T-14-07, T-14-08) are implemented and verified by grep/build.

## Self-Check: PASSED

- `crates/nosh-client/src/main.rs` exists: FOUND
- Commit `e20cba3` (Task 1) exists: FOUND
- Commit `7267307` (Task 2) exists: FOUND
- `grep -n "fn run_pump"` shows `conn: &quinn::Connection` as first parameter: CONFIRMED (line 607)
- `grep -c "run_pump(conn,"` returns 2: CONFIRMED (lines 551 + 600)
- `grep -n "ClientScreen::new"` matches (line 634): CONFIRMED
- `cargo build -p nosh-client` exits 0: CONFIRMED
- `cargo clippy -p nosh-client` exits 0 (1 pre-existing too_many_arguments warning, not an error): CONFIRMED
- `grep -n "conn.read_datagram()"` matches (line 676): CONFIRMED
- `grep -n "encode_epoch_ack"` matches (line 696): CONFIRMED
- `grep -n "send_datagram(nosh_proto::datagram::encode_epoch_ack"` matches (line 696-697): CONFIRMED
- PtyData arm has 0 stdout writes (grep confirms stdout.write_all at line 690, inside datagram arm only): CONFIRMED
- `highest_applied.saturating_add` in PtyData arm (line 657): CONFIRMED
- Reliable-stream ack_interval.tick arm unchanged (line 747-753): CONFIRMED
