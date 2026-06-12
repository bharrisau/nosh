---
phase: 19-full-screen-tui-rendering-correctness
plan: "04"
subsystem: datagram-protocol, client-predictor
tags: [tui, predictor-suppression, wire-format-break, alt-screen]
dependency_graph:
  requires: ["19-01", "19-02"]
  provides: ["alt_screen propagated via StateDiff", "predictor reset on alt-screen entry"]
  affects: ["20-repaint-pacing", "datagram wire format"]
tech_stack:
  added: []
  patterns: ["StateDiff field addition", "datagram arm transition hook", "predictor.reset() suppression"]
key_files:
  created: []
  modified:
    - crates/nosh-proto/src/datagram.rs
    - crates/nosh-server/src/server.rs
    - crates/nosh-client/src/main.rs
    - crates/nosh-client/src/predictor.rs
    - crates/nosh-client/src/screen.rs
decisions:
  - "Option A chosen: alt_screen carried in StateDiff (not TerminalControl stream) — lower latency, natural fit alongside cols/rows/cursor"
  - "Wire-format break is intentional and acceptable: v1.3 client and server are always the same version (REQUIREMENTS.md Out-of-Scope)"
metrics:
  duration: "504s"
  completed: "2026-06-07"
  tasks: 2
  files: 5
---

# Phase 19 Plan 04: TUI-05 alt_screen Propagation + Predictor Suppression Summary

Propagated the server's `?1049` alternate-screen state to the client via a new `alt_screen: bool` field on `StateDiff` and suppressed speculative local-echo predictions on alt-screen entry.

## What Was Built

**Task 1 — `alt_screen` field on `StateDiff` and `build_state_diff` wiring (c376b0a)**

Added `pub alt_screen: bool` to `StateDiff` in `nosh-proto/src/datagram.rs`, positioned after `cursor` and before `runs` (grouped with the non-content terminal-state fields). In `server.rs`, the `build_state_diff` function's `slot.with_terminal_state(...)` closure now extracts `ts.echo_state().alt_screen` (a bool copy — synchronous, no `.await`) and threads it through to the constructed `StateDiff`. Every `StateDiff` literal site in the codebase was updated to include the field (compile-error driven discovery). Two new round-trip tests confirm `alt_screen: true` and `alt_screen: false` survive postcard encode→decode.

**Task 2 — Client predictor suppression hook (6731356)**

In `main.rs`, declared `let mut was_alt_screen = false;` before the `select!` loop (same scope and pattern as the existing resize tracking locals). In the datagram arm, immediately after `screen.apply(&diff)`, the transition `diff.alt_screen && !was_alt_screen` triggers `predictor.reset()` — clearing all pending speculative predictions and going tentative. `was_alt_screen` is then updated to `diff.alt_screen`. The fix required adding `alt_screen: false` to all `StateDiff` literals in `predictor.rs` and `screen.rs` test helpers (discovered by compiler).

## Wire-Format Breaking Change

Adding `alt_screen: bool` to `StateDiff` is an **intentional postcard wire-format breaking change**. Under postcard encoding, a `bool` field is encoded as a 1-byte varint (0x00 for false, 0x01 for true). Any client built against the pre-v1.3 wire format will misparse new datagrams (and vice versa).

This is acceptable per the project's stated scope: REQUIREMENTS.md §Out-of-Scope explicitly notes that cross-version wire compatibility is not required — `nosh` is built from source and client/server are always the same version. Documented as Pitfall 6 from 19-RESEARCH.md.

## Deviations from Plan

None — plan executed exactly as written.

StateDiff literal sites were more numerous than the plan's examples listed (the plan mentioned ~350 and ~280-286/294-350 in server.rs and the test helper in datagram.rs). Compile-error discovery surfaced additional literal sites in `encode_datagram`'s internal candidate construction (line 289), plus all test-only StateDiff literals in `predictor.rs` (13 sites) and `screen.rs` (12 sites). All were fixed with `alt_screen: false` as required.

## Pre-existing Clippy Warning (Out of Scope)

`cargo clippy --workspace -- -D warnings` fails on a pre-existing `type_complexity` warning in `crates/nosh-server/src/registry.rs:521` (`drain_terminal_control` return type). This warning exists in the codebase before this plan and is unrelated to any file changed here. Logged for deferred resolution.

## Known Stubs

None — `alt_screen` is wired from the live `ts.echo_state().alt_screen` server-side and consumed directly in the datagram arm client-side. No placeholder or hardcoded value.

## Threat Flags

None — no new network endpoints, auth paths, or schema changes at trust boundaries beyond the documented intentional wire-format break (T-19-08, already in plan's threat register).

## Self-Check

```
FOUND: crates/nosh-proto/src/datagram.rs (alt_screen field at line 77)
FOUND: crates/nosh-server/src/server.rs (alt_screen extracted and used in StateDiff construction)
FOUND: crates/nosh-client/src/main.rs (was_alt_screen declaration and hook)
FOUND commit: c376b0a
FOUND commit: 6731356
cargo test -p nosh-proto --lib: 32 tests passed
cargo test -p nosh-client --lib: 96 tests passed
cargo build -p nosh-proto -p nosh-server -p nosh-client: Finished (no errors)
```

## Self-Check: PASSED
