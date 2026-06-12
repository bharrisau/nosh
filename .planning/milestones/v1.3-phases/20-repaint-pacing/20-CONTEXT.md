# Phase 20: Repaint Pacing - Context

**Gathered:** 2026-06-07
**Status:** Ready for planning

<domain>
## Phase Boundary

Make full-screen repaints land in roughly one round-trip instead of dribbling one MTU per 16 ms tick. Today the server sends **one** state-diff datagram per tick: `build_state_diff` computes a diff and `encode_datagram` returns `deferred` runs that trickle out over many ticks. Phase 20 makes the server **burst multiple state-diff datagrams within a single tick** until the diff is delivered, while architecturally designing out the two failure modes that caused the 999.4 attempt to be reverted:
- **R-1 infinite spin:** `build_state_diff` regenerated `fresh_runs` against an un-advancing acked baseline every burst iteration → the burst never drained.
- **R-2 noecho-epoch:** per-datagram epoch increments advanced `confirmed_epoch` during a `read -s` window → predictions leaked.

Scope is the server tick/send loop (`nosh-server/src/server.rs`), the client `apply()` monotonic guard (`nosh-client`), and a required noecho CI gate. Requirements: PACE-01, PACE-02, PACE-03. This is the redo of the deferred/reverted 999.4 D-01 (now 999.6), with its lessons locked.

</domain>

<decisions>
## Implementation Decisions

### Burst budget (per-tick send limiter)
- **D-20-01:** Burst datagrams within a tick until **`datagram_send_buffer_space()` is exhausted OR the diff is fully sent**, whichever first. `datagram_send_buffer_space()` is the primary backpressure gate (per SC3).
- **D-20-02:** Add a **generous safety cap** on datagrams-per-tick set well above a full 80×24 repaint worth of datagrams (e.g. ~2× a full repaint) — high enough it never trips in normal TUI use; its sole purpose is to bound a pathological diff (e.g. a huge paste flushed at once) from monopolising the send buffer in a single tick. Do NOT use a tight fixed cap (that re-introduces the dribbling this phase removes).

### Burst loop shape (designs out R-1 spin)
- **D-20-03:** Within a single tick, call **`build_state_diff` exactly once**. Then drain the resulting run list by looping **`encode_datagram` only** (no per-datagram recompute of `fresh_runs`). Each iteration sends one payload and carries its `deferred` remainder to the next `encode_datagram` call within the same tick's budget. `last_acked_snapshot` non-advancement during a burst must not be able to cause an infinite spin (SC3). This is the architectural fix for R-1.

### Epoch semantics (designs out R-2 noecho leak)
- **D-20-04:** **One epoch per tick.** All burst datagrams sent within a tick share that tick's single epoch value (SC2). `confirmed_epoch` must not advance during a `read -s` window.
- **D-20-05:** When a repaint spills past one tick's budget, the leftover runs carry to the **next tick** as `deferred` and ride the **next tick's (new) epoch** — `build_state_diff` bumps the epoch because `pending_deferred` is non-empty or the grid changed (the existing epoch guard at server.rs ~line 346). Do NOT hold one epoch across multiple ticks until a repaint fully drains (that risks reviving a 999.4-class confirmed_epoch bug).
- **D-20-06:** Preserve **deferred-first ordering** — carried-over deferred runs go ahead of freshly computed runs so cursor-proximate content stays prioritised.

### Client apply guard
- **D-20-07:** Change the `apply()` monotonic guard in `ClientScreen` from `<=` to `<` (SC4) so multiple same-epoch burst datagrams within a tick all apply their runs to the confirmed grid instead of every datagram after the first being discarded.

### Predictive-echo interaction
- **D-20-08:** **No pacing-specific predictor change.** The existing tentative-epoch machinery (predictions hidden until `confirmed_epoch` advances past them) plus Phase 19 alt-screen suppression already cover burst repaints — predictions reconcile naturally when the burst's epoch confirms. Add only test coverage asserting correct predictor behaviour during a burst; do NOT add burst-detection state to the predictor (scope creep + 999.4 epoch-trap risk).

### Required CI gate (security invariant)
- **D-20-09:** `noecho_read_dash_s_zero_predicted_chars` must pass as a **required, non-`#[ignore]` CI gate with burst code active** (SC2) — proving zero predicted chars during `read -s`. `burst_drains_when_grid_differs_from_acked_baseline` must pass RED-before-fix / GREEN-after (SC3), asserting `build_state_diff` is called exactly once per tick and the drain cannot spin.

### Claude's Discretion
- Exact value of the safety cap (within the "generous, never-trips-normally" guidance).
- Precise structure of the per-tick burst loop and how `datagram_send_buffer_space()` is queried each iteration.
- Test harness specifics for the 150 ms-RTT vim-startup assertion (SC1) — simulated/loopback acceptable.

</decisions>

<specifics>
## Specific Ideas

- Architecture should match the locked 999.6 lessons: skip `fresh_runs` recompute when draining deferred (the R-1 fix); ONE epoch per tick (the R-2 fix); `datagram_send_buffer_space()` is the budget gate; the noecho security test and a non-empty-grid-vs-empty-acked burst-drain test are mandatory gates.
- The server terminal model is already decoupled from the send (no QUIC flow-control ceiling on the model); one-datagram-per-tick is the only current limiter being removed.

</specifics>

<canonical_refs>
## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### Phase scope & requirements
- `.planning/ROADMAP.md` — Phase 20 section: goal, 4 success criteria (SC1 vim-startup ≤2 RTT; SC2 noecho CI gate + one-epoch-per-tick; SC3 single build_state_diff/tick + no-spin + datagram_send_buffer_space budget; SC4 apply() `<=`→`<`).
- `.planning/REQUIREMENTS.md` — PACE-01, PACE-02, PACE-03.

### Lessons / pitfalls (the 999.4 reverted attempt)
- `.planning/research/PITFALLS.md` — repaint-pacing pitfalls; the R-1 infinite-spin and R-2 noecho-epoch traps.
- STATE.md `[Phase 20]` concern note: both 999.4 traps (R-1 infinite-spin, R-2 noecho-epoch) must be designed out before the first burst line ships.

### Code under change
- `crates/nosh-server/src/server.rs` — `build_state_diff` (~line 308), `DiffTickResult.deferred` (~line 291), the epoch guard `cells != last_sent_snapshot || !pending_deferred.is_empty()` (~line 346), the tick/send loop and `conn.send_datagram`. This is where the per-tick burst loop is added.
- `crates/nosh-proto/src/datagram.rs` — `encode_datagram` (~line 261) returning `(payload, deferred_runs)`; `MIN_CAP`; the cap-fitting logic. The burst loop calls this repeatedly within a tick.
- `crates/nosh-client/src/screen.rs` / predictor path — the `apply()` monotonic epoch guard to change `<=`→`<` (D-20-07).
- `crates/nosh-client/src/predictor.rs` — tentative-epoch machinery (no change per D-20-08; test coverage only); the existing `noecho` test is the SC2 gate.

</canonical_refs>

<code_context>
## Existing Code Insights

### Reusable Assets
- `build_state_diff` already incorporates several 999.4 lessons: deferred-first ordering (server.rs ~353), epoch-at-tick-time not per-chunk (~342), and the `cells != last_sent_snapshot || !pending_deferred.is_empty()` epoch guard (~346). The burst loop wraps this — it does NOT replace the epoch logic.
- `encode_datagram` already returns `deferred` runs that don't fit the cap — the burst loop feeds these back in within the same tick instead of waiting for the next tick.
- `DiffTickResult` already carries `deferred` for cross-tick carry-over.
- The existing `noecho` predictor test is the basis for the required SC2 CI gate (drop its `#[ignore]` if present, run with burst active).

### Established Patterns
- One-datagram-per-tick is the current limiter; the model is already decoupled from the send (no QUIC flow-control ceiling on the server terminal model).
- `EPOCH_SNAPSHOT_CAP` (16) bounds the per-epoch sent-snapshot store (CR-01 fix) — burst must not blow this; one epoch per tick keeps in-flight epoch count unchanged.

### Integration Points
- Burst loop sits in the tick handler that currently calls `build_state_diff` once and sends one datagram — change it to: build_state_diff once → loop encode_datagram + send_datagram until `datagram_send_buffer_space()` exhausted / diff drained / safety cap hit → carry leftover deferred to next tick.
- Client `apply()` guard change (`<=`→`<`) is the receiver-side enabler for same-epoch burst datagrams.

</code_context>

<deferred>
## Deferred Ideas

- Burst-aware predictor suppression (explicit burst-detection state in the client) — rejected as scope creep (D-20-08).
- Changing the 16 ms tick interval — out of scope; bursting *within* a tick is the fix, not changing tick cadence.

</deferred>

---

*Phase: 20-repaint-pacing*
*Context gathered: 2026-06-07*
