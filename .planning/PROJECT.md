# nosh

## What This Is

`nosh` is a roaming-tolerant remote shell built on QUIC — a successor to Mosh and Eternal Terminal that reuses the user's existing SSH keys for mutual authentication and runs over a single UDP/443 port (indistinguishable from HTTP/3 on the wire). It's for developers who SSH from laptops and phones across flaky, NAT'd, or firewalled networks and want sessions that survive IP changes without re-authenticating.

The M0–M2 **architecture-validation spike** shipped in v1.0 (the three foundational bets proven end-to-end on Linux), and v1.1 (M3) added roaming + a native Windows client. v1.2 (M4) built the headline UX differentiator on that foundation — predictive local echo — and hardened nosh into a daily-drivable tool. v1.3 (M5) builds the channel-multiplexing foundation and native scrollback sync, and fixes the rendering/pacing defects that currently make full-screen TUI apps unusable.

## Current Milestone: v1.3 M5 Channel Multiplexing, Scrollback Sync & TUI Rendering Correctness

**Goal:** Build nosh's channel-multiplexing foundation and native scrollback sync, and fix the rendering/pacing defects that make full-screen TUI apps unusable today — so nosh handles vim/htop/Claude Code and large repaints correctly.

**Target features:**
- Channel multiplexing + per-channel flow control — control-first OPEN/ACCEPT/REJECT on control channel id 0 before binding a stream, per-channel flow-control windows (the M5 foundation everything else rides on)
- Scrollback sync — native server-side scrollback synced to the client beyond the live grid (the first consumer of the mux layer)
- Full-screen TUI rendering correctness (was backlog 999.5) — a genuine alternate-screen buffer (`?1049h`/`?1049l` with save/restore/clear-on-enter, not the current no-op flag) plus a cell-width/grapheme audit; fixes the "garbled, spaces missing" full-screen TUI breakage
- Repaint pacing (was backlog 999.6) — burst multiple datagrams per tick so full-screen repaints land in ~1 RTT instead of dribbling one MTU per 16 ms tick, without breaking the noecho security invariant (one epoch per tick)

**Key context:** OSC 52 clipboard already shipped in v1.2, so it is out of M5 scope. Port/agent forwarding and file transfer (the rest of M5) are deferred — scrollback sync is the immediate consumer that justifies building the mux layer now; forwarding/transfer become incremental later. 999.6 was reverted once in 999.4 (infinite-spin from recomputing `fresh_runs` during burst drain; noecho-epoch security interaction) — both have documented fixes-by-design to bake in from the first implementation. 999.5 is investigation-first: reproduce on a Linux client↔server before fixing. The 999.x security/OOM backlog (999.7 post-auth OSC OOM, 999.2 client trust-boundary) and Phase 18 security pass stay deferred.

## Previous Milestone: v1.2 M4 Predictive Echo + Daily-Driver Readiness (shipped 2026-06-07)

**Goal:** Deliver the predictive-echo differentiator (datagram state sync + full SSP-style local echo) and harden nosh into a tool the maintainer can daily-drive from the Windows client, with a security design review.

**Target features:**
- Predictive local echo — datagram state sync carrying terminal diffs + full Mosh/SSP-style speculative local echo (confirmation tracking, dim/underline "unconfirmed" rendering, prediction epochs, conservative fallback)
- Daily-driver hardening — fix the latent PTY reader-zombie race; wire a git remote + make the Windows cross-compile CI gate actually run; resolve the `WSAEMSGSIZE` quinn_udp warning
- Quality-of-life UX — connection-loss notifications (reconnecting notice + abort instructions) + a research-selected set of the highest-value QoL wins
- Security design pass — thorough threat-model review of the design as built, written up as a security design doc

**Key context:** tmux integration excluded (researching general QoL wins instead); install/packaging UX not scoped (cargo-from-source acceptable — the bar is stability + UX, not distribution); full SSP-style prediction is the brief's hardest UX problem (INIT.md §10), budget accordingly.

## Core Value

A single QUIC connection on UDP/443 can carry a live interactive shell, authenticated entirely from the user's existing SSH-key identity. If that core path works, everything else in the brief (roaming, predictive echo, forwarding, Windows) is incremental — so this milestone de-risks the architecture above all else.

## Current State

**Shipped:** v1.2 (M4 Predictive Echo + Daily-Driver Readiness) — 2026-06-07. Core phases 10-17. Audit 17/19 requirements satisfied, cross-phase integration 8/8 seams wired (no broken/orphaned), all E2E flows complete. Delivered the headline differentiator — speculative local echo over QUIC state-sync datagrams (Mosh-style SSP: epoch tracking, conservative reset, noecho suppression, adaptive-RTT, wide-char) — on an authoritative server terminal-state model, plus the QoL pack (loss banner, OSC 52 clipboard, terminal title, RTT status) and a Windows CI gate. Predictive echo was live-validated on a native Windows client → Linux server (PREDICT-07). Backlog items 999.1 (server attack-surface fuzz-hardening — ~18h campaign, zero crashes, `docs/999.1-SECURITY.md`), 999.3 and 999.4 also shipped this cycle.

All three foundational/UX milestones are now proven: v1.0 established the QUIC+SSH-key+PTY architecture on Linux; v1.1 added roaming-tolerant session persistence (migration + 1-RTT cold reattach) and a native Windows client; v1.2 added predictive echo and daily-driver readiness.

**Deferred to a future milestone:** Phase 18 — Security Design Pass (SEC-01 threat-model doc, SEC-02 interactive TOFU fingerprint-confirm prompt). (Note: TOFU/known_hosts pinning + host-key-mismatch hard-fail already exist and were re-verified in 999.1; SEC-02 is the interactive first-contact prompt.)

**Backlog carried forward (999.x parking-lot):** 999.2 client trust-boundary hardening; 999.5 full-screen TUI / alternate-screen rendering; 999.6 burst repaint-pacing (epoch-under-burst); 999.7 bound OSC accumulation before vte (post-auth OOM — Phase-16's mitigation reasoning was found incorrect in the 999.1 review).

**Operator follow-ups (post-close):** push to confirm the green Windows `build-windows` + `cargo audit` CI runs; live-test the deferred human_needed visual items and the 999.4 `read -s`/predictive-echo fixes on a Windows client.

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

<!-- v1.1 (M3 roaming + Windows client) — all shipped 2026-05-30. -->

- ✓ Identity threading: `Session.identity` is a non-optional verified SSH key, the spine for persistence/cap/reattach — v1.1 (IDENT-01)
- ✓ Server-side session persistence: orphaned sessions survive disconnect (MasterPty held, no SIGHUP; idle timeout default 0; per-identity cap before first store; zombie reaper) — v1.1 (PERSIST-01..03)
- ✓ 1-RTT cold reattach: sequence-numbered resume, two-factor (TLS re-run + identity-scoped token selector, no oracle), byte-exact replay — v1.1 (IDENT-02, ROAM-02)
- ✓ Connection migration: IP/path change continues the same QUIC connection (no re-handshake), validated headless + real network-change live check from the Windows client — v1.1 (ROAM-01)
- ✓ Native Windows client → Linux server: cross-compiles (no WSL), on-disk Ed25519 signing, raw VT I/O + resize, TERM/locale; P9 hardening (VT console-input, `~.` escape, authorized_keys warn+skip, connect timeout, migration logging) — v1.1 (WIN-01..04)

<!-- v1.2 (M4) Phase 11 — validated 2026-06-01 -->

- ✓ Sparse size-bounded datagram wire format in `nosh-proto`: `StateDiff` (changed cells, monotonic epoch, dims+cursor), total `encode_datagram` (cursor-priority fill, STRICT cap), hardened `decode_datagram` (never panics, MAX_RUNS guard), round-trip + size-cap tests — v1.2 Phase 11 (SYNC-01). Validated in Phase 11: 2026-06-01.

### Active

<!-- v1.3 (M5) scope — being decomposed into REQUIREMENTS.md / ROADMAP.md. -->

- Channel multiplexing + per-channel flow control: control-first OPEN/ACCEPT/REJECT on control channel id 0 before binding a stream; per-channel flow-control windows
- Scrollback sync: native server-side scrollback synced to the client beyond the live grid (first consumer of the mux layer)
- Full-screen TUI rendering correctness (999.5): genuine alternate-screen buffer (`?1049h`/`?1049l` save/restore/clear, not a no-op flag) + cell-width/grapheme audit
- Repaint pacing (999.6): burst datagrams per tick so full-screen repaints land in ~1 RTT, without breaking the noecho security invariant (one epoch per tick)

<!-- v1.2 (M4) scope — SHIPPED 2026-06-07. Predictive echo + QoL pack + pre-auth fuzz-hardening. See MILESTONES.md and Validated below. -->
- ✓ Predictive local echo (SSP-style speculative overlay, epoch tracking, noecho suppression, adaptive-RTT, wide-char) — v1.2; live-validated Windows→Linux (PREDICT-07)
- ✓ Daily-driver hardening (PTY reader-zombie race fix, Windows CI gate, WSAEMSGSIZE) — v1.2
- ✓ QoL pack (loss banner, OSC 52 clipboard, terminal title, RTT status) — v1.2
- ⊘ Security design pass — deferred to a future milestone (Phase 18: SEC-01/SEC-02)

### Out of Scope

<!-- Deferred to future milestones (M3–M7) or excluded outright. Each has a reason. -->

- Native scrollback sync, channel multiplexing, port forwarding, agent forwarding, file transfer — M5 (note: OSC 52 clipboard and lightweight scrollback are candidates for v1.2's research-selected QoL set; the full M5 versions stay deferred)
- Native Windows *server* (ConPTY) — M6; v1.1 brings only the Windows *client* (→ Linux server)
- Windows ssh-agent / Pageant integration — deferred; the v1.1 Windows client signs from an on-disk key file as a bounded, temporary exception
- 0-RTT cold reattach — still deferred; v1.1 cold reattach is 1-RTT (see Key Decisions)
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
*Last updated: 2026-06-07 after starting milestone v1.3 (M5)*
