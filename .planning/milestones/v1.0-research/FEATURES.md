# Feature Research

**Domain:** QUIC-based roaming remote shell (nosh — Mosh/ET successor)
**Researched:** 2026-05-29
**Confidence:** HIGH (spike-stage features derived directly from brief; deferred features from Mosh/ET public record)

---

## Framing

This document is scoped to the **architecture-validation spike (M0–M2)**: prove QUIC+SSH-auth+PTY
coexist and carry a live interactive shell on Linux. It categorizes features into four buckets:

1. **Spike table-stakes** — must exist or the session is not real / demonstrable
2. **Deferred differentiators** — the Mosh/ET-successor payoff, mapped to M3–M7
3. **Anti-features** — explicitly NOT building, with rationale
4. **Cheap now, painful later** — low-cost items worth pulling into the spike to avoid retrofit pain

---

## 1. Spike Table-Stakes

Features without which the interactive session does not work. Missing any one of these means the
spike output is a demo toy, not a usable shell.

| Feature | Why Essential for Spike | Complexity | Notes |
|---------|------------------------|------------|-------|
| **Client-side raw-mode terminal** | Without raw mode the local terminal line-disciplines intercept keystrokes before they reach the transport; interactive programs (vim, htop, readline) break immediately | LOW | `cfmakeraw` / `termios` on connect; must restore on exit (including panic/signal) |
| **Server-side PTY allocation** (`portable-pty`) | The shell needs a controlling terminal; without it `isatty()` returns false, readline disables editing, job-control signals don't work | MEDIUM | `native_pty_system()` → `openpty()` → `spawn_command()`; `PtySize` must be set at spawn |
| **Keystroke delivery client→server** | Bidirectional I/O is the whole point | LOW | Reliable QUIC stream is correct for keystrokes (ordering + no loss) |
| **Shell output delivery server→client** | Same | LOW | Same stream or a dedicated output stream; high-volume so backpressure matters |
| **TERM propagation** | Without `TERM`, terminfo-aware programs (vim, less, man) emit wrong/broken escape sequences; the session looks broken on first `vim` invocation | LOW | Pass `TERM` from client env to server PTY env at session open; part of the whitelist-only env pass-through |
| **Window size initial set** | If PTY is opened with a wrong/default size (e.g. 80×24 when the client is 220×55), every fullscreen program wraps incorrectly immediately | LOW | Read `TIOCGWINSZ` on client before first packet; include in session-open message |
| **Window resize propagation (SIGWINCH)** | Users drag terminal windows; without resize propagation vim/htop misrender after any resize | MEDIUM | Client catches `SIGWINCH`, sends resize message; server calls `resize_pty()` on `MasterPty` |
| **Exit code propagation** | Scripts and CI that call nosh need the remote shell's exit status; without it `$?` is always 0 or meaningless | LOW | `Child::wait()` → `ExitStatus`; encode in a control frame before closing the connection; client process must `std::process::exit(remote_code)` |
| **Clean connection close** | Without an explicit close sequence, the other end either hangs waiting for more data or sees a spurious error | LOW | Orderly QUIC stream FIN + connection close after shell exits |
| **Ctrl-C / signal passthrough** | Ctrl-C in raw mode sends byte 0x03; the PTY line discipline delivers SIGINT to the foreground process group on the server. This is automatic once raw mode + PTY are correct — but must be verified | LOW | No special handling needed beyond raw mode + PTY; verify with `sleep 100` then Ctrl-C |
| **SSH-key mutual auth** (M1 deliverable, prerequisite to M2) | The spike is not demonstrable without auth; anybody can connect without it | HIGH | Self-signed-cert-pinning fallback is acceptable; `authorized_keys` + `known_hosts`/TOFU; signing via `ssh-agent` |
| **QUIC datagram + stream coexistence** (M0) | Core architectural hypothesis; if these interfere the entire design collapses | MEDIUM | `quinn` unreliable datagram frames alongside bidirectional streams on one connection |

### What "spike done" looks like

- `nosh user@host` authenticates, opens a PTY, delivers an interactive `$SHELL`
- Resize client window → remote `stty size` updates
- `vim`, `htop`, `bash` readline all work correctly
- `exit 42` → client process exits with code 42
- Ctrl-C kills foreground process on server
- Connection drops cleanly; no orphan server process (or a deliberate orphan if session-object is implemented — see §4)

---

## 2. Deferred Differentiators

The Mosh/ET-successor features. Not building in M0–M2. Each is mapped to its milestone.

| Feature | Value Proposition | Deferred To | Why Defer |
|---------|------------------|-------------|-----------|
| **Roaming / QUIC connection migration** | Session survives Wi-Fi→LTE IP change with zero interruption — the primary differentiator over ET | M3 | Requires the session layer to be proven first; migration is additive on top of a working connection |
| **Cold-reattach (resume from suspend)** | Lid-close + lid-open reconnects to the running session in ~1 RTT | M3 | Needs sequence-numbered session object; building the object stub is cheap (see §4), but the reattach protocol is not |
| **Predictive local echo** | Keystroke appears locally before server round-trip; eliminates perceived latency on high-RTT links — Mosh's killer feature | M4 | Hardest UX correctness problem; requires terminal state model (VT parser) on client; wrong prediction is worse than no prediction |
| **Native scrollback sync** | Browse terminal history without tmux; fully synced buffer | M5 | Requires reliable stream + buffer protocol; ET demonstrates it's tractable but non-trivial |
| **SSH agent forwarding** | Use local SSH keys from inside the remote session | M5 | Dedicated agent channel; never via env var |
| **Port forwarding (local + remote)** | Tunnel TCP ports through the session | M5 | Reliable stream multiplexing, per-channel flow control (M5 control-first model) |
| **Channel multiplexing (multiple windows)** | Multiple shell windows over one QUIC connection | M5 | Requires control-first channel model (OPEN/ACCEPT/REJECT on channel 0) |
| **OSC 52 clipboard** | Copy-paste between local and remote | M5 | Relatively cheap once reliable stream is in place; defer to avoid scope creep |
| **Integrated file transfer** | `nosh cp` style transfer over reliable stream | M5 | Same stream infrastructure as forwarding |
| **Windows native client + server** (ConPTY) | First-class Windows support without WSL | M6 | `portable-pty` already abstracts this; defer until Linux path is validated |
| **macOS support** | macOS client/server | Post-M6 | Not Linux-only by architecture; defer for scope |
| **WebTransport / reverse-proxy topology** | Run behind nginx without losing auth or migration | M7 | Inner-auth layer over WebTransport; significant protocol work |
| **NAT hole-punch / relay** | Work through symmetric NAT; migrate off relay once direct path opens | M7 | Coordination server + TURN fallback + migration handover |
| **Host-key rotation as a signed object** | New host key signed by old; clients re-pin without scary mismatch prompt | M1+ | Can be added incrementally after basic TOFU works |
| **Connection status / latency indicator** | Show RTT, packet loss, migration state in status line | M3–M4 | Requires roaming and datagram plumbing first |
| **Periodic forward-secrecy rekey** | Bounded per-epoch byte ceiling as cheap insurance | M3+ | Not urgent for a spike; mention in architecture doc |
| **Happy-eyeballs transport selection** | Race QUIC vs TCP fallback; first handshake wins | M7 | QUIC-only this milestone by design |

---

## 3. Anti-Features

Explicitly NOT building. Scope guardrails.

| Anti-Feature | Why It Seems Appealing | Why We Reject It | What We Do Instead |
|--------------|----------------------|-----------------|-------------------|
| **Being a terminal emulator** | "Full terminal support" sounds good | nosh is a transport layer, not an emulator; implementing VT state on the client is only needed for predictive echo (M4) and scrollback (M5) — doing it now bloats scope | Pass bytes through; let the local terminal emulator (iTerm2, Windows Terminal, etc.) do rendering |
| **Cipher/algorithm negotiation** | "Security flexibility" | A negotiation protocol is a downgrade-attack surface; it adds complexity, a protocol version to maintain, and the negotiation itself is a TLS anti-pattern (TLS 1.3 already does this correctly) | TLS 1.3 via rustls; algorithm agility lives in the TLS layer, not our application layer |
| **Web/browser client** | WebTransport is on the roadmap anyway | Browser client is a full product (UI, auth flow, clipboard integration); WebTransport topology (M7) is server-to-server plumbing, not a browser UI | Document that the HTTP/3 wire shape is browser-compatible; build the browser UI only if there's explicit demand |
| **SSH CA certificate mapping** | "Enterprise key management" | SSH CA certs (`ssh-keygen -s`) don't map to X.509; the mapping is non-trivial and unproven in rustls | Raw-key trust (`authorized_keys` / RPK) first; CA mapping is a separate workstream |
| **Custom UDP protocol** (Mosh's SSP) | Direct control of wire format | We get datagram framing from QUIC RFC 9221 for free; re-inventing at the UDP layer means losing QUIC's congestion control, migration, TLS | QUIC unreliable datagrams (RFC 9221) for state sync; no custom UDP |
| **TCP fallback (this milestone)** | "Reliability" for firewalled users | Adds a second transport path to prove and maintain; distracts from the core QUIC hypothesis | QUIC/UDP/443 only for the spike; TCP fallback is a topology concern deferred to M7 |
| **0-RTT early data** | Faster reconnect | Replayable; only saves 1 RTT on cold reconnect, which is dwarfed by Wi-Fi/DHCP bring-up; the anti-replay burden isn't worth it | 1-RTT default; revisit only if profiling demonstrates real latency pain |
| **Interactive multiplexer (tmux/screen built-in)** | "Feature parity with ET+tmux combo" | ET delegates scrollback to tmux control mode for good reason — building a full multiplexer is a separate product | Native scrollback via synced buffer (M5); channel multiplexing (M5); don't build a multiplexer |
| **Inbound server port range** (Mosh model) | Mosh does it this way | Requires the client to reach back to a server-chosen port; hostile to NAT/firewalls; the central complaint about Mosh's architecture | Single UDP/443; server listens, client connects |
| **`SSH_AUTH_SOCK` forwarding via environment** | "Convenient agent forwarding" | Privilege-escalation footgun; env vars are forwarded to child processes and subshells, leaking agent access unintentionally | Dedicated agent forwarding channel (M5); `SSH_AUTH_SOCK` is explicitly stripped from the env whitelist at M2 |

---

## 4. Cheap Now, Painful to Retrofit Later

Low-cost items to include in the spike that become expensive or architecturally invasive to add later.

| Feature | Why Cheap Now | Why Painful Later | Spike Action |
|---------|--------------|------------------|-------------|
| **Environment-variable sanitization** | One pass at shell spawn time; the list is known (from quicshell spec and SSH hardening practice) | Retrofitting means auditing every call site that opens a shell; subtle security regressions possible | Implement at M2 session open: strip `LD_*`, `DYLD_*`, `BASH_ENV`, `ENV`, `IFS`, `SHELLOPTS`, `PYTHONPATH`, `NODE_OPTIONS`; whitelist `TERM`, `LC_*`, `TZ`; never pass `SSH_AUTH_SOCK` |
| **Resize-burst coalescing** | A debounce timer (~30–50 ms) at the point where SIGWINCH is caught; a few lines | Later the resize message path may be entangled with session resumption and migration logic | Implement in the SIGWINCH handler at M2; debounce before emitting a resize message |
| **Server-side session object** | A struct holding `{session_id, ssh_identity, pty_handle, shell_pid, idle_since}` is almost free to define now | Cold-reattach (M3) requires this object to survive across connection drop; if the session lifecycle is not structured as an object from the start, M3 requires a refactor of the connection handler | Define the session struct and give each session a UUID at M2; don't implement reattach yet, but don't inline everything into the connection handler either |
| **Exit-code forwarding** | `Child::wait()` already returns `ExitStatus`; encoding it in a close frame is trivial | If the close protocol is not defined now, any future feature that needs to know why the session ended (logging, automation, scripting) requires a protocol extension | Include an explicit `SessionClose { exit_code: u32, reason: CloseReason }` control frame in the protocol from M0/M2 |
| **Locale / `LC_*` pass-through** | Part of the env whitelist; negligible extra work | Without locale, non-ASCII input/output may silently corrupt; some tools fail in C locale; users report broken shells immediately | Add `LC_*` and `LANG` to the whitelist alongside `TERM` and `TZ` |
| **Structured connection-close (not just TCP-hang-up)** | Define an explicit close message in the control framing now | If the connection close is just "stream EOF", distinguishing clean exit from network drop in later features (roaming, logging, scripting) is ambiguous | Protocol-level: close message with reason code (shell exited, auth failed, server shutdown); use QUIC connection close frame with application error codes |
| **Per-session logging skeleton** | A `tracing` span per session with `session_id`, `peer_addr`, `username` fields costs nothing | Debugging production session issues (why did this session drop? what was the exit code?) is very painful without structured logs | Instrument session open/close/resize events with `tracing`; no performance cost in release builds |

---

## Feature Dependencies

```
[QUIC datagram + stream coexistence] (M0)
    └──required by──> [SSH-key mutual auth] (M1)
                          └──required by──> [PTY + interactive shell] (M2)
                                                └──required by──> [Roaming / migration] (M3)
                                                                      └──required by──> [Cold reattach] (M3)
                                                └──required by──> [Predictive echo] (M4)
                                                └──required by──> [Scrollback / forwarding] (M5)

[Raw-mode client terminal]
    └──required by──> [Keystroke delivery]
    └──required by──> [Window resize propagation]

[Server-side session object] (cheap-now)
    └──required by──> [Cold reattach] (M3)
    └──required by──> [Session multiplexing] (M5)

[Env sanitization] (cheap-now)
    └──independent but must precede──> [Shell spawn] (M2)

[Exit-code forwarding] (cheap-now)
    └──enables──> [Scripting / CI use of nosh]
```

### Dependency Notes

- **PTY requires QUIC connection**: obvious, but worth stating — M0 (transport) must pass before M1 (auth), which must pass before M2 (session).
- **Predictive echo requires VT state on client**: the client must parse terminal escape sequences to maintain a local state model; this is non-trivial and is the reason M4 is a dedicated milestone.
- **Session object is not a hard dependency for the spike**, but its absence makes M3 a refactor rather than an additive feature — worth the trivial upfront cost.
- **Agent forwarding must not use `SSH_AUTH_SOCK`**: the env sanitization and the agent-forwarding channel (M5) are complementary, not alternatives; both are correct.

---

## MVP Definition (Spike = M0–M2)

### The spike is DONE when:

- [ ] QUIC client and server exchange datagram frames and bidirectional stream data on UDP/443 (M0)
- [ ] Datagrams and streams demonstrably coexist without interference (M0)
- [ ] Client authenticates to server using Ed25519 SSH key against `authorized_keys`; server host key checked against `known_hosts`/TOFU (M1)
- [ ] Signing routed through `ssh-agent`; private key never handled directly (M1)
- [ ] Server spawns a real PTY and login shell (M2)
- [ ] Keystrokes flow client→server; shell output flows server→client; the session is interactively usable (M2)
- [ ] `vim`, `htop`, `bash` readline work correctly (raw mode + TERM + correct initial PTY size) (M2)
- [ ] Window resize propagates to server PTY with burst coalescing (M2)
- [ ] Environment sanitized on shell open (LD_*, DYLD_*, BASH_ENV, IFS, etc. stripped; TERM/LC_*/TZ whitelisted) (M2)
- [ ] Remote exit code propagated to client process exit code (M2)
- [ ] Server-side session struct defined (session_id, identity, PTY handle) even though reattach is not implemented (M2)

### Explicitly NOT in scope for the spike:

- Roaming / connection migration
- Predictive echo
- Scrollback, forwarding, multiplexing
- macOS or Windows
- WebTransport, NAT punch, relay
- 0-RTT
- Terminal emulation on client

---

## Competitor Feature Analysis

| Feature | Mosh | Eternal Terminal | nosh M0–M2 spike | nosh full (M7) |
|---------|------|-----------------|-----------------|----------------|
| Raw-mode PTY + TERM | Yes | Yes | Yes | Yes |
| Window resize | Yes | Yes | Yes | Yes |
| Exit code propagation | No (always 0) | Yes | Yes | Yes |
| Roaming / IP-change survival | Yes (custom UDP) | Yes (TCP reconnect) | No | Yes (QUIC migration) |
| Cold-reattach / resume | No | Yes (BackedReader) | No | Yes (1-RTT sequence) |
| Predictive local echo | Yes (excellent) | No | No | Yes |
| Native scrollback | No | Yes | No | Yes |
| SSH agent forwarding | No | Yes | No | Yes |
| Port forwarding | No | Yes | No | Yes |
| Session multiplexing | No | Partial (tmux CC) | No | Yes |
| UDP/443 (firewall-safe) | No (60000–61000 range) | No (TCP/2022) | Yes | Yes |
| Existing SSH keys | Partial | Yes (SSH handshake) | Yes | Yes |
| Hardware key (YubiKey) | No | No | Yes (ssh-agent) | Yes |
| Windows native | No | No | No | Yes (M6) |

---

## Sources

- INIT.md (project brief §3 goals, §8 feature checklist, §12 quicshell prior art)
- .planning/PROJECT.md (Active requirements, Out of Scope)
- Mosh paper: https://mosh.org/mosh-paper-draft.pdf
- Mosh SSH agent issue (open since 2012): https://github.com/mobile-shell/mosh/issues/120
- HN: Mosh vs ET user discussion: https://news.ycombinator.com/item?id=34069759
- portable-pty API: https://docs.rs/portable-pty/latest/portable_pty/
- RFC 4254 (SSH Connection Protocol — exit status, signal delivery): https://datatracker.ietf.org/doc/html/rfc4254
- quicshell spec (env sanitization, resize coalescing, control-first channels): borrowed via §12 of INIT.md
- SEI CERT C ENV03-C (env sanitization rationale): https://wiki.sei.cmu.edu/confluence/display/c/ENV03-C.+Sanitize+the+environment+when+invoking+external+programs

---
*Feature research for: nosh — QUIC-based roaming remote shell (architecture-validation spike)*
*Researched: 2026-05-29*
