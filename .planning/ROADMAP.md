# Roadmap: nosh

## Milestones

- ✅ **v1.0 M0–M2 Architecture-Validation Spike** — Phases 1-3 (shipped 2026-05-29)
- ✅ **v1.1 M3 Roaming + Windows Client** — Phases 4-9 (shipped 2026-05-30)
- 📋 **v1.2 M4 Predictive Echo + Daily-Driver Readiness** — Phases 10-18 (in progress)

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

### v1.2 M4 Predictive Echo + Daily-Driver Readiness (Phases 10-18)

- [ ] **Phase 10: PTY Reader Race Fix** — Replace spawn_blocking PTY read loop with nix::poll self-pipe; bounded blocking-thread count
- [ ] **Phase 11: Datagram Wire Protocol** — StateDiff sparse-diff wire format in nosh-proto; postcard encode/decode; size-cap tests
- [ ] **Phase 12: Server Terminal State Model** — TerminalState vte::Perform impl in nosh-server; unit-tested against known VT sequences
- [ ] **Phase 13: Server Datagram Sender** — Wires TerminalState into run_session select! loop; coalesced diffs over QUIC datagrams; ResumeComplete gate
- [ ] **Phase 14: Client Predictor — Confirmed Rendering** — ClientScreen renders confirmed terminal state from datagrams; ConnectionLossOverlay stub; all display through single render path
- [ ] **Phase 15: Client Predictor — Speculative Overlay** — Full SSP-style prediction engine: epoch tracking, conservative fallback, underline rendering, adaptive RTT mode, wide-char handling
- [ ] **Phase 16: QoL Feature Pack + Windows CI Gate** — Connection-loss banner, OSC 52 clipboard, terminal title, --predict flags; Windows CI job runs + WSAEMSGSIZE suppressed
- [ ] **Phase 17: Windows-Host Predictive Echo Validation** — Predictive echo confirmed on native Windows client; live validation sign-off (run on Windows host)
- [ ] **Phase 18: Security Design Pass** — Threat-model doc + TOFU fingerprint prompt implementation

## Phase Details

### Phase 10: PTY Reader Race Fix
**Goal**: Orphaned sessions cleanly terminate their PTY reader threads — a blocked read() is interruptible, so the server's blocking-thread count stays bounded under repeated session orphan/drop
**Depends on**: Nothing (first phase — no M4 dependencies; all research files flag this as the only safe starting point)
**Requirements**: HARDEN-01
**Success Criteria** (what must be TRUE):
  1. Dropping/orphaning a session reliably stops the PTY reader within one polling interval — no threads accumulate when sessions are created and dropped in a loop
  2. The server's blocking thread count (tokio blocking pool) stays bounded and does not grow after repeated session orphan cycles under load
  3. `cargo test` continues to pass with the new PTY reader implementation; no regressions in existing session tests
**Plans**: TBD

### Phase 11: Datagram Wire Protocol
**Goal**: A sparse, size-bounded terminal-diff wire format exists in nosh-proto — the shared interface that every subsequent server and client component builds on
**Depends on**: Phase 10
**Requirements**: SYNC-01
**Research flag**: Needs per-phase research — sparse-diff encoding strategy for large repaints within QUIC datagram MTU is an open design decision (options: cursor-priority partial update, skip-frame, reliable-stream fallback for full-screen repaints). Must be resolved before implementation begins.
**Success Criteria** (what must be TRUE):
  1. A `StateDiff` type in `nosh-proto/src/datagram.rs` carries changed cells only (sparse), a monotonic `epoch: u64`, terminal dimensions, and cursor position
  2. `encode_datagram` / `decode_datagram` round-trip correctly — a decoded value is identical to the original for all valid inputs
  3. Encoded payload is provably capped below `max_datagram_size() - 100` bytes in the size-cap unit test — a full 80x24 repaint does not exceed the limit
  4. The wire format decision for large repaints (partial update / skip-frame / reliable-stream fallback) is documented in a code comment at the encode callsite
**Plans**: TBD

### Phase 12: Server Terminal State Model
**Goal**: The server maintains an authoritative terminal-state model, fed from the same PTY-output callsite as the SequencedOutputBuffer, unit-tested in isolation before any QUIC plumbing is touched
**Depends on**: Phase 11
**Requirements**: SYNC-02
**Research flag**: Needs per-phase research — verify vte 0.15.0 `Perform` trait `osc_dispatch` parameter signature (`params: &[&[u8]], bell_terminated: bool`) before committing to the API. MEDIUM confidence; verify at docs.rs before implementation.
**Success Criteria** (what must be TRUE):
  1. `TerminalState` implementing `vte::Perform` tracks cell content, cursor position, and echo state; feeding a known VT sequence through it produces the expected cell grid
  2. OSC 52 sequences are detectable at the `osc_dispatch` callsite — the server can identify clipboard-write sequences in PTY output
  3. `push_output_and_parse` on `SessionSlot` feeds both `SequencedOutputBuffer` (unchanged) and `TerminalState` — cold-reattach replay is not affected
  4. Unit tests pass for representative VT sequences: plain text, cursor motion (CSI A/B/C/D), erase-in-display, OSC 0/2 title, OSC 52 clipboard
**Plans**: TBD

### Phase 13: Server Datagram Sender
**Goal**: The server emits coalesced terminal-state diffs over QUIC datagrams from the session pump, gated by a ResumeComplete signal so they never corrupt a partial cold-reattach replay
**Depends on**: Phase 12
**Requirements**: SYNC-03
**Success Criteria** (what must be TRUE):
  1. The `run_session` `select!` loop has a `diff_interval.tick()` arm that encodes one `StateDiff` per ~16 ms tick and calls `conn.send_datagram()` — not one datagram per PTY chunk
  2. An integration test connects a test client and server, types characters, and asserts that `conn.read_datagram()` on the client receives non-empty `StateDiff` frames
  3. Datagrams are suppressed until a `ResumeComplete` signal is sent after cold-reattach replay completes — a reattach session does not send datagrams during the replay window
  4. `run_reattach_session` also has the datagram sender arm with the same `ResumeComplete` gate
**Plans**: TBD

### Phase 14: Client Predictor — Confirmed Rendering
**Goal**: The client renders the confirmed terminal screen from received state-sync datagrams through a single screen-composition path — the datagram display path is proven end-to-end before speculative overlay is added
**Depends on**: Phase 13
**Requirements**: PREDICT-01
**Success Criteria** (what must be TRUE):
  1. The client's `run_pump` loop has a `conn.read_datagram()` arm that routes `StateDiff` frames through `ClientScreen.render_to_stdout()` — no direct `stdout.write_all` for display once datagrams are active
  2. Screen rendered from datagrams matches raw PTY output visually — an end-to-end test confirms the confirmed-state rendering produces the same visible characters as the reliable-stream path
  3. The `SequencedOutputBuffer` `highest_applied` counter continues to advance from `PtyData` on the reliable stream — the cold-reattach `Ack` mechanism is not broken by the new display path
  4. `ConnectionLossOverlay` exists as a stub (no-op) in `ClientScreen` — the render path is wired for it even before it activates
**Plans**: TBD

### Phase 15: Client Predictor — Speculative Overlay
**Goal**: The client speculatively echoes locally-typed input ahead of server confirmation — printable characters, backspace, left/right cursor motion — with conservative fallback and adaptive RTT-based activation, never rendering worse than no prediction
**Depends on**: Phase 14
**Requirements**: PREDICT-02, PREDICT-03, PREDICT-04, PREDICT-05, PREDICT-06
**Research flag**: Needs per-phase research — highest-complexity area of M4. Mosh `terminaloverlay.cc` epoch model, `Validity` enum, `cull()` logic, and `PendingPrediction` lifecycle all need careful translation to Rust before planning. Budget 2-3 planning passes.
**Success Criteria** (what must be TRUE):
  1. Locally-typed printable characters, backspace, and left/right cursor motion appear immediately at the client (speculative) and are confirmed or culled against the server-confirmed screen within the next server-state update
  2. Zero corrupt cells are produced in a vim session (`iHello<Esc>`) — any CSI cursor-move, erase, or alternate-screen sequence resets the prediction epoch and produces no speculative display
  3. Zero predicted characters are displayed during a `read -s` noecho prompt — the engine tracks the server's confirmed echo state and suppresses prediction when the server is not echoing
  4. Unconfirmed predictions are visually distinguished (underline) only above an RTT threshold; `--predict always|adaptive|never` overrides the adaptive default; on a loopback connection with adaptive mode, prediction underlines are invisible
  5. The cursor advances by the correct column count for CJK wide characters (validated with `你好`); ambiguous-width and ZWJ/emoji inputs trigger epoch reset rather than corrupt column tracking
**Plans**: TBD

### Phase 16: QoL Feature Pack + Windows CI Gate
**Goal**: Day-to-day ergonomics land (connection-loss banner, OSC 52 clipboard passthrough, terminal title propagation, predict-mode flag) and the Windows CI gate actually runs on every push
**Depends on**: Phase 14 (QoL features require the confirmed datagram path; Windows CI can be authored from Linux and bundled here)
**Requirements**: QOL-01, QOL-02, QOL-03, QOL-04, HARDEN-02, HARDEN-03
**Success Criteria** (what must be TRUE):
  1. When no datagram is received for >5 s, an unobtrusive overlay appears at row 0 with elapsed "last contact" time and "Press ~. to disconnect" instructions; the overlay clears automatically when traffic resumes
  2. A shell command that writes to the clipboard via OSC 52 causes the corresponding text to appear in the local clipboard on the client machine — write-only (OSC 52 read is never honored)
  3. Terminal-title sequences (OSC 0/2) from the remote shell are not stripped and cause the local terminal tab to reflect the remote context (e.g. `user@host:~`)
  4. A `.github/workflows/ci.yml` `build-windows` job runs on a `windows-latest` runner and builds `nosh-client` for `x86_64-pc-windows-msvc` on every push — CI is not false-green
  5. The `WSAEMSGSIZE` quinn_udp warning is resolved or deliberately suppressed (e.g. `quinn_udp=error` tracing filter on Windows) with the rationale and upstream issue reference recorded in a code comment
**Plans**: TBD

### Phase 17: Windows-Host Predictive Echo Validation
**Goal**: Predictive echo is confirmed working on the native Windows client against a Linux server — live validation on a physical Windows machine, signed off like the v1.1 Windows test
**Depends on**: Phase 15, Phase 16
**Requirements**: PREDICT-07
**Note**: This phase MUST be executed from a physical Windows host (not Linux cross-compile CI). The maintainer runs `/gsd:plan-phase 17` and executes it from Claude on a Windows PC, mirroring the v1.1 Phase 9 process. Human validation sign-off is a required success criterion.
**Success Criteria** (what must be TRUE):
  1. Predictive echo engages on the Windows client when connected to a Linux server over a real (non-loopback) network path — locally-typed characters appear speculatively at sub-RTT latency
  2. Conservative fallback behaves correctly on Windows — vim session produces zero corrupt cells; noecho prompts produce zero predicted characters
  3. Connection migration (network path change) works concurrently with predictive echo active — prediction epoch resets cleanly on migration without screen corruption
  4. A live Windows-host validation document (`docs/windows-echo-test.md`) is signed off by the operator, recording: auth, predicted echo, epoch reset on vim, noecho suppression, and roaming-with-prediction
**Plans**: TBD
**Note: Run on Windows host** — halt Linux execution before this phase; resume from a Windows machine.

### Phase 18: Security Design Pass
**Goal**: The threat model is formally written up as a security design document, and the one implementable gap it names (silent TOFU) is closed
**Depends on**: Phase 16 (security doc formalizes what is already implemented — noecho-suppression and the reattach two-factor must be in place before the doc can describe them accurately)
**Requirements**: SEC-01, SEC-02
**Success Criteria** (what must be TRUE):
  1. A security design document exists (e.g. `docs/security.md`) covering: TOFU first-contact gap (named honestly with mitigation path), privilege model (server runs as authenticated user, no privsep — contrasted with sshd), datagram authentication and replay/staleness analysis (QUIC TLS 1.3 per-packet auth + monotonic epoch), noecho-suppression as a security requirement of prediction, and the reattach two-factor (mint→send→commit token rotation) that any future refactor must preserve
  2. On first contact with an unknown host key, the client prompts the user to confirm the key fingerprint in SSH style (`SHA256:…  Accept? [y/N]`) before pinning it to `known_hosts` — silent TOFU is closed; a test confirms rejection declines the connection
**Plans**: TBD

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
| 10. PTY Reader Race Fix | 0/? | Not started | - |
| 11. Datagram Wire Protocol | 0/? | Not started | - |
| 12. Server Terminal State Model | 0/? | Not started | - |
| 13. Server Datagram Sender | 0/? | Not started | - |
| 14. Client Predictor — Confirmed Rendering | 0/? | Not started | - |
| 15. Client Predictor — Speculative Overlay | 0/? | Not started | - |
| 16. QoL Feature Pack + Windows CI Gate | 0/? | Not started | - |
| 17. Windows-Host Predictive Echo Validation | 0/? | Not started | - |
| 18. Security Design Pass | 0/? | Not started | - |
