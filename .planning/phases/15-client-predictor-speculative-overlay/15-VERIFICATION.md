---
phase: 15-client-predictor-speculative-overlay
verified: 2026-06-02T09:10:00Z
status: human_needed
score: 5/5 success criteria verified (logic); 2 require live-terminal confirmation
overrides_applied: 0
human_verification:
  - test: "Live vim session: ssh in, run `vim`, `iHello<Esc>`, on a high-latency link (or --predict always) and visually confirm zero corrupt/garbage cells while typing and after ESC."
    expected: "No leftover predicted characters; screen matches a non-predicting client exactly. Alt-screen/CSI entry to vim produces no speculative display."
    why_human: "Criterion 2 is validated only with synthetic StateDiffs at unit level (less_cursor_addressing_disables_prediction, vim_insert_zero_corrupt_cells). No live vim PTY drives the predictor end-to-end. Visual 'zero corrupt cells' on a real alt-screen app needs a human terminal."
  - test: "Live CJK typing: type `你好` (and an ambiguous-width char + an emoji/ZWJ sequence) into a real shell over a high-latency link with --predict always."
    expected: "你好 advances the cursor by 2 columns each with no overlap/corruption; ambiguous-width and ZWJ/emoji produce no speculative display (epoch reset), letting the server confirm."
    why_human: "Criterion 5 column-advance and reset logic is unit-tested with synthetic input, but wide-char rendering correctness and the absence of visible corruption on a real terminal emulator is a visual check."
  - test: "Underline visibility: on a genuine high-latency link (>80ms RTT) in adaptive mode, confirm unconfirmed predictions are underlined; on loopback confirm no underline / no prediction is visible."
    expected: "Underline appears only above ~80ms; loopback (adaptive) shows no prediction and no underline."
    why_human: "Criterion 4 thresholds/hysteresis are unit-tested (rtt_hysteresis_flagging, rtt_adaptive_loopback_invisible), but the actual rendered SGR underline on a real terminal at real RTT is a subjective/visual confirmation."
---

# Phase 15: Client Predictor — Speculative Overlay Verification Report

**Phase Goal:** The client speculatively echoes locally-typed input ahead of server confirmation — printable characters, backspace, left/right cursor motion — with conservative fallback and adaptive RTT-based activation, never rendering worse than no prediction.
**Verified:** 2026-06-02
**Status:** human_needed
**Re-verification:** No — initial verification

## Goal Achievement

### Observable Truths / Success Criteria

| # | Criterion | Status | Evidence |
|---|-----------|--------|----------|
| 1 | Printable/backspace/←→ appear immediately (speculative) and are confirmed/culled within the next server-state update | ✓ VERIFIED (logic) | `on_input` enqueues at predicted cursor + advances by unicode width (predictor.rs:289-318); `cull` confirms via `>=` epoch check (predictor.rs:400-450). Tests: `on_input_enqueues_prediction_and_advances_cursor`, `cull_correct_after_become_tentative_advances_epoch`, `simulated_loss_ge_epoch_confirm`, live `end_to_end_printable_echo_confirms`. Wire path: stdin arm renders then forwards UNCHANGED bytes (main.rs:796-830). |
| 2 | Zero corrupt cells in vim; CSI cursor-move/erase/alt-screen resets epoch, no speculative display | ⚠ PARTIAL → human | Logic verified: arbitrary CSI/ESC → `EpochReset`→`reset()` (predictor.rs:356-361, classify_input:657-665). Tests `less_cursor_addressing_disables_prediction`, `vim_insert_zero_corrupt_cells` use synthetic diffs only. No live vim PTY test → live visual confirm needed. |
| 3 | Zero predicted chars during `read -s` noecho; engine tracks server echo state | ✓ VERIFIED | Structural: `confirmed_epoch` never advances without a server echo; tentative predictions hidden (predictor.rs is_tentative:467-469, cull mismatch reset:417-427). LIVE test `noecho_read_dash_s_zero_predicted_chars` (predict.rs:712) drives real `/bin/sh` `read -s`, asserts `cell_at` None across all 24×80 cells (WR-04). Plus `awaiting_first_cull` one-frame guard (CR-03). |
| 4 | Underline only above RTT threshold; `--predict always/adaptive/never`; loopback adaptive underline invisible | ⚠ PARTIAL → human | `--predict` flag wired (main.rs:325, default adaptive), `PredictDisplayMode` (predictor.rs:66-76). Hysteresis tested: `rtt_hysteresis_flagging`, `rtt_hysteresis_srtt_trigger`, `rtt_adaptive_loopback_invisible`. Underline SGR emission (predictor.rs:586-590). Real-terminal underline visibility at real RTT → human. |
| 5 | CJK wide chars advance correct columns (你好); ambiguous-width and ZWJ/emoji → epoch reset | ⚠ PARTIAL → human | `width()` not `width_cjk` (predictor.rs:683); width-2 advances +2 (cjk_width_2_advances_cursor_by_2, cjk_wide_char_column_advance); combining/VS16/ZWJ → reset (classify_combining_mark_epoch_reset, classify_ambiguous_width_epoch_reset). Right-edge → become_tentative (cjk_at_right_edge_becomes_tentative). Live wide-char render visual → human. |

**Score:** 5/5 verified at logic/unit + live-noecho level; 3 criteria carry a live-terminal visual confirmation item.

### "Never render worse than no prediction" invariant

| Behavior | Status | Evidence |
|----------|--------|----------|
| Non-tentative mismatch → full reset | ✓ | predictor.rs:423-426; test `cull_mismatch_non_tentative_full_reset` |
| Epoch reset on CSI/control/Tab/Enter/ESC | ✓ | classify_input:636-665; tests classify_* + `ctrl_c_midline_clean_reset` |
| Epoch reset on ambiguous-width/ZWJ/combining | ✓ | classify_printable:683-688 |
| Bulk (>4B) / bracketed-paste suppression | ✓ | classify_input:614-625; `bracketed_paste_no_prediction`, `bulk_suppressed_becomes_tentative` |
| Tolerates dropped datagrams (`>=`, not `==`) | ✓ | cull:402; `cull_tolerates_dropped_datagrams`, `simulated_loss_ge_epoch_confirm` |

### Code-review fixes (CR-01/02/03) — confirmed REAL, not cosmetic

| Fix | Status | Evidence |
|-----|--------|----------|
| CR-01: predicted cursor seeded from CONFIRMED cursor (not row 0) | ✓ VERIFIED | `sync_cursor_from_confirmed` (predictor.rs:238) called in datagram arm after cull (main.rs:725). Test `cr01_prediction_lands_on_correct_nonzero_row` + verifier probe `probe_nonzero_row_prediction_is_confirmed_not_reset` (row 7 prediction CONFIRMED, confirmed_epoch advances — not reset). |
| CR-02: type-then-backspace leaves no stale cell (`cell_at` None at vacated col) | ✓ VERIFIED | `pending.retain` removes vacated-col prediction (predictor.rs:327). Tests `cr02_backspace_removes_char_prediction_from_overlay`, `backspace_removes_stale_char_prediction` (cell_at(0,0) None after BS). |
| CR-03 / PREDICT-04: no predicted char rendered before first-of-epoch server confirm (no one-frame leak) | ✓ VERIFIED | `awaiting_first_cull` set in on_input when pending empty (predictor.rs:305) and in reset() (predictor.rs:493); `cell_at`/`predicted_cursor` return None while set (predictor.rs:581, 265). Test `cr03_noecho_first_keystroke_not_rendered_before_cull` + verifier probe `probe_guard_rearms_for_fresh_keystroke_after_prior_cull` (guard RE-ARMS for a fresh keystroke even after a prior cull cleared it — closes the late-leak window). |

### Display-only / wire integrity

| Check | Status | Evidence |
|-------|--------|----------|
| Keystroke bytes to server UNCHANGED (no predicted bytes on wire) | ✓ VERIFIED | predictor module has no SendStream/network handle; `send_input(send, &result.bytes_to_forward)` forwards the identical slice (main.rs:828); predictor only borrows `&result.bytes_to_forward` (main.rs:796). |
| Single display path (no new direct stdout writes) | ✓ VERIFIED | All rendering via `ClientScreen::render_with_predictor` (screen.rs:343), which routes through the existing `emit_diff` (screen.rs:362). Both run_pump arms call only this (main.rs:750, 814). |

### Required Artifacts

| Artifact | Status | Details |
|----------|--------|---------|
| `crates/nosh-client/src/predictor.rs` | ✓ VERIFIED | 1643 lines; PredictionOverlay impl Overlay, PendingPrediction, Validity, InputAction, PredictDisplayMode, classify_input, on_input, cull, RTT hysteresis, unicode-width. |
| `crates/nosh-client/src/screen.rs` | ✓ VERIFIED | `render_with_predictor` + `render_to_stdout_with_cursor` cursor override; `confirmed_cursor()` getter. |
| `crates/nosh-client/src/main.rs` | ✓ VERIFIED | `--predict` flag, predictor construction (main.rs:654), on_input hook, cull hook, sync_cursor, set_size/reset on resize, latency instrumentation. |
| `crates/nosh-client/tests/predict.rs` | ✓ VERIFIED | 1001 lines; full D-15-04 matrix incl. live noecho + e2e echo. |

### Key Link Verification

| From | To | Status | Details |
|------|----|--------|---------|
| predictor.rs PredictionOverlay | screen.rs Overlay trait | ✓ WIRED | `impl Overlay for PredictionOverlay` (predictor.rs:559) |
| main.rs stdin arm | predictor.on_input | ✓ WIRED | main.rs:796 (after escape machine, before send_input) |
| main.rs datagram arm | predictor.cull + sync_cursor_from_confirmed | ✓ WIRED | main.rs:716, 725 |
| screen.rs render | predictor.predicted_cursor() | ✓ WIRED | screen.rs:361 |
| classify printable | UnicodeWidthChar::width | ✓ WIRED | predictor.rs:683 |

### Requirements Coverage

| Requirement | Status | Evidence |
|-------------|--------|----------|
| PREDICT-02 (speculative echo + per-prediction tracking) | ✓ SATISFIED | on_input/cull + e2e echo test |
| PREDICT-03 (conservative reset; no display on fresh row/before first confirm) | ✓ SATISFIED (logic) | EpochReset paths + awaiting_first_cull; vim case synthetic (see human item) |
| PREDICT-04 (noecho suppression — SECURITY) | ✓ SATISFIED | live read -s test all-rows + structural epoch lag |
| PREDICT-05 (RTT-gated underline + --predict flag) | ✓ SATISFIED (logic) | flag wired + hysteresis tests; live underline visual → human |
| PREDICT-06 (CJK width + reset on ambiguous/ZWJ) | ✓ SATISFIED (logic) | width() advance + reset tests; live render visual → human |

### Behavioral Spot-Checks / Probe Execution

| Check | Command | Result | Status |
|-------|---------|--------|--------|
| Full client suite | `cargo test -p nosh-client` | all suites GREEN (lib 75, predict 11 incl. live noecho+e2e, others pass) | ✓ PASS |
| Verifier probe: CR-03 guard re-arm | `cargo test --test probe_cr03` | pass | ✓ PASS (removed after run) |
| Verifier probe: CR-01 non-zero-row confirm-not-reset | `cargo test --test probe_cr03` | pass (confirmed_epoch advances at row 7) | ✓ PASS (removed after run) |

### Anti-Patterns Found

| File | Line | Pattern | Severity | Impact |
|------|------|---------|----------|--------|
| screen.rs | 583 | `"XXXXX"` | ℹ Info | Test-fixture diff content (idempotency test), not a debt marker. No action. |

No `TODO`/`FIXME`/`XXX`/`HACK`/`TBD`/`unimplemented!` debt markers in predictor.rs, screen.rs, or main.rs.

### Gaps Summary

No blocking gaps. All five success criteria are satisfied at the logic/unit level, the SECURITY criterion (3, PREDICT-04) is additionally proven against a LIVE server PTY running `read -s`, and the three code-review fixes (CR-01/02/03) are confirmed real by reading the code and by two independent verifier probes (fails-before/passes-after reasoning: CR-03 probe depends on the `awaiting_first_cull` re-arm mechanism; CR-01 probe asserts a non-zero-row prediction is confirmed and advances the epoch rather than being reset).

Three criteria (2 vim, 4 underline visibility, 5 CJK) are validated with synthetic StateDiffs / unit hysteresis rather than a live full-screen / high-latency / CJK terminal session. Their correctness logic is verified in code, but the final "zero corrupt cells visually" / "underline visible at real RTT" confirmation is inherently a human visual check on a live terminal — hence status human_needed, not gaps_found. These are confirmation items, not defects.

---

_Verified: 2026-06-02_
_Verifier: Claude (gsd-verifier)_
