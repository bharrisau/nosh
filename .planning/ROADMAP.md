# Roadmap: nosh

## Milestones

- ✅ **v1.0 M0–M2 Architecture-Validation Spike** — Phases 1-3 (shipped 2026-05-29)
- ✅ **v1.1 M3 Roaming + Windows Client** — Phases 4-9 (shipped 2026-05-30)
- 📋 **v1.2 (M4) — next** — to be planned (`/gsd:new-milestone`)

## Phases

<details>
<summary>✅ v1.0 M0–M2 Architecture-Validation Spike (Phases 1-3) — SHIPPED 2026-05-29</summary>

- [x] Phase 1: QUIC Transport Skeleton (4/4 plans) — completed 2026-05-29
- [x] Phase 2: SSH-Key Mutual Auth (4/4 plans) — completed 2026-05-29
- [x] Phase 3: PTY Session Core (3/3 plans) — completed 2026-05-29

Full detail archived at `.planning/milestones/v1.0-ROADMAP.md`.

</details>

<details>
<summary>✅ v1.1 M3 Roaming + Windows Client (Phases 4-9) — SHIPPED 2026-05-30</summary>

- [x] Phase 4: Identity Threading — `Session.identity` from the authenticated TLS handshake (completed 2026-05-30)
- [x] Phase 5: Session Persistence — orphaned sessions survive disconnect; per-identity cap + idle timeout (completed 2026-05-30)
- [x] Phase 6: Cold Reattach Protocol — 1-RTT reconnect to an orphaned session, two-factor authorization (completed 2026-05-30)
- [x] Phase 7: Connection Migration Validation — explicit migration config + headless and live roaming coverage (completed 2026-05-30)
- [x] Phase 8: Windows Client — native Windows client → Linux server, on-disk key signing, raw mode, resize, locale (completed 2026-05-30)
- [x] Phase 9: Windows Client Polish & Hardening — VT console-input + `~.` escape, authorized_keys warn+skip, connect timeout, server migration logging (completed 2026-05-30; Windows-host validated)

Full detail archived at `.planning/milestones/v1.1-ROADMAP.md`. Audit: `.planning/milestones/v1.1-MILESTONE-AUDIT.md` (11/11 reqs, 4/4 integration, no blockers; 3 tracked tech-debt items).

</details>

## Next

v1.2 (M4) not yet scoped. Run `/gsd:new-milestone` to begin. Carried tech debt to weigh: PTY reader-zombie race (Phase 6, latent), Windows cross-compile CI gate (never run — wire a git remote), and the `WSAEMSGSIZE` quinn_udp investigation.
