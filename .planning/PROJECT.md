# nosh

## What This Is

`nosh` is a roaming-tolerant remote shell built on QUIC — a successor to Mosh and Eternal Terminal that reuses the user's existing SSH keys for mutual authentication and runs over a single UDP/443 port (indistinguishable from HTTP/3 on the wire). It's for developers who SSH from laptops and phones across flaky, NAT'd, or firewalled networks and want sessions that survive IP changes without re-authenticating.

This milestone is an **architecture-validation spike** (M0–M2 of the full brief): prove the three foundational bets work end-to-end on Linux before investing in the harder differentiators.

## Core Value

A single QUIC connection on UDP/443 can carry a live interactive shell, authenticated entirely from the user's existing SSH-key identity. If that core path works, everything else in the brief (roaming, predictive echo, forwarding, Windows) is incremental — so this milestone de-risks the architecture above all else.

## Requirements

### Validated

(None yet — ship to validate)

### Active

<!-- M0–M2 architecture spike. Hypotheses until shipped. -->

- [ ] Client and server establish a QUIC connection over UDP/443 (quinn + rustls, TLS 1.3)
- [ ] Unreliable datagram frames and reliable bidirectional streams coexist on one connection, demonstrably independent
- [ ] Server authenticates the client key against `authorized_keys`; client authenticates the server host key against `known_hosts` (TOFU on first contact) — mutual and symmetric
- [ ] Auth reuses existing OpenSSH key material (Ed25519 at minimum); self-signed-cert key-pinning is the acceptable first implementation path, with RFC 7250 raw public keys as the preferred target if the rustls API supports it
- [ ] Signing routes through `ssh-agent` so the private key is never handled directly (enables hardware/FIDO keys)
- [ ] Server spawns a real PTY (via `portable-pty`) and runs an interactive login shell
- [ ] Keystrokes flow client→server and shell output flows server→client over the live connection, usably interactive
- [ ] Terminal resize (SIGWINCH) propagates to the server PTY, with burst coalescing
- [ ] Client-supplied environment is sanitized on shell open (strip `LD_*`, `DYLD_*`, `BASH_ENV`, `ENV`, `IFS`, `SHELLOPTS`, `PYTHONPATH`, `NODE_OPTIONS`; whitelist `TERM`, `LC_*`/locale, `TZ`); `SSH_AUTH_SOCK` is never forwarded via the environment

### Out of Scope

<!-- Deferred to future milestones (M3–M7) or excluded outright. Each has a reason. -->

- Roaming / QUIC connection migration / sequence-numbered cold reattach — M3; the spike proves the connection works before proving it survives network change
- Predictive local echo (datagram state sync) — M4; hardest UX problem, only worth building once the transport+session foundation is proven
- Native scrollback sync, channel multiplexing, port forwarding, agent forwarding, OSC 52, file transfer — M5
- Native Windows client/server (ConPTY) — M6; this milestone is Linux-only
- WebTransport-over-HTTP/3 reverse-proxy mode and NAT hole-punch/relay topologies — M7
- macOS support — deferred; Linux-only this milestone to tighten scope
- 0-RTT — deliberately not pursued; 1-RTT is the default (see Key Decisions). Revisit only if profiling shows reconnect latency matters
- SSH CA certificate (`ssh-keygen -s`) → X.509 mapping — out of scope for MVP; raw-key trust first
- Being a terminal *emulator* — `nosh` is a remote shell, like Mosh/ET, not an emulator
- Web/browser client — HTTP/3 framing leaves the door open later, but not now

## Context

- **Greenfield.** The repo currently contains only `INIT.md` (the full design brief — the authoritative source for design rationale, topology details, and the M3–M7 roadmap) and a `CLAUDE.md` summarizing locked decisions. No code yet.
- **Why QUIC.** It collapses the trade-off both incumbents were forced into: Mosh's custom UDP protocol needs an inbound server port range (NAT/firewall-hostile); ET's TCP resumption inherits head-of-line blocking and can't do good predictive echo. QUIC gives UDP/443 (HTTP/3-like), connection migration for roaming, RFC 9221 datagrams for loss-tolerant state sync alongside reliable streams, and TLS 1.3 in the handshake.
- **Why SSH keys.** An Ed25519 SSH key *is* an Ed25519 key; only the credential envelope differs. RFC 7250 raw public keys in TLS 1.3 let us authorize against `authorized_keys`/`known_hosts` exactly like SSH. Routing the TLS `CertificateVerify` signature through `ssh-agent` gives hardware-key support for free.
- **Prior art — quicshell** (haukened/quicshell, spec at `docs/spec.md`): a neighbouring QUIC-first Rust shell, but framed as a *security-first SSH replacement* (fixed hybrid PQ crypto, no negotiation), not a *mobility-first Mosh successor*. Worth reading; several concrete design details (control-first multiplexing, per-channel flow control, host-key rotation as a signed object, happy-eyeballs transport selection, env sanitization) are borrowed into later milestones. Our differentiators are predictive echo, mobility UX, native scrollback, session persistence, Windows, and reusing existing OpenSSH keys.

## Constraints

- **Tech stack**: Rust (locked). Starting-point crates: `quinn` (QUIC), `rustls` (TLS 1.3, check RFC 7250 surface), `ssh-key` + an ssh-agent client for key/agent handling, `ed25519-dalek` for signatures, `portable-pty` (wezterm) for cross-platform PTY, `tokio` async runtime, `vte` for terminal state. Verify current APIs/versions at implementation time — these are not pins.
- **Transport**: QUIC over UDP/443 only; one connection per session. No custom UDP protocol, no TCP fallback this milestone.
- **Security (bake in from the session-core work, not later)**: environment-variable sanitization on every shell/exec open; never forward `SSH_AUTH_SOCK` via the environment (agent forwarding uses a dedicated channel in a later milestone). These are privilege-escalation footguns.
- **Platform**: Linux only this milestone.
- **Name**: `nosh` (confirm crates.io / GitHub org availability before first publish).

## Key Decisions

| Decision | Rationale | Outcome |
|----------|-----------|---------|
| Scope this milestone to M0–M2 (architecture spike), defer M3–M7 | Prove QUIC+SSH-auth+PTY coexist and carry a live session before building the hard differentiators (roaming, predictive echo, Windows) on an unproven foundation | — Pending |
| QUIC as sole transport on UDP/443 | Collapses Mosh's inbound-port and ET's TCP-HOL trade-offs; looks like HTTP/3, sails through firewalls | — Pending |
| Reuse existing SSH keys; self-signed-cert pinning acceptable first, RFC 7250 RPK preferred | Ed25519 SSH key is already an Ed25519 key; mirrors `authorized_keys`/`known_hosts` trust model; RPK maturity in rustls is the open risk | — Pending |
| Route signing through `ssh-agent` | Private key never handled directly; hardware/FIDO key support for free | — Pending |
| Default to 1-RTT; 0-RTT deferred (measure-first) | Only cold-reconnect case is resume-from-suspend, where 1 RTT is dwarfed by Wi-Fi/DHCP bring-up; 0-RTT's replay risk isn't worth the imperceptible gain. Matches quicshell's stance | — Pending |
| Linux-only, name stays `nosh` | Tightest scope for a validation milestone | — Pending |

## Evolution

This document evolves at phase transitions and milestone boundaries.

**After each phase transition** (via `/gsd-transition`):
1. Requirements invalidated? → Move to Out of Scope with reason
2. Requirements validated? → Move to Validated with phase reference
3. New requirements emerged? → Add to Active
4. Decisions to log? → Add to Key Decisions
5. "What This Is" still accurate? → Update if drifted

**After each milestone** (via `/gsd:complete-milestone`):
1. Full review of all sections
2. Core Value check — still the right priority?
3. Audit Out of Scope — reasons still valid?
4. Update Context with current state

---
*Last updated: 2026-05-29 after initialization*
