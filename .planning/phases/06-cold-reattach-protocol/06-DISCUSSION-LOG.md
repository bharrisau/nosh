# Phase 6: Cold Reattach Protocol - Discussion Log

> **Audit trail only.** Not consumed by downstream agents. Decisions are in CONTEXT.md.

**Date:** 2026-05-30
**Phase:** 06-cold-reattach-protocol
**Mode:** discuss (interactive, via /gsd:autonomous --interactive)
**Areas discussed:** Reattach scope, Token lifetime, Reconnect UX, Replay/ack model

## Area selection
Presented 4 gray areas (multiSelect). User selected all four.

## Questions & decisions

### Reattach scope
- **Options:** In-memory only / Also persist to disk / In-memory, design for disk (chosen)
- **Selected:** In-memory, design for disk
- **Outcome:** D-01/D-02 — ship in-memory reattach; shape token+protocol so disk persistence is additive later (no wire change).

### Token lifetime
- **Options:** Single-use rotate (recommended, chosen) / Long-lived
- **Selected:** Single-use (rotate)
- **Outcome:** D-05 — fresh token in ReattachOk each reattach; previous invalidated; CSPRNG/uuid.

### Reconnect UX
- **Options:** Bounded auto-retry + notice (recommended) / Indefinite auto-retry (chosen) / No auto-retry
- **Selected:** Indefinite auto-retry
- **Outcome:** D-10/D-11 — exponential backoff, retry forever until session ends or user quits; minimal stderr notice; explicit quit/abort path; terminal ReattachErr ends the loop.

### Replay / ack model
- **Options:** Reattach-only seq (recommended) / Continuous acks (chosen)
- **Selected:** Continuous acks
- **Outcome:** D-08/D-09 — client periodically sends Ack{seq}; server trims the Phase-5 buffer to un-acked (64 KiB cap remains); replay from last_acked_seq with truncation indicator if older than retained.

## Cross-phase note
Continuous acks (D-08) EXTEND Phase 5's SequencedOutputBuffer (seq + bounded ring + truncation flag) — Phase 6 adds the Ack message, trim-on-ack, and replay read path. Recorded so the Phase 6 planner extends rather than rebuilds.

## User steer
User chose the more capable option on 3 of 4 axes (design-for-disk, single-use token, indefinite retry, continuous acks) — bias toward robustness, but stay within the Phase 6 boundary (no disk store, no migration).

## Scope creep redirected
None. Migration, disk persistence, named-session selection, status-bar UI, 0-RTT all explicitly deferred in CONTEXT.md.
