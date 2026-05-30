---
gsd_state_version: 1.0
milestone: v1.1
milestone_name: M3 Roaming + Windows Client
status: planning
stopped_at: Phase 4 context gathered
last_updated: "2026-05-29T23:42:55.870Z"
last_activity: 2026-05-29 -- Phase 05 planning complete
progress:
  total_phases: 5
  completed_phases: 2
  total_plans: 3
  completed_plans: 2
  percent: 40
---

# Project State

## Project Reference

See: .planning/PROJECT.md (updated 2026-05-29)

**Core value:** A single QUIC connection on UDP/443 can carry a live interactive shell, authenticated entirely from the user's existing SSH-key identity — and that session survives network changes without re-authenticating.
**Current focus:** Phase 5 — session persistence

## Current Position

Phase: 5
Plan: Not started
Status: Ready to plan
Last activity: 2026-05-29 -- Phase 05 planning complete

```
Progress: [          ] 0% (0/5 phases)
```

## Performance Metrics

**Velocity:**

- Total plans completed: 2 (v1.1)
- Average duration: - (see v1.0 archive for historical baseline)
- Total execution time: 0 hours (v1.1)

**By Phase:**

| Phase | Plans | Total | Avg/Plan |
|-------|-------|-------|----------|
| 4. Identity Threading | 0/? | - | - |
| 5. Session Persistence | 0/? | - | - |
| 6. Cold Reattach Protocol | 0/? | - | - |
| 7. Connection Migration Validation | 0/? | - | - |
| 8. Windows Client | 0/? | - | - |
| 4 | 2 | - | - |

**Recent Trend:**

- Last 5 plans: -
- Trend: -

*Updated after each plan completion*

## Accumulated Context

### Decisions

Decisions are logged in PROJECT.md Key Decisions table.
Recent decisions affecting current work:

- v1.0: Cert-pinning path for M1 (not RFC 7250 RPK) — rustls issue #2257 resolution unconfirmed in 0.23.40
- v1.0: Ed25519-first for auth; RSA must be tested before Phase 2 closes
- v1.0: `spawn_blocking` bridge for PTY I/O (not AsyncFd)
- v1.0: Session keyed on SSH identity fingerprint (not QUIC connection ID) — M3 reattach seam
- v1.1 roadmap: Identity threading is Phase 4 (IDENT-01 only) — prerequisite seam; tiny surface, blocks everything else
- v1.1 roadmap: `MasterPty` must move into `SessionSlot` and stay open for entire orphan lifetime (SIGHUP prevention — critical correctness requirement for Phase 5)
- v1.1 roadmap: Reattach token is a session selector, not a credential; full TLS handshake re-runs on every reconnect (two-factor design baked in from Phase 6 first implementation, not retrofitted)
- v1.1 roadmap: `ServerConfig::migration(true)` set explicitly even though it is the QUIC default — documents intent, guards against future default changes (Phase 7)
- v1.1 roadmap: Windows client (Phase 8) isolated behind `#[cfg]` gates in nosh-client only; nosh-proto, nosh-auth, nosh-server unchanged

### Pending Todos

- At Phase 5 start: verify `uuid` crate is already in nosh-server lockfile (research indicates yes — confirm)
- At Phase 6 start: review PITFALLS.md 13-item reattach checklist before planning; state machine spec is the risk
- At Phase 8 start: confirm `ring` 0.17.14 precompiled x86_64-windows assembly objects are present (no NASM/CMake needed)

### Blockers/Concerns

- Phase 6 (Cold Reattach): state machine correctness and two-factor authorization design are the principal risk; must be correct from first implementation (cannot retrofit identity check after token-only build without a protocol change)
- Phase 8 (Windows): Windows ACL permission check gap — `std::fs::Permissions` cannot read ACLs; best-effort warning + documented limitation is the agreed approach
- Phase 8 (Windows): crossterm `use-dev-tty` feature must NOT be enabled (Unix-only; breaks Windows build with `event-stream` per issue #935)

## Deferred Items

Items acknowledged and carried forward:

| Category | Item | Status | Deferred At |
|----------|------|--------|-------------|
| v2 (M4) | Predictive local echo / datagram state sync (ECHO-01) | Deferred to M4 | Init |
| v2 (M5) | Channel multiplexing, forwarding, OSC 52 (FEAT-01..06) | Deferred to M5 | Init |
| v2 (M6) | Windows ConPTY / native server (PLAT-01) | Deferred to M6 | Init |
| v2 (M7) | WebTransport / NAT topologies (TOPO-01..03) | Deferred to M7 | Init |
| v2 (M3+) | Connection status / latency indicator (ROAM-03) | Deferred to M4+ | v1.1 scoping |
| v2 (M3+) | Windows ssh-agent / Pageant (WIN-05) | Deferred post-v1.1 | v1.1 scoping |
| v2 (M3+) | Encrypted key passphrase prompt (WIN-06) | Deferred post-v1.1 | v1.1 scoping |
| v2 (M5+) | Named/numbered session selection | Deferred to M5+ | v1.1 scoping |

## Session Continuity

Last session: 2026-05-29T22:40:56.153Z
Stopped at: Phase 4 context gathered
Resume file: .planning/phases/04-identity-threading/04-CONTEXT.md

## Operator Next Steps

- Start Phase 4: `/gsd:plan-phase 4`
