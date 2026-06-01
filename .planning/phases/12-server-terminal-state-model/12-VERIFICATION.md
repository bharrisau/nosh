---
phase: 12-server-terminal-state-model
verified: 2026-06-01T00:00:00Z
status: gaps_found
score: 5/6 must-haves verified
requirements: [SYNC-02]
re_verification:
  previous_status: passed
  previous_score: 6/6
  note: >-
    Previous report was the SONNET EXECUTOR self-verification (no gaps section).
    This is an independent ADVERSARIAL re-verification on opus. It re-runs the
    suite AND adds adversarial VT probes the executor's tests did not cover. One
    probe reproduced a real integer-overflow panic / silent-wraparound on
    untrusted PTY input that the self-verify missed.
gaps:
  - truth: "Cursor coordinates and repeat counts from CSI params are clamped to grid bounds and never panic or unbounded-allocate"
    status: failed
    reason: >-
      CSI cursor-down (B) and cursor-right (C) compute `(self.cursor.row + n)`
      / `(self.cursor.col + n)` with PLAIN u16 addition BEFORE the `.min()`
      clamp. `n` is a raw repeat count from untrusted PTY output (vte caps it
      at u16::MAX = 65535). When the cursor is already at a nonzero
      row/col, the addition overflows u16. In a DEBUG build this PANICS
      ("attempt to add with overflow") — a denial-of-service on adversarial
      PTY output, exactly the highest-value risk this phase was told to harden.
      In a RELEASE build (no `overflow-checks` in [profile.release]) the add
      wraps silently, landing the cursor at a wrong-but-in-bounds position
      (state corruption) before the `.min()` clamp masks it. The clamp is
      applied to the WRONG (already-overflowed) value. The executor's
      `cursor_motion_clamped_to_grid_bounds` / `adversarial_huge_cursor_position_clamped`
      tests only exercised `CSI 9999;9999H` from the origin and `CSI 100A`
      (subtraction, saturating) — neither triggers the add-overflow path, so
      the self-verify reported 6/6 PASS while the defect shipped.
    artifacts:
      - path: "crates/nosh-server/src/terminal.rs"
        issue: >-
          Line 473: `self.cursor.row = (self.cursor.row + n).min(self.rows.saturating_sub(1));`
          and line 478: `self.cursor.col = (self.cursor.col + n).min(self.cols.saturating_sub(1));`
          use overflowing `+`. Reproduced: `CSI 24;80H` then `CSI 65535B` /
          `CSI 65535C` panics at terminal.rs:473 in debug.
    missing:
      - "Replace the `+` in CSI 'B' (line 473) and CSI 'C' (line 478) with `saturating_add` (e.g. `self.cursor.row.saturating_add(n).min(self.rows.saturating_sub(1))`)."
      - "Add a regression test that moves the cursor to a nonzero position FIRST, then issues `CSI 65535 B` and `CSI 65535 C`, asserting no panic and in-bounds cursor (the missing coverage)."
      - "Optional hardening: set `overflow-checks = true` on the release profile so silent wraparound on untrusted input can never mask a future arithmetic bug, or audit all cursor arithmetic for saturating semantics."
human_verification: []
---

# Phase 12: server-terminal-state-model Verification Report

**Phase Goal:** The server maintains an authoritative terminal-state model (grid + cursor +
echo state) fed from the same PTY-output callsite as the `SequencedOutputBuffer`, unit-tested
in isolation against known VT sequences (SYNC-02).
**Verified:** 2026-06-01
**Status:** GAPS_FOUND
**Re-verification:** Yes — independent adversarial opus pass over the sonnet executor's
self-reported PASS. Suite re-run + adversarial VT probes added.

---

## Goal Achievement

### Observable Truths

| # | Truth | Status | Evidence |
|---|-------|--------|----------|
| 1 | Feeding a known VT byte sequence into TerminalState produces the expected cell grid, cursor position, and echo state | ✓ VERIFIED | 67 lib tests pass; my own probes for plain text, CSI A/B/C/D/H, ED/EL, SGR confirm correct grid/cursor for non-adversarial input |
| 2 | OSC 52 clipboard-write sequences are detectable at osc_dispatch (parsed, not forwarded) | ✓ VERIFIED | `osc52_detected_and_no_clipboard_action` passes; `osc_dispatch` matches `params[0]==b"52"` (byte slice, not int — correct per Pitfall 7), stores `osc52_pending`; no clipboard side effect path exists in the code |
| 3 | OSC 0/2 title sequences are captured | ✓ VERIFIED | `osc2_sets_title`, `osc0_also_sets_title` pass |
| 4 | Scrollback bounded by a fixed line cap, cannot grow unboundedly under adversarial output | ✓ VERIFIED | `SCROLLBACK_LINE_CAP=10_000` enforced via `VecDeque::pop_front` on every `scroll_up`/`resize`; my probe fed 2M newlines (4MB input) — memory stayed bounded, no OOM, no panic |
| 5 | Cursor coordinates and repeat counts from CSI params are clamped to grid bounds and never panic or unbounded-allocate | ✗ FAILED | **CSI B/C overflow panic (debug) / silent wraparound (release).** `CSI 24;80H` then `CSI 65535B` panics at terminal.rs:473 "attempt to add with overflow". Clamp applied to wrong value. See Gaps. |
| 6 | Cell distinguishes terminal-default color (None) from explicit palette index incl. black (Some(0)) | ✓ VERIFIED | `default_color_is_none_not_some_zero`, `explicit_black_is_some_zero_distinct_from_default`, `sgr_39_49_reset_fg_bg_to_none` pass; truecolor `38;2;r;g;b` drain fix re-checked — `sgr_truecolor_38_2/48_2` confirm r/g/b are drained and don't misfire as SGR codes |

**Score:** 5/6 truths verified

---

### Required Artifacts

| Artifact | Expected | Status | Details |
|----------|----------|--------|---------|
| `crates/nosh-server/src/terminal.rs` | TerminalState (impl vte::Perform), Cell, EchoState, advance(), read API, bounded scrollback | ✓ EXISTS + SUBSTANTIVE | 1,267 lines; `impl vte::Perform for TerminalState` present; full read API (cursor/cell/echo_state/title/osc52_pending/size/viewport_rows). Wired but contains the SC5 arithmetic defect. |
| `crates/nosh-server/Cargo.toml` | vte 0.15 dependency | ✓ VERIFIED | `vte = "0.15"` present |
| `crates/nosh-server/src/lib.rs` | pub mod terminal | ✓ VERIFIED | `pub mod terminal;` at line 8 |
| `crates/nosh-server/src/registry.rs` | terminal_state field, push_output_and_parse, resize hook | ✓ EXISTS + SUBSTANTIVE | Field line 244; `push_output_and_parse` line 369 (push FIRST, advance SECOND); `resize` line 443 calls `terminal_state.resize` |
| `crates/nosh-server/src/server.rs` | 3 PTY-output callsites converted | ✓ VERIFIED | `push_output_and_parse` at lines 416, 506, 795 (3 callsites); `slot.resize` at 459, 820 |

**Artifacts:** 5/5 exist and are substantive (terminal.rs carries the SC5 defect)

---

### Key Link Verification

| From | To | Via | Status | Details |
|------|----|----|--------|---------|
| `Cell.fg/bg` | `nosh_proto::datagram::DiffRun.fg/bg` | `Option<u8>` type identity | ✓ WIRED | `use nosh_proto::datagram::{CellStyle, CursorPos}` at terminal.rs:38; `Cell.fg/bg: Option<u8>` == `DiffRun.fg/bg: Option<u8>` (datagram.rs:107/115). Zero-conversion confirmed. |
| `TerminalState::advance` | `vte::Parser::advance` | `std::mem::take` borrow-split | ✓ WIRED | Lines 207-209: `let mut parser = std::mem::take(&mut self.parser); parser.advance(self, bytes); self.parser = parser;` |
| `SessionSlot::push_output_and_parse` | `SequencedOutputBuffer::push + TerminalState::advance` | push FIRST then advance | ✓ WIRED | registry.rs 370-371: `output_buf.lock().unwrap().push(chunk)` then `terminal_state.lock().unwrap().advance(chunk)` |
| `server.rs PTY output callsites` | `slot.push_output_and_parse` | `out_rx.recv` chunk handling | ✓ WIRED | All 3 callsites (416, 506, 795) confirmed by grep |

**Wiring:** 4/4 connections verified

---

### Behavioral Spot-Checks

| Behavior | Command | Result | Status |
|----------|---------|--------|--------|
| Full lib suite passes | `cargo test -p nosh-server --lib` | 67 passed; 0 failed | ✓ PASS |
| Byte-identical replay regression | `push_output_and_parse_seq_replay_trim_byte_identical_to_push_output` | passes; asserts seq, replay_from, trim_acked all identical AND terminal observed chunks | ✓ PASS |
| SequencedOutputBuffer unmodified | `git show 5d91cbd -- registry.rs` (phase wiring commit) | no `-` lines on push/replay_from/trim_acked bodies — only additions | ✓ PASS |
| Adversarial: 4MB newline burst | throwaway probe `probe_megabyte_input_bounded_memory` | no panic, no OOM, cursor in-bounds | ✓ PASS |
| Adversarial: huge param from origin (`CSI 999999999 B`, `;H`) | throwaway probe | vte caps param at u16::MAX, `0 + 65535 = 65535` no overflow, clamps | ✓ PASS |
| Adversarial: empty/malformed DEC + CUP + SGR params | throwaway probes | no panic (`param[0]` safe: vte guarantees ≥1 subparam) | ✓ PASS |
| **Adversarial: CSI B/C repeat from NONZERO cursor** | throwaway probe `probe_csi_overflow_add` (`CSI 24;80H` + `CSI 65535B`) | **PANIC at terminal.rs:473 "attempt to add with overflow"** (debug); silent wraparound (release) | ✗ **FAIL** |

(All throwaway probe files removed; tree verified clean via `git status`.)

---

## Requirements Coverage

| Requirement | Status | Notes |
|-------------|--------|-------|
| SYNC-02: Server authoritative terminal-state model fed from same callsite as SequencedOutputBuffer, unit-tested | ⚠ PARTIAL | Model built, both buffers fed from one callsite (byte-identical reattach proven), 67 tests pass — BUT the model panics / silently corrupts cursor on adversarial CSI repeat counts, violating the "never panic" must-have. Must be fixed before SYNC-02 is fully satisfied. |

**Coverage:** 0/1 fully satisfied (1 partial — blocked on the SC5 overflow fix)

---

## Anti-Patterns Found

| File | Line | Pattern | Severity | Impact |
|------|------|---------|----------|--------|
| `crates/nosh-server/src/terminal.rs` | 473, 478 | Unchecked u16 `+` on cursor coord + untrusted repeat count, clamped only AFTER the add | 🛑 BLOCKER | Panic (debug, DoS) / silent state corruption (release) on adversarial PTY output — the exact threat surface (untrusted PTY bytes) this phase was tasked to harden |

Isolation constraint holds: `terminal.rs` imports only `std` and `nosh_proto::datagram` — no quinn/tokio/session/registry/server (grep clean). Lock discipline (`std::sync::Mutex`, documented `output_buf` → `terminal_state` order, never across `.await`) is correct.

---

## Human Verification Required

None — the gap is reproducible programmatically.

---

## Gaps Summary

The phase is one trivial fix away from done. Everything wires correctly, the reattach path is
provably byte-identical (the regression test asserts seq/replay/trim equivalence, not just
compilation), color/OSC/echo-mode/scrollback-cap semantics are all correct, and the bulk of the
adversarial surface (huge params from origin, 4MB input, malformed/empty params, garbage bytes)
is safe.

The single BLOCKER is an integer-overflow on CSI cursor-down (`B`, line 473) and cursor-right
(`C`, line 478): the repeat count `n` (untrusted, up to 65535) is added to the current cursor
coordinate with plain `+` *before* the `.min()` clamp. From a nonzero cursor this overflows
u16 — a panic in debug builds (a DoS on adversarial PTY output, the headline risk for this
phase) and a silent wraparound in release builds (the release profile sets no `overflow-checks`),
which clamps the *wrong* value. The executor's adversarial tests only fired huge motions from
the origin or used the saturating subtraction path (`A`/`D`), so they never reached the add and
the self-verify reported a false 6/6 PASS.

Fix: use `saturating_add` on lines 473 and 478, and add a regression test that moves the cursor
to a nonzero position before issuing a max-count `CSI B`/`CSI C`.

---

## Verification Metadata

**Verification approach:** Goal-backward, adversarial. Must-haves merged from ROADMAP Phase 12
(4 success criteria) + 12-01/12-02 PLAN frontmatter (6 truths).
**Must-haves source:** ROADMAP.md SC1-4 + PLAN frontmatter truths.
**Automated checks:** 67 lib tests passed (`cargo test -p nosh-server --lib`).
**Adversarial probes:** 8 throwaway integration probes run (debug + release); 2 reproduced the
overflow defect; all probe files removed afterward (tree clean).
**Isolation check:** grep for quinn/tokio/session/registry/server imports in terminal.rs — clean.
**Reattach regression:** byte-identical replay/trim/seq test passes; git diff confirms
SequencedOutputBuffer method bodies unmodified by the phase commit.

---
*Verified: 2026-06-01T00:00:00Z*
*Verifier: Claude Opus 4.8 (independent adversarial re-verification, gsd-verifier)*
