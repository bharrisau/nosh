# Phase 19: Full-Screen TUI Rendering Correctness - Discussion Log

> **Audit trail only.** Not consumed by downstream agents (researcher, planner, executor). Decisions are captured in 19-CONTEXT.md.

**Date:** 2026-06-07
**Phase:** 19-full-screen-tui-rendering-correctness
**Mode:** discuss (interactive, autonomous)
**Areas discussed:** OSC bound, Wide-char width policy, TUI-04 verification bar, Alt-screen ↔ scrollback

## Areas Selected for Discussion

User selected all 4 proposed gray areas: OSC bound (limit & overflow), Wide-char width policy, TUI-04 verification bar, Alt-screen ↔ scrollback.

## Area 1 — OSC accumulation bound (SEC-03)

**Q: On OSC overflow, what should happen?**
- Options: Truncate + resync parser (recommended) / Drop sequence keep parsing / Kill the session
- **Selected:** Truncate + resync parser → D-19-02

**Q: What ceiling bounds OSC accumulation before resync?**
- Options: 1 MiB (recommended) / 256 KiB / Match storage caps 64 KiB
- **Selected:** 1 MiB → D-19-01

## Area 2 — Wide-char width policy (TUI-03)

**Q: How to measure East Asian Ambiguous-width chars?**
- Options: Ambiguous→1 no config (recommended) / Ambiguous→2 CJK mode / Make configurable now
- **Selected:** Ambiguous→1, unicode-width default, no config → D-19-04, D-19-05

**Q: How to represent the spacer cell at col+1 after a width-2 char?**
- Options: Explicit continuation marker (recommended) / Blank space placeholder / You decide during planning
- **Selected:** Explicit continuation marker → D-19-06

## Area 3 — TUI-04 verification bar

**Q: Acceptance bar for vim/htop/Claude Code render correctly?**
- Options: Synthetic VT tests + manual visual pass (recommended) / Automated golden-master against live apps / Manual visual only
- **Selected:** Synthetic VT grid-assertion tests in CI + documented manual visual pass → D-19-07, D-19-08

## Area 4 — Alt-screen ↔ scrollback

**Q: How should scrollback behave while alt-screen is active?**
- Options: No scrollback in alt-screen, primary frozen (recommended) / Alt grid gets own scrollback / Keep current shared-history behavior
- **Selected:** No scrollback in alt-screen; primary frozen and restored intact; scroll_up gates on !alt_screen → D-19-09

## Deferred Ideas Raised

- Mode 2027 grapheme clustering → v1.4+
- Configurable ambiguous-width negotiation → out of scope
- Scrollback sync to client → Phase 22

## Claude's Discretion (explicitly left open)

- Predictor suppression mechanism (TUI-05), two-grid data structure, OSC pre-accumulation buffering mechanism, dual-grid resize implementation.
