# Roadmap: nosh

## Milestones

- â **v1.0 M0âM2 Architecture-Validation Spike** â Phases 1-3 (shipped 2026-05-29)
- â **v1.1 M3 Roaming + Windows Client** â Phases 4-9 (shipped 2026-05-30)
- â **v1.2 M4 Predictive Echo + Daily-Driver Readiness** â Phases 10-18 (shipped 2026-06-07)
- ð **v1.3 M5 Channel Multiplexing, Scrollback Sync & TUI Rendering Correctness** â Phases 19-22 (in progress)

## Phases

<details>
<summary>â v1.0 M0âM2 Architecture-Validation Spike (Phases 1-3) â SHIPPED 2026-05-29</summary>

- [x] Phase 1: QUIC Transport Skeleton (4/4 plans) â completed 2026-05-29
- [x] Phase 2: SSH-Key Mutual Auth (4/4 plans) â completed 2026-05-29
- [x] Phase 3: PTY Session Core (3/3 plans) â completed 2026-05-29

Full detail archived at `.planning/milestones/v1.0-ROADMAP.md`.

</details>

<details>
<summary>â v1.1 M3 Roaming + Windows Client (Phases 4-9) â SHIPPED 2026-05-30</summary>

- [x] Phase 4: Identity Threading â `Session.identity` from the authenticated TLS handshake (completed 2026-05-30)
- [x] Phase 5: Session Persistence â orphaned sessions survive disconnect; per-identity cap + idle timeout (completed 2026-05-30)
- [x] Phase 6: Cold Reattach Protocol â 1-RTT reconnect to an orphaned session, two-factor authorization (completed 2026-05-30)
- [x] Phase 7: Connection Migration Validation â explicit migration config + headless and live roaming coverage (completed 2026-05-30)
- [x] Phase 8: Windows Client â native Windows client â Linux server, on-disk key signing, raw mode, resize, locale (completed 2026-05-30)
- [x] Phase 9: Windows Client Polish & Hardening â VT console-input + `~.` escape, authorized_keys warn+skip, connect timeout, server migration logging (completed 2026-05-30; Windows-host validated)

Full detail archived at `.planning/milestones/v1.1-ROADMAP.md`.

</details>

<details>
<summary>â v1.2 M4 Predictive Echo + Daily-Driver Readiness (Phases 10-18) â SHIPPED 2026-06-07</summary>

- [x] Phase 10: PTY Reader Race Fix (2/2 plans) â completed 2026-06-01
- [x] Phase 11: Datagram Wire Protocol (1/1 plans) â completed 2026-06-01
- [x] Phase 12: Server Terminal State Model (2/2 plans) â completed 2026-06-01
- [x] Phase 13: Server Datagram Sender (3/3 plans) â completed 2026-06-01
- [x] Phase 14: Client Predictor â Confirmed Rendering (3/3 plans) â completed 2026-06-01
- [x] Phase 15: Client Predictor â Speculative Overlay (3/3 plans) â completed 2026-06-02
- [x] Phase 16: QoL Feature Pack + Windows CI Gate (3/3 plans) â completed 2026-06-02
- [x] Phase 17: Windows-Host Predictive Echo Validation (1/1 plans) â completed 2026-06-02
- [ ] Phase 18: Security Design Pass â deferred to a future milestone (SEC-01/SEC-02)

Full detail archived at `.planning/milestones/v1.2-ROADMAP.md`.

</details>

### v1.3 M5 Channel Multiplexing, Scrollback Sync & TUI Rendering Correctness (Phases 19-22)

- [ ] **Phase 19: Full-Screen TUI Rendering Correctness** â Real alternate-screen buffer (two-grid model), wide-char/grapheme audit, predictor suppression, OSC OOM bound; makes vim/htop/Claude Code work correctly
- [ ] **Phase 20: Repaint Pacing** â Burst multiple state-diff datagrams per tick so full-screen repaints land in ~1 RTT; one epoch per tick; both 999.4 traps designed out architecturally
- [ ] **Phase 21: Channel Multiplexing Foundation** â Control-first OPEN/ACCEPT/REJECT on control stream (id 0); discriminant-stability test first; per-channel flow control; clean lifecycle; scrollback channel type declared
- [ ] **Phase 22: Scrollback Sync** â Scrollback delivered over the reliable scrollback channel; credit-based paging; alt-screen gate; Shift-PageUp/PageDown UX; consistent live-grid handoff and reattach survival

## Phase Details

### Phase 19: Full-Screen TUI Rendering Correctness
**Goal**: Full-screen TUI apps (vim, htop, Claude Code) render correctly over nosh â a genuine two-grid alternate-screen model replaces the current no-op flag, wide characters and grapheme clusters are width-accurate, the predictor is suppressed in cursor-addressing mode, and the post-auth OSC OOM vector is bounded
**Depends on**: Nothing (first v1.3 phase; server-side terminal model change with no protocol or client dependencies)
**Requirements**: TUI-01, TUI-02, TUI-03, TUI-04, TUI-05, SEC-03
**Success Criteria** (what must be TRUE):
  1. `vim --noplugin -c q` over a Linux nosh clientâserver opens to a blank canvas (not shell text bleeding through) and leaves the primary buffer and cursor exactly where they were before vim launched â primary content survives `?1049h`/`?1049l` atomically (save+swap+clear on enter, restore+swap on exit)
  2. Resizing the terminal while a full-screen app is open resizes both the active alt grid and the saved primary grid â no stale-sized buffer is restored on `?1049l` and no content is lost from the inactive buffer
  3. CJK wide characters (width 2) advance the cursor by two columns and write a placeholder at `col + 1`; ZWJ sequences and combining marks (width 0) do not advance the cursor â a unit test with `\u{4e2d}` and a ZWJ emoji sequence proves both
  4. Running Claude Code or htop over nosh produces no garbled output and no missing spaces when compared against a reference terminal â verified against a Linux clientâserver (investigation-first: reproduce before fixing)
  5. The speculative predictor is suppressed while the alternate screen is active â no overlay appears inside vim or htop; `predictor.pending` is empty after `?1049h` is processed
  6. A multi-chunk oversized OSC sequence in PTY output (e.g. a 10 MB OSC payload across many `advance()` calls) does not exhaust server memory â OSC accumulation is bounded before vte's internal buffer, and a RED-before/GREEN-after regression test confirms bounded memory while OSC 52 clipboard and title sequences still pass

**Plans**: 5 plans
- [ ] 19-01-PLAN.md — Two-grid alt-screen model: atomic enter/exit, resize both grids, scrollback gate (TUI-01, TUI-02)
- [ ] 19-02-PLAN.md — Wide-char width: Cell.wide marker, width-aware print_char, server/client continuation skip (TUI-03)
- [ ] 19-03-PLAN.md — OSC accumulation pre-bound at 1 MiB + parser resync; RED/GREEN multi-chunk regression (SEC-03)
- [ ] 19-04-PLAN.md — alt_screen on StateDiff + client predictor suppression on alt-screen entry (TUI-05)
- [ ] 19-05-PLAN.md — Synthetic VT grid-assertion suite + manual visual pass + docs/999.7-SECURITY.md (TUI-04, SEC-03)

**Security note**: Pitfalls A-1 through A-6 and SEC-2/SEC-3 from PITFALLS.md govern this phase. Alt-screen must be atomic â a half-built implementation (swap without clear, or clear without restore) is demonstrably worse than the current no-op. SEC-03 shares the `TerminalState::advance` code path and must land here; 999.7 mitigation must be in place or `docs/999.7-SECURITY.md` updated before this phase closes.

---

### Phase 20: Repaint Pacing
**Goal**: Full-screen repaints land in roughly one round-trip instead of dribbling one MTU per 16 ms tick â multiple state-diff datagrams burst within a single tick, the two 999.4 failure modes are designed out architecturally, and the noecho security invariant is proven by a required CI gate
**Depends on**: Phase 19 (alt-screen correct so burst delivers correct content from day one; the most visible payoff of pacing is full-screen TUI startup)
**Requirements**: PACE-01, PACE-02, PACE-03
**Success Criteria** (what must be TRUE):
  1. `vim --noplugin` startup over a simulated 150 ms RTT connection renders the full interface within two round-trips (roughly 300 ms) rather than progressively over many ticks â the full 80Ã24 repaint is delivered in a single tick's burst of datagrams
  2. `noecho_read_dash_s_zero_predicted_chars` passes as a required, non-`#[ignore]` CI gate with burst code active â all burst datagrams within a tick share the same epoch value; `confirmed_epoch` does not advance during a `read -s` window
  3. `burst_drains_when_grid_differs_from_acked_baseline` passes RED-before-fix and GREEN-after â the burst drain loop calls `build_state_diff` exactly once per tick and drains `deferred` via `encode_datagram` only; `last_acked_snapshot` non-advancement during a burst cannot cause infinite spin; `datagram_send_buffer_space()` is the per-tick send budget gate
  4. The `apply()` monotonic guard in `ClientScreen` is changed from `<=` to `<` so same-epoch burst datagrams all apply their runs to the confirmed grid without being discarded after the first

**Plans**: TBD

**Security note**: Pitfalls R-1 and R-2 from PITFALLS.md are mandatory architectural constraints, not implementation options. Both caused the 999.4 revert. `burst_drains_when_grid_differs_from_acked_baseline` must be written as a RED-before test. `noecho_read_dash_s_zero_predicted_chars` must pass in CI before merge.

---

### Phase 21: Channel Multiplexing Foundation
**Goal**: Logical channels are negotiated over a dedicated control stream using OPEN/ACCEPT/REJECT before any data stream is bound â the discriminant-stability enforcement test is the first commit, and the layer is proven with a simple echo channel before scrollback adds complexity
**Depends on**: Phase 19 (discriminant-stability test complements the SEC-03 advance path changes; mux does not depend on repaint pacing â the two can proceed in parallel but sequencing after Phase 20 is cleanest for the verify-before-build pattern)
**Requirements**: MUX-01, MUX-02, MUX-03, MUX-04, MUX-05, MUX-06
**Success Criteria** (what must be TRUE):
  1. `message_discriminant_order_is_stable` test passes â encodes every `Message` variant and asserts its discriminant byte matches a hardcoded expected value; new `ChannelOpen`/`ChannelAccept`/`ChannelReject` variants are appended after `TerminalControl` (discriminant 10+) and added to the test; this test is the first commit of this phase
  2. A `ChannelOpen` on the control stream (stream id 0) is followed by `ChannelAccept` or `ChannelReject` before any data stream is bound; `ChannelReject` carries no reason-code payload (opaque); client-initiated channels use even IDs and server-initiated channels use odd IDs to prevent simultaneous-open collisions
  3. PTY input latency stays below 5 ms while a second channel is saturated at its flow-control window â per-channel application-level credit windows prevent a slow scrollback consumer from stalling the shell
  4. Channel teardown (half-close â full-close) releases all associated resources on both ends; a rejected or closed channel leaks no state; `ChannelAccept`/`ChannelReject` for an unknown or already-closed ID is a logged no-op, not a panic
  5. After a cold reattach, channels are re-established via the control stream (not replayed from the byte-stream buffer) â a reattach test opens a channel, triggers orphan, reattaches, and confirms the channel is re-opened and operational; QUIC migration preserves all open streams transparently at the transport layer with no application-layer work
  6. A concurrent simultaneous-open test fires client-open and server-open at the same time and confirms the session survives with non-colliding IDs

**Plans**: TBD

**Security note**: Pitfalls M-1 through M-6 from PITFALLS.md govern this phase. The discriminant test (M-1) is the first commit. The secondary stream accept loop must not open streams before authentication completes and must not bypass the `AuthLimits` semaphore (pre-auth cap). `SSH_AUTH_SOCK` must never be forwarded via any new channel type. Port/agent forwarding types (PFWD/AFWD) are declared in the registry but must be rejected by v1.3 peers.

---

### Phase 22: Scrollback Sync
**Goal**: Users can view shell history that has scrolled off the visible grid, served from the server's existing scrollback buffer over a dedicated reliable channel, paged on demand, gated to exclude alt-screen content, and surviving both QUIC migration and cold reattach
**Depends on**: Phase 19 (alt-screen suppression gate â `scroll_up()` must check `!alt_screen` before scrollback sync is correct), Phase 21 (scrollback channel rides the mux layer)
**Requirements**: SCROLL-01, SCROLL-02, SCROLL-03, SCROLL-04, SCROLL-05
**Success Criteria** (what must be TRUE):
  1. Pressing Shift-PageUp in the client displays terminal history that has scrolled off the visible grid â the server streams lines from `TerminalState.scrollback` over the dedicated scrollback reliable channel; the client renders them above the current viewport
  2. Scrollback is delivered over a reliable QUIC stream (the scrollback channel), never over datagrams; the scrollback sender runs as a separate tokio task with a bounded `mpsc::channel`; a `ScrollbackCredit` application-level flow-control message paces delivery so a slow client cannot stall the session pump; PTY input responsiveness is unaffected during a scrollback transfer
  3. Alt-screen content (vim, htop output) never appears in the scrollback view â `scroll_up()` is gated on `!alt_screen`; a unit test writes to the primary buffer, activates alt screen, forces scroll lines, deactivates alt screen, and asserts that only the primary lines appear in `TerminalState.scrollback`
  4. Pressing any key while in scrollback view immediately returns the display to the live viewport and delivers the keystroke to the shell; Shift-PageDown pages forward through history; reaching the live view automatically exits scrollback mode
  5. The scrollbackâlive-grid handoff contains no duplicate or missing lines â the `ScrollbackPage` wire type carries an `epoch_at_snapshot` field so the client knows at which epoch to stop replaying history and resume live datagrams; scrollback content is viewable immediately after a cold reattach (the server's `TerminalState.scrollback` survives in the `SessionSlot`)

**Plans**: TBD

**Security note**: Pitfalls S-1 through S-5 and M-6 from PITFALLS.md govern this phase. Scrollback must never travel over datagrams (type-level enforcement: scrollback sender accepts only `SendStream`). The `SCROLLBACK_LINE_CAP = 10_000` constant must not be raised without a measured reason. The scrollback sender task must use a bounded channel and drop oldest lines rather than blocking the pump. On cold reattach, the client re-opens the scrollback channel after `ResumeComplete` â channel state is never replayed from the byte buffer.

---

## Progress Table

| Phase | Plans Complete | Status | Completed |
|-------|----------------|--------|-----------|
| 1. QUIC Transport Skeleton | 4/4 | Shipped | 2026-05-29 |
| 2. SSH-Key Mutual Auth | 4/4 | Shipped | 2026-05-29 |
| 3. PTY Session Core | 3/3 | Shipped | 2026-05-29 |
| 4. Identity Threading | â | Shipped | 2026-05-30 |
| 5. Session Persistence | â | Shipped | 2026-05-30 |
| 6. Cold Reattach Protocol | â | Shipped | 2026-05-30 |
| 7. Connection Migration Validation | â | Shipped | 2026-05-30 |
| 8. Windows Client | â | Shipped | 2026-05-30 |
| 9. Windows Client Polish & Hardening | â | Shipped | 2026-05-30 |
| 10. PTY Reader Race Fix | 2/2 | Shipped | 2026-06-01 |
| 11. Datagram Wire Protocol | 1/1 | Shipped | 2026-06-01 |
| 12. Server Terminal State Model | 2/2 | Shipped | 2026-06-01 |
| 13. Server Datagram Sender | 3/3 | Shipped | 2026-06-01 |
| 14. Client Predictor â Confirmed Rendering | 3/3 | Shipped | 2026-06-01 |
| 15. Client Predictor â Speculative Overlay | 3/3 | Shipped | 2026-06-02 |
| 16. QoL Feature Pack + Windows CI Gate | 3/3 | Shipped | 2026-06-02 |
| 17. Windows-Host Predictive Echo Validation | 1/1 | Shipped | 2026-06-02 |
| 18. Security Design Pass | 0/? | Deferred | - |
| 19. Full-Screen TUI Rendering Correctness | 0/5 | Planned | - |
| 20. Repaint Pacing | 0/? | Not started | - |
| 21. Channel Multiplexing Foundation | 0/? | Not started | - |
| 22. Scrollback Sync | 0/? | Not started | - |
