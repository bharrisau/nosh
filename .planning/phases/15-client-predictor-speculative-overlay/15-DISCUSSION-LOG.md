# Phase 15: Client Predictor — Speculative Overlay - Discussion Log

> **Audit trail only.** Do not use as input to planning, research, or execution agents.
> Decisions are captured in CONTEXT.md — this log preserves the alternatives considered.

**Date:** 2026-06-02
**Phase:** 15-client-predictor-speculative-overlay
**Areas discussed:** Fallback aggressiveness, Adaptive RTT tuning, Wide/ambiguous-width policy, Validation matrix
**Mode:** discuss (interactive, via /gsd:autonomous --interactive)

---

## Fallback aggressiveness

| Option | Description | Selected |
|--------|-------------|----------|
| Maximally conservative | Predict only printable / backspace / single-cell ←→; epoch-reset on everything else (Home/End, word-motion, Tab, control/CSI, paste, bulk). | |
| Extend a few safe cases | Same base + Home/End line-bound motion (and maybe Tab). More responsive in shells; each extra case is a new corruption risk to validate. | ✓ |

**User's choice:** Extend a few safe cases.
**Notes:** Resolved to **Home/End yes, Tab no** (Tab excluded — completion/tab-stop ambiguity), word-motion stays epoch-reset. Bracketed-paste + bulk-input suppression retained. Extra predicted cases carried into the validation matrix. → D-15-01 / D-15-01a.

---

## Adaptive RTT tuning

| Option | Description | Selected |
|--------|-------------|----------|
| Mirror Mosh constants | Reuse Mosh's thresholds + hysteresis (show >~30ms / off <~20ms; underline >~80ms / off <~50ms). Proven, invisible on loopback. | ✓ |
| nosh-specific tuning | Derive our own thresholds. More validation, risks flicker; only if a specific feel is intended. | |

**User's choice:** Default to Mosh, add a backlog item to tune our own.
**Notes:** Ship Mosh's proven constants in Phase 15; deriving nosh-specific thresholds deferred to backlog (D-15-02a, see Deferred Ideas). Tune later only with a measured reason.

---

## Wide / ambiguous-width policy

| Option | Description | Selected |
|--------|-------------|----------|
| Conservative reset | Predict clean width-1 / width-2 (CJK) via unicode-width; epoch-reset on ambiguous-width, combining, ZWJ/emoji. | ✓ |
| Best-effort tracking | Attempt column tracking for all input incl. emoji/combining. Higher corruption risk (Mosh: "no easy solution"). | |

**User's choice:** Conservative reset. → D-15-03.

---

## Validation matrix

| Option | Description | Selected |
|--------|-------------|----------|
| Mandated + key adversarial | vim insert, read -s, 你好 CJK + less/htop, bracketed paste, Ctrl-C mid-line, rapid typing over simulated-loss link. | ✓ |
| Mandated minimum only | Just vim insert, read -s, CJK. | |

**User's choice:** Mandated + key adversarial. → D-15-04 (plus Home/End cases added because prediction was extended).

---

## Claude's Discretion

- Module layout (`predictor.rs` vs `screen.rs`), `PendingPrediction`/`Validity` state machine internals, `VecDeque` cull bookkeeping.
- Exact numeric RTT constants within Mosh-derived ranges; SRTT smoothing factor.
- Bulk-input batch-size threshold (~4 bytes baseline).
- Simulated-loss test harness mechanics.

## Deferred Ideas

- nosh-specific RTT threshold tuning → backlog (D-15-02a).
- Predicting Tab/completion and word-wise motion — intentionally excluded.
- ConnectionLossOverlay / OSC52 / title — Phase 16.
- Windows-host live validation — Phase 17.
- Client-side scrollback — M5.
