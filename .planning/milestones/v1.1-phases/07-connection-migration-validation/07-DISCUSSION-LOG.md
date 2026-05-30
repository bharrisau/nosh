# Phase 7: Connection Migration Validation - Discussion Log

> **Audit trail only.** Not consumed by downstream agents. Decisions are in CONTEXT.md.

**Date:** 2026-05-30
**Phase:** 07-connection-migration-validation
**Mode:** discuss (interactive, via /gsd:autonomous --interactive)
**Areas discussed:** Acceptance thresholds, qlog/CID depth, Human live-check, Path-change method

## Area selection
Presented 4 gray areas (multiSelect). User selected all four.

## Questions & decisions

### Acceptance thresholds
- **Options:** Loss/error hard, stall soft (recommended, chosen) / Also hard-fail on stall
- **Selected:** Loss/error hard, stall soft
- **Outcome:** D-03/D-04 — hard-fail on byte loss/reorder/ConnectionError; measure stall, soft-warn >3 RTT, no hard latency gate.

### qlog / CID-rotation depth
- **Options:** Parse qlog for CID rotation (recommended, chosen) / Behavioral only / Both
- **Selected:** Parse qlog for CID rotation
- **Outcome:** D-05 — enable quinn qlog, parse for CID rotation/PATH_CHALLENGE (RFC 9000 §9.5).

### Human live-check
- **Options:** Non-blocking + documented (recommended, chosen) / Blocking
- **Selected:** Non-blocking + documented
- **Outcome:** D-06 — documented manual procedure + checklist; phase marked human_needed, autonomous continues; operator records PASSED later.

### Path-change method
- **Options:** Endpoint::rebind new socket (recommended, chosen) / Dual-interface / netns
- **Selected:** Endpoint::rebind new socket
- **Outcome:** D-02 — client rebinds to a fresh local UDP socket mid-session; runs in any CI.

## Scope creep redirected
None. Cold reattach, NAT/relay topologies, status UI, and hard latency SLAs all explicitly deferred.
