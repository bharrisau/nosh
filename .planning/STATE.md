---
gsd_state_version: 1.0
milestone: v1.2
milestone_name: M4 Predictive Echo + Daily-Driver Readiness
status: executing
stopped_at: Phase 999.3 context gathered
last_updated: "2026-06-05T04:43:10.880Z"
last_activity: 2026-06-05 -- Phase 999.3 execution started
progress:
  total_phases: 12
  completed_phases: 8
  total_plans: 22
  completed_plans: 20
  percent: 67
---

# Project State

## Project Reference

See: .planning/PROJECT.md (updated 2026-06-01)

**Core value:** A single QUIC connection on UDP/443 can carry a live interactive shell, authenticated entirely from the user's existing SSH-key identity — and that session survives network changes without re-authenticating.
**Current focus:** Phase 999.3 — client-terminal-rendering-correctness-pack-platform-agnostic

## Current Position

Phase: 999.3 (client-terminal-rendering-correctness-pack-platform-agnostic) — EXECUTING
Plan: 3 of 4
Status: Executing Phase 999.3
Last activity: 2026-06-05 -- Phase 999.3 execution started

```
Progress: [█████████░] 91%
```

## Performance Metrics

**Velocity:**

- v1.0: 3 phases, 11 plans (single day, 2026-05-29)
- v1.1: 6 phases (2026-05-30)
- v1.2: 0/9 phases complete

**By Phase (v1.2):**

| Phase | Plans | Total | Avg/Plan |
|-------|-------|-------|----------|
| 10. PTY Reader Race Fix | 0/? | - | - |
| 11. Datagram Wire Protocol | 0/? | - | - |
| 12. Server Terminal State Model | 0/? | - | - |
| 13. Server Datagram Sender | 0/? | - | - |
| 14. Client Predictor — Confirmed Rendering | 0/? | - | - |
| 15. Client Predictor — Speculative Overlay | 0/? | - | - |
| 16. QoL Feature Pack + Windows CI Gate | 0/? | - | - |
| 17. Windows-Host Predictive Echo Validation | 0/? | - | - |
| 18. Security Design Pass | 0/? | - | - |
| 10 | 2 | - | - |
| 11 | 1 | - | - |
| 12 | 2 | - | - |
| 13 | 3 | - | - |
| 14 | 3 | - | - |
| 15 | 3 | - | - |

**Recent Trend:**

- Last 5 plans: -
- Trend: -

*Updated after each plan completion*
| Phase 15 P01 | 45 | 2 tasks | 3 files |
| Phase 15 P02 | 30 | 2 tasks | 2 files |
| Phase 15 P03 | 45 | 2 tasks | 2 files |
| Phase 16 P01 | 30 | 3 tasks | 6 files |
| Phase 16-qol-feature-pack-windows-ci-gate P03 | 5 | 1 tasks | 2 files |
| Phase 16-qol-feature-pack-windows-ci-gate P02 | 15 | 3 tasks | 4 files |
| Phase 999.3 P01 | 4 | 2 tasks | 1 files |
| Phase 999.3 P03 | 20 | 2 tasks | 1 files |

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
- v1.2 roadmap: PTY reader fix uses `nix::poll` self-pipe trick on `[PTY fd, shutdown pipe]` — `tokio::io::unix::AsyncFd` is the alternative; verify exact `MasterPty::as_raw_fd()` method name at Phase 10 implementation
- v1.2 roadmap: `termwiz 0.23.3` added as the single new consequential dep — `Surface` + `get_changes` / `flush_changes_older_than` provides the terminal grid and diff API without owning ~2000 lines of bespoke grid logic
- v1.2 roadmap: `PtyData` on the reliable stream MUST continue to advance `highest_applied` after datagram path lands — the Ack mechanism and SequencedOutputBuffer trim depend on it; never break this invariant
- v1.2 roadmap: Keystrokes go on the reliable stream only — never as datagrams; keystroke loss is never acceptable
- v1.2 roadmap: All output to the local terminal goes through `ClientScreen.render_to_stdout()` — never direct `stdout.write_all` once the predictor exists
- v1.2 roadmap: Datagrams suppressed on the client during reattach replay window; `ResumeComplete` signal gates fresh datagrams post-replay
- v1.2 roadmap: Epoch-reset-on-cursor-move is a day-one design gate — predicting in cursor-addressing apps produces screen corruption worse than no prediction; conservative fallback baked into initial speculative overlay design
- v1.2 roadmap: Noecho-suppression is a security requirement of prediction — engine must track server's confirmed echo state and suppress prediction during `stty -echo` prompts; validated with `read -s` test
- v1.2 roadmap: Phase 17 (Windows-host validation) must execute from a physical Windows PC — halt Linux execution, run from Windows machine like v1.1 Phase 9; HARDEN-02/03 stay in Phase 16 (authorable from Linux)
- v1.2 roadmap: 0-RTT cold reattach still deliberately deferred — 1-RTT already ships, replay-safety burden not justified
- [Phase 15-03]: EpochReset/BulkSuppressed call reset() not become_tentative() — clears all pending predictions so no stale speculative state remains visible after Ctrl-C/ESC/cursor-addressing
- [Phase 16-01]: TerminalControl appended after Ack (discriminant 9) to preserve postcard discriminant order
- [Phase 16-01]: vte std re-enabled with explicit osc_dispatch caps (OSC_52_MAX_BYTES=65536, MAX_TITLE_BYTES=1024) to re-mitigate CR-03
- [Phase 16-01]: OSC 52 read/query form ('?') silently dropped in osc_dispatch before any store (D-16-01a)
- [Phase 16-01]: drain_terminal_control() uses Option::take semantics to prevent double-forwarding
- [Phase 16-01]: TerminalControl forwarded via write_message (reliable stream) NEVER via send_datagram
- [Phase ?]: emit_diff factored as shared private method
- [Phase ?]: Predictor held in run_pump not overlays Vec
- [Phase ?]: D-17-02a latency hook uses HashMap in run_pump
- [Phase ?]: D-16-04: native windows-latest MSVC replaces Linux GNU cross-compile for nosh-client Windows CI gate (HARDEN-02)
- [Phase 17]: Phase 18 (Security Design Pass) deferred to a future milestone — user decision post Phase 17 sign-off
- [Phase 17]: Platform-agnostic terminal-rendering defects (no clear-on-connect, typematic glitch, etc.) backlogged as 999.3 — not Windows-specific, to be fixed on Linux

### Pending Todos

- At Phase 10 start: verify exact `MasterPty::as_raw_fd()` method name in `portable-pty 0.9.0` (STACK.md states `AsRawFd` is available; confirm before wiring shutdown pipe; gate `#[cfg(unix)]`)
- At Phase 11 start: run per-phase research on sparse-diff encoding strategy — how to handle large-repaint frames (vim file open, `clear`) within QUIC datagram MTU. Three options: cursor-priority partial update, skip-frame-and-wait, reliable-stream fallback for full-screen repaints
- At Phase 12 start: verify vte 0.15.0 `Perform` trait `osc_dispatch` exact parameter signature at docs.rs before committing to the API (MEDIUM confidence — `fn osc_dispatch(&mut self, params: &[&[u8]], bell_terminated: bool)` expected but not verified)
- At Phase 15 start: run per-phase research on Mosh `terminaloverlay.cc` — epoch model, `Validity` enum, `cull()` logic, `PendingPrediction` lifecycle; budget 2-3 planning passes; this is the hardest UX step in M4
- At Phase 16 start: add `osc52` feature flag to `nosh-client/Cargo.toml` for crossterm; confirm `crossterm::clipboard::CopyToClipboard` API surface
- At Phase 17: HALT — execute from a physical Windows host, not a Linux machine
- At Phase 18: use PITFALLS.md "Looks Done But Isn't" checklist as sign-off criteria for the security doc

### Blockers/Concerns

- Phase 15 (Speculative Overlay): RESOLVED — adversarial tests (vim, `read -s`, CJK) all pass; noecho security gate proven adversarially against live PTY in Always mode
- Phase 17 (Windows validation): RESOLVED — validated 2026-06-02 on physical Windows host (10.0.26100) against Linux server `sandstorm`; all C1–C6 PASSED; PREDICT-07 satisfied
- Phase 11 (wire format): sparse-diff encoding strategy for large repaints is an open design decision that blocks all prediction work; must be resolved as Phase 11's first task

## Deferred Items

Items acknowledged and carried forward:

| Category | Item | Status | Deferred At |
|----------|------|--------|-------------|
| v2 (M5) | Channel multiplexing, forwarding (MUX-01/02, FWD-01/02) | Deferred to M5 | Init |
| v2 (M5) | Full native scrollback sync (SCROLL-01) | Deferred to M5 | Init |
| v2 (M5) | Named/numbered session selection | Deferred to M5 | v1.1 scoping |
| v2 (M5) | File transfer (XFER-01) | Deferred to M5 | Init |
| v2 (M6) | Windows ConPTY / native server (PLAT-01) | Deferred to M6 | Init |
| v2 (M6) | Windows ssh-agent / Pageant signing (PLAT-02) | Deferred post-v1.1 | v1.1 scoping |
| v2 (M6) | Encrypted key passphrase prompt | Deferred post-v1.1 | v1.1 scoping |
| v2 (M7) | WebTransport / NAT topologies | Deferred to M7 | Init |
| v2 (post-M4) | 0-RTT cold reattach | Deliberately deferred | INIT.md; 1-RTT ships |
| v2 (post-M4) | RFC 7250 RPK (TLS raw public keys) | Deferred; cert-pinning proven first | v1.0 |
| v2 (post-M4) | OSC 52 clipboard read (paste remote→local) | Excluded: security hole | v1.2 scoping |
| v2 (post-M4) | tmux/screen integration | Excluded by maintainer | v1.2 scoping |
| v2 (post-M4) | Bell/notification passthrough (OSC 9) | Low daily-driver value | v1.2 research |

## Session Continuity

Last session: 2026-06-05T04:43:10.840Z
Stopped at: Phase 999.3 context gathered
Resume file: .planning/phases/999.3-client-terminal-rendering-correctness-pack-platform-agnostic/999.3-CONTEXT.md

## Operator Next Steps

1. Run `/gsd:plan-phase 10` to plan the PTY Reader Race Fix (no research phase needed — nix::poll self-pipe is a standard pattern)
2. Phase 11 and 12 will prompt for research phases at plan time (wire format sparse-diff strategy; vte osc_dispatch verification)
3. Phase 15 will prompt for a research phase (Mosh terminaloverlay.cc translation — highest complexity)
4. Before Phase 17: switch to a physical Windows host
