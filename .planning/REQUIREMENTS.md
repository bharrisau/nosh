# Requirements: nosh — v1.1 M3 Roaming + Windows Client

**Defined:** 2026-05-30
**Core Value:** A single QUIC connection on UDP/443 can carry a live interactive shell, authenticated entirely from the user's existing SSH-key identity — and that session survives network changes without re-authenticating.

## v1.1 Requirements

Requirements for the M3 milestone (roaming + bounded Windows-client slice). Each maps to a roadmap phase (numbering continues from v1.0, which ended at Phase 3). Built on the v1.0 architecture-validation spike (TRANS/AUTH/SESS — all validated).

### Identity & Reattach Security

Threading the authenticated SSH identity into the server-side session is the architectural prerequisite for persistence and reattach — it fills the deliberate v1.0 seam at `Session.identity`.

- [x] **IDENT-01**: The server threads the authenticated peer's SSH identity (SPKI extracted from the TLS handshake) into `Session.identity` on every new connection, before any session message is processed
- [ ] **IDENT-02**: Cold reattach is authorized by two factors — the full SSH/TLS mutual handshake re-runs on every reconnection, AND the presented reattach token must match an orphaned session bound to that same SSH identity (token is a selector, never a sole credential)

### Roaming

- [x] **ROAM-01**: A live session survives a client IP/path change (NAT rebind, interface switch) by continuing the *same* QUIC connection via connection migration, with no re-handshake and no extra round trips — validated headless via a forced path change, with a real Wi-Fi→cellular run as a human live check
- [ ] **ROAM-02**: After a disconnect or resume-from-suspend, the client reconnects to its orphaned session in 1 RTT via a sequence-numbered cold-reattach message; the server replays output the client had not yet acknowledged, with no duplicated or dropped bytes

### Session Persistence

- [ ] **PERSIST-01**: An orphaned session (PTY + shell + terminal state) survives client disconnect Mosh-style — it lives until the shell exits, with the master PTY held open so the shell is not SIGHUP'd; a background reaper prevents zombie shell processes
- [ ] **PERSIST-02**: Orphaned-session lifetime is governed by a configurable idle timeout that defaults to `0` (disabled — Mosh behavior)
- [ ] **PERSIST-03**: A configurable per-identity cap bounds persisted-session memory and is enforced before the first orphaned session is stored

### Windows Client

A bounded Windows *client* slice talking to a Linux server. Native Windows *server* (ConPTY) remains deferred (PLAT-01, M6).

- [ ] **WIN-01**: A native Windows client (no WSL) connects to and authenticates against a Linux nosh server; the client crate cross-compiles cleanly for the Windows target
- [ ] **WIN-02**: The Windows client signs the auth handshake from an on-disk OpenSSH Ed25519 private key (selected via an identity-file flag), without ssh-agent — a temporary, Windows-only, documented exception to the "never handle the private key directly" invariant, with the key held in the narrowest possible scope
- [ ] **WIN-03**: The Windows client provides raw VT input/output mode and propagates terminal-resize events to the server PTY (using Windows console resize events, not SIGWINCH)
- [ ] **WIN-04**: The Windows client propagates `TERM` (defaulting to `xterm-256color`) and locale so the remote shell renders correctly

## v2 Requirements

Deferred to future milestones. Tracked but not in this roadmap. (Full mapping in INIT.md §10 and research/FEATURES.md.)

### Roaming / UX (M4+)

- **ROAM-03**: Connection status / latency / migration-state indicator — deferred; only meaningful alongside M4 predictive-echo latency data

### Predictive Echo (M4)

- **ECHO-01**: Datagram-based terminal state sync with client-side predictive local echo

### Features (M5)

- **FEAT-01**: Control-first channel multiplexing (OPEN/ACCEPT/REJECT on channel 0) with per-channel flow control
- **FEAT-02**: Native scrollback sync
- **FEAT-03**: SSH agent forwarding (dedicated channel, never via env)
- **FEAT-04**: Port forwarding (local + remote)
- **FEAT-05**: OSC 52 clipboard
- **FEAT-06**: Integrated file transfer

### Platform & Topologies (M6–M7)

- **PLAT-01**: Native Windows *server* (ConPTY)
- **PLAT-02**: macOS support
- **WIN-05**: Windows ssh-agent / Pageant (named-pipe) integration — replaces the on-disk-key exception
- **WIN-06**: Interactive passphrase prompt for encrypted on-disk keys (P2; unencrypted keys work in v1.1)
- **TOPO-01**: WebTransport-over-HTTP/3 mode for reverse-proxy deployment (inner SSH-key auth)
- **TOPO-02**: NAT hole-punch / relay with live migration handover
- **TOPO-03**: Happy-eyeballs QUIC-then-TCP transport selection
- **TRUST-01**: Host-key rotation as a signed object (new key signed by old, grace window)

## Out of Scope

Explicitly excluded for v1.1. Documented to prevent scope creep.

| Feature | Reason |
|---------|--------|
| 0-RTT cold reattach | 1-RTT is the locked default; 0-RTT's replay-safety burden isn't worth a gain dwarfed by Wi-Fi/DHCP bring-up. Revisit only if profiling shows reconnect latency matters |
| RFC 7250 RPK upgrade | v1.0 self-signed-cert SPKI-pinning works; RPK not required for v1.1. Track as an upgrade path, defer unless SPKI extraction proves awkward |
| Windows ssh-agent / Pageant | Out of scope for the bounded Windows slice; on-disk key signing covers v1.1 (→ WIN-05) |
| Encrypted-key passphrase prompt | P2; unencrypted keys suffice for the v1.1 Windows slice (→ WIN-06) |
| Named/numbered session selection | M5+; single orphaned-session-per-identity reattach is enough for v1.1 |
| Windows ACL-based key-permission check | `std::fs::Permissions` can't read Windows ACLs; v1.1 emits a best-effort warning and documents the gap |
| Datagram session traffic / predictive echo | M4; v1.1 reattach replay uses the reliable stream, not datagrams |

## Traceability

Which phases cover which requirements. Phase numbering continues from v1.0 (ended at Phase 3).

| Requirement | Phase | Status |
|-------------|-------|--------|
| IDENT-01 | Phase 4 | Complete |
| IDENT-02 | Phase 6 | Pending |
| ROAM-01 | Phase 7 | Complete |
| ROAM-02 | Phase 6 | Pending |
| PERSIST-01 | Phase 5 | Pending |
| PERSIST-02 | Phase 5 | Pending |
| PERSIST-03 | Phase 5 | Pending |
| WIN-01 | Phase 8 | Pending |
| WIN-02 | Phase 8 | Pending |
| WIN-03 | Phase 8 | Pending |
| WIN-04 | Phase 8 | Pending |

**Coverage:**
- v1.1 requirements: 11 total
- Mapped to phases: 11
- Unmapped: 0 ✓

---
*Requirements defined: 2026-05-30*
*Last updated: 2026-05-30 after roadmap creation (phases 4-8 assigned)*
