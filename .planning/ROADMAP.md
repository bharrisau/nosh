# Roadmap: nosh

## Milestones

- ✅ **v1.0 M0–M2 Architecture-Validation Spike** — Phases 1-3 (shipped 2026-05-29)
- ✅ **v1.1 M3 Roaming + Windows Client** — Phases 4-9 (shipped 2026-05-30)
- ✅ **v1.2 M4 Predictive Echo + Daily-Driver Readiness** — Phases 10-18 (shipped 2026-06-07)

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

<details>
<summary>✅ v1.2 M4 Predictive Echo + Daily-Driver Readiness (Phases 10-18) — SHIPPED 2026-06-07</summary>

- [x] Phase 10: PTY Reader Race Fix (completed 2026-06-01)
- [x] Phase 11: Datagram Wire Protocol (completed 2026-06-01)
- [x] Phase 12: Server Terminal State Model (completed 2026-06-01)
- [x] Phase 13: Server Datagram Sender (completed 2026-06-01)
- [x] Phase 14: Client Predictor — Confirmed Rendering (completed 2026-06-01)
- [x] Phase 15: Client Predictor — Speculative Overlay (completed 2026-06-02)
- [x] Phase 16: QoL Feature Pack + Windows CI Gate (completed 2026-06-02)
- [x] Phase 17: Windows-Host Predictive Echo Validation (completed 2026-06-02; Windows-host validated)
- [ ] Phase 18: Security Design Pass — DEFERRED to a future milestone (SEC-01/SEC-02)

Full detail archived at `.planning/milestones/v1.2-ROADMAP.md`. Audit: `.planning/milestones/v1.2-MILESTONE-AUDIT.md` (17/19 reqs; integration 8/8 seams wired; SEC-01/02 deferred with Phase 18). Completed during cycle: backlog 999.1 (server attack-surface fuzz-hardening), 999.3, 999.4.

</details>
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
