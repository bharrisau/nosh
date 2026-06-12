---
phase: 19-full-screen-tui-rendering-correctness
plan: "03"
subsystem: nosh-server/terminal
tags: [security, osc, vte, dos-hardening, tdd, sec-03]
dependency_graph:
  requires: [19-01, 19-02]
  provides: [OSC_ACCUMULATION_MAX pre-bound, parser resync on overflow]
  affects: [fuzz/fuzz_targets/osc_accumulation.rs]
tech_stack:
  added: []
  patterns:
    - "osc_prefilter() method: O(n) byte-scan before parser.advance() to enforce 1 MiB OSC cap"
    - "vte::Parser::default() resync: replace mid-OSC parser with ground-state parser after truncated advance"
    - "Shadow-state pattern: in_osc + osc_byte_count mirror vte's private State::OscString"
key_files:
  created: []
  modified:
    - crates/nosh-server/src/terminal.rs
    - fuzz/fuzz_targets/osc_accumulation.rs
decisions:
  - "D-19-01: 1 MiB OSC_ACCUMULATION_MAX bounds vte allocation before parser.advance(); distinct from storage caps"
  - "D-19-02: overflow truncates and resyncs parser to ground (vte::Parser::default()); session is not killed"
  - "D-19-03: after resync, OSC 52 and OSC 0/2 title still dispatch correctly"
  - "Pitfall 2 honoured: parser reset happens AFTER feeding the truncated prefix, not before"
metrics:
  duration_minutes: 25
  completed_date: "2026-06-07"
  tasks_completed: 2
  files_modified: 2
requirements: [SEC-03]
---

# Phase 19 Plan 03: SEC-03 OSC Accumulation Pre-Bound Summary

One-liner: 1 MiB OSC accumulation pre-bound added before vte's unbounded osc_raw buffer via osc_prefilter() with ground-state parser resync on overflow, proven RED-before / GREEN-after with a 10 MiB multi-chunk regression test.

## What Was Built

Closes the post-authentication OSC OOM vector (SEC-03 / T-19-06 / T-19-07). Before this plan, `TerminalState::advance()` fed PTY bytes directly to `parser.advance()` with no bound on how many bytes vte could accumulate in its internal `osc_raw` Vec. With `vte 0.15` and the `std` feature enabled, `action_osc_put` pushes bytes unconditionally — a hostile or buggy app emitting a giant multi-chunk OSC (e.g. a 10 MiB title) would grow `osc_raw` until the server OOMed.

### Changes

**`crates/nosh-server/src/terminal.rs`:**

- New constant: `pub const OSC_ACCUMULATION_MAX: usize = 1_048_576;` (1 MiB), positioned after the existing `MAX_TITLE_BYTES` constant with matching doc+const pattern. This is the allocation cap — distinct from the storage caps `OSC_52_MAX_BYTES` (64 KiB) and `MAX_TITLE_BYTES` (1 KiB) which run after vte has already buffered the full payload.

- New fields on `TerminalState`:
  - `osc_byte_count: usize` — running count of bytes accumulated in the current in-flight OSC across advance() calls; reset at OSC start/end and on overflow.
  - `in_osc: bool` — shadow state mirroring vte's private `State::OscString`; set on ESC ] / 0x9D, cleared on BEL / ST.
  Both fields initialised to `0`/`false` in `new()` and reset in `esc_dispatch` RIS (`b'c'`).

- `advance()` now calls `osc_prefilter(bytes)` before the `std::mem::take` borrow-split. If the filter returns a truncated slice, the truncated prefix is fed to the parser, then `self.parser` is replaced with `vte::Parser::default()` (ground state). If no overflow, `self.parser` is restored normally.

- New private method `osc_prefilter<'a>(&mut self, bytes: &'a [u8]) -> &'a [u8]`: O(n) byte scanner that tracks `in_osc`/`osc_byte_count` across calls. Detects OSC starts (0x9D, ESC ]), accumulates payload bytes, detects ends (BEL, ST = ESC \). Returns `bytes` unchanged on no overflow; returns `&bytes[..i]` (truncated to the offending byte) on overflow, resetting both fields.

- New test `oversized_multi_chunk_osc_is_bounded_then_resyncs`: feeds a 10 MiB OSC 2 title in 4096-byte chunks, then asserts title is not held at 10 MiB, then feeds a normal OSC 2 title ("OK") and asserts it succeeds, then feeds OSC 52 and asserts it dispatches. Proves RED-before (compile error on missing constant) and GREEN-after (runs in ~1 second, all assertions pass).

**`fuzz/fuzz_targets/osc_accumulation.rs`:**

- Imports `OSC_ACCUMULATION_MAX` alongside the existing `OSC_52_MAX_BYTES` and `MAX_TITLE_BYTES`.
- Adds a deterministic multi-chunk 10 MiB OSC drive (`state2`) alongside the existing single-chunk `data` path. Asserts no OOM/panic, title bounded after overflow, and a trailing normal OSC 2 + OSC 52 both parse correctly. Fuzz target builds under `cargo +nightly fuzz build osc_accumulation`.

## TDD Gate Compliance

RED gate: `test(19-03): add failing RED test for SEC-03 OSC accumulation pre-bound` (commit `1eab3e6`) — test fails to compile because `OSC_ACCUMULATION_MAX` is referenced but not yet defined. This is the correct RED observation: the constant did not exist before the implementation was added.

GREEN gate: `feat(19-03): implement SEC-03 OSC accumulation pre-bound (GREEN gate)` (commit `4ca200a`) — all 99 nosh-server tests pass including `oversized_multi_chunk_osc_is_bounded_then_resyncs` in 0.97 seconds.

## Verification

- `cargo test -p nosh-server --lib`: 99 passed, 0 failed.
- `cargo +nightly fuzz build osc_accumulation`: compiled successfully.
- `cargo clippy -p nosh-server --lib -- -D warnings -A clippy::type-complexity`: clean (the `-A clippy::type-complexity` suppresses a pre-existing warning in `registry.rs` that is unrelated to this plan — see Deferred Items).
- `cargo build`: workspace builds clean.

## Deviations from Plan

None — plan executed exactly as written.

The clippy `-D warnings` check surfaces a pre-existing `type_complexity` lint in `registry.rs:521` (`drain_terminal_control` return type). This warning existed before this plan and is in a file not touched by this plan. It is logged to `deferred-items.md` per policy and does NOT block this plan's success criteria.

## Known Stubs

None. The pre-filter is fully wired; `OSC_ACCUMULATION_MAX` is enforced in every `advance()` call path.

## Threat Flags

None introduced. The change narrows the PTY output → TerminalState trust boundary (tighter bound on OSC accumulation); it does not introduce new network endpoints, auth paths, file access patterns, or schema changes.

## Deferred Items

- Pre-existing `clippy::type_complexity` warning in `crates/nosh-server/src/registry.rs:521` — logged, not this plan's scope.

## Self-Check: PASSED

- `crates/nosh-server/src/terminal.rs` — exists and modified (confirmed by edit operations).
- `fuzz/fuzz_targets/osc_accumulation.rs` — exists and modified (confirmed by edit operation).
- RED commit `1eab3e6` — `git log --oneline` confirms.
- GREEN commit `4ca200a` — `git log --oneline` confirms.
- All 99 tests pass — confirmed by `cargo test -p nosh-server --lib` output.
