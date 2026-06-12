# Phase 20: Repaint Pacing - Discussion Log

> **Audit trail only.** Not consumed by downstream agents. Decisions are captured in 20-CONTEXT.md.

**Date:** 2026-06-07
**Phase:** 20-repaint-pacing
**Mode:** discuss (interactive, autonomous)
**Areas discussed:** Per-tick burst budget, Mid-repaint budget exhaustion / epoch, Predictive-echo interaction during burst

## Context note
Success criteria are highly prescriptive (one-epoch-per-tick, single build_state_diff/tick, apply() <=→<, noecho CI gate, the two 999.4 trap fixes). Discussion covered only the genuinely open decisions.

## Area 1 — Per-tick burst budget
**Q: What bounds datagrams burst per tick?**
- Options: Buffer-space gate + generous safety cap (recommended) / Buffer-space only no cap / Fixed datagram cap
- **Selected:** Buffer-space gate (datagram_send_buffer_space) + generous safety cap → D-20-01, D-20-02

## Area 2 — Mid-repaint budget exhaustion & epoch
**Q: Epoch behaviour for carried-over remainder across tick boundary?**
- Options: New epoch next tick / one-epoch-per-tick (recommended) / Same epoch until repaint drained
- **Selected:** New epoch next tick; one epoch per tick; deferred-first ordering preserved → D-20-04, D-20-05, D-20-06
- Burst loop shape (build_state_diff once/tick, drain via encode_datagram only — R-1 fix) → D-20-03

## Area 3 — Predictive-echo interaction during burst
**Q: Add predictor-specific behaviour for in-flight burst repaints (non-alt)?**
- Options: No pacing-specific change, rely on existing machinery (recommended) / Add explicit suppression during burst
- **Selected:** No pacing-specific change; tentative-epoch + Phase 19 alt-screen suppression already cover it; test coverage only → D-20-08

## Locked by success criteria (not discussed)
- apply() `<=`→`<` (D-20-07), one-epoch-per-tick (D-20-04), noecho required CI gate + burst-drain RED/GREEN test (D-20-09).

## Deferred Ideas Raised
- Burst-aware predictor suppression → rejected (scope creep)
- Changing the 16 ms tick interval → out of scope
