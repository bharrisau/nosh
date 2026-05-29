# nosh

## What This Is

`nosh` is a roaming-tolerant remote shell built on QUIC — a successor to Mosh and Eternal Terminal that reuses the user's existing SSH keys for mutual authentication and runs over a single UDP/443 port (indistinguishable from HTTP/3 on the wire). It's for developers who SSH from laptops and phones across flaky, NAT'd, or firewalled networks and want sessions that survive IP changes without re-authenticating.

This milestone is an **architecture-validation spike** (M0–M2 of the full brief): prove the three foundational bets work end-to-end on Linux before investing in the harder differentiators.

## Core Value

A single QUIC connection on UDP/443 can carry a live interactive shell, authenticated entirely from the user's existing SSH-key identity. If that core path works, everything else in the brief (roaming, predictive echo, forwarding, Windows) is incremental — so this milestone de-risks the architecture above all else.

## Requirements

### Validated

<!-- M0–M2 architecture spike — all proven end-to-end on Linux in v1.0 (audit 21/21 passed). -->

- ✓ Client and server establish a QUIC connection over UDP/443 (quinn + rustls, TLS 1.3) — v1.0 (TRANS-01)
- ✓ Unreliable datagram frames and reliable bidirectional streams coexist on one connection, demonstrably independent — v1.0 (TRANS-02/03/04, RFC 9221 enabled; concurrent round-trip test passes)
- ✓ Server authenticates the client key against `authorized_keys`; client authenticates the server host key against `known_hosts` (TOFU on first contact) — mutual and symmetric — v1.0 (AUTH-01/02)
- ✓ Auth reuses existing OpenSSH key material (Ed25519) via self-signed-cert key-pinning custom rustls verifiers; signature verification delegated to the CryptoProvider (never no-op'd) — v1.0 (AUTH-03). RFC 7250 RPK deferred; SPKI-pinning was the proven first path.
- ✓ Signing routes through `ssh-agent` so the private key is never handled directly — v1.0 (AUTH-04; live ssh-agent Ed25519 handshake passes)
- ✓ Server spawns a real PTY (via `portable-pty`) and runs an interactive login shell — v1.0 (SESS-01)
- ✓ Keystrokes flow client→server and shell output flows server→client over the live connection, usably interactive — v1.0 (SESS-02; human-validated live at a real terminal)
- ✓ Terminal resize (SIGWINCH) propagates to the server PTY, with burst coalescing (~40 ms) — v1.0 (SESS-04/05)
- ✓ Client-supplied environment is sanitized on shell open (deny-by-default: strips `LD_*`, `DYLD_*`, `BASH_ENV`, `ENV`, `IFS`, `SHELLOPTS`, `PYTHONPATH`, `NODE_OPTIONS`; whitelists `TERM`, `LC_*`/locale, `TZ`); `SSH_AUTH_SOCK` is never forwarded via the environment — v1.0 (SESS-07)
- ✓ Pre-auth DoS hardening (concurrent half-open cap + auth-completion timeout), explicit `SessionClose{exit_code}` exit-code propagation, clean QUIC close, and a structured server-side `Session` struct (M3 reattach seam) — v1.0 (AUTH-05, SESS-08/09/10/11)

### Active

<!-- v1.0 (M0–M2 spike) shipped and validated. Next milestone (M3 roaming) requirements
     to be defined via /gsd:new-milestone. -->

(Next milestone not yet scoped — start with `/gsd:new-milestone`)

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

- **v1.0 shipped (2026-05-29).** The M0–M2 architecture-validation spike is complete: a Cargo workspace (`nosh-proto`, `nosh-auth`, `nosh-server`, `nosh-client`) of ~3,460 LOC Rust across 3 phases / 11 plans. A single QUIC connection on UDP/443 carries a live interactive shell mutually authenticated from SSH keys (ssh-agent signing), with env sanitization, resize, signals, exit-code propagation and clean close. 27 tests pass (+3 `#[ignore]`-gated live tests), clippy clean. Milestone audit passed 21/21. Known-by-design M3+ seams remain (Session.identity wiring, cold reattach, datagram session traffic, privilege drop). See `.planning/milestones/v1.0-*`.
- **Origin.** The repo began from `INIT.md` (the full design brief — the authoritative source for design rationale, topology details, and the M3–M7 roadmap) and a `CLAUDE.md` summarizing locked decisions.
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
| Scope this milestone to M0–M2 (architecture spike), defer M3–M7 | Prove QUIC+SSH-auth+PTY coexist and carry a live session before building the hard differentiators (roaming, predictive echo, Windows) on an unproven foundation | ✓ Good — v1.0 proved all three bets end-to-end on Linux (audit 21/21); M3 reattach seam left in place |
| QUIC as sole transport on UDP/443 | Collapses Mosh's inbound-port and ET's TCP-HOL trade-offs; looks like HTTP/3, sails through firewalls | ✓ Good — single quinn connection carries a reliable stream + RFC 9221 datagrams concurrently (TRANS-01..05) |
| Reuse existing SSH keys; self-signed-cert pinning acceptable first, RFC 7250 RPK preferred | Ed25519 SSH key is already an Ed25519 key; mirrors `authorized_keys`/`known_hosts` trust model; RPK maturity in rustls is the open risk | ✓ Good (with note) — SPKI-pinning via custom rustls verifiers shipped and validated; RFC 7250 RPK deferred (pinning was the proven first path), Ed25519-only for now |
| Route signing through `ssh-agent` | Private key never handled directly; hardware/FIDO key support for free | ✓ Good — live ssh-agent Ed25519 CertificateVerify handshake passes; private key never loaded directly (AUTH-04) |
| Default to 1-RTT; 0-RTT deferred (measure-first) | Only cold-reconnect case is resume-from-suspend, where 1 RTT is dwarfed by Wi-Fi/DHCP bring-up; 0-RTT's replay risk isn't worth the imperceptible gain. Matches quicshell's stance | — Pending — 1-RTT default held; cold reconnect/profiling is M3+, not exercised in the spike |
| Linux-only, name stays `nosh` | Tightest scope for a validation milestone | ✓ Good — Linux-only kept scope tight; spike completed on Linux |

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
*Last updated: 2026-05-29 after v1.0 milestone*
