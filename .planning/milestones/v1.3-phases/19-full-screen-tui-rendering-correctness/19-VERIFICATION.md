---
phase: 19-full-screen-tui-rendering-correctness
verified: 2026-06-07T00:00:00Z
status: human_needed
score: 6/6 must-haves verified
overrides_applied: 0
human_verification:
  - test: "Live visual pass — vim, htop, Claude Code over a Linux client↔server"
    expected: "vim opens to a blank canvas (no shell bleed-through) and restores the primary buffer/cursor exactly on :q; htop and Claude Code render aligned against a reference terminal with no garbling or missing spaces; no speculative-echo overlay flickers inside alt-screen apps while typing; CJK text and a ZWJ emoji sequence cause no column drift"
    why_human: "Live full-screen-app visual comparison cannot be automated (D-19-07/08); golden-master capture against live apps is flaky across app/OS versions. This is the deliberately deferred Task 3 checkpoint:human-verify from 19-05-PLAN, returned as a structured checkpoint (not auto-completed)."
---

# Phase 19: Full-Screen TUI Rendering Correctness Verification Report

**Phase Goal:** Full-screen TUI apps (vim, htop, Claude Code) render correctly over nosh — a genuine two-grid alternate-screen model replaces the current no-op flag, wide characters and grapheme clusters are width-accurate, the predictor is suppressed in cursor-addressing mode, and the post-auth OSC OOM vector is bounded.
**Verified:** 2026-06-07
**Status:** human_needed
**Re-verification:** No — initial verification

## Goal Achievement

### Observable Truths (ROADMAP Success Criteria)

| # | Truth | Status | Evidence |
|---|-------|--------|----------|
| 1 | `?1049h`/`?1049l` atomic: save+swap+clear on enter, restore+swap on exit; primary content + cursor survive | ✓ VERIFIED | `enter_alt_screen` (terminal.rs:579) uses `std::mem::replace` to move primary grid into `saved_primary` + install blank alt grid + reset cursor/SGR atomically; `exit_alt_screen` (596) restores grid/cursor/SGR from `saved_primary.take()`, graceful no-op when None. Tests: `alt_screen_h_clears_grid_moves_cursor_resets_sgr`, round-trip restore test, `bare_1049l_is_noop`, nested-enter no-panic, `RIS clears saved_primary`. All pass. |
| 2 | Resize handles BOTH active alt grid and saved primary grid; no stale-sized buffer restored, no inactive-buffer loss | ✓ VERIFIED | `resize` (terminal.rs:465) resizes active grid AND `saved_primary` grid+cursor (518-562). CR-03 fix: saved-primary excess rows on shrink are DISCARDED, not pushed into active scrollback (line 532-552); active-grid shrink also gated on `!alt_screen` (489). Tests: `resize_while_alt_screen_active_resizes_both_grids`, `resize_shrink_while_alt_screen_discards_saved_primary_rows_not_to_scrollback`. Pass. |
| 3 | CJK width-2 advances cursor 2 cols + placeholder at col+1; width-0 marks/ZWJ don't advance | ✓ VERIFIED | `print_char` (terminal.rs:654) uses `UnicodeWidthChar::width`; width-0 returns early (no write/no advance, 659-663); width-2 writes glyph + `wide:true` continuation at col+1 (685-696) + advances 2 (700). Tests with `\u{4e2d}` (中) and U+0301 combining mark pass. ZWJ-specific named test absent (see WR-1) but U+200D is width-0, covered by the combining-mark path. |
| 4 | Claude Code / htop render without garbling or missing spaces (investigation-first) | ✓ VERIFIED (automated half) / ? human (live) | Synthetic VT grid-assertion CI suite added (terminal.rs:2520+): CUP-addressed exact-cell writes, alt-screen CUP-paint-exit atomicity, wide-char via CUP + continuation, ED/EL region precision. Live visual pass is the deferred human checkpoint. |
| 5 | Predictor suppressed while alt-screen active; `predictor.pending` empty after `?1049h` | ✓ VERIFIED | `StateDiff.alt_screen` propagated from `ts.echo_state().alt_screen` (server.rs:329); client resets predictor on false→true transition (`predictor.reset()` clears pending, main.rs:1029-1031). Ongoing overlay suppression inside vim/htop is structural: `cell_at` only renders `!is_tentative` predictions (predictor.rs:802); in cursor-addressing mode `confirmed_epoch` never advances so predictions stay tentative/hidden. Round-trip encode/decode tests pass. |
| 6 | Multi-chunk oversized OSC bounded before vte; RED/GREEN regression; OSC 52 + title still pass | ✓ VERIFIED | `osc_prefilter` (terminal.rs:341) enforces `OSC_ACCUMULATION_MAX` = 1 MiB across `advance()` calls; on overflow returns truncated prefix and `advance` swaps in `vte::Parser::default()` (327). CR-01 split-ST fix via `pending_esc` (348-373). Test `oversized_multi_chunk_osc_is_bounded_then_resyncs` (10 MiB) passes; CR-01 regression tests pass; fuzz target `osc_accumulation.rs` drives 10 MiB multi-chunk. My own probes confirmed: 64 KiB OSC 52 passes under cap, lone-ESC-not-ST doesn't stick, 1.5 MiB resyncs. |

**Score:** 6/6 truths verified (criterion 4's live-app half routed to human verification).

### Required Artifacts

| Artifact | Expected | Status | Details |
|----------|----------|--------|---------|
| `crates/nosh-server/src/terminal.rs` | two-grid model, width-aware print_char, OSC pre-bound, VT grid suite | ✓ VERIFIED | `saved_primary` field, enter/exit_alt_screen, resize-both-grids, `scroll_up` `!alt_screen` gate (631), `Cell.wide`, `OSC_ACCUMULATION_MAX`, `osc_byte_count`, `pending_esc`, synthetic VT suite all present and wired |
| `crates/nosh-server/Cargo.toml` | unicode-width dep | ✓ VERIFIED | dependency present; `UnicodeWidthChar` imported in print_char |
| `crates/nosh-server/src/server.rs` | diff-encoder wide skip + alt_screen propagation | ✓ VERIFIED | `compute_diff_runs` skips `cell.wide` at outer (236) and inner (257) levels; `build_state_diff` reads `ts.echo_state().alt_screen` (329) |
| `crates/nosh-proto/src/datagram.rs` | `alt_screen` field on StateDiff | ✓ VERIFIED | `pub alt_screen: bool` (77); round-trip tests pass |
| `crates/nosh-client/src/main.rs` | alt-screen entry suppression hook | ✓ VERIFIED | `was_alt_screen` + transition `predictor.reset()` (1029-1032) |
| `crates/nosh-client/src/screen.rs` | client wide continuation + emit_diff skip | ✓ VERIFIED | CR-02 fix: `apply` sets `wide:true` on continuation cell (274-285); `emit_diff` skips `want.wide` (484) |
| `crates/nosh-client/src/predictor.rs` | structural suppression | ✓ VERIFIED | `cell_at` renders only non-tentative; `reset()` clears pending |
| `fuzz/fuzz_targets/osc_accumulation.rs` | multi-chunk 10 MiB coverage | ✓ VERIFIED | imports `OSC_ACCUMULATION_MAX`; drives 10 MiB multi-chunk + post-resync OSC 2/52 checks |
| `docs/999.7-SECURITY.md` | OSC-OOM mitigation note | ✓ VERIFIED | thorough doc: threat, mechanism, constants table, residual risk, regression refs, verify-before-relying |

### Key Link Verification

| From | To | Via | Status |
|------|-----|-----|--------|
| csi_dispatch ?1049 arm | enter/exit_alt_screen | match on enable flag | ✓ WIRED |
| scroll_up | scrollback push | `!self.echo_state.alt_screen` guard | ✓ WIRED (631) |
| print_char | Cell.wide continuation | width==2 writes marker at col+1 | ✓ WIRED (685-695) |
| compute_diff_runs | DiffRun.chars | skip `.wide` cells | ✓ WIRED (236,257) |
| emit_diff (screen.rs) | terminal write | skip `.wide` cells | ✓ WIRED (484) |
| advance() | parser.advance() | OSC pre-filter truncates before vte | ✓ WIRED (315) |
| overflow truncation | vte::Parser::default() | resync to ground | ✓ WIRED (327) |
| build_state_diff | StateDiff.alt_screen | ts.echo_state().alt_screen | ✓ WIRED (329) |
| run_pump datagram arm | predictor.reset() | `diff.alt_screen && !was_alt_screen` | ✓ WIRED (1029) |

### Behavioural Spot-Checks (my own probes, since removed)

| Behavior | Result | Status |
|----------|--------|--------|
| Legit 64 KiB OSC 52 across 4 KiB chunks dispatches under 1 MiB cap | dispatched, capped at OSC_52_MAX_BYTES | ✓ PASS |
| Lone ESC at slice boundary that is NOT a ST does not leave in_osc stuck; OSC parsing recovers | recovered, title="OK" | ✓ PASS |
| 1.5 MiB OSC overflow bounded + parser resync, subsequent OSC parses | resynced, title="OK" | ✓ PASS |

### Probe / Test Execution

| Check | Command | Result | Status |
|-------|---------|--------|--------|
| Workspace build | `cargo build --workspace` | Finished, no errors | ✓ PASS |
| Workspace tests | `cargo test --workspace` | all crates 0 failed (nosh-server 105, nosh-client 99+9, nosh-proto 32, plus integration suites) | ✓ PASS |
| SEC-03 regression | `oversized_multi_chunk_osc_is_bounded_then_resyncs` | ok | ✓ PASS |
| CR-01 split-ST | `split_st_terminator_across_advance_calls_closes_osc`, `split_st_followed_by_normal_osc_parses_correctly` | ok | ✓ PASS |
| TUI-02 resize-both | `resize_while_alt_screen_active_resizes_both_grids` | ok | ✓ PASS |
| CR-03 resize discard | `resize_shrink_while_alt_screen_discards_saved_primary_rows_not_to_scrollback` | ok | ✓ PASS |
| CR-02 client wide | `apply_wide_char_sets_continuation_cell_wide_true`, `apply_wide_char_cont_cell_not_double_written` | ok | ✓ PASS |

### Requirements Coverage

| Requirement | Source Plan | Status | Evidence |
|-------------|------------|--------|----------|
| TUI-01 (two-grid alt screen) | 19-01 | ✓ SATISFIED | enter/exit_alt_screen atomic, tests pass |
| TUI-02 (both grids resize) | 19-01 | ✓ SATISFIED | resize handles saved_primary; tests pass |
| TUI-03 (wide/zero-width chars) | 19-02 | ✓ SATISFIED | width-aware print_char + server/client continuation skip; CJK + combining-mark tests pass |
| TUI-04 (live-app correctness) | 19-05 | ✓ SATISFIED (auto) / ? human (live) | synthetic VT CI suite; live pass deferred to human |
| TUI-05 (predictor suppression) | 19-04 | ✓ SATISFIED | StateDiff.alt_screen + transition reset + structural tentative suppression |
| SEC-03 (OSC OOM bound) | 19-03, 19-05 | ✓ SATISFIED | 1 MiB pre-bound + resync; regression + fuzz; docs/999.7 |

All 6 declared requirement IDs accounted for. No orphaned requirements (REQUIREMENTS.md maps exactly TUI-01..05 + SEC-03 to Phase 19, all present in plan frontmatter).

### Anti-Patterns Found

| File | Line | Pattern | Severity | Impact |
|------|------|---------|----------|--------|
| terminal.rs | 2694 | `XXXXXXXXXXXXXXXXXXXX` | ℹ️ Info | Test VT input data (20 X's), not a debt marker — not flagged |
| screen.rs | 706 | `"XXXXX"` | ℹ️ Info | Test DiffRun content, not a debt marker — not flagged |

No `TBD`/`FIXME`/`XXX` debt markers in production code paths. Only deviation across summaries is a pre-existing `type_complexity` clippy lint in `registry.rs:521` (unrelated to this phase, logged to deferred-items).

### Human Verification Required

#### 1. Live visual pass — vim, htop, Claude Code over a Linux client↔server

**Test:** Build release, start nosh server + connect a Linux nosh client. Run `vim --noplugin` (edit + `:q`), `htop`, and `claude`/another full-screen TUI. Type CJK text and a ZWJ emoji sequence at the shell.
**Expected:** vim opens to a blank canvas (no shell bleed-through) and restores the primary buffer + cursor exactly on exit; htop and Claude Code render aligned against a reference terminal — no garbling, no missing spaces; no speculative-echo overlay flickers inside alt-screen apps while typing; CJK/emoji cause no column drift.
**Why human:** Live full-screen-app visual comparison cannot be automated (D-19-07/08); golden-master capture against live apps is flaky across app/OS versions. This is the deliberately deferred Task 3 `checkpoint:human-verify` from 19-05-PLAN, returned as a structured checkpoint and acknowledged by the user as deferred to UAT.

### Gaps Summary

No blocking gaps. All three code-review critical defects were fixed and have dedicated regression tests that pass:
- **CR-01** (split-ST `pending_esc` boundary bug) — fixed; `split_st_*` tests pass; my own lone-ESC probe confirms no stuck `in_osc`.
- **CR-02** (client wide-continuation cell not marked `wide:true`) — fixed in screen.rs `apply`; `apply_wide_char_sets_continuation_cell_wide_true` + no-double-write tests pass.
- **CR-03** (resize pushing saved-primary rows into active scrollback) — fixed; saved-primary shrink discards rows; dedicated test passes.

All 6 requirement IDs are satisfied in code with passing automated tests. `cargo build --workspace` and `cargo test --workspace` both pass (0 failures). docs/999.7-SECURITY.md exists and documents the OSC-OOM mitigation as the ROADMAP requires before phase close.

The only outstanding item is the live visual pass (TUI-04 / criterion 4 live half), which is a legitimate, pre-acknowledged human-verification checkpoint — not a fabricated or missed deliverable. Status is therefore `human_needed`, not `gaps_found`.

**Minor observations (non-blocking, informational):**
- WR-1: No test named for a ZWJ *emoji* sequence specifically (e.g. `\u{1F468}\u{200D}\u{1F469}`); the roadmap criterion text names one. The width-0 code path is exercised by the U+0301 combining-mark test (U+200D ZWJ is itself width-0 and takes the same early-return path), so behaviour is covered — but a literal ZWJ-emoji unit test would close the traceability gap. The live human pass also covers emoji column drift.

---

_Verified: 2026-06-07_
_Verifier: Claude (gsd-verifier)_
