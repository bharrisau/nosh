# Roadmap: nosh

## Milestones

- ✅ **v1.0 M0–M2 Architecture-Validation Spike** — Phases 1-3 (shipped 2026-05-29)
- ✅ **v1.1 M3 Roaming + Windows Client** — Phases 4-9 (shipped 2026-05-30)
- ✅ **v1.2 M4 Predictive Echo + Daily-Driver Readiness** — Phases 10-18 (shipped 2026-06-07)
- ✅ **v1.3 M5 Channel Multiplexing, Scrollback Sync & TUI Rendering Correctness** — Phases 19-22 (shipped 2026-06-12)

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

Full detail archived at `.planning/milestones/v1.1-ROADMAP.md`.

</details>

<details>
<summary>✅ v1.2 M4 Predictive Echo + Daily-Driver Readiness (Phases 10-18) — SHIPPED 2026-06-07</summary>

- [x] Phase 10: PTY Reader Race Fix (2/2 plans) — completed 2026-06-01
- [x] Phase 11: Datagram Wire Protocol (1/1 plans) — completed 2026-06-01
- [x] Phase 12: Server Terminal State Model (2/2 plans) — completed 2026-06-01
- [x] Phase 13: Server Datagram Sender (3/3 plans) — completed 2026-06-01
- [x] Phase 14: Client Predictor — Confirmed Rendering (3/3 plans) — completed 2026-06-01
- [x] Phase 15: Client Predictor — Speculative Overlay (3/3 plans) — completed 2026-06-02
- [x] Phase 16: QoL Feature Pack + Windows CI Gate (3/3 plans) — completed 2026-06-02
- [x] Phase 17: Windows-Host Predictive Echo Validation (1/1 plans) — completed 2026-06-02
- [ ] Phase 18: Security Design Pass — deferred to a future milestone (SEC-01/SEC-02)

Full detail archived at `.planning/milestones/v1.2-ROADMAP.md`.

</details>

<details>
<summary>✅ v1.3 M5 Channel Multiplexing, Scrollback Sync & TUI Rendering Correctness (Phases 19-22) — SHIPPED 2026-06-12</summary>

- [x] Phase 19: Full-Screen TUI Rendering Correctness — Real alternate-screen buffer (two-grid model), wide-char/grapheme audit, predictor suppression, OSC OOM bound; makes vim/htop/Claude Code work correctly (completed 2026-06-07; Windows visual re-test pending — see STATE.md Deferred Items)
- [x] Phase 20: Repaint Pacing — Burst multiple state-diff datagrams per tick so full-screen repaints land in ~1 RTT; one epoch per tick; both 999.4 traps designed out architecturally (completed 2026-06-11)
- [x] Phase 21: Channel Multiplexing Foundation — Control-first OPEN/ACCEPT/REJECT on control stream (id 0); discriminant-stability test first; per-channel flow control; clean lifecycle; scrollback channel type declared (completed 2026-06-11)
- [x] Phase 22: Scrollback Sync — Scrollback delivered over the reliable scrollback channel; credit-based paging; alt-screen gate; Shift-PageUp/PageDown UX; consistent live-grid handoff and reattach survival (completed 2026-06-12)

Full detail archived at `.planning/milestones/v1.3-ROADMAP.md`.

</details>

## Progress Table

| Phase | Plans Complete | Status | Completed |
|-------|----------------|--------|-----------|
| 1. QUIC Transport Skeleton | 4/4 | Shipped | 2026-05-29 |
| 2. SSH-Key Mutual Auth | 4/4 | Shipped | 2026-05-29 |
| 3. PTY Session Core | 3/3 | Shipped | 2026-05-29 |
| 4. Identity Threading | — | Shipped | 2026-05-30 |
| 5. Session Persistence | — | Shipped | 2026-05-30 |
| 6. Cold Reattach Protocol | — | Shipped | 2026-05-30 |
| 7. Connection Migration Validation | — | Shipped | 2026-05-30 |
| 8. Windows Client | — | Shipped | 2026-05-30 |
| 9. Windows Client Polish & Hardening | — | Shipped | 2026-05-30 |
| 10. PTY Reader Race Fix | 2/2 | Shipped | 2026-06-01 |
| 11. Datagram Wire Protocol | 1/1 | Shipped | 2026-06-01 |
| 12. Server Terminal State Model | 2/2 | Shipped | 2026-06-01 |
| 13. Server Datagram Sender | 3/3 | Shipped | 2026-06-01 |
| 14. Client Predictor — Confirmed Rendering | 3/3 | Shipped | 2026-06-01 |
| 15. Client Predictor — Speculative Overlay | 3/3 | Shipped | 2026-06-02 |
| 16. QoL Feature Pack + Windows CI Gate | 3/3 | Shipped | 2026-06-02 |
| 17. Windows-Host Predictive Echo Validation | 1/1 | Shipped | 2026-06-02 |
| 18. Security Design Pass | 0/? | Deferred | - |
| 19. Full-Screen TUI Rendering Correctness | 5/5 | Shipped | 2026-06-07 |
| 20. Repaint Pacing | 2/2 | Shipped | 2026-06-11 |
| 21. Channel Multiplexing Foundation | 4/4 | Shipped | 2026-06-11 |
| 22. Scrollback Sync | 5/5 | Shipped | 2026-06-12 |
