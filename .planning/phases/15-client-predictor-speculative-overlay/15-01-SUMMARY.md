---
phase: 15-client-predictor-speculative-overlay
plan: 01
subsystem: client
tags: [predictor, speculative-echo, unicode-width, epoch-state-machine, mosh, rust]

# Dependency graph
requires:
  - phase: 14-client-predictor-confirmed-rendering
    provides: Overlay trait, ClientScreen, Cell, ConnectionLossOverlay pattern, compose_desired seam
  - phase: 11-datagram-wire-protocol
    provides: StateDiff, CellStyle::UNDERLINE, CursorPos from nosh-proto
provides:
  - PredictionOverlay — Mosh PredictionEngine translated to Rust, implementing the Phase 14 Overlay trait
  - PendingPrediction — per-cell speculative prediction with epoch_required and tentative_until_epoch
  - Validity enum — five-state Mosh validity machine (Pending, Correct, CorrectNoCredit, IncorrectOrExpired, Inactive)
  - InputAction enum — classify_input byte-level classifier (15 input classes)
  - PredictDisplayMode enum — Always/Adaptive/Never with clap::ValueEnum for --predict CLI flag
  - on_input, cull, become_tentative, reset, kill_epoch, update_rtt_thresholds
  - Noecho suppression: structural (confirmed_epoch never advances without server echo)
  - RTT hysteresis: SRTT_TRIGGER 30/20ms, FLAG_TRIGGER 80/50ms (Mosh values)
  - 49 inline unit tests covering all behavior
affects: [15-02, 15-03, 16-client-predictor-integration]

# Tech tracking
tech-stack:
  added: [unicode-width = "0.2" (unicode-rs org, crates.io approved)]
  patterns:
    - "Mosh PredictionEngine epoch model — PendingPrediction.tentative_until_epoch > confirmed_epoch → hidden"
    - "Noecho suppression structural — no flag needed; confirmed_epoch stays at 0 if server never echoes"
    - "Bulk/paste before classify — paste markers (6 bytes) matched before bulk (>4 bytes) guard"
    - "cull() >= check — epoch_required <= new_epoch tolerates dropped datagrams (Pitfall 4)"
    - "Full reset on non-tentative mismatch — self.reset(); return; never partial remove (Pitfall 1)"

key-files:
  created:
    - crates/nosh-client/src/predictor.rs
  modified:
    - crates/nosh-client/Cargo.toml
    - crates/nosh-client/src/lib.rs

key-decisions:
  - "Paste-marker detection before bulk guard: b\"\\x1b[200~\" matched first so 6-byte markers are not BulkSuppressed"
  - "PredictDisplayMode derives clap::ValueEnum with rename_all=lower so --predict always|adaptive|never works directly"
  - "become_tentative() and reset() made pub for test surface; update_rtt_thresholds/is_tentative/should_display also pub for integration plan"
  - "CJK right-edge guard: col.saturating_add(col_width) > term_cols → become_tentative (not predict), per Pitfall 6"
  - "term_rows stored in PredictionOverlay struct with #[allow(dead_code)] — reserved for future row bounds checking"

patterns-established:
  - "Predictor tests use assert!(expr.is_none()) not assert_eq!(expr, None) because Cell lacks Debug"
  - "classify_input byte slice match — paste markers before bulk guard, ESC catch-all after specific ESC seqs"
  - "cull() two-pass: first pass detects mismatch/confirm and calls reset() on non-tentative mismatch; second pass collects epochs_to_kill for tentative mismatches"

requirements-completed: [PREDICT-02, PREDICT-03, PREDICT-04, PREDICT-06]

# Metrics
duration: 45min
completed: 2026-06-02
---

# Phase 15 Plan 01: Predictor Engine Core Summary

**Mosh PredictionEngine translated to Rust: full epoch/Validity state machine in predictor.rs with 49 unit tests proving noecho suppression, CJK width, dropped-datagram tolerance, and RTT hysteresis**

## Performance

- **Duration:** ~45 min
- **Started:** 2026-06-02
- **Completed:** 2026-06-02
- **Tasks:** 2 (Task 1 + Task 2 implemented as one coherent module)
- **Files modified:** 3 (predictor.rs created, Cargo.toml + lib.rs modified)

## Accomplishments

- Created `crates/nosh-client/src/predictor.rs` (490+ lines of implementation, 700+ lines of tests) implementing the Mosh `PredictionEngine` epoch/Validity model in Rust
- Implemented full state machine: `PredictDisplayMode`, `Validity`, `InputAction`, `PendingPrediction`, `PredictionOverlay` structs/enums; `classify_input`, `classify_printable`, `on_input`, `cull`, `become_tentative`, `reset`, `kill_epoch`, `update_rtt_thresholds`
- Proved noecho suppression is structural: `confirmed_epoch` never advances when server doesn't echo → all predictions stay tentative → `cell_at` returns `None` (PREDICT-04 unit test)
- All 49 unit tests pass with no clippy warnings; `unicode-width 0.2.2` correctly integrated; no `width_cjk` calls

## Task Commits

1. **Task 1 + Task 2: predictor module** - `9228aa6` (feat)

**Plan metadata:** (this commit)

## Files Created/Modified

- `/home/bharris/github.com/bharrisau/nosh/crates/nosh-client/src/predictor.rs` - Full PredictionOverlay engine: enums, PendingPrediction, on_input, cull, Overlay impl, RTT hysteresis, 49 inline unit tests
- `/home/bharris/github.com/bharrisau/nosh/crates/nosh-client/Cargo.toml` - Added `unicode-width = "0.2"` after dirs dep
- `/home/bharris/github.com/bharrisau/nosh/crates/nosh-client/src/lib.rs` - Added `pub mod predictor;` declaration

## Decisions Made

- Paste-marker before bulk guard: `b"\x1b[200~"` (6 bytes, > BULK_SUPPRESS_THRESHOLD=4) matched first in `classify_input` before the bulk guard so paste markers are correctly classified as `BracketedPasteStart`/`BracketedPasteEnd`.
- `PredictDisplayMode` derives `clap::ValueEnum` with `#[value(rename_all = "lower")]` so the integration plan's `--predict always|adaptive|never` arg works directly.
- Key methods made `pub` for integration plan access: `become_tentative`, `reset`, `kill_epoch`, `update_rtt_thresholds`, `should_display`, `is_tentative`.
- `term_rows` stored in struct with `#[allow(dead_code)]` — reserved for future per-row bounds checking without requiring a struct change.
- `assert!(expr.is_none())` pattern used in tests instead of `assert_eq!(expr, None)` because `screen::Cell` does not implement `Debug` — avoids modifying screen.rs (not in plan scope).

## Deviations from Plan

None - plan executed as written. Tasks 1 and 2 were implemented together in a single atomic commit since the state machine types (Task 1) and their methods (Task 2) form a single coherent unit. All behavior specified in both tasks is present and tested.

## Issues Encountered

- `screen::Cell` lacks `Debug` derive — tests using `assert_eq!(cell_at(...), None)` failed to compile. Fixed by using `assert!(cell_at(...).is_none())` pattern throughout. No changes to screen.rs required.

## Threat Coverage

| Threat ID | Status | Evidence |
|-----------|--------|---------|
| T-15-01 (noecho info disclosure) | Mitigated | `noecho_suppression` test asserts `confirmed_epoch() < prediction_epoch()` and `cell_at(...)` returns None throughout; structural via `is_tentative` |
| T-15-02 (wire write) | Mitigated | predictor.rs imports no quinn types; no SendStream handle; structurally cannot write |
| T-15-03 (VecDeque growth) | Mitigated | `cull()` removes confirmed; `reset()` clears all on mismatch; bulk/paste suppress enqueue |
| T-15-04 (CJK miscount) | Mitigated | `width()` not `width_cjk()`; ambiguous/combining → EpochReset; right-edge → become_tentative |

## Self-Check

Files created/modified:
- `/home/bharris/github.com/bharrisau/nosh/crates/nosh-client/src/predictor.rs` — created (commit 9228aa6)
- `/home/bharris/github.com/bharrisau/nosh/crates/nosh-client/Cargo.toml` — modified (commit 9228aa6)
- `/home/bharris/github.com/bharrisau/nosh/crates/nosh-client/src/lib.rs` — modified (commit 9228aa6)

Build: `cargo build -p nosh-client` — PASSED (no warnings)
Tests: `cargo test -p nosh-client --lib predictor::` — 49/49 PASSED
Clippy: `cargo clippy -p nosh-client --all-targets -- -D warnings` — PASSED (no warnings)

## Self-Check: PASSED

## Next Phase Readiness

- Plan 15-02 (integration) can immediately hook `PredictionOverlay::on_input()` after the escape machine in `main.rs` stdin arm and `PredictionOverlay::cull()` after `screen.apply()` in the datagram arm
- `PredictDisplayMode` is `clap::ValueEnum`-ready for `--predict` CLI arg addition
- `predicted_cursor()` returns `Option<CursorPos>` for the render path cursor override (Pitfall 3 mitigation)
- `impl Overlay for PredictionOverlay` compiles against the unchanged Phase 14 `Overlay` trait — no screen.rs changes needed for basic integration

---
*Phase: 15-client-predictor-speculative-overlay*
*Completed: 2026-06-02*
