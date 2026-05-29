---
gsd_state_version: 1.0
milestone: v1.1
milestone_name: M3 Roaming + Windows Client
status: planning
last_updated: "2026-05-29T12:17:00.129Z"
last_activity: 2026-05-29
progress:
  total_phases: 0
  completed_phases: 0
  total_plans: 0
  completed_plans: 0
  percent: 0
---

# Project State

## Project Reference

See: .planning/PROJECT.md (updated 2026-05-29)

**Core value:** A single QUIC connection on UDP/443 can carry a live interactive shell, authenticated entirely from the user's existing SSH-key identity.
**Current focus:** v1.0 milestone shipped and archived — planning next milestone (M3 roaming)

## Current Position

Phase: Not started (defining requirements)
Plan: —
Status: Defining requirements
Last activity: 2026-05-29 — Milestone v1.1 started

## Performance Metrics

**Velocity:**

- Total plans completed: 0
- Average duration: -
- Total execution time: 0 hours

**By Phase:**

| Phase | Plans | Total | Avg/Plan |
|-------|-------|-------|----------|
| - | - | - | - |

**Recent Trend:**

- Last 5 plans: -
- Trend: -

*Updated after each plan completion*

## Accumulated Context

### Decisions

Decisions are logged in PROJECT.md Key Decisions table.
Recent decisions affecting current work:

- Init: Cert-pinning path for M1 (not RFC 7250 RPK) — rustls issue #2257 resolution unconfirmed in 0.23.40
- Init: Ed25519-first for auth; RSA must be tested before Phase 2 closes
- Init: `spawn_blocking` bridge for PTY I/O in Phase 3 spike (not AsyncFd)
- Init: Session keyed on SSH identity fingerprint (not QUIC connection ID) — M3 reattach seam

### Pending Todos

None yet.

### Blockers/Concerns

- Phase 2: `ssh-agent-client-rs` 1.1.2 RSA SHA-2 flag exposure is unconfirmed — inspect source at implementation time; may need to limit to Ed25519 initially
- Phase 2: `verify_tls13_signature` delegation pattern is non-trivial — plan a focused spike on the signing round-trip before declaring Phase 2 done

## Deferred Items

Items acknowledged and carried forward from initial scoping:

| Category | Item | Status | Deferred At |
|----------|------|--------|-------------|
| v2 | Roaming / QUIC connection migration (ROAM-01..03) | Deferred to M3 | Init |
| v2 | Predictive local echo / datagram state sync (ECHO-01) | Deferred to M4 | Init |
| v2 | Channel multiplexing, forwarding, OSC 52 (FEAT-01..06) | Deferred to M5 | Init |
| v2 | Windows ConPTY (PLAT-01) | Deferred to M6 | Init |
| v2 | WebTransport / NAT topologies (TOPO-01..03) | Deferred to M7 | Init |

## Session Continuity

Last session: 2026-05-29T10:44:40.559Z
Stopped at: Phase 3 context gathered
Resume file: .planning/phases/03-pty-session-core/03-CONTEXT.md

## Operator Next Steps

- Start the next milestone with /gsd-new-milestone
