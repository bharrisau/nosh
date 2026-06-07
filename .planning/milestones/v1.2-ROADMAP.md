# Roadmap: nosh

## Milestones

- ✅ **v1.0 M0–M2 Architecture-Validation Spike** — Phases 1-3 (shipped 2026-05-29)
- 📋 **v1.1 M3 Roaming + Windows Client** — Phases 4-9 (shipped 2026-05-30)
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

- [x] **Phase 10: PTY Reader Race Fix** — Replace spawn_blocking PTY read loop with nix::poll self-pipe; bounded blocking-thread count (completed 2026-06-01)
- [x] **Phase 11: Datagram Wire Protocol** — StateDiff sparse-diff wire format in nosh-proto; postcard encode/decode; size-cap tests (completed 2026-06-01)
- [x] **Phase 12: Server Terminal State Model** — TerminalState vte::Perform impl in nosh-server; unit-tested against known VT sequences (completed 2026-06-01)
- [x] **Phase 13: Server Datagram Sender** — Wires TerminalState into run_session select! loop; coalesced diffs over QUIC datagrams; ResumeComplete gate (completed 2026-06-01)
- [x] **Phase 14: Client Predictor — Confirmed Rendering** — ClientScreen renders confirmed terminal state from datagrams; ConnectionLossOverlay stub; all display through single render path (completed 2026-06-01)
- [x] **Phase 15: Client Predictor — Speculative Overlay** — Full SSP-style prediction engine: epoch tracking, conservative fallback, underline rendering, adaptive RTT mode, wide-char handling (completed 2026-06-02)
- [x] **Phase 16: QoL Feature Pack + Windows CI Gate** — Connection-loss banner, OSC 52 clipboard, terminal title, --predict flags; Windows CI job runs + WSAEMSGSIZE suppressed
 (completed 2026-06-02)
- [x] **Phase 17: Windows-Host Predictive Echo Validation** — Predictive echo confirmed on native Windows client; live validation sign-off (run on Windows host)
 (completed 2026-06-02)
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

**Plans**: 2 plans

- [x] 10-01-PLAN.md — Interruptible PTY reader foundation: pty_io trait boundary + Unix self-pipe/nix::poll impl, master_raw_fd accessors, nix poll feature
- [x] 10-02-PLAN.md — Wire interruptible reader into both pumps, await reader exit before orphan (D-03), remove abort() no-op, D-04 completion-barrier test

### Phase 11: Datagram Wire Protocol

**Goal**: A sparse, size-bounded terminal-diff wire format exists in nosh-proto — the shared interface that every subsequent server and client component builds on
**Depends on**: Phase 10
**Research flag**: Needs per-phase research — sparse-diff encoding strategy for large repaints within QUIC datagram MTU is an open design decision (options: cursor-priority partial update, skip-frame, reliable-stream fallback for full-screen repaints). Must be resolved before implementation begins.
**Success Criteria** (what must be TRUE):

  1. A `StateDiff` type in `nosh-proto/src/datagram.rs` carries changed cells only (sparse), a monotonic `epoch: u64`, terminal dimensions, and cursor position
  2. `encode_datagram` / `decode_datagram` round-trip correctly — a decoded value is identical to the original for all valid inputs
  3. Encoded payload is provably capped below `max_datagram_size() - 100` bytes in the size-cap unit test — a full 80x24 repaint does not exceed the limit
  4. The wire format decision for large repaints (partial update / skip-frame / reliable-stream fallback) is documented in a code comment at the encode callsite

**Plans**: 1 plan

- [x] 11-01-PLAN.md — datagram.rs wire format: StateDiff/DiffRun/CellStyle types, total cursor-priority encode_datagram + decode_datagram, provable size cap, round-trip + hardening tests

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

**Plans**: 2 plans

- [x] 12-01-PLAN.md — Build TerminalState (vte::Perform) with grid, cursor, bounded scrollback, echo-state, OSC handling; full isolation unit tests
- [x] 12-02-PLAN.md — Wire push_output_and_parse into SessionSlot + 3 server.rs callsites; resize hook; prove reattach replay byte-identical

### Phase 13: Server Datagram Sender

**Goal**: The server emits coalesced terminal-state diffs over QUIC datagrams from the session pump, gated by a ResumeComplete signal so they never corrupt a partial cold-reattach replay
**Depends on**: Phase 12
**Requirements**: SYNC-03
**Success Criteria** (what must be TRUE):

  1. The `run_session` `select!` loop has a `diff_interval.tick()` arm that encodes one `StateDiff` per ~16 ms tick and calls `conn.send_datagram()` — not one datagram per PTY chunk
  2. An integration test connects a test client and server, types characters, and asserts that `conn.read_datagram()` on the client receives non-empty `StateDiff` frames
  3. Datagrams are suppressed until a `ResumeComplete` signal is sent after cold-reattach replay completes — a reattach session does not send datagrams during the replay window
  4. `run_reattach_session` also has the datagram sender arm with the same `ResumeComplete` gate

**Plans**: 3 plans

- [x] 13-01-PLAN.md — Foundation (Wave 1): nosh-proto epoch-ack wire format (TAG_CLIENT_EPOCH/ClientEpoch/encode/decode) + SessionSlot with_terminal_state delegate
- [x] 13-02-PLAN.md — Server sender (Wave 2): diff_interval + epoch-ack select! arms in run_session and run_reattach_session; acked-epoch diff; ResumeComplete gate; additive PtyData
- [x] 13-03-PLAN.md — Integration test (Wave 3): tests/sync.rs — datagram arrival, full acked-epoch loop, ResumeComplete-gated resume flow

### Phase 14: Client Predictor — Confirmed Rendering

**Goal**: The client renders the confirmed terminal screen from received state-sync datagrams through a single screen-composition path — the datagram display path is proven end-to-end before speculative overlay is added
**Depends on**: Phase 13
**Requirements**: PREDICT-01
**Success Criteria** (what must be TRUE):

  1. The client's `run_pump` loop has a `conn.read_datagram()` arm that routes `StateDiff` frames through `ClientScreen.render_to_stdout()` — no direct `stdout.write_all` for display once datagrams are active
  2. Screen rendered from datagrams matches raw PTY output visually — an end-to-end test confirms the confirmed-state rendering produces the same visible characters as the reliable-stream path
  3. The `SequencedOutputBuffer` `highest_applied` counter continues to advance from `PtyData` on the reliable stream — the cold-reattach `Ack` mechanism is not broken by the new display path
  4. `ConnectionLossOverlay` exists as a stub (no-op) in `ClientScreen` — the render path is wired for it even before it activates

**Plans**: 3 plans

- [x] 14-01-PLAN.md — ClientScreen compositor: local Cell + Overlay/ConnectionLossOverlay stub, monotonic apply (D-14-05), dual-grid resize, minimal-ANSI render_to_stdout (Wave 1)
- [x] 14-02-PLAN.md — Wire into run_pump: conn.read_datagram() arm (apply→render→epoch-ack), PtyData display removal keeping highest_applied, reset on reattach (Wave 2)
- [x] 14-03-PLAN.md — End-to-end tests: grid-comparison vs server TerminalState + live datagram render integration test (Wave 3)

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

**Plans**: 3 plans
Plans:
**Wave 1**

- [x] 15-01-PLAN.md — Predictor engine core: Validity/PendingPrediction state machine, byte classifier, on_input/cull, RTT hysteresis, unicode-width (PREDICT-02/03/04/06)

**Wave 2** *(blocked on Wave 1 completion)*

- [x] 15-02-PLAN.md — Integration: render cursor-override, compositor wiring, --predict flag, run_pump stdin/datagram hooks, Phase-17 latency instrumentation (PREDICT-02/04/05)

**Wave 3** *(blocked on Wave 2 completion)*

- [x] 15-03-PLAN.md — D-15-04 adversarial test suite: vim/CJK/less/paste/Ctrl-C/simulated-loss/Home-End + live read -s noecho security gate (PREDICT-02..06)

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

**Plans**: 3 plans

- [x] 16-01-PLAN.md — Server OSC passthrough: Message::TerminalControl proto variant, osc_dispatch read-gate + bounded caps, vte std re-enable, drain methods, forwarding
- [x] 16-02-PLAN.md — Client integration: OSC 52/0/2 re-emit, ConnectionLossOverlay activation + >5s silence timer, --status RTT title, WSAEMSGSIZE Windows filter
- [x] 16-03-PLAN.md — Windows CI gate: native ci.yml (Linux + windows-latest MSVC), retire windows-cross.yml (HARDEN-02 green-run is human sign-off)

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

**Plans**: 1 plan

Plans:
- [x] 17-01-PLAN.md — Author and operator-sign-off docs/windows-echo-test.md: live Windows-client predictive-echo + roaming validation against a Linux server over a real network
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
| 10. PTY Reader Race Fix | 2/2 | Complete    | 2026-06-01 |
| 11. Datagram Wire Protocol | 1/1 | Complete    | 2026-06-01 |
| 12. Server Terminal State Model | 2/2 | Complete    | 2026-06-01 |
| 13. Server Datagram Sender | 3/3 | Complete    | 2026-06-01 |
| 14. Client Predictor — Confirmed Rendering | 3/3 | Complete    | 2026-06-01 |
| 15. Client Predictor — Speculative Overlay | 3/3 | Complete    | 2026-06-02 |
| 16. QoL Feature Pack + Windows CI Gate | 3/3 | Complete   | 2026-06-02 |
| 17. Windows-Host Predictive Echo Validation | 1/1 | Complete   | 2026-06-02 |
| 18. Security Design Pass | 0/? | Not started | - |

## Backlog

Parking lot for ideas not scheduled into a milestone yet (999.x). Promote via `/gsd:review-backlog`.

### Phase 999.1: Server attack-surface hardening (expose-to-internet readiness)
**Goal**: Be confident the server's UDP/443 QUIC ingress is safe to expose raw to the public internet — via fuzzing and a focused security scan of everything reachable before/at authentication.
**Scope**: `cargo-fuzz`/libFuzzer harnesses on the `nosh-proto` decoders (datagram `StateDiff`/`DiffRun`, reliable-stream `Message` postcard decode, OSC accumulation), plus a QUIC-packet fuzzer against the server socket (malformed/oversized/truncated packets). Audit half-open / unauthenticated connection memory caps, amplification potential, and pre-auth resource exhaustion (DoS hardening — CLAUDE.md invariant). Output: no panics/OOM/unbounded growth on hostile input; documented residual risk.
**Origin**: requested 2026-06-02 during M4.
**Plans:** 5/5 plans complete
Plans:
- [x] 999.1-01-PLAN.md — Scaffold fuzz/ crate (workspace-excluded), declare six [[bin]] targets, prove harness with codec_decode (D-01)
- [x] 999.1-02-PLAN.md — Add cargo-audit peer job to CI; no CI fuzz job (D-03)
- [x] 999.1-03-PLAN.md — read_message / decode_datagram / decode_epoch_ack / osc_accumulation fuzz targets + corpora (D-01)
- [x] 999.1-04-PLAN.md — Raw QUIC-packet fuzzer via quinn-proto Endpoint::handle, migration(true) (D-02)
- [x] 999.1-05-PLAN.md — Residual-risk security doc + deny.toml; resolve amplification A1/A2 from quinn-proto source (D-04)

### Phase 999.2: Client trust-boundary hardening (malicious-server resistance)
**Goal**: Prove a hostile/compromised server cannot extract sensitive local material from the client, cannot escape the terminal, and cannot succeed at MitM.
**Scope**: Adversarial malicious-server test harness driving the real client. Verify: (a) no exfiltration of local secrets — OSC 52 clipboard *read* never honored (already a non-goal; prove it), no env-var/file/`SSH_AUTH_SOCK`/agent leakage; (b) no terminal escape via injected control sequences in datagram/stream payloads; (c) MitM resistance — TOFU/known_hosts pinning + the Phase 18 fingerprint-confirm hold, and a *changed* host key hard-fails. Confirms the Phase 18 TOFU work actually closes the MitM gap end-to-end.
**Origin**: requested 2026-06-02 during M4.

### Phase 999.3: Client terminal-rendering correctness pack (platform-agnostic; fix + test on Linux)
**Goal**: Resolve the terminal-handling defects surfaced during Phase 17 live validation. All items reproduce on a Linux client — fix and test on Linux where the full test suite compiles.
**Scope** (all flagged platform-agnostic):
- **No clear-on-connect / blank cells not painted as spaces** → prior terminal content bleeds through on connect; Ctrl-L erases one line at a time instead of clearing the screen (BUG-H family). Root: `crates/nosh-client/src/screen.rs` full-framebuffer diff skips blank cells + no initial physical clear sent on connect; server ED/clear handling in `crates/nosh-server/src/terminal.rs`.
- **Backspace can move the predicted caret past the prompt start** (BUG-E). Root: `predictor.rs` clamps at col 0 not prompt-start col (`PredictBackspace` / `PredictCursorLeft`).
- **Enter after a `read -s` noecho prompt doesn't advance the line** (BUG-F). Root: post-noecho-epoch render relies on server StateDiff cursor; predicted caret may be stale after the noecho epoch ends.
- **Typematic / fast-typing glitch in vim** — `BulkSuppressed` fires on >4-byte stdin batches in `predictor.rs`; threshold may be too aggressive for fast typists.
- **D-17-02a latency instrumentation measures epoch-confirmation time** (inclusive of think-time), not per-keystroke RTT — too coarse for measured-timing evidence; consider per-keystroke timing hooks.
**Origin**: surfaced during Phase 17 live validation 2026-06-02.
**Plans:** 4/4 plans complete
Plans:
- [x] 999.3-01-PLAN.md — D-01 BUG-E epoch-start clamp + D-05 BUG-F noecho cursor sync (predictor.rs)
- [x] 999.3-02-PLAN.md — D-02 typematic content-inspection batch classification (predictor.rs)
- [x] 999.3-03-PLAN.md — D-03 BUG-H blank-cell painting + emit_connect_clear (screen.rs)
- [x] 999.3-04-PLAN.md — D-04 per-keystroke RTT instrumentation + D-03b connect-clear wiring (main.rs)

### Phase 999.4: Predictive-echo & repaint-pacing live-fix round 2
**Goal**: Resolve the daily-driver UX defects surfaced during the 999.3 live validation round (2026-06-05, Windows client vs Linux server at ~150 ms RTT). Predictor fixes are platform-agnostic (fix + test on Linux); the repaint-pacing change is server-side.
**Scope**:
- **`read -s` newline not predicted** (BUG-F round 2). The Enter that runs a `read -s`/noecho command does not visibly drop the line until the shell reprints (after the *second* Enter). Root: Enter is classified purely as `EpochReset` (`predictor.rs:415`) with NO positive prediction of the line-advance, and during noecho `confirmed_epoch` never advances (structural suppression), so nothing confirms the drop. 999.3 D-05 synced the caret from confirmed but did not predict the newline itself. Fix: positively predict the Enter line-advance (cursor → col 0 of next row, with scroll-at-bottom handling), Mosh-style, only at an echoing prompt; reconcile with the noecho/tentative-epoch machinery so it does not mispredict during the hidden secret entry.
- **Startup typing glitch** — a larger prediction glitch occurs at the very beginning of a session (first keystrokes / first epoch) but not later. Needs reproduction; suspected `awaiting_first_cull` / initial `epoch_start_col` / first-epoch baseline interaction in `predictor.rs`.
- **Slow / progressive full-screen repaint** — vim TUI startup paints top-down and a pasted multi-line block paints bottom-up in visible waves at 150 ms RTT. Root: the server session pump (`nosh-server/src/server.rs:580`) emits at most ONE state-diff datagram per 16 ms tick, MTU-capped (`conn.max_datagram_size()`), deferring overflow cells to later ticks (`server.rs:684-703`); a full-screen repaint is many MTUs so it drips over many ticks × RTT. NOT QUIC flow control (datagrams are not ack-gated; send buffer is 1 MiB). Design decision required: burst multiple datagrams per tick (congestion-bounded) vs raise per-tick byte budget vs reliable-stream fallback for full-screen repaints (the Phase 11 deferred large-repaint strategy). Direction artefact (top-down/bottom-up) is the `build_state_diff` cell-walk + deferral order.
**Origin**: surfaced during 999.3 live validation, 2026-06-05.
**Plans**: 2 plans
Plans:
- [~] 999.4-01-PLAN.md — D-01 burst datagram send: implemented then REVERTED (infinite-spin + noecho-epoch security interaction); DEFERRED to phase 999.6
- [x] 999.4-02-PLAN.md — D-02 PredictEnter line-advance + D-03 on_input decision-trace instrumentation & BulkSuppressed preserves-pending fix (predictor.rs)

### Phase 999.5: Full-screen TUI rendering correctness (alternate-screen buffer + cell width)
**Goal**: Make complex full-screen TUI applications (Claude Code, vim, htop) render correctly over nosh. Investigation-first — reproduce on a Linux client↔server before fixing.
**Symptom**: running the Claude Code TUI over nosh is unusable — "spaces are all missing, the screen is horribly garbled" (reported 2026-06-05, Windows client; expected to reproduce on Linux).
**Suspected root causes** (to confirm by repro, not assume):
- **Alternate screen is a no-op flag, not a real buffer.** `crates/nosh-server/src/terminal.rs:504` handles `?1049h`/`?1049l` as just `self.echo_state.alt_screen = enable` — there is NO separate alternate-screen grid, no save/restore of the primary buffer, no clear-on-enter. Full-screen TUIs assume a fresh, correctly-sized alt buffer with real semantics. This likely needs a genuine alternate-screen buffer in the server terminal model.
- **Cell width / grapheme handling** — box-drawing, wide (CJK/emoji) glyphs, or combining marks may drift columns in the model, consistent with "spaces missing + garbled". Audit the model's width tracking vs a reference terminal.
- NOT the simple blank-cell diff path (that was fixed in 999.3; `compute_diff_runs` emits spaces over changed cells correctly — verified).
**Approach**: live Linux reproduction (run a TUI through a Linux nosh client↔server, diff the server model grid against a reference terminal / capture the datagram run stream) → pin the root cause → fix in `nosh-server/src/terminal.rs` (terminal model) with adversarial tests; client `screen.rs`/`emit_diff` only if the repro implicates it.
**Origin**: surfaced during 999.4 discussion / 999.3 live validation follow-up, 2026-06-05.

### Phase 999.6: Repaint pacing — burst datagrams per tick (epoch-under-burst redesign)
**Goal**: Make full-screen repaints (vim startup, multi-line paste) land in ~1 RTT instead of dribbling one MTU per 16 ms tick — WITHOUT breaking the noecho security invariant. This is D-01 from 999.4, reverted there because the first implementation surfaced two bugs.
**Why it was deferred from 999.4** (lessons — design these in, don't re-discover):
- **Infinite spin**: `build_state_diff` (`nosh-server/src/server.rs`) computes `fresh_runs` against `last_acked_snapshot`, which does NOT advance within a burst (epoch-acks are processed in a different `select!` arm). Re-merging `fresh_runs` every burst iteration refills the deferred queue → it never drains → the pump task spins (hung `mutual_auth_inprocess_happy_path`). Mitigation proven in 999.4: when draining (pending_deferred non-empty) do NOT recompute fresh_runs.
- **Noecho-epoch security interaction**: the burst incremented `current_epoch` once PER datagram and changed delivery timing, so the client's `confirmed_epoch` advanced during a `read -s` window — tripping `noecho_read_dash_s_zero_predicted_chars` (the literal secret chars stayed suppressed; the confirmed-state proxy moved). Likely fix: ONE epoch per tick (all burst datagrams of one screen state share an epoch), restoring the per-tick epoch cadence but delivered faster.
**Mandatory gates**: `crates/nosh-client/tests/predict.rs::noecho_read_dash_s_zero_predicted_chars` and the `auth.rs` integration tests MUST pass; add a burst-drain unit test that fails-before/passes-after against a non-empty grid vs an empty acked baseline (see the reverted 999.4 `burst_drains_when_grid_differs_from_acked_baseline`). `datagram_send_buffer_space()` (public on quinn 0.11.9) is the per-tick budget gate; fixed-count fallback if needed.
**Confirmed mechanism (from 999.4 investigation)**: the server terminal model is already decoupled from the send (PTY output feeds the model on the `out_rx.recv()` arm via `push_output_and_parse`, `server.rs:612`), so the full final state is available at tick time. There is no QUIC flow-control ceiling (datagrams are not ack-gated; send buffer is 1 MiB). The only limiter is the one-datagram-per-tick policy.
**Origin**: deferred from phase 999.4 D-01, 2026-06-05.

### Phase 999.7: Bound OSC accumulation before vte (post-auth OOM / CR-03 done right)
**Goal**: Close the unbounded-OSC-accumulation DoS in the server terminal model. A long OSC sequence in PTY output (e.g. `ESC]52;c;<hundreds of MB>`) can OOM the server because vte (with the default `std` feature) buffers OSC bytes in an unbounded `Vec<u8>` (`vte-0.15.0/src/lib.rs:63-64`) and accumulates them ACROSS `advance()`/read calls until the terminator — before `osc_dispatch` (where our `OSC_52_MAX_BYTES`/`MAX_TITLE_BYTES` caps live) ever runs.
**Why it exists**: surfaced by the 999.1 code review. The Phase-16 CR-03 "re-mitigation" reasoning was WRONG — it assumed vte's buffer was bounded by the OS pipe-buffer size, but the buffer accumulates across reads, not per read (see the corrected comment in `crates/nosh-server/Cargo.toml` and `docs/999.1-SECURITY.md` §7 OSC-OOM). The osc_dispatch caps only bound what is STORED, not what vte ALLOCATES while parsing.
**Scope**:
- Add an OSC-length guard in `TerminalState::advance` (`crates/nosh-server/src/terminal.rs`) that detects an in-progress OSC sequence (ESC] … ST/BEL) across chunks and DROPS bytes once the sequence exceeds a cap (a few × OSC_52_MAX_BYTES) before they reach vte — a small cross-chunk state machine (vte's `std` Vec can't be capped directly; no-std vte's fixed OSC_RAW_BUF_SIZE is too small for legitimate large OSC 52 clipboard, which is why std was chosen).
- Strengthen the `osc_accumulation` fuzz target / add a deterministic regression test that feeds a multi-chunk OSC FAR larger than libFuzzer's default `max_len` (4096) and asserts bounded memory (RED before fix / GREEN after).
**Mandatory gate**: a multi-chunk giant-OSC test proves bounded server memory; existing OSC 52 clipboard + title behaviour (and the Phase-16 caps) must still pass.
**Reachability**: post-authentication (PTY output from a live session) — NOT the pre-auth surface 999.1 cleared; matters for exposed / multi-user servers (terminal model lives in the shared server process).
**Origin**: surfaced during 999.1 code review, 2026-06-07.
