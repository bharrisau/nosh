# Requirements: nosh

**Defined:** 2026-05-29
**Core Value:** A single QUIC connection on UDP/443 can carry a live interactive shell, authenticated entirely from the user's existing SSH-key identity.

## v1 Requirements

v1 = the M0–M2 architecture-validation spike (Linux-only). Hard dependency chain: Transport → Auth → Session. Each requirement maps to a roadmap phase.

### Transport (M0)

- [ ] **TRANS-01**: Client and server establish a QUIC connection over UDP/443 (quinn + rustls, TLS 1.3, shared ALPN constant)
- [ ] **TRANS-02**: A reliable bidirectional QUIC stream carries application bytes both directions (echo round-trip proves it)
- [ ] **TRANS-03**: Unreliable datagram frames (RFC 9221) send and receive on the same connection, with the datagram receive buffer explicitly enabled
- [ ] **TRANS-04**: Datagrams and streams demonstrably coexist on one connection without interfering (concurrent round-trip test passes)
- [ ] **TRANS-05**: Connection stays alive during interactive idle (keep-alive configured so the default ~30s idle timeout does not drop a quiet shell)

### Authentication (M1)

- [ ] **AUTH-01**: Server authorizes the client's public key against `authorized_keys`; unknown keys are rejected during the TLS handshake (before any session is created)
- [ ] **AUTH-02**: Client verifies the server host key against `known_hosts`, with TOFU on first contact; mismatches abort the connection
- [ ] **AUTH-03**: Auth reuses existing OpenSSH key material (Ed25519 at minimum), using self-signed-cert key-pinning via custom rustls `ServerCertVerifier`/`ClientCertVerifier` (signature verification delegated to the CryptoProvider — never no-op'd)
- [ ] **AUTH-04**: The TLS `CertificateVerify` signature is produced via `ssh-agent` (private key never handled directly), signing the correctly-constructed TLS 1.3 CertificateVerify input
- [ ] **AUTH-05**: The accept loop caps concurrent unauthenticated/half-open connections and enforces an auth-completion timeout (pre-auth DoS hardening)

### Session (M2)

- [ ] **SESS-01**: Server allocates a real PTY (via `portable-pty`) and spawns the user's interactive login shell
- [ ] **SESS-02**: Keystrokes flow client→server to PTY stdin, and shell output flows PTY→server→client, over a reliable stream — interactively usable
- [ ] **SESS-03**: Client puts its local terminal in raw mode and restores it on exit, panic, or abrupt disconnect (RAII guard)
- [ ] **SESS-04**: `TERM` and the initial window size (rows×cols) propagate to the server PTY at session open, so terminfo-aware and fullscreen programs render correctly
- [ ] **SESS-05**: Window resize (SIGWINCH) propagates to the server PTY, debounced/coalesced (~30–50 ms) to avoid resize storms
- [ ] **SESS-06**: Ctrl-C and other signals reach the foreground process group on the server (verified: e.g. `sleep 100` interrupts)
- [ ] **SESS-07**: Client-supplied environment is sanitized at shell open — strip `LD_*`, `DYLD_*`, `BASH_ENV`, `ENV`, `IFS`, `SHELLOPTS`, `PYTHONPATH`, `NODE_OPTIONS`; whitelist `TERM`, `LANG`/`LC_*`, `TZ`; `SSH_AUTH_SOCK` is never forwarded via the environment
- [ ] **SESS-08**: The remote shell's exit code is delivered to the client via an explicit `SessionClose { exit_code, reason }` control frame, and the client process exits with that code
- [ ] **SESS-09**: Connection closes cleanly with a structured reason (shell exited / auth failed / server shutdown) using QUIC application error codes — no hangs or spurious errors
- [ ] **SESS-10**: A server-side session object (`session_id`, SSH identity, PTY handle, shell pid, idle-since) holds session state as a discrete struct, so M3 reattach is additive rather than a refactor (reattach itself NOT implemented)
- [ ] **SESS-11**: Session open/close/resize events are instrumented with `tracing` spans (`session_id`, `peer_addr`, `username`)

## v2 Requirements

Deferred to future milestones. Tracked but not in this roadmap. (Full mapping in INIT.md §10 and research/FEATURES.md §2.)

### Roaming (M3)

- **ROAM-01**: Session survives client IP change (Wi-Fi→cellular) via QUIC connection migration, no re-handshake
- **ROAM-02**: Sequence-numbered 1-RTT cold-reattach resumes an orphaned session after resume-from-suspend
- **ROAM-03**: Connection status / latency / migration-state indicator

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

- **PLAT-01**: Native Windows client + server (ConPTY)
- **PLAT-02**: macOS support
- **TOPO-01**: WebTransport-over-HTTP/3 mode for reverse-proxy deployment (inner SSH-key auth)
- **TOPO-02**: NAT hole-punch / relay with live migration handover
- **TOPO-03**: Happy-eyeballs QUIC-then-TCP transport selection
- **TRUST-01**: Host-key rotation as a signed object (new key signed by old, grace window)

## Out of Scope

Explicitly excluded from the project (not merely deferred). Documented to prevent scope creep.

| Feature | Reason |
|---------|--------|
| Terminal emulator on the client | nosh is a transport; the local terminal (iTerm2, Windows Terminal, etc.) renders. VT state on client only arrives with predictive echo (M4) |
| Cipher/algorithm negotiation | Downgrade-attack surface; TLS 1.3 (rustls) already handles algorithm agility correctly |
| Custom UDP protocol (Mosh's SSP) | QUIC RFC 9221 datagrams give framing for free without losing congestion control, migration, TLS |
| 0-RTT early data | Replayable; saves only 1 RTT on cold reconnect, dwarfed by Wi-Fi/DHCP bring-up. 1-RTT default; revisit only if profiling shows pain |
| SSH CA certificate (`ssh-keygen -s`) → X.509 mapping | Doesn't map cleanly to X.509; non-trivial and unproven. Raw-key trust first |
| Web/browser client | A full product (UI, auth flow); HTTP/3 wire shape leaves the door open but not built now |
| Inbound server port range (Mosh model) | NAT/firewall-hostile — the central complaint about Mosh. Single UDP/443, client connects |

## Traceability

Which phases cover which requirements. Updated during roadmap creation.

| Requirement | Phase | Status |
|-------------|-------|--------|
| TRANS-01 | Phase 1 | Pending |
| TRANS-02 | Phase 1 | Pending |
| TRANS-03 | Phase 1 | Pending |
| TRANS-04 | Phase 1 | Pending |
| TRANS-05 | Phase 1 | Pending |
| AUTH-01 | Phase 2 | Pending |
| AUTH-02 | Phase 2 | Pending |
| AUTH-03 | Phase 2 | Pending |
| AUTH-04 | Phase 2 | Pending |
| AUTH-05 | Phase 2 | Pending |
| SESS-01 | Phase 3 | Pending |
| SESS-02 | Phase 3 | Pending |
| SESS-03 | Phase 3 | Pending |
| SESS-04 | Phase 3 | Pending |
| SESS-05 | Phase 3 | Pending |
| SESS-06 | Phase 3 | Pending |
| SESS-07 | Phase 3 | Pending |
| SESS-08 | Phase 3 | Pending |
| SESS-09 | Phase 3 | Pending |
| SESS-10 | Phase 3 | Pending |
| SESS-11 | Phase 3 | Pending |

**Coverage:**
- v1 requirements: 21 total
- Mapped to phases: 21
- Unmapped: 0 ✓

---
*Requirements defined: 2026-05-29*
*Last updated: 2026-05-29 after roadmap creation — traceability complete*
