---
gsd_state_version: 1.0
milestone: v1.3
milestone_name: M5 Channel Multiplexing, Scrollback Sync & TUI Rendering Correctness
status: ready_to_plan
stopped_at: Phase 19 complete (5/5) — ready to discuss Phase 20
last_updated: 2026-06-07T07:13:43.210Z
last_activity: 2026-06-07 -- Phase 19 execution started
progress:
  total_phases: 4
  completed_phases: 1
  total_plans: 5
  completed_plans: 16
  percent: 25
---

# Project State

## Project Reference

See: .planning/PROJECT.md (updated 2026-06-07)

**Core value:** A single QUIC connection on UDP/443 can carry a live interactive shell, authenticated entirely from the user's existing SSH-key identity — and that session survives network changes without re-authenticating.
**Current focus:** Phase 20 — repaint pacing

## Current Position

Phase: 20
Plan: Not started
Status: Ready to plan
Last activity: 2026-06-07

```
Progress: [██████████] 100%
```

## Performance Metrics

**Velocity:**

- v1.0: 3 phases, 11 plans (single day, 2026-05-29)
- v1.1: 6 phases (2026-05-30)
- v1.2: 8 phases complete (10-17), 1 deferred (18); ~29 plans
- v1.3: 0/4 phases complete

**By Phase (v1.3):**

| Phase | Plans | Total | Avg/Plan |
|-------|-------|-------|----------|
| 19. Full-Screen TUI Rendering Correctness | 0/? | - | - |
| 20. Repaint Pacing | 0/? | - | - |
| 21. Channel Multiplexing Foundation | 0/? | - | - |
| 22. Scrollback Sync | 0/? | - | - |
| 19 | 5 | - | - |

**Recent Trend:**

- Last 5 plans: -
- Trend: -

*Updated after each plan completion*
| Phase 19-full-screen-tui-rendering-correctness P01 | 20 | 2 tasks | 1 files |
| Phase 19 P03 | 25 | 2 tasks | 2 files |
| Phase 19 P04 | 504 | 2 tasks | 5 files |
| Phase 19-full-screen-tui-rendering-correctness P05 | 25 | 3 tasks | 2 files |

## Accumulated Context

### Decisions

Decisions are logged in PROJECT.md Key Decisions table.
Recent decisions affecting current work:

- v1.3 roadmap: Alt-screen implementation is atomic — save+swap+clear on `?1049h`, restore+swap on `?1049l`, both grids resized together; a half-built alt-screen is demonstrably worse than the current no-op flag (PITFALLS A-1/A-6)
- v1.3 roadmap: `Cell.ch` changes from `char` to `String`; `unicode-segmentation 1.12` added to workspace for grapheme cluster splitting; `termwiz::cell::grapheme_column_width()` used for width measurement (transitive via portable-pty, no explicit dep)
- v1.3 roadmap: SEC-03 (999.7 OSC OOM bound) folds into Phase 19 — shares the `TerminalState::advance` code path with the alt-screen work; if Phase 19 ships without it, `docs/999.7-SECURITY.md` must be created documenting residual risk
- v1.3 roadmap: Repaint pacing — one epoch per tick shared across all burst datagrams (fixes 999.4 noecho-epoch regression); `build_state_diff` called exactly once per tick; burst drain calls `encode_datagram` only; `datagram_send_buffer_space()` is the send budget gate
- v1.3 roadmap: `ClientScreen::apply()` monotonic guard changes from `<=` to `<` to allow same-epoch burst datagrams to all apply their runs
- v1.3 roadmap: New quinn streams per channel (not in-stream framing); control stream (id 0) is the first bidi stream; PTY data moves to a second bidi stream; scrollback gets its own bidi stream — all unanimous from three independent researchers
- v1.3 roadmap: `message_discriminant_order_is_stable` test is Phase 21's first commit; new `Message` variants appended after `TerminalControl` (discriminant 10); inserting anywhere else silently corrupts the wire format
- v1.3 roadmap: Channel IDs — client-initiated channels use even IDs, server-initiated use odd IDs (prevents simultaneous-open collision); `ChannelReject` is opaque (no reason-code payload — prevents capability-enumeration oracle)
- v1.3 roadmap: Channels are ephemeral per-connection — on cold reattach, client re-opens channels via control stream after `ResumeComplete`; `SequencedOutputBuffer` never replays `ChannelOpen`/`ChannelAccept` frames
- v1.3 roadmap: Scrollback over reliable QUIC stream only (never datagrams); type-level enforcement: scrollback sender accepts `SendStream` only; `ScrollbackCredit` flow-control message paces delivery
- v1.3 roadmap: Scrollback sender runs as separate tokio task with bounded `mpsc::channel` to avoid stalling the session pump under a slow consumer
- v1.3 roadmap: `scroll_up()` gates on `!alt_screen` before pushing to `TerminalState.scrollback` — alt-screen content must not contaminate primary scrollback
- v1.3 roadmap: `epoch_at_snapshot` field in `ScrollbackPage` wire type is mandatory — client uses it to transition from scrollback-replay to live-grid rendering without duplicated or missing lines
- v1.3 roadmap: `SCROLLBACK_LINE_CAP = 10_000` is not raised; bounded mpsc for scrollback sender; add `tracing::warn!` as per-session scrollback approaches cap
- [Phase ?]: plan 19-01 execution

### Pending Todos

- At Phase 19 start: investigation-first — reproduce the garbled-TUI bug on a Linux client↔server before implementing any fix (PITFALLS A-1 mandate); confirm `grapheme_column_width` pub surface in transitive termwiz version
- At Phase 19 start: decide wire-format strategy for `StateDiff` width field (adding `width: u8` to cell runs is a v1.2-breaking change; treat as milestone-level breaking change or add version field — document decision before coding)
- At Phase 20 start: write `burst_drains_when_grid_differs_from_acked_baseline` as RED-before test before any burst code touches server.rs
- At Phase 21 start: `message_discriminant_order_is_stable` is the first commit; confirm `TerminalControl` is currently discriminant 10 (not 9 — the v1.2 ROADMAP says discriminant 9 in some places and 10 in others; verify in `messages.rs` before writing the test)
- At Phase 21 start: confirm `datagram_send_buffer_space()` is still public on quinn 0.11.9 at implementation time
- At Phase 21 start: decide exact who-reopens-TTY-channel-on-reattach contract (suggested: client sends `ChannelOpen { type: Tty }` after `ReattachOk`; server does not proactively re-open)
- At Phase 22 start: decide keystrokes-while-in-scrollback behaviour: (a) buffer and deliver on return to live, (b) immediately return to live — SUMMARY.md says TBD; decide before client state machine is coded
- At Phase 22 start: decide scrollback resume sequence number on cold reattach — `ScrollbackRequest { resume_seq: u64 }` after `ReattachOk` is the suggested shape
- At Phase 22 start: decide whether per-line column-width metadata in `ScrollbackPage` is mandatory for v1.3 or deferred (PITFALLS S-3); SUMMARY.md flags this as "should-have, not blocking"

### Blockers/Concerns

None at roadmap time. Concerns to watch during execution:

- Phase 19: alt-screen atomicity invariant — do not ship a half-built implementation (Pitfall A-1)
- Phase 20: both 999.4 traps (R-1 infinite-spin, R-2 noecho-epoch) must be designed out before first burst line ships
- Phase 21: discriminant stability test is the gating first commit; any insertion before `TerminalControl` corrupts deployed connections
- Phase 22: scrollback sender must be a separate tokio task (Pitfall M-6); inline implementation in the pump stalls PTY output under a slow consumer

## Deferred Items

Items acknowledged and carried forward:

| Category | Item | Status | Deferred At |
|----------|------|--------|-------------|
| Phase 18 | Security Design Pass (SEC-01 threat-model doc, SEC-02 TOFU prompt) | Deferred | v1.2 user decision |
| Future milestone | SEC-04 client trust-boundary hardening (999.2) | Deferred | v1.2 backlog |
| v1.4+ | Port forwarding (FWD-01), Agent forwarding (FWD-02) | Declared in mux registry, rejected by v1.3 peers | v1.3 scoping |
| v1.4+ | File transfer (XFER-01) | Deferred | v1.3 scoping |
| v1.4+ | Mode 2027 grapheme clustering | Deferred; wcwidth-per-codepoint is safe v1.3 baseline | v1.3 scoping |
| v1.4+ | Scrollback search / copy-mode text selection | Deferred | v1.3 scoping |
| v1.4+ | Scrollback reflow on resize (per-line width metadata) | Should-have; may fold into Phase 22 or defer | v1.3 scoping |
| v2 (M6) | Windows ConPTY / native server (PLAT-01) | Deferred to M6 | Init |
| v2 (M6) | Windows ssh-agent / Pageant signing (PLAT-02) | Deferred post-v1.1 | v1.1 scoping |
| v2 (M7) | WebTransport / NAT topologies | Deferred to M7 | Init |

## Session Continuity

Last session: 2026-06-07T05:57:40.203Z
Stopped at: Phase 19 context gathered
Resume file: .planning/phases/19-full-screen-tui-rendering-correctness/19-CONTEXT.md

## Operator Next Steps

- Start Phase 19: `/gsd:plan-phase 19`
- Investigation-first: reproduce the garbled-TUI bug on Linux before fixing (PITFALLS mandate)
- Decide wire-format strategy for `StateDiff` width field before Phase 19 coding begins
