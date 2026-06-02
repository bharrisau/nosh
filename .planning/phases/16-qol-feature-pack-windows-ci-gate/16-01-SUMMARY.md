---
phase: 16-qol-feature-pack-windows-ci-gate
plan: "01"
subsystem: proto+server
tags: [osc-passthrough, clipboard, terminal-control, security, vte, quic-reliable-stream]
dependency_graph:
  requires: []
  provides: [TerminalControl-proto-variant, osc52-server-forwarding, drain-terminal-control]
  affects: [nosh-proto/messages, nosh-server/terminal, nosh-server/registry, nosh-server/server]
tech_stack:
  added: []
  patterns: [Option::take drain semantics, append-only postcard discriminant, write_message reliable stream]
key_files:
  created: []
  modified:
    - crates/nosh-proto/src/messages.rs
    - crates/nosh-proto/src/lib.rs
    - crates/nosh-server/src/terminal.rs
    - crates/nosh-server/Cargo.toml
    - crates/nosh-server/src/registry.rs
    - crates/nosh-server/src/server.rs
decisions:
  - "TerminalControl appended after Ack (discriminant 9) to preserve postcard discriminant order (append-only invariant)"
  - "vte std re-enabled with explicit osc_dispatch caps (OSC_52_MAX_BYTES=65536, MAX_TITLE_BYTES=1024) to re-mitigate CR-03"
  - "OSC 52 read/query form ('?') silently dropped in osc_dispatch before any store (D-16-01a)"
  - "drain_terminal_control returns (title, clipboard) tuple; both fields use Option::take to prevent double-forwarding"
  - "TerminalControl forwarded via write_message (reliable stream) NEVER via conn.send_datagram"
metrics:
  duration_minutes: 30
  completed: "2026-06-02T01:30:15Z"
  tasks_completed: 3
  tasks_total: 3
  files_modified: 6
---

# Phase 16 Plan 01: Server-side OSC Passthrough Infrastructure Summary

**One-liner:** Reliable-stream `Message::TerminalControl` proto variant + server OSC 52/0/2 detection, security gate, caps, drain methods, and forwarding from both session loops with vte std re-enabled.

## What Was Built

### Task 1: Message::TerminalControl proto variant (commit 1bb1f4d)

Added `Message::TerminalControl(TerminalControlPayload)` as discriminant 9 (appended after `Ack`, preserving postcard ordering). Defined `TerminalControlPayload` enum with:
- `Clipboard { selection: Vec<u8>, data: Vec<u8> }` — OSC 52 write-only (read/query form never carried per D-16-01a)
- `Title { title: String }` — OSC 0/2 window title

Exported `TerminalControlPayload` from `nosh-proto` lib. Added round-trip tests for both variants and extended `variant_name_never_leaks_token_bytes` with TerminalControl cases.

**Tests:** 30 nosh-proto tests pass (2 new round-trip tests + 2 new variant_name cases).

### Task 2: osc_dispatch read-gate + caps, drain methods, vte std re-enable (commit eea74c2)

- **vte Cargo.toml:** Removed `default-features = false`; replaced CR-03 comment with Phase-16 re-mitigation rationale.
- **Constants:** Added `OSC_52_MAX_BYTES: usize = 65_536` and `MAX_TITLE_BYTES: usize = 1_024`.
- **osc_dispatch b"52":** Added security gate `if data == b"?" { return; }` BEFORE any store (D-16-01a / T-16-01). Truncates data to `OSC_52_MAX_BYTES` before storing.
- **osc_dispatch b"0"|b"2":** Only stores title when `len <= MAX_TITLE_BYTES`; oversized titles silently discarded.
- **Drain methods:** `take_osc52(&mut self)` and `take_title(&mut self)` using `Option::take` (prevents double-forwarding).
- **Adversarial tests updated:** Now assert against `OSC_52_MAX_BYTES` and `MAX_TITLE_BYTES` instead of hard-coded 1024.
- **New tests:** `osc52_read_form_is_silently_dropped`, `osc52_write_form_is_stored`, `take_osc52_drains_and_clears`, `take_title_drains_and_clears`.
- **run_session match:** Added `Message::TerminalControl(_)` arm (treat as unexpected client→server frame).

**Tests:** 86 nosh-server tests pass (9 new: 4 terminal + 3 registry drain + existing adversarial updated).

### Task 3: SessionSlot drain accessor + forwarding in both server loops (commit 07f1442)

- **registry.rs:** Added `drain_terminal_control(&self) -> (Option<String>, Option<(Vec<u8>, Vec<u8>)>)` to `SessionSlot`. Locks `terminal_state` with poison-recovery pattern; calls `take_title()` then `take_osc52()`.
- **server.rs run_session:** After `push_output_and_parse`, drains `slot.drain_terminal_control()` and forwards title then clipboard as `Message::TerminalControl` via `write_message(&mut send, ...)` (reliable stream). Transport error breaks the loop.
- **server.rs run_reattach_session:** Identical drain+forward pattern added after `push_output_and_parse`.
- **Registry tests:** `drain_terminal_control_clipboard_write_yields_payload`, `drain_terminal_control_title_yields_payload`, `drain_terminal_control_osc52_read_form_yields_none` (defense-in-depth regression at the slot level).

**Tests:** 86 nosh-server + 1 main = 87 total pass.

## Security Properties Delivered

| Threat ID | Mitigated By |
|-----------|-------------|
| T-16-01 (OSC 52 read-form info disclosure) | `if data == b"?" { return; }` gate in `osc_dispatch` + registry test confirming drain yields None |
| T-16-02 (vte std re-enable DoS) | `OSC_52_MAX_BYTES=65536` truncation + `MAX_TITLE_BYTES=1024` discard + adversarial tests |
| T-16-04 (discriminant reordering) | TerminalControl appended after Ack; append-only invariant preserved |
| T-16-SC (package installs) | No new packages; only existing vte `std` feature re-enabled |

## Verification

- `cargo build --locked` (workspace): passes
- `cargo test -p nosh-proto --locked`: 30/30 pass
- `cargo test -p nosh-server --locked`: 87/87 pass
- OSC 52 read-form drop: `osc52_read_form_is_silently_dropped` + `drain_terminal_control_osc52_read_form_yields_none` both pass
- Adversarial large-OSC tests: pass against new explicit caps
- TerminalControl uses `write_message` (reliable stream) only; `send_datagram` carries only StateDiff payloads (verified by grep)

## Deviations from Plan

**1. [Rule 3 - Blocking] Added TerminalControl arm to client→server match in run_session**
- **Found during:** Task 2 compilation (non-exhaustive pattern error after adding TerminalControl to the Message enum)
- **Issue:** The existing `match msg` in `run_session`'s client→server receive arm did not cover `Ok(Message::TerminalControl(_))`, causing a compile error.
- **Fix:** Added `Ok(Message::TerminalControl(_))` to the existing "unexpected frames" arm (alongside SessionOpened, Reattach, ReattachOk, ReattachErr). TerminalControl is a server→client-only direction; any client that sends it is treated as a protocol error (SessionEnd::ClientClosed).
- **Files modified:** `crates/nosh-server/src/server.rs`
- **Committed with:** Task 2 commit (eea74c2)

None of the core plan deviations occurred — all security gates, caps, drain methods, and forwarding wired as designed.

## Known Stubs

None — all OSC 52 write payloads and OSC 0/2 titles are now forwarded via the reliable stream. The client-side consumption (Plan 02) is the next step; the server half is complete.

## Threat Flags

None — no new network endpoints, auth paths, file access patterns, or schema changes beyond those documented in the plan's threat model.

## Self-Check: PASSED

Files exist:
- crates/nosh-proto/src/messages.rs: FOUND
- crates/nosh-proto/src/lib.rs: FOUND
- crates/nosh-server/src/terminal.rs: FOUND
- crates/nosh-server/Cargo.toml: FOUND
- crates/nosh-server/src/registry.rs: FOUND
- crates/nosh-server/src/server.rs: FOUND

Commits:
- 1bb1f4d (Task 1: TerminalControl proto variant): FOUND
- eea74c2 (Task 2: osc_dispatch gate + caps + drain methods): FOUND
- 07f1442 (Task 3: SessionSlot drain accessor + server loop forwarding): FOUND
