# Requirements: nosh — v1.2 M4 Predictive Echo + Daily-Driver Readiness

**Defined:** 2026-06-01
**Core Value:** A single QUIC connection on UDP/443 can carry a live interactive shell, authenticated entirely from the user's existing SSH-key identity — and that session survives network changes without re-authenticating.

## v1.2 Requirements

Requirements for the M4 milestone. Each maps to a roadmap phase (numbering continues from v1.1, which ended at Phase 9). Built on the v1.0 architecture spike (TRANS/AUTH/SESS) and the v1.1 roaming + Windows-client work (IDENT/ROAM/PERSIST/WIN — all validated).

The build order is dependency-strict (all four research files converge on it): the latent PTY reader race is fixed first; the datagram wire format is locked before any prediction code; the server state model and datagram sender land before the client predictor; QoL and security build on a stable datagram path. See `.planning/research/SUMMARY.md`.

### Daily-Driver Hardening

Clear the carried tech debt so nosh is dependable enough to daily-drive. The PTY reader race fix is a prerequisite for everything else — orphaned sessions under M4 load would otherwise leak blocking threads.

- [x] **HARDEN-01**: Orphaned/cleaned-up sessions cleanly terminate their PTY reader — a blocked `read()` is interruptible (self-pipe / `nix::poll` over `[PTY fd, shutdown pipe]`), so `abort()` actually stops the reader and the server's blocking-thread count stays bounded under repeated session orphan/drop
- [x] **HARDEN-02**: The Windows cross-compile CI gate actually runs — a git remote is configured and a `windows-latest` job builds `nosh-client` for `x86_64-pc-windows-msvc` on every push (no false-green)
- [ ] **HARDEN-03**: The `WSAEMSGSIZE` quinn_udp warning on Windows is resolved or deliberately suppressed (e.g. `quinn_udp=error` tracing filter), with the rationale recorded and the upstream issue referenced

### Datagram State Sync

The loss-tolerant server→client display path that predictive echo is built on. Runs *parallel* to the existing reliable-stream `PtyData` path — the `SequencedOutputBuffer` and cold-reattach replay are preserved unchanged.

- [x] **SYNC-01**: A sparse, size-bounded terminal-diff datagram wire format exists in `nosh-proto` (changed cells only, monotonic `epoch`, dimensions + cursor; payload capped below `max_datagram_size()`), with round-trip and size-cap unit tests; postcard/serde, no new serialization crate
- [x] **SYNC-02**: The server maintains an authoritative terminal-state model (grid + cursor + echo state) fed from the same PTY-output call site as the `SequencedOutputBuffer`, unit-tested against known VT sequences
- [x] **SYNC-03**: The server emits coalesced state diffs over QUIC datagrams (one diff per ~16 ms tick, not per chunk) from the session pump; fresh datagrams are gated by a `ResumeComplete` signal so they never apply to a partial cold-reattach replay

### Predictive Local Echo

The headline differentiator — full Mosh/SSP-style speculative echo. Conservative-by-design: it must never render worse than no prediction.

- [x] **PREDICT-01**: The client renders the confirmed terminal screen from received state-sync datagrams (display routed through a single screen-composition path, never direct `stdout` writes once the predictor exists), matching raw PTY output
- [x] **PREDICT-02**: The client speculatively echoes locally-typed input — printable characters, backspace, and left/right cursor motion — ahead of server confirmation, with per-prediction confirmation tracking against the confirmed screen
- [x] **PREDICT-03**: Prediction is conservative by design — any cursor-addressing / control sequence (CSI cursor move, erase, alternate-screen) or non-printing control key resets the prediction epoch; no prediction is displayed on a fresh row or before the server confirms the first character of an epoch (validated adversarially: a `vim` insert produces zero corrupt cells)
- [x] **PREDICT-04**: Prediction is suppressed during non-echoing input (`stty -echo` / `read -s` password prompts) — the engine tracks the server's confirmed echo state and never speculatively renders an unechoed character (validated with a `read -s` test; this is a security requirement of the feature)
- [x] **PREDICT-05**: Unconfirmed predictions are visually distinguished (underline) only above an RTT threshold; an adaptive default engages prediction on high-latency links and stays invisible on fast ones, with a `--predict always|adaptive|never` override
- [x] **PREDICT-06**: Predicted echo advances the cursor correctly for wide / multi-column characters (CJK), with an explicit conservative policy (epoch reset) for ambiguous-width and ZWJ/emoji input
- [ ] **PREDICT-07**: Predictive echo works on the native Windows client (engine shared with Linux, raw VT rendering), confirmed by a live Windows-host validation run (auth + predicted echo + roaming over a real network change), signed off like the v1.1 Windows test

### Quality of Life

Day-to-day ergonomics for a roaming shell. All three escape-sequence features share the server-side "detect sequence in PTY output" mechanism.

- [ ] **QOL-01**: When the link goes silent (no datagram for >5 s) the client shows an unobtrusive overlay (row 0) with an elapsed "last contact" counter and abort instructions (`Press ~. to disconnect`), which clears automatically when traffic resumes
- [ ] **QOL-02**: Terminal output that sets the clipboard via OSC 52 is forwarded to the client (over the reliable stream, no MTU limit) and applied to the local clipboard — write-only (OSC 52 read is never honored)
- [ ] **QOL-03**: Terminal-title sequences (OSC 0/2) from the remote shell propagate to the local terminal (not stripped), so the local tab reflects the remote context
- [ ] **QOL-04**: The client can surface the measured round-trip time (e.g. in the terminal title via a `--status` option), reusing the SRTT already tracked for adaptive prediction

### Security Design

A thorough threat-model pass over the design as built, plus closing the one gap the review names as implementable now.

- [ ] **SEC-01**: A security design document captures the threat model as built — TOFU first-contact gap (named honestly, with mitigation path), the privilege model (server runs as the authenticated user, no privsep — contrasted with sshd), datagram authentication & replay/staleness analysis (QUIC TLS 1.3 per-packet auth + application-layer monotonic epoch), noecho-suppression as a prediction security requirement, and the reattach two-factor (mint→send→commit token, no oracle) that any M4 refactor must preserve
- [ ] **SEC-02**: On first contact with an unknown host key, the client prompts the user to confirm the key fingerprint (ssh-style `SHA256:…` — accept/reject) before pinning it to `known_hosts`, closing the silent-TOFU gap named in SEC-01

## Future Requirements

Deferred beyond v1.2. Tracked but not in this roadmap.

### Scrollback & Multiplexing (M5)

- **SCROLL-01**: Native scrollback sync (server-side scrollback buffer streamed to client on demand)
- **MUX-01**: Control-first channel multiplexing (OPEN/ACCEPT/REJECT on control channel id 0)
- **MUX-02**: Per-channel flow-control windows
- **FWD-01**: Port forwarding · **FWD-02**: Agent forwarding (dedicated channel, never via env)
- **XFER-01**: File transfer over a dedicated stream

### Platform (M6+)

- **PLAT-01**: Native Windows *server* (ConPTY) · **PLAT-02**: Windows ssh-agent / Pageant signing (replace the on-disk-key client exception)

## Out of Scope

Explicitly excluded for v1.2. Documented to prevent scope creep.

| Feature | Reason |
|---------|--------|
| Predicting control sequences / vim commands | Epoch reset suppresses prediction in cursor-addressing apps by design; predicting CSI sequences produces screen corruption worse than no prediction |
| OSC 52 clipboard *read* (paste remote→local) | Security hole (lets the remote exfiltrate the local clipboard); most terminals disable it |
| tmux/screen integration | Excluded by maintainer decision; conflicts with the native-scrollback story (M5) |
| Full native scrollback sync | M5; OSC 52 passthrough covers the main "copy from the terminal" case for now |
| Named/numbered session listing | M5; v1.1 auto-reattach covers the solo daily-driver case |
| 0-RTT cold reattach | Deliberately deferred per INIT.md; 1-RTT already ships, replay-safety burden not justified |
| `prost`/protobuf for datagrams | `termwiz::Change` is serde-serializable; postcard is smaller and faster |
| Install/packaging & distribution | The daily-driver bar this milestone is stability + UX, not release engineering; cargo-from-source is acceptable |

## Traceability

| Requirement | Phase | Status |
|-------------|-------|--------|
| HARDEN-01 | Phase 10 | Complete |
| HARDEN-02 | Phase 16 | Complete |
| HARDEN-03 | Phase 16 | Pending |
| SYNC-01 | Phase 11 | Complete |
| SYNC-02 | Phase 12 | Complete |
| SYNC-03 | Phase 13 | Complete |
| PREDICT-01 | Phase 14 | Complete |
| PREDICT-02 | Phase 15 | Complete |
| PREDICT-03 | Phase 15 | Complete |
| PREDICT-04 | Phase 15 | Complete |
| PREDICT-05 | Phase 15 | Complete |
| PREDICT-06 | Phase 15 | Complete |
| PREDICT-07 | Phase 17 | Pending |
| QOL-01 | Phase 16 | Pending |
| QOL-02 | Phase 16 | Pending |
| QOL-03 | Phase 16 | Pending |
| QOL-04 | Phase 16 | Pending |
| SEC-01 | Phase 18 | Pending |
| SEC-02 | Phase 18 | Pending |

**Coverage:**
- v1.2 requirements: 19 total
- Mapped to phases: 19
- Unmapped: 0 ✓

---
*Requirements defined: 2026-06-01*
*Last updated: 2026-06-01 — traceability populated after roadmap creation*
