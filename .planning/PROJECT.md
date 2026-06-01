# nosh

## What This Is

`nosh` is a roaming-tolerant remote shell built on QUIC — a successor to Mosh and Eternal Terminal that reuses the user's existing SSH keys for mutual authentication and runs over a single UDP/443 port (indistinguishable from HTTP/3 on the wire). It's for developers who SSH from laptops and phones across flaky, NAT'd, or firewalled networks and want sessions that survive IP changes without re-authenticating.

The M0–M2 **architecture-validation spike** shipped in v1.0 (the three foundational bets proven end-to-end on Linux), and v1.1 (M3) added roaming + a native Windows client. The current milestone (v1.2, M4) builds the headline UX differentiator on that foundation — predictive local echo — and hardens nosh into a daily-drivable tool.

## Current Milestone: v1.2 M4 Predictive Echo + Daily-Driver Readiness

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

**Shipped:** v1.1 (M3 Roaming + Windows Client) — 2026-05-30. Phases 4-9. Audit 11/11 requirements, 4/4 cross-phase integration invariants, no blockers; validated end-to-end on a live native-Windows client → Linux server (auth, interactive shell, locale, resize, vim/arrows, `~.` quit, Ctrl-C→remote, and real network roaming all confirmed).

Both foundational milestones are now proven: v1.0 established the QUIC+SSH-key+PTY architecture on Linux; v1.1 added the differentiators that justify nosh over plain SSH — roaming-tolerant session persistence (migration + 1-RTT cold reattach) and a native Windows client.

**Carried tech debt (weigh at M4 start):** ~~PTY reader-zombie race (Phase 6, latent — `spawn_blocking`+`abort()` can't interrupt a blocked `read()`)~~ **CLEARED in v1.2 Phase 10** — interruptible reader (self-pipe + nix::poll) with deterministic exit via D-04 completion-barrier test. Windows cross-compile CI gate exists but has never run (no git remote configured — wire one); `WSAEMSGSIZE` quinn_udp warning on Windows (deferred; connection works).

**Phase 10 complete (2026-06-01):** PTY reader-zombie race resolved. Both `server.rs` output-pump sites converted to `crate::pty_io::start_interruptible_reader`; both `TransportLost` arms now await reader exit before `registry.orphan()`. `cargo test` green (25/25).

**Phase 11 complete (2026-06-01):** Datagram wire-protocol module delivered. `nosh-proto/src/datagram.rs` — `StateDiff` sparse-diff type, total `encode_datagram` (cursor-priority fill, STRICT payload < cap, continue-past-rejection), hardened `decode_datagram` (TAG_STATE_DIFF, MAX_RUNS guard, never panics on malformed input), 16 inline tests (22 total passing). SYNC-01 satisfied.

**Phase 12 complete (2026-06-01):** Server terminal state model delivered. `crates/nosh-server/src/terminal.rs` — `TerminalState` implementing `vte::Perform` with viewport grid, bounded scrollback (SCROLLBACK_LINE_CAP=10_000), DEC private modes (?25/?1049/?2004/?1), OSC 0/2 title, OSC 52 clipboard detection (D-12-04), and full SGR mapping. `Cell.fg/bg` are `Option<u8>` matching `DiffRun` for zero-conversion Phase 13 extraction. `SessionSlot::push_output_and_parse` feeds both `SequencedOutputBuffer` and `TerminalState`; 3 server.rs callsites converted; 67 lib tests pass. SYNC-02 satisfied.

**Current milestone:** v1.2 (M4) — **in progress.** Phase 12 (server terminal state model) done. Next: Phase 13 (server-datagram-sender) — StateDiff extraction + datagram send loop.

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

<!-- v1.2 (M4) scope — being decomposed into REQUIREMENTS.md / ROADMAP.md. -->

- Predictive local echo: datagram state sync carrying terminal diffs + full SSP-style speculative local echo (confirmation tracking, unconfirmed rendering, prediction epochs, conservative fallback)
- Daily-driver hardening: ~~fix PTY reader-zombie race~~ (✓ Phase 10); wire git remote + run Windows cross-compile CI; resolve `WSAEMSGSIZE` warning
- QoL UX: connection-loss notifications (reconnecting + abort instructions) + research-selected QoL wins
- Security design pass: threat-model review + security design doc

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
*Last updated: 2026-06-01 after Phase 11 complete — datagram wire format (SYNC-01) delivered; Phase 12 next (server terminal state model)*
