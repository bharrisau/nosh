# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project status

This is a **greenfield project with no application code yet** — the repository contains `INIT.md` (the design brief) and the `.planning/` GSD artifacts (PROJECT.md, REQUIREMENTS.md, ROADMAP.md, research/). There is no `Cargo.toml`, no build system, and no tests. The first implementation work is to scaffold the Rust workspace per the design below and the roadmap in `.planning/ROADMAP.md`.

`INIT.md` is the authoritative design brief — read it in full before making architectural decisions. This file summarizes the *locked* decisions and the rationale that isn't obvious from the brief; INIT.md has the complete reasoning, topology diagrams, and risk register.

## What this is

`nosh` is a roaming-tolerant remote shell built on QUIC — a successor to Mosh and Eternal Terminal. It authenticates with the user's **existing SSH keys**, runs over a single **UDP/443** port (looks like HTTP/3 on the wire), and survives network changes via QUIC connection migration. Language is **Rust (locked)**.

## Architecture: the load-bearing decisions

These are the decisions that drive the whole design. Violating them defeats the project's reason to exist.

- **QUIC is the sole transport, on UDP/443.** Not a custom UDP protocol (Mosh's mistake — server opens an inbound port range) and not TCP (ET's mistake — head-of-line blocking, no good predictive echo). One QUIC connection per session.
- **Two channel types, by reliability need:**
  - **Datagram frames (RFC 9221)** carry the terminal state-sync object (idempotent diffs + predictive local echo, modeled on Mosh's SSP). Loss-tolerant, latest-state-wins.
  - **Reliable bidirectional streams** carry everything else: control channel, scrollback sync, port forwards, file transfer, agent forwarding. One logical concern per stream type.
- **Roaming = QUIC connection migration**, not a reattach handshake. IP change (Wi-Fi→cellular) continues the *same* connection via connection IDs, zero extra round trips. This is distinct from 0-RTT.
- **Cold reconnect (resume-from-suspend) is 1-RTT**, via a sequence-numbered re-attach control message (ET's BackedReader idea) so the server resumes an orphaned session. **0-RTT is deliberately deferred** — it could only carry the idempotent reattach message, the gain is dwarfed by Wi-Fi/DHCP bring-up, and it introduces replay-safety burden. Do not add 0-RTT without a measured reason.
- **Auth reuses existing SSH keys, mutually and symmetrically.** Client key checked against `authorized_keys`; server host key against `known_hosts`/TOFU — mirroring SSH exactly.
  - **Preferred:** RFC 7250 raw public keys in TLS 1.3 (rustls supports it — *confirm the API and minimum rustls version at implementation time*).
  - **Fallback:** wrap the SSH public key in an ephemeral self-signed X.509 cert and pin the key. Safe default if RPK is awkward.
  - **ssh-agent integration is the key win:** route the TLS `CertificateVerify` signing operation to `ssh-agent` so the private key is never handled directly — gives YubiKey/FIDO/hardware-key support for free.
- **Server-side session persistence:** orphaned sessions survive client disconnect (idle-timeout + a reattach token bound to the SSH identity).

## Security invariants (bake in from M2, do not defer)

- **Environment-variable sanitization on every shell/exec open.** Strip `LD_*`, `DYLD_*`, `BASH_ENV`, `ENV`, `IFS`, `SHELLOPTS`, `PYTHONPATH`, `NODE_OPTIONS`, etc. Whitelist `TERM`, `LC_*`/locale, `TZ`. This is a privilege-escalation footgun.
- **Never forward `SSH_AUTH_SOCK` via the environment.** Agent forwarding goes through the dedicated agent channel, never an env var.
- Cap memory for unauthenticated / half-open connections (DoS hardening).

## Planned Rust stack

Versions/APIs drift — verify each crate's current API and pick versions at implementation time. Starting points, not pins:

- **QUIC:** `quinn` (async, rustls-backed) · **TLS:** `rustls` (check RFC 7250 surface)
- **WebTransport (reverse-proxy topology):** `wtransport`
- **SSH keys/agent:** `ssh-key` (RustCrypto) + an ssh-agent client (`ssh-agent-client-rs` or `russh` agent support)
- **Signatures:** `ed25519-dalek` + ECDSA/RSA · **PTY (incl. Windows ConPTY):** `portable-pty` (wezterm)
- **Async runtime:** `tokio` · **Terminal model:** a VT parser like `vte`

`portable-pty` is the choice that makes the native-Windows-server goal tractable — preserve it.

## Milestone path

Implement in this order (full detail in INIT.md §10). Each builds on the prior:

1. **M0 Spike** — quinn client/server over UDP/443; prove datagram + stream coexistence.
2. **M1 Auth** — SSH-key mutual auth (start with self-signed-cert pinning; wire ssh-agent signing; check `authorized_keys`/`known_hosts`).
3. **M2 Session core** — PTY via `portable-pty`, terminal state model, basic interactive shell (no prediction yet), Unix first. Includes env sanitization + resize signal from the start.
4. **M3 Roaming** — connection migration across IP change; 1-RTT cold-reattach.
5. **M4 Predictive echo** — datagram state sync with local echo prediction (the hardest UX problem — budget for it).
6. **M5 Features** — control-first channel multiplexing (OPEN/ACCEPT/REJECT on control channel id 0 before binding a stream), per-channel flow-control windows, scrollback sync, agent/port forwarding, OSC 52.
7. **M6 Windows** — native server (ConPTY) + client.
8. **M7 Topologies** — WebTransport mode for reverse-proxy deployment (inner SSH-key auth inside the tunnel); hole-punch/relay with migration handover.

## Topology note that constrains the design

Behind an HTTP/3 reverse proxy (e.g. nginx), the proxy **terminates** QUIC — end-to-end transport-layer mutual auth and migration die at the proxy. The mandatory (not optional) answer is to run `nosh` as **WebTransport over HTTP/3** with the SSH-key handshake as an **inner** auth layer inside the tunnel (ET's outer-transport/inner-handshake model). Avoid L4 UDP passthrough — QUIC's routing key is the connection ID, not the 5-tuple.

## Prior art

`quicshell` (haukened/quicshell, spec at `docs/spec.md`) is a neighbouring QUIC-first Rust shell, but framed as a *security-first SSH replacement* (fixed hybrid PQ crypto, no negotiation), not a *mobility-first Mosh successor*. Several of its concrete design details (control-first multiplexing, per-channel flow control, host-key rotation as a signed object, happy-eyeballs QUIC-then-TCP transport selection) are borrowed into the roadmap above — see INIT.md §12. Our explicit differentiators are predictive echo, native scrollback, roaming UX, session persistence, Windows/ConPTY, and reusing existing OpenSSH keys via RFC 7250.

<!-- GSD:project-start source:PROJECT.md -->
## Project

**nosh**

`nosh` is a roaming-tolerant remote shell built on QUIC — a successor to Mosh and Eternal Terminal that reuses the user's existing SSH keys for mutual authentication and runs over a single UDP/443 port (indistinguishable from HTTP/3 on the wire). It's for developers who SSH from laptops and phones across flaky, NAT'd, or firewalled networks and want sessions that survive IP changes without re-authenticating.

This milestone is an **architecture-validation spike** (M0–M2 of the full brief): prove the three foundational bets work end-to-end on Linux before investing in the harder differentiators.

**Core Value:** A single QUIC connection on UDP/443 can carry a live interactive shell, authenticated entirely from the user's existing SSH-key identity. If that core path works, everything else in the brief (roaming, predictive echo, forwarding, Windows) is incremental — so this milestone de-risks the architecture above all else.

### Constraints

- **Tech stack**: Rust (locked). Starting-point crates: `quinn` (QUIC), `rustls` (TLS 1.3, check RFC 7250 surface), `ssh-key` + an ssh-agent client for key/agent handling, `ed25519-dalek` for signatures, `portable-pty` (wezterm) for cross-platform PTY, `tokio` async runtime, `vte` for terminal state. Verify current APIs/versions at implementation time — these are not pins.
- **Transport**: QUIC over UDP/443 only; one connection per session. No custom UDP protocol, no TCP fallback this milestone.
- **Security (bake in from the session-core work, not later)**: environment-variable sanitization on every shell/exec open; never forward `SSH_AUTH_SOCK` via the environment (agent forwarding uses a dedicated channel in a later milestone). These are privilege-escalation footguns.
- **Platform**: Linux only this milestone.
- **Name**: `nosh` (confirm crates.io / GitHub org availability before first publish).
<!-- GSD:project-end -->

<!-- GSD:stack-start source:research/STACK.md -->
## Technology Stack

## Recommended Stack
### Core Technologies
| Technology | Version | Purpose | Why Recommended |
|------------|---------|---------|-----------------|
| `quinn` | 0.11.9 | QUIC transport (client + server) | Only mature async/tokio QUIC crate in Rust; RFC 9221 datagram support built in; `rustls` backend is first-class; active maintenance (quinn-rs org) |
| `rustls` | 0.23.40 | TLS 1.3 (used via quinn's crypto layer) | RFC 7250 raw public key support shipped in 0.23.16 (Oct 2024); `AlwaysResolvesServerRawPublicKeys` and `AlwaysResolvesClientRawPublicKeys` ready to use; custom `SigningKey`/`Signer` traits enable ssh-agent delegation |
| `tokio` | 1.52.x (LTS until Mar 2027) | Async runtime | quinn's default and only production-quality async I/O for QUIC; 1.47.x and 1.51.x are current LTS lines |
| `portable-pty` | 0.9.0 | Cross-platform PTY (Linux + future Windows) | wezterm's battle-tested PTY abstraction; trait-based so ConPTY can drop in for M6; `native_pty_system()` → `openpty()` → `spawn_command()` pattern is clean |
| `ssh-key` | 0.6.7 | Parse OpenSSH public/private keys, authorized_keys, known_hosts | RustCrypto project; pure Rust; parses all common key types (Ed25519, ECDSA, RSA); `authorized_keys` and `known_hosts` module support built in; `ed25519` feature enables keygen/sign/verify |
| `ssh-agent-client-rs` | 1.1.2 | ssh-agent protocol client | Provides `Client::sign(&mut self, key: impl Into<Identity>, data: &[u8]) -> Result<Signature>`; connects via Unix socket; synchronous API suitable for use inside blocking `rustls::sign::Signer::sign()` |
### Supporting Libraries
| Library | Version | Purpose | When to Use |
|---------|---------|---------|-------------|
| `ed25519-dalek` | 2.2.0 | Ed25519 public key material (verify agent signatures in tests; derive SPKI) | Use stable 2.2.0; 3.0.0-pre.7 exists (2026-05-06) but is pre-release — avoid in spike |
| `rcgen` | 0.14.8 | Generate ephemeral self-signed X.509 certs (cert-pinning fallback) | Use if RPK config is too involved for initial spike — see RPK vs Cert-Pinning section below |
| `vte` | 0.15.0 | VT/ANSI parser for server-side terminal state model | Alacritty project; implements Paul Williams state machine; use for M2 terminal model; `Perform` trait is the extension point |
| `bytes` | 1.x | Zero-copy byte buffer (already a quinn transitive dep) | Frame serialisation / datagram payload handling |
| `tracing` | 0.1.x | Structured async logging (already a quinn transitive dep) | Prefer over `log` for async spans |
### Development Tools
| Tool | Purpose | Notes |
|------|---------|-------|
| `cargo nextest` | Faster test runner | Parallel test execution; useful for integration tests that bind ports |
| `quinn` `qlog` feature | QUIC event logging | Enable for transport-level debugging during spike; `TransportConfig` events |
| OpenSSL or ssh-keygen | Generate test keys | Generate Ed25519 test keys for authorized_keys/known_hosts fixtures; no Rust dep needed |
## Cargo.toml Sketch
# Transport
# TLS / Crypto
# SSH key material
# PTY
# Async runtime
# Terminal model (M2)
# Cert generation fallback
# Nothing extra needed for spike tests
## Critical Design Decisions
### RPK vs Self-Signed-Cert Key Pinning
### ssh-agent → rustls Signing Integration
- Ed25519: `SignatureScheme::ED25519` maps directly; ssh-agent signs with Ed25519 natively.
- ECDSA P-256: `SignatureScheme::ECDSA_NISTP256_SHA256`; ssh-agent `sign` flag `0` (default).
- RSA: ssh-agent uses `sign` flag `4` for `rsa-sha2-256` (maps to `RSA_PSS_SHA256` or `RSA_PKCS1_SHA256`). Flag `8` for `rsa-sha2-512`. The `ssh-agent-client-rs` `sign()` method accepts the identity but does not currently expose flags in the public API (v1.1.2); may need to call the lower-level protocol directly or use `ssh-agent-lib` for RSA flag control.
- The blocking `sign()` call happens inside the TLS handshake on a tokio thread; wrap in `tokio::task::spawn_blocking` at the point where you drive the quinn/rustls handshake if needed, or accept the brief block on the handshake task.
### QUIC DATAGRAM Configuration (RFC 9221)
### PTY Spawn and I/O (portable-pty 0.9.0)
## Alternatives Considered
| Category | Recommended | Alternative | Why Not |
|----------|-------------|-------------|---------|
| QUIC crate | `quinn` 0.11 | `s2n-quic` (AWS) | s2n-quic is production-grade but uses its own TLS layer (s2n-tls), not rustls — harder to wire ssh-agent signing. quinn's rustls integration is the native path |
| QUIC crate | `quinn` 0.11 | `quiche` (Cloudflare, via C FFI) | FFI, not pure Rust; async story is messier |
| Crypto backend | `rustls-ring` | `rustls-aws-lc-rs` | aws-lc-rs requires a C build step (cmake); ring is pure Rust build. For the spike, simpler wins |
| SSH agent | `ssh-agent-client-rs` | `russh` agent support | russh carries the full SSH protocol implementation; agent-only use pulls in large transitive deps. `ssh-agent-client-rs` is minimal and purpose-built |
| SSH agent | `ssh-agent-client-rs` | `ssh-agent-lib` | `ssh-agent-lib` is for *writing* an agent server, not a client |
| PTY | `portable-pty` | `nix` raw pty | nix raw PTY is Linux-only; portable-pty's trait abstraction already bakes in the M6 Windows path |
| Terminal parser | `vte` | `termwiz` (also wezterm) | termwiz is a full terminal emulator; vte is just the parser state machine. For M2 server-side terminal model, vte + custom `Perform` impl is the right granularity |
| Cert generation | `rcgen` | `x509-cert` (RustCrypto) | x509-cert is a lower-level DER builder with no signing convenience; rcgen's `SigningKey` trait + `CertificateParams::self_signed()` is ergonomic |
## What NOT to Use
| Avoid | Why | Use Instead |
|-------|-----|-------------|
| `rustls` < 0.23.16 | No RPK support; older API shapes | `rustls` 0.23.40 |
| `quinn` < 0.11 | Pre-0.11 used a different rustls wiring (`quinn::ServerConfigBuilder`); API completely changed | `quinn` 0.11.x |
| `ed25519-dalek` 3.0.0-pre.*  | Pre-release API may change | `ed25519-dalek` 2.2.0 (stable) |
| `rustls-native-certs` for host verification | Platform cert store verifies PKI chains; nosh never uses PKI chains — it pins keys | Custom `ServerCertVerifier` or RPK |
| `openssh` crate | Spawns a subprocess `ssh`; provides no primitives we need | `ssh-key` + `ssh-agent-client-rs` |
| `tokio-rustls` directly | Not needed — quinn handles the TLS/QUIC integration internally; tokio-rustls is for TCP+TLS | Only quinn |
| `mio` or raw `epoll` | quinn requires tokio; mixing runtimes creates pain | tokio only |
## Version Compatibility
| Package | Compatible With | Notes |
|---------|-----------------|-------|
| `quinn` 0.11.9 | `rustls` 0.23.x | quinn 0.11 requires rustls 0.23; the conversion path is `rustls::ServerConfig` → `quinn::crypto::rustls::QuicServerConfig::try_from()` → `quinn::ServerConfig::with_crypto()` |
| `quinn` 0.11.9 | `tokio` 1.x | `runtime-tokio` feature; tokio 1.52.x is current |
| `ssh-key` 0.6.7 | `ed25519-dalek` 2.x | ssh-key's `ed25519` feature pulls `ed25519-dalek` as a transitive dep; pin to same major |
| `portable-pty` 0.9.0 | `tokio` 1.x | No direct tokio dep; bridge via `spawn_blocking` |
| `rcgen` 0.14.8 | `rustls` 0.23.x | rcgen 0.14.x integrates with rustls 0.23 `CertifiedKey` |
## Stack Patterns by Phase
- quinn client + server, `rustls::ServerConfig::builder().with_no_client_auth().with_single_cert(rcgen-generated cert)`, datagram + stream on one connection.
- Goal: confirm RFC 9221 datagrams and bidi streams are independent on one connection.
- `AgentSigningKey` / `AgentSigner` delegating to `ssh-agent-client-rs`.
- Server: custom `ClientCertVerifier` checking presented cert's SPKI against `authorized_keys` entries parsed by `ssh-key`.
- Client: custom `ServerCertVerifier` checking server cert's SPKI against `known_hosts` (TOFU on first connection).
- Defer RPK upgrade until after M1 is working end-to-end.
- `portable-pty` 0.9.0 on Linux; env sanitization before `spawn_command`.
- `vte` 0.15.0 for terminal state; `PtySize` resize with ~40 ms burst coalescing.
## Sources
- https://docs.rs/crate/quinn/latest (version 0.11.9, features list — verified)
- https://docs.rs/quinn/latest/quinn/struct.Connection.html (send_datagram, read_datagram, max_datagram_size API — verified)
- https://docs.rs/quinn/latest/quinn/struct.TransportConfig.html (datagram_receive_buffer_size — verified)
- https://quinn-rs.github.io/quinn/quinn/certificate.html (QuicServerConfig::try_from wiring pattern — verified)
- https://docs.rs/rustls/latest/rustls/ (version 0.23.40 confirmed)
- https://docs.rs/rustls/latest/rustls/client/danger/trait.ServerCertVerifier.html (requires_raw_public_keys, verify_server_cert API — verified)
- https://docs.rs/rustls/latest/rustls/server/struct.AlwaysResolvesServerRawPublicKeys.html (RPK resolver API — verified)
- https://docs.rs/rustls/latest/rustls/client/struct.AlwaysResolvesClientRawPublicKeys.html (RPK client resolver — verified)
- https://docs.rs/rustls/latest/rustls/sign/struct.CertifiedKey.html (CertifiedKey for RPK and cert-pinning — verified)
- https://github.com/rustls/rustls/releases/tag/v%2F0.23.16 (RPK added in 0.23.16, PR #2062 — verified)
- https://github.com/rustls/rustls/issues/2257 (RPK compliance caveat — LOW confidence on current resolution status)
- https://docs.rs/crate/rustls/latest/source/src/manual/howto.rs (custom SigningKey/Signer delegation pattern — verified)
- https://docs.rs/ssh-agent-client-rs/latest/ssh_agent_client_rs/ (version 1.1.2, sign() method — verified)
- https://github.com/nresare/ssh-agent-client-rs (sign API, list_identities, synchronous — verified)
- https://docs.rs/portable-pty/latest/portable_pty/ (version 0.9.0, openpty/spawn_command/resize API — verified)
- https://docs.rs/ssh-key/latest/ssh_key/ (version 0.6.7, authorized_keys/known_hosts modules — verified)
- https://docs.rs/crate/rcgen/latest (version 0.14.8, SigningKey trait, self_signed() — verified)
- https://docs.rs/crate/ed25519-dalek/latest (version 2.2.0 stable; 3.0.0-pre.7 pre-release noted — verified)
- https://github.com/alacritty/vte (version 0.15.0 current — verified)
- https://github.com/tokio-rs/tokio/releases (tokio 1.52.x current; 1.47.x and 1.51.x LTS — verified)
<!-- GSD:stack-end -->

<!-- GSD:conventions-start source:CONVENTIONS.md -->
## Conventions

### GSD agent model selection

When launching any GSD subagent (planner, researcher, executor, verifier, plan-checker, roadmapper, synthesizer, code-reviewer, etc.), resolve its model from the configured profile **before** spawning and pass it explicitly as `model=`:

```bash
gsd-sdk query resolve-model gsd-planner   # → {"model": "opus", "profile": "balanced"}
```

- Use the agent type as a **positional** arg. `--agent` silently falls back to a sonnet default (`"unknown_agent": true`).
- The profile maps **per-agent**, not globally. On `balanced` (this project): `gsd-planner` → **opus**; executor / verifier / researcher / plan-checker / roadmapper / synthesizer / code-reviewer → **sonnet**. Don't generalize one role's tier to the others or blanket-default to opus or sonnet.
- When wrapping a GSD skill (e.g. `gsd:plan-phase`) in an `Agent`, let the skill resolve its own subagents' models — do **not** inject a blanket model override into the wrapper prompt (it can downgrade the planner off opus).
- Profile is changed via `/gsd:set-profile` / `/gsd:settings`; never silently override it.

### Autonomous loop: drive workflows inline, don't reload Skills

In a multi-phase loop (`/gsd:autonomous`, repeated `plan-phase`/`execute-phase`), do **not** re-invoke the per-phase Skill wrappers (`gsd:plan-phase`, `gsd:execute-phase`, `gsd:code-review`, `gsd:code-review-fix`) once you've read that workflow's `.md` this session. Each Skill call re-injects the **same** ~34–38k-token workflow document — pure waste across phases (~80–100k tokens/phase). Once a workflow is in context, drive its gate sequence **directly** with the `gsd-sdk` queries and subagent spawns the workflow specifies.

Preserve these two behaviours exactly — they are why the wrappers existed:
- **Resolve the model via `gsd-sdk`, per agent, before spawning, and pass `model=` explicitly.** The `init.*` queries already return the resolved tiers — `init.plan-phase` → `researcher_model`/`planner_model`/`checker_model`; `init.execute-phase` → `executor_model`/`verifier_model`. Use those values (don't hardcode); they honor the profile + `model_overrides` (e.g. `gsd-verifier` → opus). See [GSD agent model selection] above and [GSD verification: separate pass, on opus] below.
- **Spawn the correct plugin subagent type** — the namespaced ids: `gsd:gsd-planner`, `gsd:gsd-plan-checker`, `gsd:gsd-phase-researcher`, `gsd:gsd-pattern-mapper`, `gsd:gsd-executor`, `gsd:gsd-verifier`, `gsd:gsd-code-reviewer`, `gsd:gsd-code-fixer`. Never fall back to `general-purpose`.

Still follow every workflow gate faithfully (the Skill text is the spec, you're just not re-paying to reload it): init → research/pattern-map → plan → plan-checker (+revision loop) → coverage gates → `state.planned-phase`; then per wave: executor(s) → merge/cleanup → post-merge build+test → tracking; then code-review → fix → **opus** verifier → `phase.complete`. **Load a skill once** if you hit a workflow shape not yet read this session (e.g. `gsd:ui-phase`, or the milestone-lifecycle `audit-milestone`/`complete-milestone`/`cleanup`).

### GSD verification: separate pass, on opus

Verification runs as an **independent pass on opus**, never collapsed inline into the (sonnet) executor — a peer grading a peer rubber-stamps subtle bugs (happened in three consecutive M3 phases; opus re-verify caught a real bug each time).

- Pinned in `.planning/config.json` via per-agent override (GSD #3227): `"model_overrides": { "gsd-verifier": "opus", "gsd-integration-checker": "opus" }` (use `model_overrides.<agent-id>`, the per-agent knob — not `model_profile_overrides`).
- Don't wrap `execute-phase` in a background agent and let it self-verify: background agents can't spawn subagents (one-level nesting), so the separate `gsd-verifier` collapses inline. Run a dedicated verifier agent after execute.
- Make the verifier **adversarial** — write a probe test that reproduces the suspected failure (fails before fix, passes after); trust the code over the executor's summary.
<!-- GSD:conventions-end -->

<!-- GSD:architecture-start source:ARCHITECTURE.md -->
## Architecture

Architecture not yet mapped. Follow existing patterns found in the codebase.
<!-- GSD:architecture-end -->

<!-- GSD:skills-start source:skills/ -->
## Project Skills

No project skills found. Add skills to any of: `.claude/skills/`, `.agents/skills/`, `.cursor/skills/`, `.github/skills/`, or `.codex/skills/` with a `SKILL.md` index file.
<!-- GSD:skills-end -->

<!-- GSD:workflow-start source:GSD defaults -->
## GSD Workflow Enforcement

Before using Edit, Write, or other file-changing tools, start work through a GSD command so planning artifacts and execution context stay in sync.

Use these entry points:
- `/gsd-quick` for small fixes, doc updates, and ad-hoc tasks
- `/gsd-debug` for investigation and bug fixing
- `/gsd-execute-phase` for planned phase work

Do not make direct repo edits outside a GSD workflow unless the user explicitly asks to bypass it.
<!-- GSD:workflow-end -->

<!-- GSD:profile-start -->
## Developer Profile

> Profile not yet configured. Run `/gsd-profile-user` to generate your developer profile.
> This section is managed by `generate-claude-profile` -- do not edit manually.
<!-- GSD:profile-end -->
