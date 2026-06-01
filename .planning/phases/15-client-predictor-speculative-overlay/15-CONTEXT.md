# Phase 15: Client Predictor — Speculative Overlay - Context

**Gathered:** 2026-06-02
**Status:** Ready for planning

<domain>
## Phase Boundary

The client speculatively echoes locally-typed input ahead of server confirmation —
printable characters, backspace, and left/right cursor motion — rendered as a
`PredictionOverlay` layer on the Phase 14 compositor (`desired = confirmed ⊕ overlays`).
Predictions are confirmed or culled against incoming state-sync `StateDiff` datagrams,
engage adaptively based on RTT, and **never render worse than no prediction**. Requirements:
PREDICT-02, PREDICT-03, PREDICT-04, PREDICT-05, PREDICT-06.

Keystrokes still travel on the reliable bidi stream (unchanged from Phase 14) — prediction is
purely client-local speculation. Display authority remains the datagram-fed `ClientScreen`.
Out of scope: Windows-host live validation (Phase 17), QoL/loss-banner/OSC52 (Phase 16),
client-side scrollback (M5).

</domain>

<decisions>
## Implementation Decisions

### Prediction scope / conservative fallback (D-15-01)
- **D-15-01:** **Predict the mandated set + Home/End, reset on everything else.** Speculatively
  echo: printable characters, backspace, and single-cell left/right cursor motion (PREDICT-02).
  **Extend** prediction to **Home/End line-bound motion** (incl. their shell equivalents
  Ctrl-A / Ctrl-E) — a safe, common case.
- **D-15-01a (explicitly NOT predicted — epoch reset):** Tab (completion / tab-stop ambiguity),
  word-wise motion (Alt/Ctrl-arrow), Enter/`\r`, ESC, any CSI cursor-addressing / erase /
  alternate-screen sequence, and any non-printing control key all **reset the prediction epoch**
  and display nothing speculative (PREDICT-03). Each extra predicted case (Home/End) MUST be
  carried into the validation matrix (D-15-04).
- **D-15-01b (bulk / paste suppression):** Suppress all prediction during bracketed paste
  (`CSI ?2004h`/`l`) and on bulk input (heuristic: >4 bytes arriving in one input read batch) —
  paste and bulk runs are unpredictable and a known corruption source (research Pitfall 3).
- **D-15-01c (no-echo suppression — security):** Track the server's confirmed echo state; when
  the server is not echoing (e.g. `read -s` / `stty -echo`), display **zero** predicted
  characters (PREDICT-04). This falls out of the epoch model — after an epoch reset, the new
  epoch stays dark until the server confirms its first character — but it is a hard security
  requirement and must be validated adversarially, not assumed.

### Adaptive RTT activation (D-15-02)
- **D-15-02:** **Mirror Mosh's battle-tested thresholds + hysteresis now.** Show predictions
  when smoothed RTT is above the high trigger (~30ms) and stop below the low trigger (~20ms);
  underline unconfirmed predictions above the flag-high trigger (~80ms) and stop below
  flag-low (~50ms). Hysteresis (separate on/off triggers) prevents flicker on link jitter. On a
  loopback connection in adaptive mode, prediction underlines are invisible (criterion #4 /
  PREDICT-05). `--predict always|adaptive|never` overrides the adaptive default; default is
  `adaptive`.
- **D-15-02a (deferred → backlog):** Deriving nosh-specific RTT thresholds (vs. Mosh's
  constants) is **deferred to the backlog** — ship Mosh's proven values first, tune later only
  with a measured reason. See `<deferred>`.

### Wide / ambiguous-width policy (D-15-03)
- **D-15-03:** **Conservative reset on tricky widths.** Use `unicode-width`
  (`UnicodeWidthChar`) to advance the predicted cursor by the correct column count for clean
  **width-1 and width-2 (CJK, e.g. `你好`)** characters (PREDICT-06). For **ambiguous-width**
  characters, **combining marks**, and **ZWJ / emoji** sequences — **epoch-reset** and let the
  server confirm rather than risk column-tracking corruption. Matches PREDICT-06's stated
  conservative policy; zero corrupt cells over guessing.

### Validation matrix (gate to mark phase done) (D-15-04)
- **D-15-04:** Phase is **not done** until the following adversarial cases all pass (the
  mandated three from the success criteria PLUS the agreed extras):
  - **Mandated:** vim insert (`iHello<Esc>`) → zero corrupt cells; `read -s` noecho prompt →
    zero predicted characters; `你好` CJK → correct cursor column advance.
  - **Extras:** `less` / `htop` (cursor-addressing full-screen apps) → prediction effectively
    disabled, no corruption; bracketed paste → no prediction during paste; Ctrl-C mid-line →
    clean epoch reset, no stale predictions; rapid typing over a **simulated-loss** link →
    predictions confirm/cull correctly, never render worse than no prediction.
  - Because D-15-01 extends prediction to Home/End, add explicit Home/End-motion cases to the
    matrix (cursor lands on the correct confirmed column after a server update).

### Locked by REQUIREMENTS/roadmap (NOT relitigated)
- Prediction is a `PredictionOverlay` compositor layer (D-14-01a seam); render path
  (`render_to_stdout`) stays the single display writer. Predictions carry underline style when
  unconfirmed-and-above-RTT-flag.
- Each `PendingPrediction` carries the server `epoch` that would confirm it; a `StateDiff` with
  `epoch >= epoch_required` confirms it (confirmed cell wins on mismatch — no animation).
  Tolerate multiple dropped datagram confirmations (don't reset after a single miss — Pitfall 4).
- Keystrokes stay on the reliable bidi stream; only display is lossy datagrams.

### Claude's Discretion
- Module layout (`predictor.rs` vs folding into `screen.rs`), the `PendingPrediction` /
  `Validity` state machine internals, and the `VecDeque` cull bookkeeping.
- Exact numeric RTT constants within the Mosh-derived ranges; SRTT smoothing factor.
- The bulk-input batch-size threshold (D-15-01b says ~4 bytes — Claude may tune).
- How the simulated-loss test harness injects datagram drops.

</decisions>

<specifics>
## Specific Ideas

- North star, repeated for the planner: **never render worse than no prediction.** When in
  doubt, epoch-reset and show nothing speculative — correctness beats responsiveness.
- The compositor seam from Phase 14 is load-bearing: prediction is an overlay added to
  `desired`; epoch confirmation removes predictions as the confirmed grid catches up. Reuse it,
  do not add a second display path.

</specifics>

<canonical_refs>
## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### Requirements & success criteria
- `.planning/REQUIREMENTS.md` — **PREDICT-02, PREDICT-03, PREDICT-04, PREDICT-05, PREDICT-06**.
- `.planning/ROADMAP.md` — Phase 15 section (5 success criteria).

### Research (Mosh overlay model — read before planning; flagged highest-complexity)
- `.planning/research/FEATURES.md` — Mosh prediction model, epoch/`Validity` states,
  `cull()` logic, `PendingPrediction` lifecycle, RTT thresholds (show >30ms / underline >80ms),
  no-echo suppression.
- `.planning/research/ARCHITECTURE.md` — planned `crates/nosh-client/src/predictor.rs`
  (`Predictor`, `PendingPrediction`, `PredictDisplayMode`); epoch-confirmation flow.
- `.planning/research/PITFALLS.md` — Pitfall 1 (cursor-addressing apps), Pitfall 2 (CJK width),
  Pitfall 3 (paste/bulk), Pitfall 4 (datagram loss ≠ confirmation loss).

### Code this builds on (Phase 14 compositor + Phase 11 wire format)
- `crates/nosh-client/src/screen.rs` — `ClientScreen`, `Cell` (ch/style/fg/bg), `Overlay` trait,
  `ConnectionLossOverlay` (model the new `PredictionOverlay` on it), `compose_desired()`,
  `render_to_stdout()`, `apply(diff)` + epoch staleness guard.
- `crates/nosh-client/src/client.rs` — `send_input` (~609) and the escape state machine: hook
  prediction AFTER the escape machine, BEFORE `send_input`. Keystroke still goes to server.
- `crates/nosh-client/src/main.rs` — `Args` (clap, ~273); add `--predict always|adaptive|never`
  (default `adaptive`); `run_pump` select loop (datagram apply + epoch-ack).
- `crates/nosh-proto/src/datagram.rs` — `StateDiff`/`DiffRun`/`CellStyle`, `epoch`,
  `decode_datagram`, `encode_epoch_ack`. (`UNDERLINE = 0x04` already exists for the overlay.)
- `.planning/phases/14-client-predictor-confirmed-rendering/14-CONTEXT.md` — D-14-01a seam,
  D-14-05 apply semantics, D-14-03a epoch-ack.

### Architecture
- `CLAUDE.md` — datagram = state-sync display path; single screen-composition path, never direct
  stdout once predictor exists.

</canonical_refs>

<code_context>
## Existing Code Insights

### Reusable Assets
- `ClientScreen` compositor (`screen.rs`): confirmed grid + `Overlay` trait + `compose_desired`
  + `render_to_stdout` are done. Prediction is a new `Overlay` impl returning `Some(Cell)` for
  speculative cells (underline when unconfirmed); the render loop is unchanged.
- `Cell` already has `style: CellStyle` with `UNDERLINE = 0x04` (nosh-proto) — no new style bits.
- Epoch staleness guard + `encode_epoch_ack` (Phase 13/14) — the confirmation signal already exists.

### Established Patterns
- `run_pump` `tokio::select!` arms (datagram-apply arm exists from Phase 14; prediction hooks the
  stdin-input arm + the datagram-apply arm for confirm/cull).
- crossterm raw mode; ANSI emitted only via `render_to_stdout`.

### Integration Points
- **New dependency:** `unicode-width` for column-advance (D-15-03).
- Input arm: escape machine → predictor (enqueue + mark dirty) → `send_input` (unchanged).
- Datagram arm: `apply(diff)` → predictor cull/confirm against new confirmed epoch → re-render.
- CLI: new `--predict` flag drives `PredictDisplayMode`.

</code_context>

<deferred>
## Deferred Ideas

- **nosh-specific RTT threshold tuning** (D-15-02a) — derive our own activation/underline
  thresholds vs. Mosh's constants. → **backlog** (promote via `/gsd:add-backlog`); ship Mosh's
  proven values in Phase 15, tune later only with a measured reason.
- Predicting Tab / completion and word-wise motion — intentionally excluded (D-15-01a); revisit
  only if there's demand and a safe model.
- ConnectionLossOverlay activation, OSC52, terminal title — Phase 16.
- Windows-host live predictive-echo validation — Phase 17.
- Client-side scrollback — M5.

</deferred>

---

*Phase: 15-client-predictor-speculative-overlay*
*Context gathered: 2026-06-02*
