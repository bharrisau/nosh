---
status: resolved
trigger: "Phase 17 live validation: 4 client bugs (host-key retry, Ctrl-C in connect window, false reconnect overlay, predictive echo broken)"
created: 2026-06-02
updated: 2026-06-02
---

## Current Focus

Investigation complete on all four bugs (read of main.rs, client.rs, predictor.rs, screen.rs, verifier.rs, transport.rs, platform.rs). Implementing fixes.

## Symptoms

expected: clean fatal abort on host-key mismatch; ctrl-c works in connect window; no false reconnect overlay when idle; predictive echo advances caret incl. space + arrows.
actual: infinite retry on host-key mismatch; stuck in connect loop on Windows; reconnect overlay after 5s idle; space/arrows not predicted, overwrite-mode caret.

## Eliminated

(none — root causes identified directly from code reading + live evidence)

## Evidence

- BUG-A: verifier.rs:74 returns `Error::General("host key mismatch ...")`. In main.rs:485-501 ALL connect() errors are treated transient → infinite retry. rustls surfaces the verifier error as a connection error inside `connect()`'s `?`.
- BUG-B: RawModeGuard clears ENABLE_PROCESSED_INPUT (client.rs:354) so Ctrl-C is delivered as byte 0x03 on stdin, NOT as a console signal → tokio ctrl_c never fires on Windows. Backoff select (main.rs:490-496) only awaits quit_signal()+sleep, nothing reads stdin → stuck.
- BUG-C: transport.rs:20 KEEP_ALIVE=15s keeps QUIC connection healthy when idle, but silence_sleep (main.rs:730) trips overlay at 5s of datagram silence. Idle healthy shell sends no datagrams → false overlay. Fix: gate on conn.close_reason() (true loss) not datagram silence.
- BUG-D: (1) on_input sets awaiting_first_cull=true when pending was empty (predictor.rs:305); cell_at + predicted_cursor() both return None while awaiting_first_cull (predictor.rs:265, 581) → keystroke render shows nothing until a datagram arrives & culls. This is the "space/char not shown until next char" + "caret not advancing". (2) sync_cursor_from_confirmed only syncs when pending empty (predictor.rs:239) — when a datagram confirms+empties pending, predicted_cursor snaps to confirmed; arrows that only moved predicted_cursor (no pending) get clobbered on every datagram → "arrows don't move / overwrite mode".

## Resolution

root_cause: see Evidence per bug.
fix: in progress
verification: pending
files_changed: []
