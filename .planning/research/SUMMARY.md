# Project Research Summary

**Project:** nosh — QUIC-based roaming remote shell
**Domain:** Network transport / interactive terminal / systems Rust
**Researched:** 2026-05-29
**Confidence:** HIGH

## Executive Summary

nosh is a roaming-tolerant remote shell that routes an interactive PTY over a single QUIC connection on UDP/443, using the user's existing SSH keys for mutual authentication. The M0–M2 milestone is an architecture-validation spike with a hard sequential dependency chain: prove QUIC datagrams and streams coexist (M0), prove SSH-key mutual auth gates the connection at the TLS layer (M1), prove a live PTY session rides the authenticated connection (M2). All four research streams agree this chain is tractable with well-understood Rust crates, and the spike scope is deliberately narrow — Linux only, no roaming, no predictive echo, no forwarding.

The recommended approach is: quinn 0.11.x as the QUIC layer (the only mature async/tokio QUIC crate), rustls 0.23.x as the TLS 1.3 backend, and self-signed-cert key-pinning (custom `ServerCertVerifier` / `ClientCertVerifier`) as the M1 auth path — not RFC 7250 raw public keys (RPK), which are technically available in rustls 0.23.16+ but carry a known compliance caveat (issue #2257) whose current resolution status is unconfirmed. RPK is the right long-term target; cert-pinning is the right spike target. Signing routes through `ssh-agent-client-rs` (synchronous, purpose-built, v1.1.2) so the private key is never handled directly and hardware keys work without extra effort.

The primary risks fall into two categories. First, there are correctness gotchas at the quinn/rustls boundary that silently pass if you do not test for them: datagrams are disabled by default and must be explicitly enabled on both endpoints; keep-alive is off by default and the 30 s idle timeout will kill a quiet interactive session; the custom cert verifier must not stub out `verify_tls13_signature` or it provides no authentication at all; and the TLS 1.3 `CertificateVerify` input is not just the transcript hash. Second, there are three non-negotiable security items that belong in M2, not later: environment-variable injection (strip `LD_*`, `BASH_ENV`, etc.; deny-all whitelist); `SSH_AUTH_SOCK` forwarded via environment (explicit exclusion from the whitelist); and unbounded pre-auth connection state (cap half-open connections, abort on auth timeout).

---

## Key Findings

### Recommended Stack

The stack is well-understood and all versions are verified against current crates.io / docs.rs data. The quinn 0.11.x series requires rustls 0.23.x (via `quinn::crypto::rustls::QuicServerConfig::try_from()`); these two are the non-negotiable core. Everything else follows directly from the design goals. The `rustls-ring` crypto backend is preferred over `rustls-aws-lc-rs` for the spike because ring builds without a C toolchain dependency.

**Core technologies:**

| Crate | Version | Purpose |
|-------|---------|---------|
| `quinn` | 0.11.9 | QUIC transport — only mature async/tokio QUIC crate; RFC 9221 datagram support built in |
| `rustls` | 0.23.40 | TLS 1.3 via quinn's crypto layer; custom verifier traits enable SSH-key pinning |
| `tokio` | 1.52.x | Async runtime (LTS until Mar 2027); quinn's only supported runtime |
| `portable-pty` | 0.9.0 | PTY abstraction (wezterm); `native_pty_system()` already returns ConPTY on Windows for M6 |
| `ssh-key` | 0.6.7 | Parse Ed25519/ECDSA/RSA OpenSSH public keys, `authorized_keys`, `known_hosts` |
| `ssh-agent-client-rs` | 1.1.2 | Synchronous ssh-agent client; matches rustls's synchronous `Signer::sign()` trait |
| `ed25519-dalek` | 2.2.0 | Ed25519 key material (use stable 2.2.0; 3.0.0-pre.7 is pre-release — avoid) |
| `rcgen` | 0.14.8 | Generate ephemeral self-signed X.509 certs for the cert-pinning path |
| `vte` | 0.15.0 | VT/ANSI parser for server-side terminal state model (M2+) |

**Do not use:** `rustls` < 0.23.16 (no RPK), `quinn` < 0.11 (different API), `openssh` crate (spawns subprocess), `tokio-rustls` directly (quinn handles TLS/QUIC integration), `rustls-native-certs` (for PKI chains only), `ssh-agent-lib` (for writing agent servers, not clients).

**Auth path decision — cert-pinning first, RPK as follow-up:**

The cert-pinning path uses `rustls::client::danger::ServerCertVerifier` / `rustls::server::danger::ClientCertVerifier` with custom `verify_server_cert()`/`verify_client_cert()` that extract and compare `SubjectPublicKeyInfo` rather than walking a PKI chain. This is battle-tested, fully supported, and is what quinn's `dangerous_configuration` feature is designed for. The RPK path (`AlwaysResolvesServerRawPublicKeys` / `AlwaysResolvesClientRawPublicKeys`, available since rustls 0.23.16) is the preferred long-term target, but rustls issue #2257 (RPK wire-format non-compliance) was open as of late 2024; its status in 0.23.40 is not confirmed. Start with cert-pinning; validate RPK on a dedicated follow-up branch.

**ssh-agent to rustls signing:** `rustls::sign::Signer::sign()` is synchronous (matching `ssh-agent-client-rs`). The integration pattern is `AgentSigningKey` + `AgentSigner` that open a new socket connection to the agent per signing call. Ed25519 path is straightforward. RSA path requires explicit `SSH_AGENT_RSA_SHA2_256` (flag 0x2) or `SHA2_512` (flag 0x4) in the sign request — the `ssh-agent-client-rs` 1.1.2 public API does not expose these flags; validate at implementation time or fall back to `ssh-agent-lib` for RSA.

**QUIC datagram configuration:** Datagrams are disabled by default in quinn. Both endpoints must set `transport_config.datagram_receive_buffer_size(Some(1 << 20))`. Fail to do this and `conn.max_datagram_size()` returns `None` silently and all datagram sends fail. This is the M0 spike's first test assertion.

**PTY I/O bridge:** `portable_pty::MasterPty::try_clone_reader()` returns a blocking `Box<dyn Read + Send>`. Bridge to tokio with `tokio::task::spawn_blocking` per read chunk (correct and simple for the spike) or `tokio::io::unix::AsyncFd` wrapping the raw PTY fd (lower overhead, valid for M2+).

### Expected Features

**Spike table-stakes (M0–M2 — must exist for the session to be real):**

- QUIC datagram + stream coexistence on one connection (M0)
- SSH-key mutual auth via TLS handshake (M1) — self-signed-cert-pinning path; signing via ssh-agent
- Server-side PTY allocation and login shell spawn (M2)
- Client raw-mode terminal + keystroke delivery + shell output delivery (M2)
- `TERM` propagation and correct initial PTY size (M2)
- Window resize propagation / SIGWINCH to `PtyBridge.resize()` with burst coalescing (M2)
- Exit code propagation: `Child::wait()` to `SessionClose { exit_code }` control frame to client `process::exit()` (M2)
- Clean connection close (ordered QUIC stream FIN + connection close) (M2)
- Env-var sanitization: deny-all whitelist on shell spawn (M2)

**Cheap now, painful later — pull into spike to avoid retrofit:**

- **Server-side session struct**: `{ session_id: Uuid, ssh_identity, pty_handle, shell_pid, idle_since }` defined at M2; M3 cold-reattach wraps it in a `SessionStore` without refactoring the connection handler
- **Exit-code forwarding via explicit `SessionClose` control frame**: define the close protocol now; distinguishes clean exit from network drop in all future features
- **Resize-burst coalescing**: 30–50 ms debounce on `SIGWINCH`; cheap now, entangled with migration logic at M3
- **Structured per-session logging**: one `tracing` span per session with `session_id`, `peer_addr`, `username`; zero cost in release builds
- **Locale / `LC_*` pass-through**: part of the env whitelist; missing it causes non-ASCII corruption immediately

**Deferred differentiators (M3–M7, not in spike):**

- M3: Roaming / QUIC connection migration; cold-reattach (sequence-numbered session)
- M4: Predictive local echo (requires VT state model on client; hardest UX correctness problem)
- M5: Native scrollback, agent forwarding, port forwarding, channel multiplexing, OSC 52
- M6: Windows native (ConPTY — `portable-pty` already abstracts it)
- M7: WebTransport / NAT hole-punch / relay; happy-eyeballs transport selection

**Anti-features (explicitly rejected):**

- Being a terminal emulator (nosh is a transport layer; the local emulator renders)
- Application-layer cipher/algorithm negotiation (TLS 1.3 handles this; negotiation is a downgrade surface)
- 0-RTT (replayable; the one use case is dwarfed by Wi-Fi/DHCP bring-up time)
- Inbound server port range (Mosh model — NAT/firewall-hostile)
- `SSH_AUTH_SOCK` via environment (dedicated agent channel in M5; never env)
- Custom UDP protocol (QUIC RFC 9221 provides this free)

### Architecture Approach

nosh is structured as a Cargo workspace with four crates: `nosh-proto` (shared message types, codec, ALPN constant — no external nosh deps), `nosh-auth` (shared SSH-key verifiers and agent signing key — depends only on proto), `nosh-server` (the `noshd` binary — depends on proto + auth), and `nosh-client` (the `nosh` binary — depends on proto + auth). This layering means protocol changes are reviewable in one place, both binaries share identical key-parsing logic, and the server's PTY spawning logic never leaks into the client binary.

**Major components:**

1. **Transport layer** (`quinn::Endpoint` + `quinn::Connection`) — UDP socket, QUIC connection lifecycle, stream multiplexing, datagram delivery; owns nothing above the wire
2. **Auth layer** (`nosh-auth`) — `SshKeyServerVerifier` (client-side: pins server host key vs `known_hosts`/TOFU), `SshKeyClientVerifier` (server-side: checks client key vs `authorized_keys`), `AgentSigningKey`/`AgentSigner` (delegates TLS `CertificateVerify` signing to ssh-agent); auth happens inside the TLS handshake, before any session code runs
3. **Protocol framing** (`nosh-proto`) — typed `ControlMsg` enum (`Resize`, `Signal`, `EnvVar`, `ShellOpen`, `SessionClose`), `ShellData`, ALPN constant `b"nosh/0"`, length-prefixed codec; seams pre-cut for `DatagramMsg` (M4) and `ChannelOpen/Accept/Reject` (M5)
4. **Session layer** (`ShellSession` in `nosh-server`) — owns the `quinn::Connection` ref, three async tasks (`net_to_pty`, `pty_to_net`, `control_loop`), and the `SanitizedEnv`; the session struct is the seam for M3 cold-reattach (`SessionStore: HashMap<SessionToken, Arc<Mutex<ShellSession>>>`)
5. **PTY abstraction** (`PtyBridge` in `nosh-server`) — wraps `portable-pty`; `spawn()` / `reader()` / `writer()` / `resize()` / `kill()`; `native_pty_system()` call isolated here for the M6 ConPTY swap
6. **Environment sanitization** (`nosh::env` in `nosh-server`) — deny-all whitelist on shell spawn; pure function, testable independently
7. **Terminal proxy** (`TerminalProxy` in `nosh-client`) — raw-mode stdin/stdout, `SIGWINCH` to debounced `ControlMsg::Resize` via `tokio::sync::mpsc`

**Key patterns:**
- Auth-before-session: reject unknown keys inside the TLS handshake; unauthenticated connections never reach session code
- Session as owned async task tree: three `tokio::spawn` tasks per session, selected over in the accept loop; any one completing triggers cleanup of the others
- Blocking PTY I/O via `spawn_blocking`: PTY master fd is not async-native; bridge via `tokio::task::spawn_blocking` (spike) or `AsyncFd` (M2+)
- Session keyed on SSH identity fingerprint, not QUIC connection ID (connection IDs rotate; addresses change on migration)

**Stream / datagram assignment:**

| Channel | QUIC primitive | Why |
|---------|---------------|-----|
| Shell I/O (stdin/stdout) | Reliable bidi stream | Shell output is stateful; ordering required |
| Control (resize, signals) | Reliable bidi stream | Must not be lost or reordered |
| State-sync object (M4) | Unreliable datagram | Latest-wins terminal diffs; loss-tolerant |
| Agent forwarding (M5) | Reliable bidi stream (per request) | Request-response protocol |

**Build order (dependency-driven):**

1. `nosh-proto` — no nosh deps; defines types and codec
2. `nosh-auth` — depends on proto; testable in isolation with test key fixtures
3. Transport skeleton — quinn endpoints, echo over stream, datagram round-trip (M0)
4. Auth wired into transport — mutual key auth, unknown keys rejected (M1)
5. `PtyBridge` + env sanitization — server spawns PTY; env stripped (M2 prep)
6. `ShellSession` tasks wired together — live interactive shell over QUIC (M2)
7. Terminal resize — SIGWINCH to coalesced ControlMsg to PTY resize (M2)

### Critical Pitfalls

**Security footguns — must address this milestone (non-negotiable):**

1. **Env-var injection into the shell** (FOOTGUN-1, M2): passing the client's env dict to `CommandBuilder` allows `LD_PRELOAD`, `BASH_ENV`, `IFS`, etc. to inject code into the shell. Mitigation: deny-all whitelist (`TERM`, `LC_*`, `LANG`, `TZ`, `COLORTERM`, `DISPLAY`); construct server env from scratch; never pass `SSH_AUTH_SOCK`.

2. **`SSH_AUTH_SOCK` forwarded via environment** (FOOTGUN-2, M2): any process on the server that can read `/proc/<pid>/environ` can hijack the agent socket. Mitigation: `SSH_AUTH_SOCK` is absent from the env whitelist. Agent forwarding via dedicated stream is M5.

3. **Unbounded pre-auth connection state** (FOOTGUN-3, M1): CVE-2024-22189 pattern. Cap simultaneous in-progress connections (~256); enforce auth timeout (10 s); use `max_concurrent_bidi_streams` to limit stream proliferation.

**Correctness gotchas — silently pass if not tested:**

4. **Custom verifier stubs out `verify_tls13_signature`** (M1): copying `SkipServerVerification` as the starting point leaves signature verification as `Ok(assertion())` — a MITM can present the correct pinned key but forge the `CertificateVerify` signature. Mitigation: delegate `verify_tls13_signature` to the `CryptoProvider`'s signature verification algorithms; write a test that presents a cert with the right key but a forged signature and asserts rejection.

5. **TLS 1.3 `CertificateVerify` input is not just the transcript hash** (M1): the agent must sign `repeat(0x20, 64) || context_string || 0x00 || transcript_hash` (RFC 8446 §4.4.3), not the raw hash. rustls passes the pre-constructed message to `verify_tls13_signature`; the signer must produce bytes in the same format.

6. **QUIC datagrams silently disabled** (M0): `datagram_receive_buffer_size` defaults to `None`; `conn.max_datagram_size()` returns `None`; all datagram sends fail silently. Mitigation: set `datagram_receive_buffer_size(Some(1 << 20))` on both endpoints' `TransportConfig`; assert `max_datagram_size()` returns `Some(_)` as the first M0 test.

7. **30 s idle timeout kills a quiet interactive session** (M0): `max_idle_timeout` defaults to 30 s; `keep_alive_interval` defaults to `None`. Watching a build or `tail -f` drops the session. Mitigation: set `keep_alive_interval(Some(Duration::from_secs(15)))` on the client side; set `max_idle_timeout` to several minutes. Never set `max_idle_timeout(None)` (causes hung futures on broken paths).

8. **ECDSA curve not checked in custom verifier** (M1): rustls does not enforce that the public key curve matches the claimed `SignatureScheme`. If `ECDSA_NISTP256_SHA256` is in `supported_verify_schemes`, the verifier must explicitly check the key is on P-256.

9. **ssh-agent RSA returns SHA-1 signature** (M1): OpenSSH agent defaults to legacy `ssh-rsa` (SHA-1) unless the sign request explicitly sets flag `0x2` (`rsa-sha2-256`) or `0x4` (`rsa-sha2-512`). `ssh-agent-client-rs` 1.1.2 may not expose these flags — validate at implementation time; test with RSA keys specifically.

10. **ALPN mismatch fails handshake with cryptic error** (M0): QUIC mandates ALPN; `b"nosh/0"` vs `b"nosh"` produces `no_application_protocol` (error 0x178). Mitigation: define a single `ALPN` constant in `nosh-proto`; import on both sides; assert `handshake_data.protocol` post-handshake.

---

## Implications for Roadmap

Based on research, the spike decomposes naturally into three sequential phases with no parallelism possible across the M0→M1→M2 chain.

### Phase 1 (M0): QUIC Transport Skeleton

**Rationale:** The foundational hypothesis — that RFC 9221 unreliable datagrams and reliable bidi streams coexist independently on one QUIC connection — must be proven before any higher-level work. Auth and PTY both depend on a working connection. Nothing can be built in parallel.

**Delivers:** quinn `Endpoint` on both sides; echo bytes over a bidi stream; datagram round-trip; ALPN constant defined in `nosh-proto` and asserted post-handshake; datagrams explicitly enabled and verified via `max_datagram_size()`; keep-alive and idle-timeout set to session-appropriate values.

**Addresses:** QUIC datagram + stream coexistence (primary spike hypothesis); ALPN constant (nosh-proto skeleton); datagram silently disabled pitfall; idle timeout pitfall.

**Avoids:** Datagrams-disabled gotcha (assert `Some(_)` in first test); ALPN mismatch (shared constant from day one); idle timeout drops quiet session (set keep-alive in TransportConfig).

**Research flag:** Well-documented patterns; no deeper research needed.

---

### Phase 2 (M1): SSH-Key Mutual Auth

**Rationale:** Auth is a hard prerequisite for M2. It must complete before PTY work begins because auth-before-session is a fundamental design principle: unknown keys must be rejected inside the TLS handshake, before session code runs. Also addresses FOOTGUN-3 (unbounded pre-auth connections).

**Delivers:** `nosh-auth` crate with `SshKeyServerVerifier`, `SshKeyClientVerifier`, `AgentSigningKey`, `AgentSigner`; `known_hosts` parser + TOFU logic; `authorized_keys` parser; self-signed-cert-pinning path wired into quinn; connection rejected for unknown client keys; `verify_tls13_signature` properly delegated (not stubbed); pre-auth connection cap + auth timeout.

**Uses:** `rustls` 0.23.40 custom verifier traits; `ssh-key` 0.6.7; `ssh-agent-client-rs` 1.1.2; `rcgen` 0.14.8 for ephemeral certs.

**Implements:** Auth layer component; `nosh-auth` crate.

**Avoids (must test explicitly):** Verifier-stubs-signature-check pitfall (negative test: forged `CertificateVerify` must be rejected); TLS transcript bytes wrong pitfall (unit test: known key + known transcript + expected signature); RSA SHA-1 fallback (test with RSA keys specifically); unbounded pre-auth connections (load test).

**Decision point:** Cert-pinning only for this phase. RPK (`requires_raw_public_keys()`) is explicitly deferred until rustls issue #2257 resolution is confirmed on a dedicated follow-up branch.

**Research flag:** The `verify_tls13_signature` delegation pattern and RSA agent flag handling are non-trivial. Plan for a focused implementation spike on the signing path before declaring M1 done. Ed25519-only is acceptable as the first passing state; RSA must be tested before M1 is closed.

---

### Phase 3 (M2): PTY Session Core

**Rationale:** PTY requires an authenticated connection (M1). The "cheap now, painful later" items (session struct, exit-code protocol, env sanitization, resize coalescing) belong here because they become architecturally invasive to retrofit once the session lifecycle has state (M3 reattach, M4 datagram, M5 channels).

**Delivers:** `PtyBridge` wrapping `portable-pty`; `SanitizedEnv` (deny-all whitelist); `ShellSession` with three async tasks; `TerminalProxy` (client raw mode, SIGWINCH to debounced resize); `SessionClose { exit_code, reason }` control frame in protocol; server-side session struct with UUID; structured `tracing` spans per session; interactive shell works (`vim`, `htop`, `bash` readline); resize propagates; exit code propagates; env whitelist enforced.

**Uses:** `portable-pty` 0.9.0; `vte` 0.15.0 (terminal state); `tokio::task::spawn_blocking` for PTY I/O bridge.

**Implements:** Session layer, PTY abstraction, environment sanitization, terminal proxy, protocol message definitions.

**Avoids (must test explicitly):** Env injection (assert `LD_PRELOAD`, `BASH_ENV`, `SSH_AUTH_SOCK` absent from shell env); terminal raw mode not restored (kill client with SIGKILL; verify local terminal usable); zombie PTY children (disconnect mid-session; verify no zombies); SIGWINCH burst storm (drag window; verify coalescing); exit code not propagated (`exit 42`; verify client exits 42).

**Cheap-now items that MUST be in M2 (not deferred):**

- `SessionStore` stub: define the struct and UUID-keyed map even though reattach is not implemented; otherwise M3 requires refactoring the connection handler
- `SessionClose` control frame: define the close protocol now or M3/M5 features cannot distinguish clean exit from network drop
- Resize-burst coalescing: 30–50 ms debounce; entangled with migration logic at M3
- Env whitelist (`LC_*`, `LANG`, `TZ` must be in the allow-list alongside `TERM`)
- Per-session `tracing` spans

**Research flag:** PTY async bridging has well-documented patterns; no research needed. The three-task `ShellSession` structure is standard tokio design.

---

### Phase Ordering Rationale

The M0 → M1 → M2 ordering is a strict dependency chain with no flexibility:
- M0 is a prerequisite for M1 (auth requires a working QUIC connection to wire into)
- M1 is a prerequisite for M2 (PTY session must not run on unauthenticated connections)

Within each phase, the build order from ARCHITECTURE.md enforces a sub-dependency sequence: `nosh-proto` compiles first (no nosh deps), then `nosh-auth` (depends on proto, testable in isolation), then transport skeleton (M0), then auth wired in (M1), then PTY + env (M2 prep), then session tasks wired together (M2).

The "cheap now, painful later" items are explicitly assigned to M2 (not deferred to M3) because they either must precede the first PTY spawn (env sanitization — security non-negotiable) or become refactor-sized changes the moment M3 adds cold-reattach state (session struct, session-close protocol, resize coalescing).

### Research Flags

Phases likely needing deeper research during planning:

- **M1 (ssh-agent signing path):** `verify_tls13_signature` delegation to the `CryptoProvider`, RSA SHA-2 flag handling in `ssh-agent-client-rs` 1.1.2, and the self-signed-cert SPKI extraction pattern are each non-trivial. PITFALLS flags three distinct failure modes here (Pitfalls 5, 7, 8). Plan time for a focused implementation spike on the signing round-trip before declaring M1 done.
- **RPK follow-up (post-M1):** rustls issue #2257 resolution status is unconfirmed. Before attempting RPK migration, run a small research spike to read the 0.23.40 changelog and test `requires_raw_public_keys()` against actual wire behavior.

Phases with standard patterns (skip research-phase):

- **M0 (QUIC transport skeleton):** quinn 0.11 docs are thorough and the `TransportConfig` / `Connection` API is well-understood. Write code, not research.
- **M2 (PTY + session):** `portable-pty` API is straightforward; `spawn_blocking` bridge is idiomatic tokio. The env whitelist is a known list from SSH hardening practice.

---

## Confidence Assessment

| Area | Confidence | Notes |
|------|------------|-------|
| Stack | HIGH | All crate versions verified against docs.rs/crates.io live data. quinn 0.11.9, rustls 0.23.40, portable-pty 0.9.0, ssh-key 0.6.7, ssh-agent-client-rs 1.1.2, ed25519-dalek 2.2.0 confirmed stable. |
| Features | HIGH | Spike features derived directly from PROJECT.md brief. Done checklist is concrete and verifiable. Deferred features mapped from Mosh/ET public record. |
| Architecture | HIGH | Component boundaries, crate layout, data flow confirmed against quinn/rustls/portable-pty current APIs. Build order is dependency-driven with no ambiguity. |
| Pitfalls | HIGH | Transport pitfalls confirmed from quinn 0.11.8 docs (datagram defaults, idle timeout defaults). Security footguns confirmed from CVE record (CVE-2024-22189), RFC 8446 §4.4.3, SSH agent protocol spec. rustls verifier pitfalls confirmed from trait documentation. |

**Overall confidence:** HIGH

### Gaps to Address

- **rustls issue #2257 (RPK wire compliance):** Current resolution status in rustls 0.23.40 is unconfirmed. The cert-pinning fallback removes this from the critical path for M1, but a targeted test is needed before any future RPK migration branch. Handle by: run a dedicated research spike on the RPK follow-up branch before committing to RPK.

- **`ssh-agent-client-rs` RSA flag exposure:** The 1.1.2 public API does not visibly expose the `SSH_AGENT_RSA_SHA2_256` / `SSH_AGENT_RSA_SHA2_512` sign-request flags. Ed25519 is unaffected. Handle by: inspect the 1.1.2 source at implementation time; if flags are not exposed, limit M1 auth to Ed25519 keys initially, add RSA in a targeted follow-up before closing M1, or use the lower-level protocol directly.

- **PTY fd async behavior under load:** `spawn_blocking` is the safe baseline for the spike; `AsyncFd` is lower-overhead but has edge cases on some Linux kernels with PTY fds. Handle by: start with `spawn_blocking`; measure in M2 interactive testing; switch to `AsyncFd` only if profiling identifies it as a bottleneck.

---

## Sources

### Primary (HIGH confidence)

- https://docs.rs/quinn/0.11.9/quinn/ — Transport API, Connection methods, TransportConfig defaults (datagram_receive_buffer_size, max_idle_timeout, keep_alive_interval)
- https://docs.rs/rustls/0.23.40/rustls/ — ServerCertVerifier / ClientCertVerifier traits, CryptoProvider, SigningKey/Signer traits, CertifiedKey, RPK resolvers
- https://docs.rs/portable-pty/0.9.0/portable_pty/ — MasterPty trait, openpty, spawn_command, resize API
- https://docs.rs/ssh-key/0.6.7/ssh_key/ — authorized_keys / known_hosts modules, PublicKey parsing
- https://docs.rs/ssh-agent-client-rs/1.1.2/ssh_agent_client_rs/ — sign() method, list_identities
- https://docs.rs/rcgen/0.14.8/rcgen/ — SigningKey trait, self_signed(), CertifiedKey integration
- https://docs.rs/ed25519-dalek/2.2.0/ed25519_dalek/ — stable release confirmed
- https://quinn-rs.github.io/quinn/quinn/certificate.html — dangerous_configuration pattern, QuicServerConfig::try_from wiring
- RFC 8446 §4.4.3 — TLS 1.3 CertificateVerify input construction (64 spaces + context + 0x00 + transcript hash)
- RFC 9221 — Unreliable Datagram Extension to QUIC

### Secondary (MEDIUM confidence)

- https://github.com/rustls/rustls/issues/2257 — RPK UnsolicitedCertificateTypeExtension compliance caveat; open as of late 2024; resolution in 0.23.40 not confirmed
- https://github.com/rustls/rustls/releases/tag/v%2F0.23.16 — RPK added in 0.23.16 (PR #2062)
- https://eternalterminal.dev/howitworks/ — Session-persistence patterns (used for session-struct seam design)
- https://github.com/haukened/quicshell — Control-first channel model, env sanitization, per-session flow control

### Tertiary (LOW confidence — needs validation at implementation time)

- ssh-agent RSA SHA-2 flag behavior in `ssh-agent-client-rs` 1.1.2 — confirm by reading source; flag 0x2 / 0x4 may require lower-level call
- rustls RPK `requires_raw_public_keys()` behavior in 0.23.40 — confirm by running a targeted test on a follow-up branch

---
*Research completed: 2026-05-29*
*Ready for roadmap: yes*
