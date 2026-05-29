# Phase 5: Session Persistence - Discussion Log

> **Audit trail only.** Not consumed by downstream agents. Decisions are in CONTEXT.md.

**Date:** 2026-05-30
**Phase:** 05-session-persistence
**Mode:** discuss (interactive, via /gsd:autonomous --interactive)
**Areas discussed:** Which disconnects persist, Cap-exceeded behavior, Idle-timeout surface, Buffer-overflow behavior

## Area selection
Presented 4 gray areas (multiSelect). User selected all four.

## Questions & decisions

### Which disconnects persist
- **Options:** Only transport loss (recommended) / Any disconnect persists
- **Selected:** Only transport loss
- **Outcome:** D-01/D-02 — orphan only on transport-level loss; explicit SessionClose + shell exit tear down immediately.

### Cap-exceeded behavior
- **Options:** Reject new + error (recommended) / Evict oldest orphan / Higher cap
- **User response:** Proposed evicting the *least-recently-refreshed* orphan, and asked whether marking a session "refreshed" when in use (and reusing that for the timeout) is easy.
- **Resolution:** Confirmed easy — unify on a single `last_active` timestamp serving both idle-timeout and LRU eviction. Switched the decision from "reject" to **LRU evict oldest-active orphan**, default cap 5, never evict an attached session.
- **Outcome:** D-05/D-06/D-07 + D-03 (the unified timestamp).

### Idle-timeout surface
- **Options:** CLI flag from orphan time (recommended) / Also support env var
- **Selected:** Also support env var
- **Outcome:** D-08/D-09 — `--idle-timeout-secs` flag + `NOSH_IDLE_TIMEOUT_SECS` env fallback; default 0; measured from orphan time; cleared on reattach.

### Buffer-overflow behavior
- **Options:** Drop-oldest + truncation marker (recommended) / Drop-oldest silently
- **Selected:** Drop-oldest + truncation marker
- **Outcome:** D-10/D-11 — 64 KiB ring, drop oldest, record truncation for Phase 6 to surface.

## Notable user steer
The unified `last_active` timestamp (one mechanism for idle-timeout AND LRU eviction) was the user's idea — recorded as a specific in CONTEXT.md so it isn't split into two timestamps downstream.

## Scope creep redirected
None — stayed within the persistence boundary; reattach/replay/migration explicitly deferred to Phases 6-7.
