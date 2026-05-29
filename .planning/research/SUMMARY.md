# Project Research Summary

**Project:** nosh — QUIC-based roaming remote shell
**Domain:** v1.1 M3 Roaming + Windows Client (incremental milestone on a working Rust QUIC shell)
**Researched:** 2026-05-30
**Confidence:** HIGH

## Executive Summary

nosh v1.1 adds two orthogonal but architecturally well-understood capabilities to an already working QUIC shell: QUIC connection migration (roaming) and a bounded Windows-client slice. The research finding that changes the implementation order is that `Session.identity` threading is a prerequisite for nearly everything in this milestone — session persistence, reattach authorization, and the per-identity cap all depend on the authenticated peer SPKI being extracted from the TLS handshake and stored in `Session.identity`. This seam was deliberately deferred in v1.0 and must be the first implementation task. Migration, by contrast, requires almost no application code: `ServerConfig::migration(true)` is the effective totality of the QUIC-layer change, and the pump tasks continue transparently.

The recommended build order groups work around its dependencies: identity threading first (unblocks everything), then session persistence and the session state machine (precondition for reattach), then cold reattach protocol (the primary implementation complexity), then connection migration validation (nearly independent, lightest by implementation weight), then Windows client (isolated behind `#[cfg]` gates, can overlap with Phases 3-4 once nosh-auth's FileSigner is stable). The stack requires no new crates beyond a `crossterm` bump from 0.28.1 to 0.29.0 and adding `futures = "0.3"` for EventStream; all other building blocks (uuid, ssh-key, ed25519-dalek, postcard) are already in the lockfile.

The main implementation risk is the cold reattach subsystem: the `SequencedOutputBuffer` ring buffer plus the session state machine (Active → Orphaned → Reconnecting) must be correct before any reconnect logic is exercised, and the two-factor reattach authorization (SSH handshake identity check AND session token) must be designed in from the start — retrofitting the identity check after a token-only implementation requires a protocol change. The secondary risk is Windows platform behavior: resize events arrive as `WINDOW_BUFFER_SIZE_RECORD` (not SIGWINCH), VT processing must be explicitly enabled in legacy console hosts, and the file-permission check cannot read Windows ACLs and must be documented as best-effort.

## Key Findings

### Recommended Stack

The v1.1 stack is additive on top of the locked v1.0 crates. The single dependency change is bumping `crossterm` from 0.28.1 to 0.29.0 (OSC52 support useful for M5; rustix 1.0; no breaking API changes) and adding `futures = "0.3"` for the `EventStream` async trait bound. All session persistence, reattach token generation, and output buffering logic is implemented over existing crates (`uuid`, `postcard`, `bytes`, `VecDeque`). The Windows on-disk key signing path is pure Rust (`ssh-key` 0.6.7 `encryption` feature + `ed25519-dalek` 2.2.0), requires no C build toolchain, and builds cleanly on `x86_64-pc-windows-msvc` once `ring` 0.17.14's precompiled assembly objects are in place.

**Core technologies (v1.1 additions and confirmations):**

- `quinn` 0.11.9: migration is `ServerConfig::migration(true)` on the server; `Endpoint::rebind()` for deliberate interface switches on the client; `Connection::stable_id()` is the correct live-session map key before orphaning. Methods `Connection::migrate()`, `set_path()`, `network_path()`, `path()` do NOT exist — do not search for them.
- `crossterm` 0.29.0: Windows raw mode, resize events, and EventStream; upgrade from 0.28.1; do NOT enable `use-dev-tty` (Unix-only, breaks Windows build with `event-stream` per issue #935)
- `ssh-key` 0.6.7 + `encryption` feature: on-disk private key loading with passphrase decryption for Windows client; gate the `encryption` feature behind `cfg(target_os = "windows")` to avoid inflating the Linux server binary
- `ed25519-dalek` 2.2.0: Ed25519 signing inside `FileSigner` for Windows; already in tree; do not upgrade to 3.0.0-pre (pre-release API)
- `uuid` 1.x v4: session token generation (already in nosh-server); 122 bits of CSPRNG entropy sufficient for reattach tokens
- `rustls-ring` (not `aws-lc-rs`): ring 0.17.14 ships precompiled x86_64-windows assembly — no NASM, no CMake; keep this backend for the Windows build path

### Expected Features

**Must have (table stakes for v1.1 launch):**

- `Session.identity` threaded from authenticated TLS cert — fills the explicit v1.0 seam at `session.rs:119`; prerequisite for all other persistence and reattach features
- Server-side session persistence — PTY + shell survive QUIC connection drop; sessions enter orphaned state; `idle_timeout` default 0 (disabled); per-identity cap (default 5) is the memory bound
- Cold reattach (1-RTT) — `Reattach{token, last_acked_seq}` on the new connection's first stream; server validates identity + token; replays buffered output; 2 RTTs total client-perceived (handshake + 1 message)
- Reattach authorization bound to SSH identity — two-factor: TLS handshake re-runs on every reconnect (same `authorized_keys` check); token is a session selector, not a credential
- QUIC connection migration — headless CI test via `Endpoint::rebind()`; `ServerConfig::migration(true)` set explicitly; real Wi-Fi→cellular is a human live-check complement
- Windows client: VT raw mode and terminal resize — `crossterm::terminal::enable_raw_mode()` on startup; `Event::Resize` via `EventStream`; SIGWINCH handler gated `#[cfg(unix)]`
- Windows client: on-disk Ed25519 key signing — `ssh-key` parse → optional passphrase decrypt → `ed25519-dalek` sign → `FileSigner` implementing `RawEd25519Signer`
- Windows client: TERM and locale propagation — `TERM` defaulting to `xterm-256color`; `LANG=en_US.UTF-8` if unset

**Should have (competitive differentiation):**

- Headless migration CI test — makes the "zero-RTT invisible roaming" differentiator testable vs Mosh (seconds-visible stall) and ET (reconnection dialog)
- Native Windows client without WSL — first among Mosh/ET successors; opens the tool to Windows-first developers
- Reattach authorization via SSH identity (not a session password) — stronger than ET's separate session password; reattach is as secure as the original login

**Defer (post-v1.1):**

- Windows ssh-agent / Pageant integration — named-pipe IPC; out of scope for bounded Windows slice
- Passphrase-encrypted key interactive prompt — P2; unencrypted keys work for v1.1
- 0-RTT reattach — replay-safety burden; gain dwarfed by Wi-Fi/DHCP bring-up; deferred per INIT.md
- Named/numbered session selection — M5+
- Connection status bar — only meaningful with M4 predictive echo latency data

### Architecture Approach

v1.1 inserts a `SessionRegistry` between the accept loop and the per-connection session pump, decoupling the QUIC `Connection` lifetime from the `Session` (PTY + child) lifetime. The `SessionSlot` wraps a replaceable `conn: Option<Connection>`, the existing `Session`, and a new `SequencedOutputBuffer` ring buffer. The `Message` enum gains four variants: `SessionOpened`, `Reattach`, `ReattachOk`, `ReattachErr`. The signing abstraction (`RawEd25519Signer` trait) already exists; v1.1 adds `FileSigner` alongside `AgentSigner`. Platform `#[cfg]` gates are confined to `nosh-client/src/main.rs` (SIGWINCH handler and Windows resize polling); no forks needed in nosh-proto, nosh-auth, or nosh-server.

**Major components:**

1. `SessionRegistry` (`nosh-server/src/registry.rs`, new) — authoritative map of live and orphaned sessions; `open()` / `reattach()` / `mark_idle()` / `remove()` operations; per-identity cap enforced on insert; `Arc<SessionRegistry>` shared into every connection handler task
2. `SessionSlot` (`nosh-server/src/registry.rs`, new) — wraps `Session` + replaceable `quinn::Connection` + `SequencedOutputBuffer`; identity bound at creation; drives the Active → Orphaned → Reconnecting state machine; pump task `AbortHandle`s aborted on disconnect, restarted on reattach
3. `SequencedOutputBuffer` (`nosh-server/src/registry.rs`, new) — monotonic u64 sequence counter; `VecDeque<(u64, Bytes)>` ring bounded at 64 KiB; `push()` archives each outgoing PTY chunk; `since(last_acked_seq)` produces the replay slice for cold reattach
4. Identity threading (`nosh-server/src/server.rs`, modified) — `conn.peer_identity()` downcast to `Vec<CertificateDer<'static>>` → `extract_spki_from_cert` → `nosh_key_from_spki` (exposed from `nosh-auth/src/keys.rs`) immediately after handshake, before any message is read
5. `FileSigner` (`nosh-auth/src/signer.rs`, new) — `RawEd25519Signer` impl for on-disk OpenSSH private keys; `ZeroizeOnDrop` on the key field; narrow scope: load → sign → drop before first `await`

**Recommended build order:**

1. nosh-proto: add 4 new `Message` variants
2. nosh-auth: add `FileSigner`; expose `nosh_key_from_spki`
3. nosh-server: `SessionRegistry` + `SessionSlot` + `SequencedOutputBuffer` (independently unit-testable)
4. nosh-server: identity threading in `handle_connection`
5. nosh-server: reattach dispatch (branch on first message: `SessionOpen` vs `Reattach`)
6. nosh-client: `--identity-file` flag; store `session_token`; send `Reattach` on reconnect; `#[cfg]` gates
7. Migration headless test (`Endpoint::rebind()`)
8. Windows cross-compile check (`cargo check --target x86_64-pc-windows-gnu`)

### Critical Pitfalls

1. **SIGHUP kills shell on client disconnect** — Do NOT close `MasterPty` when the QUIC connection drops. The kernel delivers SIGHUP to the shell session leader when the last open master fd is closed. `MasterPty` must move into `SessionSlot` and remain open for the entire orphan lifetime. This is the core correctness requirement for session persistence and cannot be restructured after the fact.

2. **Reattach token as sole auth factor (session hijacking)** — The reattach flow must run the full mutual TLS handshake on every new connection. The `session_token` is a session selector, not a credential. Both the identity fingerprint match AND the token check must pass. Return the same error for bad-token and bad-identity to prevent oracle enumeration. Never log the token.

3. **`Session.identity` populated before handshake completes** — Call `conn.peer_identity()` only after `connecting.await?` resolves to a `Connection`. Wrap extraction in a constructor that returns `Err` if identity is absent. Make `identity` a non-optional `NoshPublicKey` field, not `Option<T>`, so the type system enforces the invariant.

4. **No per-identity session cap before first orphan** — The cap must exist before the first orphaned session is stored. With `idle_timeout = 0` (the correct default), there is no automatic eviction. Also run a background reaper task calling `child.try_wait()` on all orphaned sessions to prevent zombie accumulation.

5. **`ServerConfig::migration` not set explicitly** — Set it explicitly even though the default is `true`, to guard against future default changes and to document intent. Validate with an integration test: `Endpoint::rebind()` mid-session → assert the active stream continues without `ConnectionError`.

## Implications for Roadmap

Based on the dependency graph in the research, the milestone maps to five phases. Phases 4 and 5 can run in parallel with or after Phase 3 once the nosh-auth boundary is stable.

### Phase 1: Identity Threading

**Rationale:** The explicit v1.0 seam. Session persistence keying, reattach authorization, and per-identity cap all require `Session.identity`. Nothing else in v1.1 can be correctly built without it. The implementation touches three files and is the lowest-risk place to start — it is a seam fill, not new design.

**Delivers:** `Session.identity: NoshPublicKey` populated from the TLS handshake on every new connection; existing handshake tests still pass; `identity` is a non-optional field (type-level invariant).

**Addresses:** Prerequisite for all reattach and persistence features.

**Avoids:** Pitfall 11 (identity captured before handshake completes); precondition for avoiding Pitfall 8 (token-only reattach).

**Research flag:** Standard patterns; no additional research needed. Implementation: `conn.peer_identity()` → downcast → `extract_spki_from_cert` → `nosh_key_from_spki`; expose from `nosh-auth/src/keys.rs`.

### Phase 2: Session Persistence

**Rationale:** Sessions must survive connection drop before there is anything to reattach to. All three correctness requirements (keep MasterPty open to prevent SIGHUP, enforce per-identity cap before first orphan, run zombie reaper) must be in place before this phase is complete. The `SequencedOutputBuffer` must also start accumulating output from session open — it cannot be added retroactively when cold reattach arrives.

**Delivers:** `SessionRegistry` + `SessionSlot`; orphaned sessions survive QUIC disconnect; PTY + shell continue running; per-identity cap enforced; zombie reaper running; all outgoing PTY chunks assigned monotonic u64 sequence numbers from the moment of session open.

**Addresses:** Server-side session persistence; per-identity session cap; configurable idle timeout (default off); output ring-buffer for replay (prerequisite for Phase 3).

**Avoids:** Pitfall 7 (SIGHUP kills shell); Pitfall 5 (unbounded orphan memory); Pitfall 6 (zombie shell processes).

**Research flag:** Standard patterns. `SessionRegistry` and `SequencedOutputBuffer` are fully specified in the architecture research with complete struct definitions. Unit-testable independently of the full server.

### Phase 3: Cold Reattach Protocol

**Rationale:** The highest-complexity deliverable. Requires the state machine (Active → Orphaned → Reconnecting) to prevent the two-clients-one-session race, and requires the two-factor authorization to be correct from the first implementation. The `Message` variants are added early in the build order, but the reattach dispatch and replay logic is the principal new protocol work.

**Delivers:** New QUIC connection from same SSH identity sends `Reattach{token, last_acked_seq}`; server validates identity + token; replays buffered output; session rebound; pump tasks restarted; headless positive test passes; negative test (correct token, wrong key) rejected.

**Addresses:** Cold reattach 1-RTT (table stakes); reattach authorization bound to SSH identity (table stakes); output sequence numbering and replay (table stakes).

**Avoids:** Pitfall 8 (token without SSH re-auth); Pitfall 9 (sequence resync duplicates/gaps); Pitfall 10 (reattach race — two clients one session).

**Research flag:** Needs careful implementation against the state machine spec. The PITFALLS.md "Looks Done But Isn't" checklist has the definitive test matrix for this phase (13 items).

### Phase 4: Connection Migration Validation

**Rationale:** Migration requires almost no production code (`ServerConfig::migration(true)` plus an explicit transport config call). The deliverable is the test suite. Placed after Phase 3 so migration tests run against a server with full session registry — this validates that migration does not disturb the session slot state.

**Delivers:** `ServerConfig::migration(true)` explicit; headless migration test via `Endpoint::rebind()`; large-output-stream test through simulated migration (measures anti-amplification stall); qlog inspection confirming CID rotation on path change; human-verified Wi-Fi→cellular live check.

**Addresses:** Connection migration NAT rebind (table stakes); connection migration explicit path switch (table stakes); migration invisible to user; QUIC migration as headline differentiator over Mosh and ET.

**Avoids:** Pitfall 1 (migration flag not set); Pitfall 2 (anti-amplification stall); Pitfall 3 (CID linkability); Pitfall 4 (keep-alive/idle timeout misconfig during migration).

**Research flag:** Standard patterns; implementation is largely test authorship. Anti-amplification stall duration should be measured empirically in the test environment.

### Phase 5: Windows Client

**Rationale:** Isolated behind `#[cfg]` gates; shares nosh-proto message types and the existing connection/auth path. Can proceed in parallel with Phases 3-4 once Phase 2's nosh-auth boundary (FileSigner, crossterm bump) is stable. Platform-specific work is confined to nosh-client: SIGWINCH gating, Windows resize via EventStream, `FileSigner` behind `cfg(windows)`, `--identity-file` CLI flag.

**Delivers:** Native Windows client (no WSL); `cargo check --target x86_64-pc-windows-gnu` clean; on-disk Ed25519 key authenticated against Linux server; raw mode and resize working in Windows Terminal; TERM and locale propagation; best-effort file permission warning with documented ACL gap.

**Addresses:** All four Windows table-stakes features (raw mode, resize, on-disk signing, TERM/locale).

**Avoids:** Pitfall 12 (private key in memory — narrow scope + ZeroizeOnDrop); Pitfall 13 (ACL gap — document limitation); Pitfall 14 (WINDOW_BUFFER_SIZE_RECORD vs SIGWINCH — use EventStream); Pitfall 15 (VT processing in legacy console hosts).

**Research flag:** Standard patterns for nosh-client integration. Windows-specific behavioral gaps (ACL check, codepage, legacy console host VT processing) are documented in PITFALLS.md; implement to the best-effort level specified there and record all limitations.

### Phase Ordering Rationale

- Identity first because it is the prerequisite seam for both persistence and reattach; building either without it produces an anonymous session that cannot be securely reattached.
- Persistence before reattach because reattach requires an orphaned session to exist, and the `SequencedOutputBuffer` must accumulate from session open.
- Migration validation after reattach because migration tests are more meaningful against a fully-equipped server, and migration is the lightest phase by production code weight.
- Windows in parallel after Phase 2 because `FileSigner` and the `cfg` gates are independent of server-side reattach logic once the auth trait boundary is stable.
- No session-by-name, no 0-RTT, no Pageant — explicitly deferred; these add complexity not validated by this milestone's architecture-validation goal.

### Research Flags

Phases needing closer attention during planning:

- **Phase 3 (Cold Reattach):** State machine correctness and two-factor authorization are the principal risk. Architecture research specifies exact state transitions; PITFALLS.md has a 13-item "Looks Done But Isn't" checklist — use both as acceptance criteria.
- **Phase 5 (Windows Client):** Windows platform behavior (ACL permissions, codepage, legacy console VT processing) has known gaps. Plan for Windows-specific integration tests from the start; Linux CI will not catch Windows behavioral issues.

Phases with standard well-documented patterns:

- **Phase 1 (Identity Threading):** ~30 lines across three files; grounded in verified quinn/rustls API.
- **Phase 2 (Session Persistence):** Standard Rust data structures (VecDeque, HashMap); architecture file has complete struct definitions.
- **Phase 4 (Connection Migration):** One config flag + three test scenarios; test authorship is more effort than the production code change.

## Confidence Assessment

| Area | Confidence | Notes |
|------|------------|-------|
| Stack | HIGH | All crate versions and APIs verified against docs.rs live data. No new crates needed. ring precompiled-assembly Windows claim verified from BUILDING.md. crossterm 0.29.0 + futures are the only dependency changes. |
| Features | HIGH | Derived from INIT.md design brief, Mosh/ET public record, QUIC RFC 9000/9221, and the v1.0 validated codebase. Feature dependency graph grounded in actual code file:line citations. |
| Architecture | HIGH | Every struct, method, and file path grounded in the actual v1.0 codebase with verified line numbers. quinn API surface (`peer_identity`, `stable_id`, `rebind`) verified from docs.rs. |
| Pitfalls | HIGH | SIGHUP/MasterPty behavior is standard Unix; reattach race and sequence-number design have clear documented mitigations; Windows pitfalls verified from official Microsoft and crossterm issue trackers. |

**Overall confidence:** HIGH

### Gaps to Address

- **Windows ACL permission check:** `std::fs::Permissions` on Windows does not read ACLs. Emit a best-effort warning based on `FILE_ATTRIBUTE_READONLY` and document the gap. A proper ACL check (`GetNamedSecurityInfo` via the `windows` crate) is optional scope for v1.1.
- **RPK (RFC 7250) upgrade path:** rustls 0.23.16+ supports raw public keys but v1.0 uses self-signed cert pinning. v1.1 does not require RPK, but the upgrade path should be tracked — it would remove the `rcgen` dependency and simplify the `x509-parser` SPKI extraction. Defer unless the extraction in `handle_connection` proves awkward.
- **Passphrase-encrypted keys on Windows:** The `ssh-key` `encryption` feature supports decryption but the interactive passphrase prompt is not in scope for v1.1. Unencrypted keys work. Document the limitation; prompt implementation is P2.
- **Anti-amplification stall duration in practice:** RFC 9000 §9.4 mandates a 1-2 RTT output stall after migration. Actual severity depends on RTT and burst rate in the test environment. The migration headless test should measure this empirically and gate on "no pause longer than 3 RTTs."

## Sources

### Primary (HIGH confidence)

- https://docs.rs/quinn/0.11.9/quinn/struct.Connection.html — complete method list; `peer_identity`, `stable_id`, `remote_address` verified; absence of `migrate()`/`set_path()`/`network_path()` confirmed
- https://docs.rs/quinn/0.11.9/quinn/struct.ServerConfig.html — `migration()` method and default confirmed
- https://docs.rs/quinn/0.11.9/quinn/struct.Endpoint.html — `rebind()` / `rebind_abstract()` signatures verified
- https://docs.rs/quinn/0.11.9/quinn/struct.TransportConfig.html — `keep_alive_interval`, `max_idle_timeout`, `mtu_discovery_config` verified
- https://docs.rs/rustls/latest/rustls/ — version 0.23.40; `peer_identity` downcast path under rustls-ring backend
- https://docs.rs/ssh-key/0.6.7/ssh_key/private/struct.PrivateKey.html — `read_openssh_file`, `is_encrypted`, `decrypt`, key_data API; `encryption` feature deps (bcrypt-pbkdf, AES-CTR, ChaCha20Poly1305) pure Rust — all verified
- https://docs.rs/crossterm/0.29.0/crossterm/ — `event-stream` feature, `x86_64-pc-windows-msvc` target, `enable_raw_mode` verified; 0.29.0 released 2025-04-05
- https://github.com/crossterm-rs/crossterm/issues/935 — `use-dev-tty` + `event-stream` incompatibility documented
- https://github.com/briansmith/ring/blob/main/BUILDING.md — x86_64-windows precompiled asm objects; no NASM from crates.io
- https://www.rfc-editor.org/rfc/rfc9000.html §9.4 — anti-amplification limit on new paths (3x bytes received)
- https://www.rfc-editor.org/rfc/rfc9000.html §9.5 — CID rotation requirement on migration for privacy
- Codebase (verified line citations): `session.rs:119` identity seam; `server.rs:101/145/185/264` accept loop / handle_connection / run_session / pump select!; `messages.rs` 4-variant Message enum; `signer.rs:26` RawEd25519Signer trait; `keys.rs:172` extract_spki_from_cert; `verifier.rs:218` parse_ed25519_from_spki; `client.rs:27` ClientIdentity::from_signer; `client.rs:208` RawModeGuard; `main.rs:103` SIGWINCH handler; `transport.rs:28` transport_config

### Secondary (MEDIUM confidence)

- https://github.com/mobile-shell/mosh/issues/394 and /806 — Mosh session persistence / orphan issues; UX expectations for idle-timeout-off default
- https://eternalterminal.dev/howitworks/ and https://github.com/MisterTea/EternalTerminal/blob/master/docs/protocol.md — ET BackedReader/BackedWriter sequence-number reattach model; `RETURNING_CLIENT` response; session-password auth model
- Marten Seemann 2023 (https://seemann.io/posts/2023-12-18---exploiting-quics-path-validation/) — PATH_CHALLENGE flood attack; 256-frame cap fix in quinn >= 0.10.4

### Tertiary (LOW confidence)

- https://github.com/rustls/rustls/issues/2257 — RPK compliance caveat; current resolution status unverified
- https://github.com/microsoft/terminal/issues/394 — Windows Terminal `WINDOW_BUFFER_SIZE_RECORD` quirks; behavior may vary across Windows Terminal versions

---
*Research completed: 2026-05-30*
*Ready for roadmap: yes*
