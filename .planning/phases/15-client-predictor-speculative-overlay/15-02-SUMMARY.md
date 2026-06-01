---
phase: 15-client-predictor-speculative-overlay
plan: 02
subsystem: client
tags: [predictor, speculative-echo, mosh, quinn, rtt, latency-instrumentation, rust]

# Dependency graph
requires:
  - phase: 15-client-predictor-speculative-overlay
    plan: 01
    provides: PredictionOverlay, PredictDisplayMode, on_input, cull, predicted_cursor, Overlay impl
  - phase: 14-client-predictor-confirmed-rendering
    provides: ClientScreen, render_to_stdout, Overlay trait, single display path seam, emit_diff pattern

provides:
  - render_to_stdout_with_cursor — cursor override variant of render_to_stdout (final MoveTo at predicted pos)
  - render_with_predictor — single display path for speculative echo; confirmed ⊕ overlays ⊕ prediction
  - emit_diff — shared ANSI-diff loop factored out of render methods (single cell-writing location)
  - --predict always|adaptive|never CLI flag (default adaptive; PREDICT-05, D-15-02)
  - run_pump PredictionOverlay integration: on_input in stdin arm, cull in datagram arm with quinn RTT
  - D-17-02a latency instrumentation hook: debug-level tracing of predicted-keystroke vs confirm epoch timing

affects: [15-03, 16-client-predictor-integration, 17-windows-predictive-echo-validation]

# Tech tracking
tech-stack:
  added: []
  patterns:
    - "emit_diff factored as shared private method — single ANSI-diff loop shared by all render variants"
    - "render_with_predictor applies predictor overlay AFTER existing overlays Vec (ConnectionLossOverlay etc.)"
    - "Predictor mutably owned by run_pump; passed to render by shared ref (not in overlays Vec)"
    - "stdin arm: on_input → render_with_predictor → send_input (predictor never mutates forwarded bytes)"
    - "datagram arm: apply → cull(rtt) → render_with_predictor (single display path)"
    - "D-17-02a latency hook: HashMap<epoch, Instant> in run_pump; debug-level nosh::predict target"

key-files:
  created: []
  modified:
    - crates/nosh-client/src/screen.rs
    - crates/nosh-client/src/main.rs

key-decisions:
  - "emit_diff is private to ClientScreen — the shared cell-write loop is one location; render_to_stdout_with_cursor and render_with_predictor both call it"
  - "Predictor is NOT pushed into overlays Vec — must remain mutably owned by run_pump for on_input/cull calls while also supplying cursor override via predicted_cursor()"
  - "Latency instrumentation uses HashMap<u64, Instant> keyed by prediction_epoch in run_pump — no struct change to PendingPrediction (epoch map is simpler for per-epoch first-enqueue tracking)"
  - "fresh_session gets #[allow(clippy::too_many_arguments)] to cover the 8th predict_mode arg without changing run_pump's existing allow"

patterns-established:
  - "render_to_stdout delegates to render_to_stdout_with_cursor(out, None) — thin wrapper, backward compatible"
  - "render_with_predictor: compose_desired → apply predictor cells → emit_diff with predicted_cursor override"
  - "Stdin arm ordering: escape.process → on_input → render_with_predictor → send_input (prediction before send)"

requirements-completed: [PREDICT-02, PREDICT-04, PREDICT-05]

# Metrics
duration: 30min
completed: 2026-06-02
---

# Phase 15 Plan 02: Client Predictor Integration Summary

**Mosh PredictionOverlay wired into the live client: speculative echo renders immediately on keypress through the single display path, predictions cull against StateDiffs using quinn RTT, `--predict always|adaptive|never` selects the mode (adaptive default), keystroke bytes to server unchanged, and Phase 17 latency instrumentation hook in place**

## Performance

- **Duration:** ~30 min
- **Started:** 2026-06-02
- **Completed:** 2026-06-02
- **Tasks:** 2
- **Files modified:** 2 (screen.rs, main.rs)

## Accomplishments

- Extended `ClientScreen` with `render_to_stdout_with_cursor` (cursor override) and `render_with_predictor` (prediction overlay + cursor); factored shared `emit_diff` loop — single ANSI-diff location, no duplication
- Added 3 new inline tests to screen.rs: cursor override positioning, render_to_stdout regression, render_with_predictor predicted-cell overlay
- Wired `PredictionOverlay` into `run_pump`: stdin arm calls `on_input` before `send_input` (keystroke bytes unchanged to server); datagram arm calls `cull` after `screen.apply` with `conn.rtt()`; both arms render via `render_with_predictor` through the single display path
- Added `--predict always|adaptive|never` CLI flag (default `adaptive`; PREDICT-05) with full clap help text including adaptive behavior description
- Implemented D-17-02a latency instrumentation hook: `HashMap<u64, Instant>` in run_pump records enqueue time per prediction epoch; `tracing::debug!(target: "nosh::predict")` emits `predict` and `confirm` events with latency_ms (no character content — T-15-08)
- All 22 screen:: tests pass; all nosh-client tests pass; `cargo clippy -D warnings` clean

## Task Commits

1. **Task 1: cursor override + render_with_predictor** - `e227378` (feat)
2. **Task 2: --predict flag, run_pump wiring, hooks, latency instrumentation** - `7e4b1a7` (feat)

**Plan metadata:** (this commit)

## Files Created/Modified

- `/home/bharris/github.com/bharrisau/nosh/crates/nosh-client/src/screen.rs` — Added `render_to_stdout_with_cursor`, `render_with_predictor`, `emit_diff` (private shared loop), 3 new tests; `render_to_stdout` becomes thin wrapper; imported `PredictionOverlay`
- `/home/bharris/github.com/bharrisau/nosh/crates/nosh-client/src/main.rs` — Added `--predict` field to Args, `predict_mode` param to `fresh_session`/`reattach_session`/`run_pump`, PredictionOverlay construction, stdin arm on_input hook, datagram arm cull hook, latency instrumentation HashMap, D-17-02a tracing events

## Decisions Made

- `emit_diff` factored as a private method on `ClientScreen` — keeps the single ANSI-diff loop acceptance criterion (one call to `out.queue(MoveTo(col, row))`) while enabling both render variants to share it.
- Predictor held in `run_pump` (not in `overlays` Vec) — must be mutably owned for `on_input`/`cull` calls while supplying the cursor to `render_with_predictor` by shared ref. Pushing it into `overlays: Vec<Box<dyn Overlay>>` would require interior mutability or splitting ownership.
- Latency instrumentation uses `HashMap<u64, Instant>` keyed by `prediction_epoch` in `run_pump` rather than adding an `enqueued_at: Instant` field to `PendingPrediction` in predictor.rs — avoids a struct change to Plan 01 code and is simpler for the first-enqueue-per-epoch pattern.
- `#[allow(clippy::too_many_arguments)]` added to `fresh_session` — it gained an 8th arg (`predict_mode`); the existing allow on `run_pump` covers its 9th arg.

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 1 - Bug] Added #[allow(clippy::too_many_arguments)] to fresh_session**
- **Found during:** Task 2 (clippy check)
- **Issue:** Adding `predict_mode` to `fresh_session` raised it to 8 args; clippy -D warnings failed
- **Fix:** Added `#[allow(clippy::too_many_arguments)]` attribute to `fresh_session`; `run_pump` already had this attr
- **Files modified:** crates/nosh-client/src/main.rs
- **Verification:** `cargo clippy -p nosh-client --all-targets -- -D warnings` — clean
- **Committed in:** `7e4b1a7` (Task 2 commit)

---

**Total deviations:** 1 auto-fixed (Rule 1 — clippy lint)
**Impact on plan:** Trivial fix, no scope change.

## Threat Coverage

| Threat ID | Status | Evidence |
|-----------|--------|---------|
| T-15-05 (noecho info disclosure) | Mitigated (render path) | render_with_predictor calls predictor.cell_at which returns None for tentative predictions; password chars never reach display (structural via Plan 01 noecho suppression) |
| T-15-06 (prediction alters wire bytes) | Mitigated | on_input called before send_input; send_input still forwards result.bytes_to_forward verbatim; grep confirms unchanged call at line 816 |
| T-15-07 (second display path) | Mitigated | All speculative output goes through render_with_predictor (single display path); no new stdout.write_all outside the buffered-flush pattern; emit_diff is the only cell-writing location |
| T-15-08 (latency logging leaks chars) | Mitigated | D-17-02a hook logs only epoch + latency_ms under target "nosh::predict" at debug level; no character content emitted |
| T-15-09 (bulk paste render cost) | Accepted | Bulk/paste suppression in predictor (Plan 01) prevents per-char floods; render is the same minimal-diff path |

## Known Stubs

None — speculative echo is live through the full display path. `--predict adaptive` will show predictions only when quinn RTT > 30ms (invisible on loopback as designed).

## Self-Check

Files modified:
- `/home/bharris/github.com/bharrisau/nosh/crates/nosh-client/src/screen.rs` — modified (commit e227378)
- `/home/bharris/github.com/bharrisau/nosh/crates/nosh-client/src/main.rs` — modified (commit 7e4b1a7)

Build: `cargo build -p nosh-client` — PASSED
Tests: `cargo test -p nosh-client` — all passed (22 screen:: tests, 7 lib tests, 4 transport tests)
Clippy: `cargo clippy -p nosh-client --all-targets -- -D warnings` — PASSED (no warnings)

## Self-Check: PASSED

## Next Phase Readiness

- Plan 15-03 (adversarial tests: vim, `read -s`, CJK, simulated loss) can immediately test the live integration via `render_with_predictor` and the `--predict always` flag
- The D-17-02a instrumentation hook is in place; Phase 17 (Windows) needs only to enable `RUST_LOG=nosh::predict=debug` to collect latency evidence
- `PredictDisplayMode` is fully threaded; adding `--predict-debug` flag in a future plan requires only a new `Args` field

---
*Phase: 15-client-predictor-speculative-overlay*
*Completed: 2026-06-02*
