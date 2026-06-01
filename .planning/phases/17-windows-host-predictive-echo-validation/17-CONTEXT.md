# Phase 17: Windows-Host Predictive Echo Validation - Context

**Gathered:** 2026-06-02
**Status:** Ready for planning

<domain>
## Phase Boundary

Live, **operator-run** validation that predictive echo (Phase 15) plus the QoL pack (Phase 16)
work on the **native Windows client against a Linux server over a real, non-loopback network
path** — signed off in a validation document (`docs/windows-echo-test.md`), mirroring the v1.1
Phase 9 Windows sign-off (`docs/windows-client-test.md`). Requirement: PREDICT-07.

**This phase runs on a physical Windows host** — Linux execution halts before this phase and
resumes from Claude on a Windows PC. No cross-platform dev work happens here; the engine is the
shared one from Phase 15. The deliverable is evidence + a signed-off doc, not new features.
Out of scope: any predictor/QoL implementation (that is Phase 15/16); the security doc (Phase 18).

</domain>

<decisions>
## Implementation Decisions

### Migration test method (D-17-01)
- **D-17-01:** **WireGuard-tunnel teardown drives the path change.** For criterion #3 (connection
  migration concurrent with active prediction): the operator brings up a WireGuard tunnel,
  connects `nosh` through it, then **closes the tunnel** mid-session to force the underlying
  network-path change. Validate that QUIC connection migration continues the session and the
  prediction epoch resets cleanly (no screen corruption) across the change. The doc records the
  exact WG config / teardown step used.

### Evidence standard (D-17-02)
- **D-17-02:** **Measured timing, not just attestation.** The "predictive echo engages at
  sub-RTT latency" criterion must be evidenced with **measured** predicted-vs-confirmed timing
  recorded in the sign-off doc (actual numbers), not only operator observation.
- **D-17-02a (prerequisite — instrumentation must exist before Windows):** Measured timing
  requires the client to expose predicted-keystroke time vs the confirming-datagram time (e.g. a
  `--predict-debug` tracing span / latency log). Because Phase 17 is Windows-only with **no dev
  work**, this instrumentation MUST be implemented on Linux **in Phase 15 or Phase 16** and ship
  in the client the operator runs on Windows. Flagged as a cross-phase dependency (see
  `<deferred>` / Phase 16 folding note). If absent, Phase 17 cannot satisfy D-17-02.

### App / fallback coverage (D-17-03)
- **D-17-03:** **Mandated minimum + Windows-native.** Validate the mandated cases — vim insert
  (zero corrupt cells) and a noecho prompt (zero predicted characters) — **plus** a
  Windows-native editor and PowerShell/cmd quirks (the platform-specific risk surface). Lighter
  than re-running the full Phase 15 adversarial matrix; focused on Windows-specific behavior.

### Sign-off doc (D-17-04)
- **D-17-04:** **Mirror the v1.1 Windows sign-off format.** Structure `docs/windows-echo-test.md`
  on `docs/windows-client-test.md`: environment/versions, step-by-step checklist with
  observed/expected, and an explicit operator sign-off line. Record (per criterion #4): auth,
  predicted echo (with measured latency), epoch reset on vim, noecho suppression, and
  roaming-with-prediction (the WG teardown).

### Locked by REQUIREMENTS/roadmap (NOT relitigated)
- Must be executed from a physical Windows host (not Linux cross-compile / CI). Human validation
  sign-off is a required success criterion. The maintainer runs `/gsd:plan-phase 17` and executes
  from Claude on the Windows machine.

### Claude's Discretion
- Exact doc section ordering within the v1.1-mirrored format; how the WG config snippet is shown.
- The measured-timing capture mechanism's exact log format (within the Phase 15/16 instrumentation).

</decisions>

<specifics>
## Specific Ideas

- Halt Linux execution before this phase; resume from a Windows machine (per roadmap note).
- This is an attestation/evidence phase — the bar is "a reviewer can read the doc and believe
  predictive echo works on Windows over a real network, including roaming."

</specifics>

<canonical_refs>
## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### Requirements & success criteria
- `.planning/REQUIREMENTS.md` — **PREDICT-07**.
- `.planning/ROADMAP.md` — Phase 17 section (4 success criteria + the Windows-host execution note).

### Validation precedent to mirror
- `docs/windows-client-test.md` — the v1.1 Phase 9 Windows sign-off; format and rigor to mirror.

### Upstream phases this validates
- `.planning/phases/15-client-predictor-speculative-overlay/15-CONTEXT.md` — the predictor under
  test (esp. D-15-04 adversarial matrix; conservative fallback; noecho suppression).
- Phase 16 (QoL + Windows CI gate) CONTEXT — the QoL features + the `--predict` flag; the home for
  the measured-timing instrumentation (D-17-02a).

### Architecture
- `CLAUDE.md` — `portable-pty` ConPTY path; native-Windows-server/client goal; predictor engine
  shared Linux/Windows.

</canonical_refs>

<code_context>
## Existing Code Insights

### Reusable Assets
- `docs/windows-client-test.md` — existing sign-off template to clone.
- The Windows client + ConPTY path already shipped (v1.1 Phases 8–9).

### Established Patterns
- v1.1 Windows validation was a human-run checklist with operator sign-off; this repeats that
  pattern for predictive echo + roaming.

### Integration Points
- Measured-timing evidence (D-17-02) consumes a client instrumentation hook that must be added in
  Phase 15/16 (Linux) — not in this phase.

</code_context>

<deferred>
## Deferred Ideas

- **Prediction-latency instrumentation** (`--predict-debug` / timing log) — must be built in
  Phase 15 or Phase 16 on Linux so it ships in the Windows client for D-17-02. To be folded into
  Phase 16 planning (its context predates this decision).
- Full Phase 15 adversarial matrix on Windows — intentionally reduced to mandated + Windows-native
  (D-17-03); revisit only if Windows-specific corruption appears.

</deferred>

---

*Phase: 17-windows-host-predictive-echo-validation*
*Context gathered: 2026-06-02*
