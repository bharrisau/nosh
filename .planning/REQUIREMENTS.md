# Requirements: nosh — v1.3 (M5)

**Defined:** 2026-06-07
**Core Value:** A single QUIC connection on UDP/443 can carry a live interactive shell, authenticated entirely from the user's existing SSH-key identity — and that session survives network changes without re-authenticating.

Milestone v1.3 (M5) builds the channel-multiplexing foundation and native scrollback sync, and fixes the rendering/pacing defects that make full-screen TUI apps unusable today. Build order (from research): alt-screen + unicode → repaint pacing → channel mux → scrollback sync.

## v1 Requirements

Requirements for this milestone. Each maps to a roadmap phase.

### Full-Screen TUI Rendering Correctness (TUI)

<!-- was backlog 999.5. Alt-screen is currently a no-op flag; print_char is width-agnostic. -->

- [ ] **TUI-01**: A full-screen app entering the alternate screen (`?1049h`) sees a cleared alternate buffer with the cursor saved; on exit (`?1049l`) the primary buffer and cursor are restored exactly — a genuine two-grid model in `TerminalState`, not a no-op flag (atomic save+swap+clear / restore+swap)
- [ ] **TUI-02**: Both the primary and alternate grids resize correctly on terminal resize (SIGWINCH) with no loss of the inactive buffer
- [ ] **TUI-03**: Wide characters (CJK, width 2) occupy two columns without column drift; zero-width combining marks and multi-codepoint grapheme clusters (ZWJ/emoji + variation selectors) render as one cluster without advancing the cursor incorrectly
- [ ] **TUI-04**: Full-screen TUI applications (vim, htop, Claude Code) render correctly over nosh — no garbling, no missing spaces — verified against a reference terminal on a Linux client↔server
- [ ] **TUI-05**: The predictor suppresses speculative local echo while the alternate screen is active (no overlay inside cursor-addressing apps)

### Repaint Pacing (PACE)

<!-- was backlog 999.6. Reverted once in 999.4 for two documented bugs. -->

- [ ] **PACE-01**: A full-screen repaint (vim startup, multi-line paste) lands in roughly one round-trip instead of dribbling one MTU per 16 ms tick — multiple state-diff datagrams burst per tick, bounded by the congestion-aware send budget (`datagram_send_buffer_space()`)
- [ ] **PACE-02**: Burst pacing preserves the noecho security invariant — all burst datagrams in a tick share one epoch, and the `read -s` noecho-suppression test (`noecho_read_dash_s_zero_predicted_chars`) passes as a required, non-ignored CI gate
- [ ] **PACE-03**: The two 999.4 traps cannot recur — the burst drain never recomputes the diff against a non-advancing acked baseline (no infinite-spin), guarded by a RED-before/GREEN-after burst-drain regression test

### Channel Multiplexing & Flow Control (MUX)

<!-- M5 foundation. New quinn streams per channel (not in-stream framing). -->

- [ ] **MUX-01**: Logical channels are negotiated control-first — a dedicated control channel (id 0) carries OPEN / ACCEPT / REJECT before a channel binds its stream; REJECT is opaque (no reason-code oracle)
- [ ] **MUX-02**: Multiple logical channels run concurrently over the single QUIC connection, each on its own stream, with no head-of-line blocking between channels (PTY input stays responsive while another channel is saturated)
- [ ] **MUX-03**: Reliable channels have per-channel application-level flow control (credit-based windows) so a slow consumer on one channel cannot stall the connection or other channels
- [ ] **MUX-04**: Channel lifecycle is clean — half-close and full-close release resources on both ends; a rejected or closed channel leaks nothing; channel-id parity prevents simultaneous-open collisions
- [ ] **MUX-05**: Channels behave correctly across mobility — they survive QUIC migration transparently, and on cold reattach they are re-established over the control channel after resume (channels are per-connection, not byte-replayed)
- [ ] **MUX-06**: The wire format is stable — the `Message` enum discriminant order is append-only, enforced by a discriminant-stability test that is the first commit of the mux work

### Scrollback Sync (SCROLL)

<!-- First consumer of the mux layer. Reliable channel only, never datagrams. -->

- [ ] **SCROLL-01**: The client can view terminal history that has scrolled off the visible grid — the server retains scrollback and serves requested lines to the client
- [ ] **SCROLL-02**: Scrollback is delivered over a reliable channel (never datagrams), paged with client-driven credit-based flow control — no bulk dump, no PTY stall while history is fetched (scrollback sender runs off the main pump)
- [ ] **SCROLL-03**: Alternate-screen content never enters primary scrollback — history excludes vim/htop/full-screen-app output (gated on `!alt_screen`)
- [ ] **SCROLL-04**: The client enters scrollback view with Shift-PageUp / pages with Shift-PageUp/PageDown; any keystroke snaps back to the live view and is sent to the shell
- [ ] **SCROLL-05**: The scrollback↔live-grid handoff is consistent — no gap or duplicated lines at the transition (an `epoch_at_snapshot` / framing contract), and scrollback remains usable across a cold reattach

### Security (SEC)

<!-- 999.7 folded into M5 — same TerminalState::advance code path as the alt-screen/unicode work. -->

- [ ] **SEC-03**: Oversized OSC sequences in PTY output cannot exhaust server memory — OSC accumulation is bounded before it reaches vte's unbounded buffer, closing the post-authentication OOM vector; a multi-chunk giant-OSC regression test proves bounded memory while legitimate OSC 52 clipboard and title behaviour still pass

## Future Requirements

Deferred to a future milestone. Tracked but not in this roadmap.

### Forwarding & Transfer (rest of M5)

- **FWD-01**: Local/remote TCP port forwarding over multiplexed channels
- **FWD-02**: Agent forwarding over a dedicated channel (never via `SSH_AUTH_SOCK` env)
- **XFER-01**: File transfer over a reliable channel

### Security Pass (deferred Phase 18)

- **SEC-01**: Threat-model / security design document for the design as built
- **SEC-02**: Interactive TOFU fingerprint-confirm prompt on first contact
- **SEC-04**: Client trust-boundary hardening against a malicious/compromised server (was backlog 999.2)

## Out of Scope

Explicitly excluded from v1.3. Documented to prevent scope creep.

| Feature | Reason |
|---------|--------|
| Port/agent forwarding, file transfer | Rest of M5; mux declares these channel types but REJECTs them this milestone. Scrollback is the only mux consumer now |
| Phase 18 security pass (SEC-01/SEC-02) + 999.2 client trust-boundary | Deferred by user decision; kept in parking lot. Only the adjacent 999.7 OOM (SEC-03) folds into M5 |
| Cross-milestone wire compatibility (v1.2 client ↔ v1.3 server) | nosh is built from source; client and server are the same version. Wire-format changes (e.g. cell `width`) are a milestone-level breaking change, not a compat requirement |
| Mode 2027 grapheme clustering | wcwidth-per-codepoint / per-cluster width is the safe v1.3 baseline; Mode 2027 is not yet safe to default on |
| Scrollback search / copy-mode text selection | v1.3 scrollback is view + page only; selection/search is a later UX add |
| Native Windows server (ConPTY) | M6 |
| WebTransport / NAT topologies | M7 |
| 0-RTT cold reattach | Deliberately deferred; 1-RTT ships |

## Traceability

Which phases cover which requirements. Populated during roadmap creation.

| Requirement | Phase | Status |
|-------------|-------|--------|
| TUI-01 | TBD | Pending |
| TUI-02 | TBD | Pending |
| TUI-03 | TBD | Pending |
| TUI-04 | TBD | Pending |
| TUI-05 | TBD | Pending |
| PACE-01 | TBD | Pending |
| PACE-02 | TBD | Pending |
| PACE-03 | TBD | Pending |
| MUX-01 | TBD | Pending |
| MUX-02 | TBD | Pending |
| MUX-03 | TBD | Pending |
| MUX-04 | TBD | Pending |
| MUX-05 | TBD | Pending |
| MUX-06 | TBD | Pending |
| SCROLL-01 | TBD | Pending |
| SCROLL-02 | TBD | Pending |
| SCROLL-03 | TBD | Pending |
| SCROLL-04 | TBD | Pending |
| SCROLL-05 | TBD | Pending |
| SEC-03 | TBD | Pending |

**Coverage:**
- v1.3 requirements: 20 total
- Mapped to phases: 0 (set by roadmapper)
- Unmapped: 20 ⚠️ (resolved at roadmap step)

---
*Requirements defined: 2026-06-07*
*Last updated: 2026-06-07 after initial definition*
