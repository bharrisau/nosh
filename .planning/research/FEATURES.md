# Feature Research

**Domain:** Roaming remote shell — M3 Roaming + Windows Client (nosh v1.1)
**Researched:** 2026-05-30
**Confidence:** HIGH (derived from INIT.md design brief, Mosh/ET public record, QUIC RFC 9000/9221, quinn docs, and v1.0 validated codebase)

---

## Framing

This document is scoped to **v1.1 (M3)**: adding connection migration, server-side session
persistence, 1-RTT cold reattach, Session.identity threading, and a bounded Windows-client slice
to the already-working v1.0 QUIC shell. All v1.0 table-stakes (PTY, auth, I/O, resize, env
sanitization) are shipped and are not re-listed unless they have direct v1.1 interactions.

Features are categorized relative to this milestone, not the full product roadmap.

---

## Feature Landscape

### Table Stakes (Users Expect These)

These are the behaviors that define whether the M3 milestone is complete. Missing any one means the
claimed feature does not work.

| Feature | Why Expected | Complexity | Notes |
|---------|--------------|------------|-------|
| **Connection migration (NAT rebind)** | Any remote shell claiming "roaming" must survive the most common IP change: NAT port reassignment after a brief network blip. This happens invisibly on mobile; users don't know it happened, they just know the shell didn't drop. | LOW | `ServerConfig::migration = true` in quinn is the server-side toggle. QUIC handles NAT rebind automatically: the server accepts packets from the new 4-tuple, probes the path with PATH_CHALLENGE/PATH_RESPONSE, and continues the connection. No application-layer code needed for the NAT-rebind case specifically. |
| **Connection migration (explicit path switch)** | Switching from Wi-Fi to cellular changes the source IP/port. The same QUIC connection must continue — zero interruption, zero reconnection prompt, zero re-auth. | MEDIUM | The OS network change causes packets to arrive from a new source address. With `ServerConfig::migration = true`, quinn's transport layer validates the new path and migrates automatically. The nosh application sees only brief packet-level loss at worst. No explicit "migrate" API call is needed from nosh-client for the common case; the QUIC stack handles it. |
| **Migration is invisible to the user** | Mosh users never see a reconnection dialog during a network change — the terminal just keeps working. Users who switch from Mosh to nosh will expect the same. | LOW | No user-visible UI is required during migration. Existing streams remain open; the connection continues on the new path. An RTT/latency status indicator is a differentiator (see below), not table stakes. |
| **Session.identity threaded from authenticated cert** | Cold reattach authorization is bound to the SSH identity — this is the anti-hijack invariant. Without identity threading, reattach cannot verify the reconnecting client owns the session. | MEDIUM | This is the known-by-design v1.0 seam. The authenticated peer SPKI must be extracted from the rustls handshake context (post-handshake, before session creation) and stored as `Session.identity`. **This is the first implementation task** — every other v1.1 feature except migration depends on it. |
| **Server-side session persistence (Mosh lifetime model)** | Users expect closing the laptop lid does not kill their remote shell. The mosh-server model — server process persists until the shell exits — is the established user expectation for roaming shells. | MEDIUM | Session lives in a `SessionManager` registry on the server. When the QUIC connection drops, the session enters an orphaned state but the PTY+shell process continues running. Session is keyed by SSH identity fingerprint, not QUIC connection ID (the connection is gone). A configurable idle timeout (default 0 = disabled) governs cleanup. A per-identity cap prevents unbounded memory growth. |
| **Configurable idle timeout (default: disabled)** | Server admins need a safety valve; users need to know idle sessions don't vanish by default. | LOW | Mosh's experience confirms: users who return to a session hours/days later and find it dead consider this a regression. The correct default is no idle timeout. A `--session-timeout <seconds>` flag on `nosh-server` serves operator needs without surprising users. |
| **Per-identity session cap** | Without a cap, a client that creates many sessions and disconnects without closing them exhausts server memory indefinitely. | LOW | Configurable max-orphaned-sessions-per-identity (e.g., default 5). When exceeded, the oldest orphaned session is evicted (shell is killed). This is a safety boundary, not a primary UX feature. |
| **Cold reattach (1-RTT)** | After suspend/resume or a prolonged network outage, the user runs nosh again and re-enters their existing session. ET's BackedReader/BackedWriter model set this expectation; users who know ET will expect it. | HIGH | The client sends a `Reattach{token, last_acked_seq, identity_proof}` control message on the new QUIC connection immediately after the TLS handshake completes. The server validates both the token and the authenticated identity. If valid, the session is rebound to the new connection and undelivered output since `last_acked_seq` is replayed. This is 1-RTT (one message round-trip after the handshake). |
| **Reattach authorization bound to SSH identity** | A reattach token alone is not sufficient — a stolen token must not enable session hijack. The SSH identity binding is the anti-hijack invariant, matching the "reuses your SSH keys" design principle. | MEDIUM | The `Reattach` message is only accepted after the new TLS handshake completes and the server verifies the peer SPKI against the stored `Session.identity`. The token is also required as a second factor against coincidence/brute-force. **Both checks must pass.** This is table stakes, not optional hardening. |
| **Reattach token entropy** | A low-entropy token is brute-forceable over the network during the session's lifetime. | LOW | Token is a 32-byte random value generated by a CSPRNG at session creation. `getrandom` or `rand` (likely already in the transitive dep tree via quinn/rustls) is sufficient. No new crate needed. |
| **Output sequence numbering for replay** | Cold reattach requires the server to replay bytes the client missed. Without a per-session monotonic sequence on the output stream, the server cannot know what to replay. | MEDIUM | A lightweight ring-buffer of the last N bytes of server→client output plus a monotonic byte-offset counter must be maintained per session. The client sends the last seq it received; the server re-sends bytes from that offset. This is a new subsystem (no equivalent in v1.0). It is the primary implementation complexity of cold reattach. |
| **Windows client: VT raw mode** | A Windows terminal (Windows Terminal, ConHost) must pass raw VT bytes through without interception. Without this, vim/htop/readline all break and the client is not usable. | MEDIUM | Windows 10+ supports `ENABLE_VIRTUAL_TERMINAL_INPUT` (input handle) and `ENABLE_VIRTUAL_TERMINAL_PROCESSING` (output handle) console modes. The Windows client must set these modes at startup and restore them on exit. `crossterm` provides `enable_raw_mode()` / `disable_raw_mode()` which handle both Unix termios and Windows console mode flags transparently. |
| **Windows client: terminal resize** | The user resizes the Windows Terminal window; the remote PTY must update. Without resize propagation, fullscreen apps misrender. | MEDIUM | On Windows there is no `SIGWINCH`. Resize events arrive as `INPUT_RECORD` structures in the console input buffer (type `WINDOW_BUFFER_SIZE_RECORD`) — there is no VT encoding for them. `crossterm::event::Event::Resize` wraps this cross-platform. The Windows client must poll for this event type and send the same `Resize` message the Linux client sends. |
| **Windows client: on-disk Ed25519 key signing** | The Windows client must authenticate using an existing OpenSSH private key. Windows ssh-agent/Pageant integration is deferred; on-disk key signing is the explicitly bounded temporary path. | MEDIUM | `ssh-key` 0.6.7 parses `~/.ssh/id_ed25519` (OpenSSH private key format). `PrivateKey::sign()` with `ed25519-dalek` produces the same Ed25519 signature the Linux ssh-agent path produces. Key material must be zeroed after use (`zeroize`). Passphrase-encrypted keys require a prompt (see below — P2). |
| **Windows client: TERM and locale propagation** | Without a correct `TERM` sent to the server, terminfo-aware programs emit wrong sequences. | LOW | Same mechanism as Linux: client reads `TERM` env var (default `xterm-256color` if unset on Windows) and includes it in the session-open message. `LC_ALL`, `LANG` follow the same whitelist pattern as Linux. Windows does not have locale env vars by default; the client should send `LANG=en_US.UTF-8` as a safe default if not set. |

### Differentiators (Competitive Advantage)

Features that make nosh meaningfully better than Mosh or ET for this milestone's goals.

| Feature | Value Proposition | Complexity | Notes |
|---------|-------------------|------------|-------|
| **QUIC migration: zero-RTT, zero-code, invisible** | Mosh roams by detecting the new client source address and re-sending to it (within seconds, with a visible stall when the network has already changed). ET roams by dropping the TCP connection and reconnecting (user sees "reconnecting" briefly). QUIC migration is instantaneous at the transport layer — no application round-trip, no reconnect, no re-auth. This is the headline differentiator over both incumbents. | LOW | The feature complexity is genuinely low because `ServerConfig::migration = true` is almost all the nosh code required. The differentiation comes from QUIC's architecture, not from application logic nosh writes. The claim "your shell just keeps working" is accurate and demonstrable. |
| **Reattach authorization via SSH identity (not a session password)** | ET's reattach uses a server-generated session password separate from the user's SSH identity. nosh binds reattach to the same SSH key used for authentication — reattach is as secure as the original login, and the user manages one credential, not two. A stolen reattach token plus a different key is rejected. | HIGH | ET uses a bootstrap-phase password valid for the session duration. nosh requires both the token AND the TLS-handshake identity check. This is the correct design for a tool that advertises "reuses your SSH keys" — the reattach path must be as strong as the initial auth path. |
| **Session persistence without a separate daemon** | Mosh has no reattach at all (orphaned sessions accumulate silently). ET requires `etserver` as a persistent daemon process. nosh's server is a single `systemd`-managed binary that manages all sessions internally via `SessionManager` — no separate daemon, no auxiliary service, simpler deployment. | MEDIUM | `SessionManager` is an in-process registry. Sessions are lightweight (PTY handle + shell PID + output ring-buffer + identity). Multiple concurrent sessions from different identities are all held in one process. |
| **First native Windows client without WSL** | Mosh has no Windows client. ET's Windows support is through WSL or limited native builds. nosh will have a native Windows client (Windows Terminal, crossterm, on-disk key) that works without WSL — opening the tool to a large population of Windows developers who use SSH from Windows Terminal daily. | MEDIUM | quinn and tokio both build for `x86_64-pc-windows-msvc`. `crossterm` is explicitly cross-platform. The nosh-client portability shim (`#[cfg(unix)]` / `#[cfg(windows)]`) isolates platform-specific signing and raw-mode handling. The main challenge is CI/cross-compile setup. |
| **Roaming headless-testable** | Mosh and ET roaming is effectively only human-verified. nosh's architecture (QUIC migration + forced-path-change test) allows a headless CI test of migration using two network interfaces (or `ip addr add` / `ip route` manipulation). This validates the feature deterministically, not just in live demos. | MEDIUM | The forced-path-change test uses the Linux `ip` tool to simulate an address change. A real Wi-Fi→cellular live check remains a human-verified complement. Having both gives confidence. |

### Anti-Features (Commonly Requested, Often Problematic)

| Feature | Why Requested | Why Problematic | Alternative |
|---------|---------------|-----------------|-------------|
| **Session reattach by name/number** | "I want to pick which session to reattach to" — tmux-style named session selection. | Requires a session enumeration protocol (leaks session metadata), a selection UI, and complicates the reattach handshake. At this milestone there is at most one orphaned session per identity (or a small cap). | Automatic latest-session reattach for the authenticated identity. Named sessions are M5+ if needed. |
| **0-RTT reattach** | "Faster resume from suspend" — skip one round-trip vs 1-RTT. | 0-RTT QUIC early data is replayable. The `Reattach` message carries a token and last-acked sequence. Replay against a server that already accepted the message produces a duplicate session bind — undefined behavior. The latency gain is imperceptible: 1 RTT is dwarfed by Wi-Fi/DHCP bring-up (hundreds of ms). | 1-RTT is the correct default. 0-RTT is explicitly deferred and should only be reconsidered with measured latency profiling data. |
| **Windows ssh-agent / Pageant integration (v1.1)** | Windows developers use Pageant or Win32-OpenSSH agent. This is architecturally correct and desirable. | Correct in a later milestone, but adds Windows named-pipe IPC plumbing that is out of scope for the bounded Windows-client slice. On-disk key is the explicitly scoped interim. | On-disk key signing for v1.1 Windows client. Agent integration deferred. |
| **Idle timeout on by default** | "Prevent zombie sessions from accumulating." | Mosh's open issue history shows users who leave sessions overnight and return to find them killed consider this a regression from mosh-server's model. The per-identity cap handles the real concern (memory bounds) without surprising users. | Per-identity session cap (bounds memory). Idle timeout configurable but off by default. |
| **Mosh-style always-visible status bar** | "Show network quality / connection state at the bottom." | A status bar requires terminal real-estate, conflicts with fullscreen apps, and is only meaningful with latency/packet-loss data (requires predictive echo M4 to be useful). | Structured tracing logs per session give operator visibility. A status indicator is deferred to M4. |
| **TCP fallback during migration** | "What if UDP/443 is blocked mid-session?" | For a network that blocks UDP, the session never established in the first place — this is a connect-time concern, not a migration concern. Adding a second transport path mid-session is a different feature from migration. | TCP/WebTransport fallback is an M7 topology concern, not a migration feature. |
| **Mosh-style inbound port range for reattach** | "Server opens a new port for each reattached session." | Requires the client to reach back to a server-chosen ephemeral port — NAT/firewall hostile. This is the central complaint about Mosh's architecture and the exact problem QUIC solves. | Single UDP/443 for all sessions, including reattach. The new QUIC connection uses the same server port. |

---

## Feature Dependencies

```
[v1.0 validated stack: QUIC, auth, PTY, I/O, resize, env sanitization]
    └──prerequisite for──> [all v1.1 features]

[Session.identity threading] ← first task
    └──required by──> [Cold reattach authorization (identity check)]
    └──required by──> [Session persistence keyed by identity fingerprint]
    └──required by──> [Per-identity session cap]

[Server-side session persistence (SessionManager + orphaned state)]
    └──required by──> [Cold reattach (something to reattach to)]
    └──required by──> [Output ring-buffer for replay]

[Output sequence numbering + ring-buffer]
    └──required by──> [Cold reattach output replay]

[Reattach token (CSPRNG, stored in Session)]
    └──required by──> [Cold reattach first factor check]

[QUIC ServerConfig::migration = true]  ← nearly independent
    └──enables──> [NAT rebind survival (automatic)]
    └──enables──> [Explicit path switch (automatic)]
    └──independent of──> [Session persistence]
    └──independent of──> [Cold reattach]

[Windows: crossterm raw mode + resize event polling]
    └──required by──> [Windows client: usable interactive shell]
    └──required by──> [Windows client: terminal resize propagation]

[Windows: on-disk key parsing + signing (ssh-key + ed25519-dalek)]
    └──required by──> [Windows client: TLS handshake authentication]
```

### Dependency Notes

- **Session.identity threading is the critical prerequisite**: it must be the first
  implementation task. Session persistence keying, reattach identity check, and per-identity
  cap all depend on it. It was deliberately left as a v1.0 seam.
- **QUIC migration is nearly independent**: enabling migration requires one server-side config
  flag. No session state is involved. It can be validated as soon as the v1.0 server code is
  compiled with `ServerConfig::migration = true`. It does not depend on identity threading.
- **Session persistence must precede cold reattach**: there is nothing to reattach to until the
  session survives the connection dropping.
- **Output ring-buffer is a new subsystem**: v1.0 delivers bytes on a reliable stream but
  maintains no application-layer sequence number. Adding the ring-buffer and seq counter to the
  session output path is the principal implementation complexity of cold reattach.
- **Windows client is largely independent**: it shares nosh-proto message types and the same
  connection/auth flow, but its platform-specific code (crossterm raw mode, on-disk signing) is
  isolated to nosh-client under `#[cfg]` gates. It can be developed in parallel with session
  persistence work once Session.identity is threaded (because the auth path is shared).
- **Migration and cold reattach are complementary, not redundant**: migration keeps the
  *same* QUIC connection alive through a path change (handled inside quinn). Cold reattach
  creates a *new* QUIC connection to an orphaned session (handled in the nosh application layer).
  These are distinct code paths with distinct UX triggers.

---

## MVP Definition

### v1.1 Launch Requirements

The milestone is done when all of these pass:

- [ ] **Session.identity threaded** — authenticated peer SPKI extracted and stored in `Session.identity` post-handshake; existing live-handshake tests still pass
- [ ] **Connection migration (NAT rebind)** — headless test: forced source address/port change while a stream is active; stream continues without application-level reconnect
- [ ] **Connection migration (path switch)** — headless test via dual-interface or `ip addr` manipulation; human-verified Wi-Fi→cellular live check as complement
- [ ] **Session persistence** — server session survives QUIC connection drop; PTY+shell continues running; session enters orphaned state; configurable idle timeout (default off); per-identity cap enforced
- [ ] **Cold reattach (1-RTT)** — new QUIC connection from same identity sends `Reattach{token, last_acked_seq}`; server validates identity + token; output since `last_acked_seq` replayed; session rebound; headless test passes
- [ ] **Reattach identity binding** — reattach attempt from a different SSH identity (different SPKI) with a valid token is rejected
- [ ] **Windows client: VT raw mode** — `crossterm::terminal::enable_raw_mode()` called on Windows; interactive programs (vim, htop) work correctly against Linux server
- [ ] **Windows client: terminal resize** — Windows Terminal window resize fires `crossterm::event::Event::Resize`; client sends `Resize` to server; `stty size` updates
- [ ] **Windows client: on-disk key signing** — client reads `~/.ssh/id_ed25519`, signs with `ssh-key` + `ed25519-dalek`; authenticates against Linux server `authorized_keys`; TLS handshake passes

### Add After Validation (Post-v1.1)

- [ ] **Windows ssh-agent / Pageant integration** — deferred; requires Windows named-pipe IPC
- [ ] **Passphrase-encrypted on-disk key prompt** — `rpassword` or similar; prompting UX is not in the v1.1 bounded slice, but unencrypted keys work
- [ ] **Connection status indicator** — meaningful only with latency data from M4 (predictive echo)
- [ ] **Per-identity session list** — enumerate and select a specific orphaned session; M5+

### Future Consideration (v2+)

- [ ] **Named/numbered sessions** — tmux-style; M5+
- [ ] **0-RTT reattach** — only with profiling data showing 1-RTT matters
- [ ] **Status bar** — only with M4 predictive echo data

---

## Feature Prioritization Matrix

| Feature | User Value | Implementation Cost | Priority |
|---------|------------|---------------------|----------|
| Session.identity threading | HIGH (enables all reattach + persistence) | LOW (seam fill, not new design) | P1 |
| QUIC connection migration | HIGH (headline differentiator) | LOW (one server config flag) | P1 |
| Server-side session persistence | HIGH (user expectation from Mosh) | MEDIUM (SessionManager, orphan state) | P1 |
| Output ring-buffer + sequence numbering | HIGH (required for cold reattach replay) | MEDIUM (new subsystem) | P1 |
| Cold reattach (1-RTT) | HIGH (ET's key UX contribution) | HIGH (new proto messages + replay logic) | P1 |
| Reattach identity binding | HIGH (security table stakes) | LOW (piggybacks on identity threading) | P1 |
| Reattach token entropy | MEDIUM (security hygiene) | LOW (CSPRNG at session creation) | P1 |
| Per-identity session cap | MEDIUM (memory safety) | LOW (counter in SessionManager) | P1 |
| Windows VT raw mode | HIGH (without it Windows client unusable) | LOW (crossterm + cfg flag) | P1 |
| Windows terminal resize | HIGH (fullscreen apps break without it) | LOW (crossterm event + cfg) | P1 |
| Windows on-disk key signing | HIGH (required for Windows auth) | MEDIUM (ssh-key parse + ed25519 sign) | P1 |
| Windows TERM/locale propagation | MEDIUM (UX polish) | LOW | P1 |
| Configurable idle timeout | MEDIUM (operator safety) | LOW (timer in SessionManager) | P2 |
| Passphrase-encrypted key (Windows) | MEDIUM (most production keys are encrypted) | MEDIUM (interactive prompt) | P2 |
| Connection status indicator | LOW (UX polish; blocked on M4) | HIGH | P3 |

---

## Competitor Feature Analysis

| Feature | Mosh | Eternal Terminal | nosh v1.1 |
|---------|------|-----------------|-----------|
| Roaming / IP-change survival | Yes — detects new source addr, re-sends within seconds; brief stall visible to user | Yes — TCP drop + reconnect; user sees "reconnecting" briefly | Yes — QUIC migration, zero extra round trips, completely invisible to user |
| Session persistence (server survives client disconnect) | Yes — mosh-server runs until shell exits; no idle timeout by default; no reattach possible; orphaned sessions accumulate | Yes — etserver daemon persists; idle timeout configurable | Yes — SessionManager in-process; idle timeout off by default; per-identity cap |
| Cold reattach | No — cannot reattach to a detached mosh session; each `mosh` invocation starts a new session | Yes — BackedReader/BackedWriter byte-offset sequence numbers; client reconnects with same client-id; server replays missing bytes; RETURNING_CLIENT response | Yes — 1-RTT `Reattach{token, last_acked_seq}`; server replays undelivered output; identity check required |
| Reattach authorization | N/A | Session password (separate from SSH key; not bound to user's identity) | SSH identity binding (same key used for initial auth; token + identity check both required) |
| Transport | Custom UDP SSP; server opens port range 60000–61000; NAT/firewall hostile | TCP with reconnect; head-of-line blocking | QUIC/UDP/443; HTTP/3 wire shape; NAT/firewall friendly |
| Windows client | No native client | Limited; primarily WSL | Native Windows client (crossterm + on-disk key); no WSL required |
| Auth model | SSH key at bootstrap only; SSP session key thereafter | SSH key at bootstrap; session key thereafter | SSH key throughout TLS handshake via SPKI pinning; reattach also bound to SSH identity |
| Predictive echo | Yes (excellent, Mosh's killer feature) | No | Deferred to M4 |
| Firewall-friendly port | No (60000–61000 range) | No (TCP/2022 default) | Yes (UDP/443 only) |
| Hardware key (YubiKey) | No | No | Yes via ssh-agent (Linux); on-disk key only (Windows v1.1) |

---

## Sources

- INIT.md §3 (goals), §8 (feature checklist), §10 (milestone path with M3 detail), §12 (quicshell prior art)
- .planning/PROJECT.md (Active requirements, Out of Scope, Key Decisions)
- .planning/milestones/v1.0-research/FEATURES.md (v1.0 feature research; deferred items now in scope)
- .planning/MILESTONES.md (v1.0 shipped state, v1.1 active)
- Mosh session persistence / orphan issues: https://github.com/mobile-shell/mosh/issues/394 and https://github.com/mobile-shell/mosh/issues/806
- Mosh idle timeout configuration (MOSH_SERVER_NETWORK_TMOUT): https://manpages.debian.org/unstable/mosh/mosh-server.1.en.html
- Eternal Terminal protocol (BackedReader/BackedWriter, SequenceHeader, CatchupBuffer, RETURNING_CLIENT): https://github.com/MisterTea/EternalTerminal/blob/master/docs/protocol.md
- ET how it works: https://eternalterminal.dev/howitworks/
- QUIC connection migration transparent to application: https://pulse.internetsociety.org/blog/how-quic-helps-you-seamlessly-connect-to-different-networks
- quic-go migration docs (PATH_CHALLENGE/PATH_RESPONSE, automatic NAT rebind): https://quic-go.net/docs/quic/connection-migration/
- quinn ServerConfig::migration API: https://docs.rs/quinn/latest/quinn/struct.ServerConfig.html
- crossterm raw mode and resize events (Windows INPUT_RECORD / WINDOW_BUFFER_SIZE_RECORD): https://docs.rs/crossterm/latest/crossterm/terminal/index.html
- Windows VT processing console modes (ENABLE_VIRTUAL_TERMINAL_INPUT, ENABLE_VIRTUAL_TERMINAL_PROCESSING): https://github.com/PowerShell/Win32-OpenSSH/issues/1310
- ssh-key 0.6.7 private key parsing and signing: https://docs.rs/ssh-key/latest/ssh_key/

---
*Feature research for: nosh v1.1 — M3 Roaming + Windows Client*
*Researched: 2026-05-30*
