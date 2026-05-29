# Roadmap: nosh

## Overview

Three sequential phases that prove the architecture's three foundational bets on Linux. Each phase is a hard prerequisite for the next: a working QUIC connection (Phase 1) is required before auth can be wired into the handshake (Phase 2), and an authenticated connection (Phase 2) is required before a PTY session can ride it (Phase 3). No parallelism is possible across this chain. Completing all three phases validates that a single QUIC connection on UDP/443 can carry a live interactive shell authenticated entirely from the user's existing SSH-key identity.

## Phases

**Phase Numbering:**
- Integer phases (1, 2, 3): Planned milestone work
- Decimal phases (2.1, 2.2): Urgent insertions (marked with INSERTED)

Decimal phases appear between their surrounding integers in numeric order.

- [ ] **Phase 1: QUIC Transport Skeleton** - quinn endpoints, stream echo, datagram round-trip, ALPN, keep-alive, nosh-proto skeleton
- [ ] **Phase 2: SSH-Key Mutual Auth** - cert-pinning verifiers, ssh-agent signing, known_hosts/authorized_keys, pre-auth DoS cap
- [ ] **Phase 3: PTY Session Core** - PTY spawn, bidirectional I/O, raw mode, resize, signals, env sanitization, exit code, session struct

## Phase Details

### Phase 1: QUIC Transport Skeleton
**Goal**: A quinn endpoint on each side can exchange bytes over a reliable stream and unreliable datagrams on a single UDP/443 connection, with the shared ALPN constant, datagram buffer, and keep-alive correctly configured
**Mode:** mvp
**Depends on**: Nothing (first phase)
**Requirements**: TRANS-01, TRANS-02, TRANS-03, TRANS-04, TRANS-05
**Success Criteria** (what must be TRUE):
  1. Client and server complete a TLS 1.3 handshake on UDP/443; post-handshake ALPN assertion equals the shared `nosh-proto` constant
  2. A byte sequence echoed over a reliable bidirectional stream arrives intact at both endpoints
  3. A datagram sent from client arrives at the server and `connection.max_datagram_size()` returns `Some(_)` on both sides — proving datagrams are explicitly enabled
  4. A concurrent stream echo and datagram round-trip complete without interfering with each other
  5. A connected session left idle for 60 seconds does not drop (keep-alive interval and idle timeout are set to session-appropriate values)
**Plans**: TBD

### Phase 2: SSH-Key Mutual Auth
**Goal**: An unknown client key is rejected inside the TLS handshake before any session code runs; a known client key completes mutual auth with signing routed through ssh-agent; concurrent unauthenticated connections are capped
**Mode:** mvp
**Depends on**: Phase 1
**Requirements**: AUTH-01, AUTH-02, AUTH-03, AUTH-04, AUTH-05
**Success Criteria** (what must be TRUE):
  1. A connection attempt using a client key absent from `authorized_keys` is rejected at the TLS handshake — no session code executes
  2. A connection attempt where the server host key does not match `known_hosts` is aborted by the client; on first contact with an unknown host the fingerprint is written and the connection proceeds (TOFU)
  3. A connection signed by a known Ed25519 key via ssh-agent succeeds end-to-end; a connection presenting the correct pinned key but a forged `CertificateVerify` signature is rejected (signature verification is not stubbed)
  4. The private key is never loaded directly — all `CertificateVerify` signing goes through the ssh-agent socket
  5. Flooding the server accept loop with unauthenticated connection attempts does not exhaust memory; connections that do not complete auth within the timeout are closed
**Plans**: TBD

### Phase 3: PTY Session Core
**Goal**: An authenticated connection spawns a real PTY login shell whose keystrokes, output, resize events, signals, and exit code all flow correctly between client and server, with environment sanitized and session state structured for M3 reattach
**Mode:** mvp
**Depends on**: Phase 2
**Requirements**: SESS-01, SESS-02, SESS-03, SESS-04, SESS-05, SESS-06, SESS-07, SESS-08, SESS-09, SESS-10, SESS-11
**Success Criteria** (what must be TRUE):
  1. An interactive login shell is reachable over the authenticated QUIC connection; fullscreen programs (`vim`, `htop`) render correctly and respond to keyboard input — `TERM` and initial window dimensions are propagated to the server PTY
  2. Dragging the local terminal window propagates resize to the remote PTY within ~50 ms of the burst settling; `SIGWINCH`-aware programs reflow correctly
  3. `Ctrl-C` interrupts a foreground `sleep 100` on the server; `exit 42` in the remote shell causes the local `nosh` client process to exit with code 42 via an explicit `SessionClose` control frame
  4. Killing the `nosh` client with `SIGKILL` mid-session leaves the local terminal fully usable (raw mode RAII guard fires); no zombie PTY child processes remain on the server after disconnect
  5. The shell environment on the server contains `TERM`, `LC_*`, and `TZ` but does not contain `LD_PRELOAD`, `BASH_ENV`, or `SSH_AUTH_SOCK`; a server-side session struct keyed on a UUID holds `session_id`, SSH identity, PTY handle, shell PID, and `idle_since` as a discrete type
**Plans**: TBD

## Progress

**Execution Order:**
Phases execute in numeric order: 1 → 2 → 3

| Phase | Plans Complete | Status | Completed |
|-------|----------------|--------|-----------|
| 1. QUIC Transport Skeleton | 0/TBD | Not started | - |
| 2. SSH-Key Mutual Auth | 0/TBD | Not started | - |
| 3. PTY Session Core | 0/TBD | Not started | - |
