# Requirements: nosh — v1.4 (M7 Remote Access over HTTP/3 + Security Hardening)

**Defined:** 2026-06-13
**Core Value:** A single QUIC connection on UDP/443 can carry a live interactive shell, authenticated entirely from the user's existing SSH-key identity — and that session survives network changes without re-authenticating.

## v1 Requirements

Requirements for milestone v1.4. Each maps to a roadmap phase. v1.0–v1.3 requirements are already validated (see PROJECT.md).

### WebTransport Remote Access (WT)

Direct WebTransport mode (Mode A — nosh binds its own listener on UDP/443) is the committed deliverable. Proxy-fronted mode (Mode B) is an experimental stretch goal (see Future Requirements).

- [x] **WT-01**: A `NoshTransport` / `NoshSendStream` / `NoshRecvStream` abstraction lets the session pump (live session, cold reattach, channels, scrollback) run over either native QUIC or WebTransport with no behavioural change — all existing tests pass unchanged against the Quinn wrapper
- [x] **WT-02**: User can start the server in direct WebTransport mode (Mode A) — it binds its own `wtransport` listener on UDP/443 and accepts WebTransport-over-HTTP/3 connections, with outer TLS configured from the existing nosh cert/key material
- [x] **WT-03**: A client can connect to the WebTransport endpoint and reach a fully interactive shell — datagram state-sync, predictive echo, reliable control/scrollback channels all carried over the WebTransport session
- [ ] **WT-04**: The WebTransport session performs an inner SSH-key mutual handshake (server checked against `authorized_keys`, server host key checked against `known_hosts`) before any session-open or reattach frame is processed; the handshake is bound to the outer TLS session (RFC 9266 `tls-exporter`, or a documented CSPRNG-nonce fallback) so a terminating proxy cannot relay it; inner-auth failure is opaque (no key-existence or signature-validity oracle)
- [x] **WT-05**: Transport selection is explicit (CLI flag); a server started in WebTransport-only mode rejects raw-QUIC connections (downgrade protection)
- [ ] **WT-06**: A client survives a network change in WebTransport mode — it detects session loss, reconnects, re-runs the inner handshake, and resumes the orphaned server-side session via 1-RTT cold reattach with byte-exact replay; concurrent same-token reattach resolves atomically to exactly one active session

### Security Hardening (SEC)

- [ ] **SEC-01**: A threat-model document (`docs/SECURITY.md`) covers the internet-exposed topology — assets, trust boundaries, attacker capabilities, the proxy trust model, the Mode A vs Mode B distinction, the mandatory-inner-auth rationale, and residual risks
- [ ] **SEC-02**: On first contact with an unknown server host key, the client shows an interactive, blocking TOFU fingerprint-confirm dialogue (SHA-256 hex fingerprint, explicit `yes` required, no PTY output until resolved), replacing the current silent-record behaviour
- [ ] **SEC-04**: The client is hardened against a malicious or compromised server — per-OSC byte-count gate before the VT parser, OSC 52 clipboard-read rejection, DCS/PM/APC no-op, title escape-byte stripping, clipboard selection-field validation, server-issued resize rate-limit, `PtyData` receive cap, and channel-ID range validation
- [ ] **SEC-05**: The post-auth OSC-accumulation OOM bound (999.7) is adversarially re-verified and regression-gated — the named bound test re-run, the fuzz target re-run at raised `max_len`, and the prefilter confirmed to bound all OSC categories nosh handles (not just OSC 0/2/52); a CI gate prevents regression from M7 changes to the terminal advance path

### Interactive Validation (UAT)

The milestone is finished with a guided, conversational UAT pass — one item at a time, confirm each before moving on (not a single dumped document).

- [ ] **UAT-01**: A guided interactive walkthrough clears the carried-forward backlog — Phase 19 Windows alt-screen visual re-test (vim/htop/Claude Code, 4 scenarios), 999.3 client rendering-correctness pack, 999.4 `read -s` / predictive-echo fix on the Windows client, and confirmation of green `build-windows` + `cargo audit` CI runs
- [ ] **UAT-02**: A guided interactive walkthrough validates the new M7 remote-access path end-to-end — Mode A connect over WebTransport, blocking TOFU fingerprint dialogue, interactive shell, scrollback, predictive echo, and a simulated network change triggering reattach

## Future Requirements

Acknowledged but deferred — not in the v1.4 roadmap.

### Proxy-Fronted Deployment (stretch)

- **WT-STRETCH-01**: nosh works behind an Envoy proxy configured for extended-CONNECT WebTransport (Mode B), validated in a lab environment. Experimental, best-effort — not a launch blocker. nginx/HAProxy do not proxy WebTransport today; Envoy's support is experimental and not covered by its security team. The roadmapper may add this as a clearly-marked stretch phase only if it does not jeopardise the committed Mode A scope.

### Connection UX (should-have)

- **WT-UX-01**: URL-scheme dispatch (`https://` → WebTransport, raw host → native QUIC) for transparent protocol selection
- **WT-UX-02**: `--trust-key <fingerprint>` and `--strict-host-key-checking` CLI flags for non-interactive/scripting contexts

### Deferred by design

- **0-RTT cold reattach** — measure-first; 1-RTT is dwarfed by Wi-Fi/DHCP bring-up
- **NAT hole-punch / relay with migration handover** — explicitly deferred from this milestone
- **SSH CA certificate (`ssh-keygen -s`) → host verification** — raw-key trust first
- **Windows ConPTY native server (M6)** — separate milestone

## Out of Scope

Explicitly excluded. Documented to prevent scope creep.

| Feature | Reason |
|---------|--------|
| L4 UDP passthrough through the proxy | QUIC routes by connection ID, not 5-tuple; breaks migration and the routing model — WebTransport-with-inner-auth is the correct answer |
| TCP / TLS fallback transport | Cannot carry RFC 9221 datagrams; defeats the predictive-echo latency model |
| Auto-accept TOFU (silent host-key record) | A MITM on first contact goes unnoticed; must be replaced by the interactive fingerprint dialogue (SEC-02) |
| mTLS at the WebTransport outer layer in proxy mode | The proxy terminates TLS; inner SSH-key auth is the correct end-to-end mechanism |
| Browser / web client | HTTP/3 framing leaves the door open later, but not this milestone |
| nginx / HAProxy proxy-fronted mode | Neither proxies WebTransport to a backend today; no upstream delivery timeline |

## Traceability

Which phases cover which requirements. Updated during roadmap creation.

| Requirement | Phase | Status |
|-------------|-------|--------|
| WT-01 | Phase 23 | Complete |
| WT-02 | Phase 24 | Complete |
| WT-03 | Phase 24 | Complete |
| WT-04 | Phase 25 | Pending |
| WT-05 | Phase 24 | Complete |
| WT-06 | Phase 26 | Pending |
| SEC-01 | Phase 27 | Pending |
| SEC-02 | Phase 25 | Pending |
| SEC-04 | Phase 27 | Pending |
| SEC-05 | Phase 27 | Pending |
| UAT-01 | Phase 28 | Pending |
| UAT-02 | Phase 28 | Pending |

**Coverage:**
- v1.4 requirements: 12 total
- Mapped to phases: 12
- Unmapped: 0 ✓

---
*Requirements defined: 2026-06-13*
*Last updated: 2026-06-13 after roadmap creation (Phases 23-28)*
