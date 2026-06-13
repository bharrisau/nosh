# Roadmap: nosh

## Milestones

- ✅ **v1.0 M0–M2 Architecture-Validation Spike** — Phases 1-3 (shipped 2026-05-29)
- ✅ **v1.1 M3 Roaming + Windows Client** — Phases 4-9 (shipped 2026-05-30)
- ✅ **v1.2 M4 Predictive Echo + Daily-Driver Readiness** — Phases 10-18 (shipped 2026-06-07)
- ✅ **v1.3 M5 Channel Multiplexing, Scrollback Sync & TUI Rendering Correctness** — Phases 19-22 (shipped 2026-06-12)
- 🔲 **v1.4 M7 Remote Access over HTTP/3 + Security Hardening** — Phases 23-28 (in progress)

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

### v1.4 M7 Remote Access over HTTP/3 + Security Hardening (Phases 23-28)

- [x] **Phase 23: Transport Abstraction Seam** — `NoshTransport`/`NoshSendStream`/`NoshRecvStream` traits; Quinn concrete wrappers; session pump made generic; all existing tests green with zero behavioural change (completed 2026-06-13)
- [ ] **Phase 24: WebTransport Endpoint + Mode A** — `wtransport` 0.7.1 integrated; server binds UDP/443 as a WebTransport-over-HTTP/3 listener; client connects; full shell session (datagrams, streams, channels) over the WebTransport pump; downgrade protection
- [ ] **Phase 25: Inner SSH-Key Handshake + TOFU Prompt** — four-step mutual challenge-response (discriminants 18-21) with RFC 9266 tls-exporter channel binding; interactive blocking TOFU fingerprint prompt on first contact; inner auth gated before any `SessionOpen` or `Reattach`
- [ ] **Phase 26: Migration Handover over WebTransport** — client detects WebTransport session loss, reconnects, re-runs inner auth, resumes via 1-RTT cold reattach with byte-exact replay; concurrent same-token reattach resolved atomically
- [ ] **Phase 27: Security Hardening Pass** — OSC OOM adversarial re-verification (999.7) + regression CI gate; SEC-04 client trust-boundary hardening (per-OSC byte gate, clipboard-read rejection, escape stripping, resize rate-limit, recv cap, channel-ID validation); SEC-01 threat-model document (`docs/SECURITY.md`)
- [ ] **Phase 28: Interactive UAT Clearing** — human-driven, one item at a time: carried-forward backlog (Windows alt-screen, 999.3, 999.4, CI gates) then new M7 path end-to-end

## Phase Details

### Phase 23: Transport Abstraction Seam
**Goal**: The session pump is generic over a transport trait so WebTransport and native QUIC share identical session code
**Depends on**: Phase 22 (v1.3 completed)
**Requirements**: WT-01
**Research flag**: Standard patterns — pure refactoring of existing code against a thin trait; no external API research needed
**Success Criteria** (what must be TRUE):
  1. `cargo test --workspace` passes unchanged after the refactor — zero test modifications, zero behavioural changes
  2. `run_session`, `run_reattach_session`, `send_burst`, `run_channel_task`, and `run_scrollback_sender_task` are all generic over `NoshTransport` rather than `quinn::Connection`
  3. The Quinn concrete wrapper (`QuinnConnection` implementing `NoshTransport`) is a pure pass-through with no added logic
  4. `ChannelEvent::Stream` uses boxed trait streams (`Box<dyn NoshSendStream>`, `Box<dyn NoshRecvStream>`) so channel code is also transport-agnostic
**Plans**: 2 plans
- [x] 23-01-PLAN.md — nosh-proto transport traits (NoshTransport/NoshSendStream/NoshRecvStream) + write_message_ns/read_message_ns helpers
- [x] 23-02-PLAN.md — Quinn pass-through wrappers + server session pump made generic over the traits (boxed ChannelEvent::Stream); zero test changes

### Phase 24: WebTransport Endpoint + Mode A
**Goal**: A nosh server can listen on UDP/443 as a WebTransport-over-HTTP/3 endpoint and carry a fully interactive shell session over it
**Depends on**: Phase 23
**Requirements**: WT-02, WT-03, WT-05
**Research flag**: Needs phase research — first `wtransport` integration; verify `max_datagram_payload_size()` API name on `wtransport::Connection` 0.7.1; confirm wtransport issue #311 (time crate build failure 2026-06-12) is resolved; verify `ServerConfigBuilder` pattern with `with_bind_address` + `with_custom_tls`
**Success Criteria** (what must be TRUE):
  1. `nosh-server --mode webtransport` binds UDP/443, accepts a WebTransport-over-HTTP/3 connection from a client started with `--webtransport`, and delivers a live interactive shell — keystrokes, output, resize all working
  2. Datagram state-sync, predictive echo, and reliable control/scrollback channels all function identically over the WebTransport session as they do over native QUIC
  3. A server started with `--mode webtransport` explicitly rejects a raw QUIC (non-WebTransport) connection attempt (downgrade protection)
  4. `wtransport` is added to the workspace with `default-features = false` and the `ring` provider pinned; `cargo tree -f "{p} {f}" | grep rustls` shows only `ring` — no `aws-lc-rs` conflict
  5. Datagram MTU sizing uses the WebTransport session's `wtransport::Connection::max_datagram_size()` (capsule/Quarter-Stream-ID overhead already netted out per D-03), NOT the raw `quic_connection().max_datagram_size()` value — corrected from the originally-drafted `max_datagram_payload_size()`, which does not exist on wtransport 0.7.1
**Plans**: 5 plans
- [ ] 24-01-PLAN.md — Cargo wiring: wtransport 0.7.1 (ring-only) + time pin + webtransport feature on both crates (SC#4)
- [ ] 24-02-PLAN.md — Client session pump made generic over NoshTransport (Phase 23 left the client concrete; prerequisite for WT-03)
- [ ] 24-03-PLAN.md — Server WT wrapper + accept loop + outer TLS + --mode flag + downgrade protection + test-support auth stub (WT-02, WT-05)
- [ ] 24-04-PLAN.md — Client WT wrapper + connect_wt + --webtransport flag feeding the generic pump (WT-03, WT-05)
- [ ] 24-05-PLAN.md — End-to-end integration test: live shell + datagram sync over WebTransport + raw-QUIC downgrade rejection (win condition)
**UI hint**: yes

### Phase 25: Inner SSH-Key Handshake + TOFU Prompt
**Goal**: A WebTransport session performs full mutual SSH-key authentication before any session or reattach frame is processed, with the exchange bound to the outer TLS session and an interactive TOFU prompt on first contact
**Depends on**: Phase 24
**Requirements**: WT-04, SEC-02
**Research flag**: Needs phase research — verify `ConnectionCommon::export_keying_material` accessibility through the `quinn::Connection`/`wtransport` handshake data path before finalising wire format; if unreachable, a CSPRNG-nonce fallback must be documented in SEC-01; inner-auth wire format is a new design that cannot be changed without a protocol version bump
**Success Criteria** (what must be TRUE):
  1. The WebTransport control stream exchanges `InnerAuthChallenge` (discriminant 18), `InnerAuthResponse` (19), `InnerAuthComplete` (20), and `InnerAuthFail` (21) in strict order before any `SessionOpen` or `Reattach` frame is accepted; `message_discriminant_order_is_stable` test is updated in the same commit
  2. The server's challenge includes exported TLS keying material (RFC 9266 `tls-exporter`) or a documented CSPRNG-nonce pair, ensuring a terminating proxy cannot silently replay the exchange
  3. `InnerAuthFail` is fieldless — it reveals neither whether the key exists nor whether the signature was valid
  4. On first contact with an unknown server host key, the client displays a blocking, explicit fingerprint-confirm prompt (SHA-256 hex fingerprint, requires typing `yes`) and produces no PTY output until resolved
  5. A server that passes inner auth with a key not in `authorized_keys` is rejected; a client that receives a server key not in `known_hosts` (and declines the TOFU prompt) disconnects cleanly
**Plans**: TBD

### Phase 26: Migration Handover over WebTransport
**Goal**: A nosh client survives a network change in WebTransport mode by transparently reconnecting, re-authenticating, and resuming the server-side session with byte-exact replay
**Depends on**: Phase 25
**Requirements**: WT-06
**Research flag**: Standard patterns — the 1-RTT cold-reattach protocol is proven from v1.1; this phase wires the existing `run_reattach_session` path into the WebTransport reconnect loop
**Success Criteria** (what must be TRUE):
  1. When the WebTransport session drops (write error or datagram timeout), the client automatically reconnects, re-runs the inner SSH-key handshake, and sends a `Reattach` frame — the server-side session resumes with byte-exact replay from `SequencedOutputBuffer`
  2. Two clients presenting the same reattach token concurrently resolve atomically to exactly one active session (no double-attach); the losing client receives `ReattachErr`
  3. The reattach token is rotated on every successful reattach so an intercepted token cannot be replayed
  4. A simulated network change (client IP swap in a test) causes a seamless session resume with no visible shell disruption beyond a brief reconnecting notice
**Plans**: TBD

### Phase 27: Security Hardening Pass
**Goal**: The server is safe to expose to the internet — OSC OOM bounds are adversarially confirmed, the client is hardened against a malicious server, and the threat model is documented
**Depends on**: Phase 26
**Requirements**: SEC-01, SEC-04, SEC-05
**Research flag**: Standard patterns — OSC OOM re-verification uses existing test infrastructure; SEC-04 items are codebase-grounded; SEC-01 is authored against the deployed topology
**Success Criteria** (what must be TRUE):
  1. `oversized_multi_chunk_osc_is_bounded_then_resyncs` passes; the fuzz target re-runs at `LIBFUZZER_MAX_LEN=2097152` without OOM; the `osc_prefilter` is confirmed to bound all OSC categories nosh handles (not only OSC 0/2/52); a CI gate prevents future regression from changes to `TerminalState::advance`
  2. The client applies a per-OSC byte-count gate before `vte::Parser::advance()`, rejects OSC 52 clipboard-read requests, no-ops `dcs_hook`/PM/APC, strips escape bytes from title re-emission, and validates the clipboard selection field against known values
  3. The client enforces a server-issued resize rate-limit, a `PtyData` receive cap, and channel-ID range validation
  4. `docs/SECURITY.md` exists and covers: assets and trust boundaries, attacker capabilities in internet-exposed deployment, the proxy trust model, the Mode A vs Mode B distinction, the mandatory-inner-auth rationale, and residual risks
**Plans**: TBD

### Phase 28: Interactive UAT Clearing
**Goal**: Every carried-forward validation item and the new M7 remote-access path are confirmed working by a human operator in a live environment, one item at a time
**Depends on**: Phase 27
**Requirements**: UAT-01, UAT-02
**Research flag**: Standard patterns — process-driven; no novel technical unknowns; Mode A test environment (nosh server binding UDP/443 directly) is straightforward
**Success Criteria** (what must be TRUE):
  1. Phase 19 Windows alt-screen visual re-test completes: vim, htop, Claude Code, and a fourth full-screen TUI each render correctly on the Windows client connected to a Linux server, all four scenarios confirmed by the operator
  2. 999.3 client rendering-correctness pack and 999.4 `read -s`/predictive-echo fix on the Windows client are each confirmed working or a tracked gap-closure plan is created before the phase closes
  3. `build-windows` and `cargo audit` CI runs are confirmed green
  4. Mode A WebTransport end-to-end walkthrough completes: client connects, blocking TOFU prompt displays SHA-256 hex fingerprint and requires explicit `yes`, interactive shell works, scrollback and predictive echo function, and a simulated network change triggers a transparent reattach — all confirmed by the operator
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
| 23. Transport Abstraction Seam | 2/2 | Complete    | 2026-06-13 |
| 24. WebTransport Endpoint + Mode A | 0/5 | Planned | - |
| 25. Inner SSH-Key Handshake + TOFU Prompt | 0/? | Not started | - |
| 26. Migration Handover over WebTransport | 0/? | Not started | - |
| 27. Security Hardening Pass | 0/? | Not started | - |
| 28. Interactive UAT Clearing | 0/? | Not started | - |
