---
phase: 14-client-predictor-confirmed-rendering
verified: 2026-06-02T00:00:00Z
status: passed
score: 11/11 must-haves verified
overrides_applied: 0
re_verification:
  previous_status: none
  previous_score: n/a
---

# Phase 14: Client Predictor — Confirmed Rendering Verification Report

**Phase Goal:** The client renders the CONFIRMED terminal screen from received state-sync datagrams through a single screen-composition path (`ClientScreen` + `render_to_stdout`) — datagram display path proven end-to-end before the Phase 15 speculative overlay. Both the confirmed-grid apply path and the `run_pump` datagram wiring must work; `PtyData` is kept ONLY to advance `highest_applied`/reattach-ack (not written to stdout); the real client emits the datagram epoch-ack.
**Verified:** 2026-06-02
**Status:** passed
**Re-verification:** No — initial verification

## Goal Achievement

### Observable Truths

| #   | Truth                                                                                                                          | Status     | Evidence |
| --- | ---------------------------------------------------------------------------------------------------------------------------- | ---------- | -------- |
| 1   | `run_pump` has a `conn.read_datagram()` arm routing `StateDiff` → `ClientScreen.render_to_stdout()` (SC1)                     | ✓ VERIFIED | main.rs:678–717 datagram arm decode→apply→render→epoch-ack; single `stdout.write_all` (line 692) lives only here |
| 2   | Datagram-rendered confirmed grid matches server `TerminalState` visible chars, end-to-end (SC2, PREDICT-01)                    | ✓ VERIFIED | render.rs:69–245 full 80×24 cell-by-cell `assert_eq!` on ch+fg+bg+style; live integration render.rs:324–399 finds "hello"; both pass |
| 3   | `highest_applied` keeps advancing from `PtyData` on the reliable stream (cold-reattach Ack not broken) (SC3, D-14-03)          | ✓ VERIFIED | main.rs:658 `*highest_applied = highest_applied.saturating_add(1)` retained in PtyData arm; reliable `send_ack` at main.rs:757 intact; reattach.rs (3) + persistence.rs (3) pass |
| 4   | `ConnectionLossOverlay` exists as a no-op `Overlay` stub wired into the render path (SC4, D-14-01a)                            | ✓ VERIFIED | screen.rs:88–94 returns `None`; pre-loaded into `overlays` (line 137); composed in `compose_desired` (line 271–283); test `connection_loss_overlay_is_noop` passes |
| 5   | Display comes exclusively from datagrams — PtyData arm no longer writes to stdout (D-14-02)                                    | ✓ VERIFIED | grep: only `stdout.write_all` in main.rs is the datagram arm (line 692); PtyData arm discards `let _ = data` (line 657); file-level comment at :55 updated |
| 6   | Client emits real datagram epoch-ack (`encode_epoch_ack`), distinct from reliable-stream `Ack{seq}` (D-14-03a)                 | ✓ VERIFIED | main.rs:703–704 `conn.send_datagram(encode_epoch_ack(diff.epoch))`; encode_epoch_ack uses `TAG_CLIENT_EPOCH=0x02` (datagram.rs:161), separate from stream `send_ack` |
| 7   | `apply` updates confirmed grid only when epoch strictly > `last_applied_epoch` (D-14-05 monotonic; T-14-03)                    | ✓ VERIFIED | screen.rs:166 `if diff.epoch <= self.last_applied_epoch { return; }`; run_pump arm also gates (main.rs:683); tests `apply_monotonic_same_epoch_is_noop`/`_lower_epoch_` pass |
| 8   | `render_to_stdout` composes desired=confirmed+overlays, diffs vs physical, emits minimal ANSI, sets physical=desired           | ✓ VERIFIED | screen.rs:302–363; idempotency tests `duplicate_datagram_*` pass (buf2 < buf1, no cell chars) |
| 9   | OOB row/col or oversized run in a malformed StateDiff is clamped/skipped — never panics or writes OOB (T-14-01)               | ✓ VERIFIED | screen.rs:197 `continue` on OOB row, :203 `break` on OOB col; tests `apply_oob_row_*`/`apply_oob_col_*` pass |
| 10  | CR-01: oversized/zero StateDiff dimensions rejected BEFORE resize allocation (T-14-02 OOM guard)                               | ✓ VERIFIED | screen.rs:175–187 guard (`cols==0\|\|rows==0\|\|cols>512\|\|rows>256`) precedes resize() at :190; 5 unit tests incl. zero + at-cap all pass |
| 11  | Reattach path starts run_pump with a blank physical grid → first post-resume datagram forces full repaint                     | ✓ VERIFIED | main.rs:635 fresh `ClientScreen::new` per run_pump invocation (documented as the reattach repaint reset, :630–634); `reset_physical` available (screen.rs:371) + called on write failure (main.rs:694,697) |

**Score:** 11/11 truths verified

### Required Artifacts

| Artifact | Expected | Status | Details |
| -------- | -------- | ------ | ------- |
| `crates/nosh-client/src/screen.rs` | ClientScreen compositor, Cell, Overlay, ConnectionLossOverlay, apply/render/resize/reset_physical | ✓ VERIFIED | 835 lines; `pub struct ClientScreen` present; uses `nosh_proto::datagram`; no production `nosh_server` import (only doc comments) |
| `crates/nosh-client/src/lib.rs` | `pub mod screen` export | ✓ VERIFIED | lib.rs:6 `pub mod screen;` |
| `crates/nosh-client/src/main.rs` | run_pump datagram arm, conn threading, PtyData arm change, screen instantiation | ✓ VERIFIED | `conn.read_datagram()` (678), `ClientScreen::new` (635), 2 callers pass conn (551,600) |
| `crates/nosh-client/tests/render.rs` | grid-comparison + idempotency + live integration tests | ✓ VERIFIED | 399 lines; 3 tests; full-grid loop (218–219); `TerminalState::new`, `read_datagram`, `send_input`, `contains("hello")` all present |

### Key Link Verification

| From | To | Via | Status | Details |
| ---- | -- | --- | ------ | ------- |
| screen.rs | nosh_proto::datagram | StateDiff/DiffRun/CellStyle/CursorPos reuse (D-14-04) | ✓ WIRED | `use nosh_proto::datagram::{CellStyle, CursorPos, StateDiff}` (screen.rs:30) |
| main.rs | nosh_client::screen::ClientScreen | `screen.apply(&diff); screen.render_to_stdout(&mut buf)` | ✓ WIRED | apply (684), render (688) |
| main.rs | server datagram channel | `conn.send_datagram(encode_epoch_ack(diff.epoch))` | ✓ WIRED | main.rs:703–704 |
| render.rs | nosh_server::terminal::TerminalState | dev-dep import; drive both, compare grids | ✓ WIRED | render.rs:23,75 |
| render.rs | conn.read_datagram() | live integration loop | ✓ WIRED | render.rs:367 |

### Behavioral Spot-Checks

| Behavior | Command | Result | Status |
| -------- | ------- | ------ | ------ |
| Workspace builds | `cargo build --workspace` | Finished, 0 errors | ✓ PASS |
| Screen unit tests | `cargo test -p nosh-client --lib` | 19 passed, 0 failed | ✓ PASS |
| Render tests (incl. live integration, not skipped) | `cargo test -p nosh-client --test render` | 3 passed, 0 failed | ✓ PASS |
| Full workspace suite | `cargo test --workspace` | all suites 0 failed (client lib 19, render 3, sync 3, reattach 3, persistence 3, server 79, proto 28, …) | ✓ PASS |
| Lint gate | `cargo clippy -p nosh-client --tests -- -D warnings` | exit 0, no warnings | ✓ PASS |

### Requirements Coverage

| Requirement | Source Plan | Description | Status | Evidence |
| ----------- | ----------- | ----------- | ------ | -------- |
| PREDICT-01 | 14-01, 14-02, 14-03 | Client renders confirmed screen from datagrams via single composition path, never direct stdout once predictor exists, matching raw PTY output | ✓ SATISFIED | Single display path enforced (only datagram-arm stdout writer); full-grid equality test + live "hello" integration test prove datagram render matches server TerminalState |

### Anti-Patterns Found

| File | Line | Pattern | Severity | Impact |
| ---- | ---- | ------- | -------- | ------ |
| screen.rs | 501 | `"XXXXX".to_string()` matched `XXX` grep | ℹ️ Info | Test fixture string (verifies monotonic guard ignores it), not a debt marker — no impact |

No `TBD`/`FIXME`/`HACK`/`PLACEHOLDER`/"not yet implemented" markers in any phase-14-modified file. The `nosh_server` strings in screen.rs are doc-comment references explaining the deliberate non-import (D-14-04).

## Adversarial Mandate Findings

1. **CR-01 (OOM DoS) fix confirmed real and correct:** screen.rs:175–187 — the dimension guard (`cols==0 || rows==0 || cols>MAX_TERMINAL_COLS(512) || rows>MAX_TERMINAL_ROWS(256)`) sits AFTER the monotonic check and BEFORE `resize()` (line 190) which is the only allocation site. Early-return leaves grid + epoch unchanged. Zero dimensions explicitly handled. Five unit tests (`apply_oversized_cols/rows_is_rejected_grid_unchanged`, `apply_zero_cols/rows_is_rejected_no_panic`, `apply_max_allowed_dimensions_is_accepted`) feed oversized/zero/at-cap dims and assert no panic + grid/epoch unchanged. All pass.
2. **Single display path confirmed:** grep shows the ONLY `stdout.write_all` in main.rs is in the datagram arm (line 692). PtyData arm (657) discards data, retains `highest_applied.saturating_add(1)` (658). Reliable-stream `send_ack` intact (757).
3. **Monotonic epoch guard confirmed at both layers:** apply() (screen.rs:166) and the run_pump arm (main.rs:683). No rollback.
4. **OOB safety confirmed:** continue on OOB row (197), break on OOB col (203); both unit-tested with no panic.
5. **epoch-ack distinct confirmed:** `encode_epoch_ack` → `TAG_CLIENT_EPOCH=0x02` on the datagram channel via `send_datagram`; reliable-stream `Ack{seq}` is the separate `send_ack` path.
6. **PREDICT-01 e2e confirmed:** render.rs asserts full 80×24 cell-by-cell (ch+fg+bg+style), not a spot-check; live integration test asserts "hello" lands in the confirmed grid and ran (not skipped — /bin/sh present).

WR-01 (reset_physical on write failure) and WR-02 (log on transport drop) fixes are present in main.rs (692–698, 670, 713). IN-01/IN-02 were info-level and intentionally skipped; neither affects goal achievement.

### Human Verification Required

None. All success criteria are programmatically verifiable and verified: the live integration test exercises the real client↔server datagram path end-to-end on Linux, and the full-grid comparison test mechanically asserts visual equivalence.

### Gaps Summary

No gaps. All four ROADMAP success criteria, all phase-goal clauses, the PREDICT-01 requirement, the CR-01 critical fix, and both warning fixes are verified against the actual codebase. Build, full test suite (all crates, 0 failures), and the `-D warnings` clippy gate all pass.

---

_Verified: 2026-06-02_
_Verifier: Claude (gsd-verifier)_
