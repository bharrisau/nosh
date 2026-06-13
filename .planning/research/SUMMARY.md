# Project Research Summary

**Project:** nosh v1.4 — M7 Remote Access over HTTP/3 + Security Hardening
**Domain:** WebTransport-over-HTTP/3 reverse-proxy mode for a Rust QUIC remote shell, with inner SSH-key mutual auth, proxy-transparent roaming handover, and a deferred security hardening pass
**Researched:** 2026-06-13
**Confidence:** HIGH (stack and architecture grounded in first-party codebase reads and verified crates.io manifests; proxy ecosystem state HIGH from primary sources; tls-exporter channel binding MEDIUM pending rustls API confirmation at implementation time)

---

## Executive Summary

nosh v1.4 adds WebTransport-over-HTTP/3 support so the shell can be reached through a QUIC-terminating HTTP/3 reverse proxy. The central architectural insight — confirmed by all four researchers — is that the primary v1.4 deliverable is **direct WebTransport mode (Mode A)**: nosh binds its own `wtransport` listener on UDP/443 and clients connect without any proxy. This is not a compromise; it is the only production-safe path available today. nginx and HAProxy do not proxy WebTransport connections to upstream backends. Envoy has experimental support only (not covered by Envoy's security team, API unstable). The "reverse-proxy topology" in the milestone brief is therefore scoped as: Mode A ships as the primary path; Envoy as a proxy (Mode B) is an experimental stretch goal for internal/lab use. This scope distinction must be prominent in the roadmap and the SEC-01 threat-model document.

The recommended approach builds in strict dependency order. No WebTransport code can share the session pump with native QUIC until a `NoshTransport`/`NoshSendStream`/`NoshRecvStream` abstraction trait exists — currently all session code is concrete against `quinn::Connection`. That trait is the mandatory prerequisite Phase A. From there, the build sequence is: transport trait and Quinn wrappers → WebTransport endpoint and outer TLS wiring → inner SSH-key handshake (the most security-critical element) → migration handover (which reuses the existing 1-RTT cold-reattach machinery unchanged) → security hardening pass → interactive UAT. The stack risk is low: `wtransport` 0.7.1 uses the same `quinn ^0.11.6` and `rustls ^0.23.23` the workspace already pins, so Cargo resolves a single copy of each with no version conflict. The one build-time trap is crypto-provider feature unification — `wtransport` must be added with `default-features = false` and the `ring` provider pinned explicitly.

The highest-severity risk is the inner-auth channel binding gap (Pitfall WT-3). An inner SSH-key handshake that signs only the challenge bytes — without binding to the outer TLS session's exported keying material — makes a trusted proxy a silent MITM. RFC 9266 `tls-exporter` binding resolves this, but it is a wire-format decision: it cannot be retrofitted after the inner-auth protocol is deployed without a breaking protocol change. The design must be settled before any inner-auth implementation begins. The second risk is the live 999.7 OSC OOM: Phase-16's mitigation was found incorrect in the 999.1 review. Research confirms the Phase-19 prefilter (`osc_prefilter` in `TerminalState::advance`) is the correct fix and is in place; v1.4's task is regression-verification (re-run `oversized_multi_chunk_osc_is_bounded_then_resyncs`, fuzz with increased `max_len`) and confirming the bound generalises to all OSC categories, not just OSC 52 and OSC 0/2. This must land before the server is internet-exposed.

---

## Key Findings

### Recommended Stack

`wtransport` 0.7.1 is the correct and only viable Rust WebTransport implementation for this milestone. Its dependency graph is a clean superset of the existing workspace: it requires `quinn ^0.11.6` (workspace pins 0.11.9 — satisfied), `rustls ^0.23.23` (workspace pins 0.23.40 — satisfied), and `tokio ^1.28.1` (workspace uses 1.52.x — satisfied). There are no version conflicts; Cargo resolves a single copy of quinn and rustls across the whole workspace. The `quinn` feature flag on `wtransport` exposes `Connection::quic_connection()` for reaching through to the underlying quinn connection for diagnostics. `web-transport-quinn` was considered but rejected: it is a single-session-owns-the-whole-QUIC-connection design with no HTTP/3 multiplexing, targeting WASM/browser clients, not a server deployment. `h3-webtransport` is not yet a published production crate.

No new auth crates are needed. The inner handshake reuses `ssh-key`, `ssh-agent-client-rs`, `ed25519-dalek`, and the existing `nosh-auth` verifier/signer logic. The only new workspace dependency is `wtransport 0.7.1`. One known issue to watch before release: wtransport issue #311 (a `time` crate version update breaks the build, filed 2026-06-12) — verify it is resolved before cutting a release.

**Core technologies:**
- `wtransport` 0.7.1: WebTransport-over-HTTP/3 session layer — only mature async-native Rust WebTransport implementation; `with_custom_tls(rustls::ServerConfig/ClientConfig)` escape hatch lets existing nosh-auth configs be reused verbatim; exposes `send_datagram`/`open_bi`/`accept_bi` that map 1:1 to the existing channel model
- `quinn` 0.11.9 + `rustls` 0.23.40: unchanged — wtransport wraps them; no second QUIC library or TLS library introduced
- `ring` (existing crypto provider): must be pinned explicitly when adding wtransport (`default-features = false`) to prevent Cargo feature unification from activating `aws-lc-rs` alongside `ring`, which causes a runtime panic on the first TLS handshake
- Existing `nosh-auth` (`RawEd25519Signer`, `AgentSigner`, `HostKeyVerifier`, `AuthorizedKeysVerifier`, `keys.rs`): reused for inner-auth signing and key verification; minor additions (`extract_spki_from_bytes`, `check_authorized_key` extracted as standalone functions, `TofuPolicy` enum on `HostKeyVerifier`)

### Expected Features

**Must have (v1.4 launch blockers):**
- WebTransport-over-HTTP/3 mode (Mode A — direct, no proxy) with inner SSH-key mutual auth — the headline deliverable; without inner auth, any client reaching the WebTransport endpoint is authenticated
- Inner-auth channel binding via RFC 9266 `tls-exporter` — must be designed into the wire format before implementation; cannot be retrofitted
- Migration handover via 1-RTT cold reattach behind a proxy — reuses existing `run_reattach_session` unchanged; client detects WebTransport session loss, re-connects, re-runs inner auth, then sends `Reattach`
- SEC-01 threat-model document covering the internet-exposed topology, including explicit proxy trust model and the direct-vs-proxy mode distinction
- SEC-02 interactive TOFU fingerprint-confirm prompt — blocking, explicit `yes` required, SHA-256 hex fingerprint displayed, no PTY output until resolved
- 999.7 OSC OOM re-verification — the Phase-19 prefilter is in place; v1.4 must confirm it holds for all OSC categories and survives M7 changes to `TerminalState::advance`
- SEC-04 / 999.2 client trust-boundary hardening — at minimum: per-OSC byte-count gate, OSC 52 clipboard-read rejection, DCS/PM/APC no-op, resize rate-limit, `PtyData` recv cap, channel ID validation

**Should have (competitive/UX):**
- URL-scheme dispatch (`https://` to WebTransport, raw QUIC to native) for transparent protocol selection
- `--trust-key <fingerprint>` and `--strict-host-key-checking` CLI flags for scripting contexts
- Inner session token bound to the SSH identity (proxy cannot replay it)
- Server `--mode webtransport` flag that rejects raw QUIC connections when in proxy-mode deployment

**Defer to v1.x or later:**
- Mode B (Envoy as proxy) — experimental stretch goal only; Envoy WebTransport is "work-in-progress" per Envoy's own docs, not covered by their security team
- NAT hole-punch/relay — explicitly deferred by design
- 0-RTT reattach — measure first; 1-RTT is dwarfed by Wi-Fi/DHCP bring-up
- SSH CA certificate support for host verification — out of scope for MVP
- Browser/web client

**Anti-features (explicitly excluded):**
- L4 UDP passthrough through the proxy — QUIC connection ID routing makes this incorrect; breaks migration
- TCP/TLS fallback — cannot carry RFC 9221 datagrams; defeats the predictive-echo latency model
- Auto-accept TOFU (silent record, current v1.3 behaviour) — a MITM on first contact goes unnoticed; must be replaced by the interactive prompt
- mTLS at the WebTransport outer layer in proxy mode — proxy terminates TLS; inner auth is the correct mechanism

### Architecture Approach

The existing architecture needs one load-bearing seam before any WebTransport code can land: a `NoshTransport`/`NoshSendStream`/`NoshRecvStream` abstraction trait in `nosh-proto`. All session code (`run_session`, `run_reattach_session`, `send_burst`, `build_state_diff`, the `ChannelEvent` layer, the channel tasks) currently uses `quinn::Connection`, `quinn::SendStream`, and `quinn::RecvStream` by concrete type. The trait is a thin I/O boundary — it wraps `send_datagram`, `datagram_send_buffer_space`, `max_datagram_size`, `accept_bi`, `open_bi`, `remote_address`, `close`. Everything above the trait (codec, terminal model, session registry, predictor, scrollback buffer) is untouched. The Quinn and wtransport concrete impls are thin wrappers. Once the trait exists, all existing tests pass unchanged (the Quinn wrapper is a pass-through), and the WebTransport session pump is the same code.

The inner SSH-key handshake is the most significant new component. It is a four-step mutual challenge-response on the control stream, before any `SessionOpen` or `Reattach` frame is processed: server sends `InnerAuthChallenge {server_nonce, server_spki, tls_exported_material}` → client verifies server key against `known_hosts` (triggering TOFU prompt if new), responds with `InnerAuthResponse {client_nonce, client_spki, client_sig}` → server verifies against `authorized_keys`, responds with `InnerAuthComplete {server_sig}` → client verifies server signature. The `tls_exported_material` field in the challenge is the RFC 9266 channel binding; it binds the signature to the specific outer TLS session so a proxy cannot relay the exchange. Migration handover in WebTransport mode is the existing cold-reattach path — no new protocol. Message discriminants 18–21 are appended after `ScrollbackCredit` (17) in strict append-only order.

**Major components:**
1. `NoshTransport` trait (`nosh-proto/src/transport_trait.rs`) — new; the prerequisite for everything else; Quinn and wtransport wrappers live in `nosh-server` and `nosh-client`
2. `WtransportServerConnection` / `WtransportClientConnection` + `run_wt_accept_loop` / `build_wt_server_config` — new; WebTransport endpoint wiring; gated behind `feature = "webtransport"` Cargo flag
3. `inner_auth.rs` (server + client) — new; `run_inner_auth_server` / `run_inner_auth_client`; `InnerAuthChallenge/Response/Complete/Fail` message variants (18–21); reuses existing `nosh-auth` crypto
4. Migration handover in WT mode — modified client reconnect loop: detect WT session loss → reconnect → inner auth → `Reattach`; `run_reattach_session` on the server is unchanged
5. `HostKeyVerifier` + `TofuPolicy` (`nosh-auth/src/verifier.rs`) — modified; interactive blocking prompt for SEC-02; `extract_spki_from_bytes` and `check_authorized_key` extracted to `keys.rs` for inner-auth reuse
6. Security hardening (`nosh-client/src/screen.rs`, `nosh-server/src/terminal.rs`) — modified; OSC byte-count gate, clipboard selection validation, title escape stripping, DCS/PM/APC no-op, resize rate-limit, `PtyData` recv cap

### Critical Pitfalls

1. **Inner-auth channel binding missing (WT-3 — highest severity, wire-breaking if missed)** — a challenge-response without `tls-exporter` material lets a proxy relay the exchange undetected, making it a transparent MITM. The fix — including RFC 9266 exported keying material in the signed challenge — must be a wire-format design decision before any implementation begins. It cannot be retrofitted without a protocol version bump. Verify the rustls API for exporting keying material (`rustls::ConnectionCommon::export_keying_material`) at implementation time.

2. **crypto-provider feature unification (WT-1 — build-time, day-one blocker)** — adding `wtransport` without `default-features = false` can activate `aws-lc-rs` alongside the workspace's `ring` provider. Rustls panics at runtime when two providers are registered. Prevent by adding: `wtransport = { version = "0.7", default-features = false, features = ["runtime-tokio", "ring"] }` and immediately running `cargo tree -f "{p} {f}" | grep rustls` to confirm only `ring` appears.

3. **Message discriminant corruption (WF-1 — silent, session-corrupting)** — inserting any new `Message` variant before `ScrollbackCredit` (discriminant 17) shifts all following discriminants and silently corrupts every existing live connection. All four new inner-auth variants must be appended at positions 18–21. The `message_discriminant_order_is_stable` test must be updated in the same commit as the new variants.

4. **Reattach token transmitted before inner auth completes (MH-1 — session hijack)** — in the WebTransport path, the proxy can read the tunnel before inner auth is proven. The state machine must strictly enforce: `Unauthenticated -> ChallengeExchanged -> Authenticated`, and only in `Authenticated` state accept `SessionOpen` or `Reattach`. `InnerAuthFail` must be fieldless (same as `ReattachErr`) — no oracle for key existence or signature validity.

5. **OSC OOM regression (SEC-1 — availability DoS on multi-user servers)** — the Phase-16 mitigation was wrong; the Phase-19 prefilter is the correct fix and is in place. Any change to `TerminalState::advance` in M7 must re-run `oversized_multi_chunk_osc_is_bounded_then_resyncs` as a mandatory gate. The fuzz target must be re-run with `LIBFUZZER_MAX_LEN=2097152` to catch multi-MiB sequences. This must be confirmed before internet exposure.

Subsidiary pitfalls to address per phase:
- WT-2: WebTransport datagram MTU is smaller than raw QUIC MTU (Quarter Stream ID capsule overhead ~8-20 bytes); use WT session's `max_datagram_payload_size()` not raw `conn.max_datagram_size()`
- WT-4: inner-auth nonce replay — both challenges must be 32 bytes of CSPRNG output, single-use server-side
- WT-5: inner-auth downgrade — server in WT mode must reject raw QUIC; server `--mode` flag mandatory
- MH-2: double-attach race on concurrent same-token reattach — `SessionRegistry::try_reattach` must use atomic `HashMap::remove` (not get-then-remove)
- SEC-2: terminal escape injection from malicious server — audit `TerminalControl` forwarding path; validate clipboard selection field, strip escape bytes from title, decide on OSC 8 URL scheme whitelist
- SEC-3: TOFU prompt fatigue — blocking prompt, explicit `yes` required, SHA-256 hex format, no PTY output until resolved

---

## Implications for Roadmap

Based on combined research, the build sequence below is dependency-ordered and matches the sequence all four researchers converged on independently. The six suggested phases map to the architecture researcher's Phase A-F naming; roadmap phase numbers will be assigned by the roadmapper.

### Phase 1: Transport Abstraction Seam

**Rationale:** Every subsequent phase depends on this. No WebTransport code can share the session pump without the `NoshTransport` trait. This phase has zero functional change — all existing tests must still pass. The Quinn wrapper is a pure pass-through. Gate on `cargo test --workspace` green before proceeding.
**Delivers:** `NoshTransport` / `NoshSendStream` / `NoshRecvStream` traits in `nosh-proto`; Quinn concrete wrappers in `nosh-server` and `nosh-client`; `ChannelEvent::Stream` variant changed to boxed trait streams; `run_session`, `run_reattach_session`, `handle_connection`, `run_channel_task`, `run_scrollback_sender_task` all generic over the trait.
**Addresses:** Prerequisite for all WebTransport work; no features from FEATURES.md yet.
**Avoids:** Future code duplication (400+ lines of session pump would otherwise be duplicated for the WT path).
**Research flag:** Standard patterns — refactoring existing code against a thin trait; no external API research needed.

### Phase 2: WebTransport Endpoint + Outer TLS Wiring (Mode A — Direct)

**Rationale:** This phase proves that `wtransport` 0.7.1 integrates cleanly with the workspace and that the WT session pump works end-to-end. Inner auth is not yet wired — this phase uses a test-only stub that must be gated behind `#[cfg(test)]` only; production builds must reject connections without inner auth. The WT-1 crypto-provider conflict must be resolved on day one of this phase.
**Delivers:** `wtransport` added to workspace (`default-features = false`, `ring` provider pinned); `WtransportServerConnection` and `WtransportClientConnection` `impl NoshTransport`; `build_wt_server_config`, `run_wt_accept_loop`, `build_wt_client_config`, `connect_wt`; `--webtransport` CLI flag on both binaries; `--mode webtransport` server flag that rejects raw QUIC; datagram MTU sizing uses WT session's `max_datagram_payload_size()`.
**Uses:** `wtransport` 0.7.1 `with_custom_tls` builder; existing `rcgen`-generated self-signed cert for direct mode.
**Avoids:** WT-1 (crypto provider), WT-2 (MTU sizing), WT-5 (downgrade — server mode flag), wtransport #285 (prefer bidi streams, avoid relying on uni-stream `finish()`).
**Research flag:** Needs phase research — first wtransport integration; verify `max_datagram_payload_size()` API name on `wtransport::Connection 0.7.1` (docs.rs); confirm wtransport issue #311 (time crate build failure) is resolved; verify `wtransport::ServerConfig` builder pattern with `with_bind_address` + `with_custom_tls` chain.

### Phase 3: Inner SSH-Key Handshake + TOFU Prompt (SEC-02)

**Rationale:** This is the most security-critical phase. The inner-auth channel binding design (RFC 9266 `tls-exporter`) must be settled before a single line of inner-auth code is written — it is a wire-format decision. Once the design is locked, implementation reuses existing `nosh-auth` crypto entirely. `InnerAuthFail` must be fieldless from the start. The TOFU prompt (SEC-02) lands here because it is needed in the WebTransport inner-auth code path (client-side TOFU for the server key in the inner handshake) — doing it alongside the inner auth avoids touching the same code twice.
**Delivers:** `InnerAuthChallenge/Response/Complete/Fail` message variants (discriminants 18-21, appended after `ScrollbackCredit`); `run_inner_auth_server` and `run_inner_auth_client` modules; `extract_spki_from_bytes` and `check_authorized_key` in `nosh-auth/src/keys.rs`; `TofuPolicy` on `HostKeyVerifier`; interactive blocking TOFU prompt (SHA-256 hex, explicit `yes`, blocks `SessionOpen`); inner auth wired into `handle_connection_wt` (server) and the WebTransport connect path (client); `message_discriminant_order_is_stable` test updated.
**Implements:** Inner SSH-key handshake component; SEC-02 interactive TOFU prompt.
**Avoids:** WT-3 (channel binding), WT-4 (nonce replay), WF-1 (discriminant corruption), MH-1 (reattach token before inner auth), SEC-3 (TOFU fatigue).
**Open design question (resolve before implementation):** Confirm the rustls API for exporting keying material from a `quinn::Connection`'s TLS session — specifically `ConnectionCommon::export_keying_material` accessibility through the quinn handshake data. If unreachable, a CSPRNG-nonce fallback (client and server each contribute 32-byte CSPRNG nonces) provides replay protection but not strict channel binding — this limitation must be documented explicitly in SEC-01.
**Research flag:** Needs phase research — tls-exporter availability via quinn/rustls API surface must be verified before implementation; inner-auth wire format is a new design.

### Phase 4: Migration Handover (Cold Reattach over WebTransport)

**Rationale:** The server-side `run_reattach_session` requires no changes — Phase 1 already made it generic over `NoshTransport`. The work is entirely client-side: detect WT session loss, reconnect, re-run inner auth, then send `Reattach`. The `SequencedOutputBuffer` replay, token rotation, and channel re-open are unchanged. The double-attach race (MH-2) must be confirmed atomic before this phase ships.
**Delivers:** Client reconnect loop for WebTransport mode; detection of WT session loss (write error or datagram timeout); re-connection, inner auth, then `Reattach` sequencing; confirmed byte-exact replay from `SequencedOutputBuffer` in WT mode; double-attach race test (concurrent same-token reattach — exactly one session active); token rotated on each successful reattach.
**Avoids:** MH-1 (token before inner auth — state machine enforced), MH-2 (double-attach race — atomic `HashMap::remove`).
**Research flag:** Standard patterns — the reattach machinery is proven from v1.1; this is wiring, not new protocol.

### Phase 5: Security Hardening Pass (999.7 Re-verify + SEC-01 Doc + SEC-04 Client Hardening)

**Rationale:** 999.7 must land before the server is internet-exposed. SEC-04 client hardening closes the remaining client trust-boundary gaps. SEC-01 is a documentation artefact but is a required gate for an internet-exposed release — it must describe the actual deployed topology including the Mode A / Mode B distinction. These are grouped because the SEC-01 document should reference the specific code mitigations being landed in this phase.
**Delivers:** Re-run `oversized_multi_chunk_osc_is_bounded_then_resyncs`; fuzz target re-run with `LIBFUZZER_MAX_LEN=2097152`; audit prefilter against all OSC categories nosh handles; SEC-01 threat-model document (`docs/SECURITY.md`) covering proxy trust model, Mode A vs Mode B topology, inner-auth mandatory status, and residual risks; SEC-04 hardening: per-OSC byte-count gate (applied before `vte::Parser::advance()`), OSC 52 clipboard-read rejection, DCS/PM/APC no-op on `dcs_hook`, resize rate-limit (explicit cap on server-issued resizes), `PtyData` recv cap on client, channel ID range validation, `TerminalControl(Clipboard)` selection field validated against known values, `TerminalControl(Title)` stripped of escape bytes before re-emission.
**Avoids:** SEC-1 (OSC OOM regression), SEC-2 (terminal escape injection), SEC-3 (TOFU prompt usability).
**Research flag:** Standard patterns for 999.7 (existing test infrastructure) and SEC-04 (codebase-grounded analysis); SEC-01 is authored by the team.

### Phase 6: Interactive UAT Clearing

**Rationale:** A human-driven validation pass covering both carried-forward backlog and the new M7 path. Not automated — one item at a time, conversational confirmation before proceeding. No new code unless a gap is found (in which case a gap-closure plan is created and tracked before the phase closes).
**Delivers:** Confirmed: Phase 19 Windows alt-screen re-test (vim over nosh Windows client to Linux server); 999.3 rendering quality; 999.4 `read -s` predictive-echo fix on Windows; green `build-windows` and `cargo audit` CI; WebTransport end-to-end live test (Mode A: nosh binds UDP/443, client connects, TOFU prompt, scrollback, predictive echo, simulated network change triggering reattach); TOFU prompt confirmed blocking and displaying SHA-256 hex; SEC-04 hardening accepted.
**Avoids:** Shipping an unvalidated internet-exposed feature.
**Research flag:** Standard patterns — UAT process is defined; the WebTransport live test environment (a nosh server binding UDP/443 directly, no proxy required for Mode A) is straightforward to set up.

### Phase Ordering Rationale

- Phase 1 before everything: the transport trait is the dependency-zero prerequisite; nothing that touches the session pump in WebTransport mode can land without it.
- Phase 2 before Phase 3: the WebTransport endpoint must exist before inner auth can be wired into it; the outer TLS wiring is also needed to understand what keying material is available for RFC 9266 channel binding.
- Phase 3 before Phase 4: inner auth must be complete before the migration handover can be tested — handover requires re-running inner auth on every new WebTransport session.
- Phase 5 before Phase 6: security hardening must be code-complete before the UAT that validates it; 999.7 must land before the server is declared internet-ready.
- 999.7 within Phase 5, not earlier: the OSC OOM prefilter is already in place from Phase 19; this is a re-verification task, not a net-new fix; grouping it with SEC-04 and SEC-01 keeps the security pass cohesive.

### Research Flags

Phases needing deeper research during planning:
- **Phase 2 (WebTransport endpoint):** First `wtransport` integration; verify `max_datagram_payload_size()` API name on `wtransport::Connection 0.7.1` (docs.rs); confirm issue #311 (time crate build failure) status before pulling the crate; verify `wtransport::ServerConfig` builder pattern with `with_bind_address` + `with_custom_tls` chain.
- **Phase 3 (Inner auth):** Verify rustls `export_keying_material` availability through the quinn `Connection::handshake_data()` path — this determines whether RFC 9266 channel binding is achievable or whether a CSPRNG-nonce fallback is required; the wire format cannot be finalised until this is answered.

Phases with standard patterns (research phase can be skipped or light):
- **Phase 1 (Transport trait):** Pure refactoring of existing code; well-understood Rust trait abstraction pattern; no external API research needed.
- **Phase 4 (Migration handover):** The cold-reattach protocol is proven from v1.1; this is wiring the existing path into the WT reconnect loop; the double-attach race fix is a one-line `HashMap::remove` confirmation.
- **Phase 5 (Security hardening):** The OSC prefilter code and tests exist; SEC-04 items are codebase-grounded; SEC-01 is written by the team based on the design.
- **Phase 6 (UAT):** Process-driven, no novel technical unknowns.

---

## Confidence Assessment

| Area | Confidence | Notes |
|------|------------|-------|
| Stack | HIGH | `wtransport` 0.7.1 dependency graph verified against crates.io manifest; quinn/rustls version compatibility confirmed; crypto-provider conflict mechanism confirmed against rustls issue #1877. One open issue: wtransport #311 (time crate build failure 2026-06-12) — status unknown, verify before pulling crate |
| Features | HIGH | Grounded in the existing nosh codebase (v1.3 source files read directly), verified prior art (ET, Mosh, SSH3 draft), and the CVE research corpus on terminal escape-sequence attacks. WebTransport proxy ecosystem state confirmed from primary sources (nginx maintainer, HAProxy issue tracker, Envoy docs) |
| Architecture | HIGH | Based on reading actual source files (`messages.rs`, `server.rs`, `channel.rs`, `verifier.rs`, `registry.rs`); wtransport 0.7.x API surface verified against docs.rs 0.7.1. The double-await on `wtransport::Connection::open_bi()` is the only API difference from quinn |
| Pitfalls | HIGH | Grounded in first-party security reviews (`999.1-SECURITY.md`, `999.7-SECURITY.md`), codebase reads, and RFC 9266 / RFC 9297. The tls-exporter channel binding pitfall is MEDIUM for the rustls API surface — the attack is well-understood but the implementation path must be confirmed at Phase 3 |

**Overall confidence:** HIGH

### Gaps to Address

- **RFC 9266 tls-exporter API via quinn:** The inner-auth channel binding design depends on being able to export keying material from the outer TLS 1.3 session via the quinn/rustls API. The attack vector is well understood; the rustls API surface (`ConnectionCommon::export_keying_material`) is documented; but accessibility through `quinn::Connection::handshake_data()` in the WebTransport path must be verified at Phase 3 implementation time. If unreachable, a CSPRNG-nonce fallback (both client and server contribute 32-byte CSPRNG nonces) provides replay protection but not strict channel binding — this limitation must be documented explicitly in SEC-01.
- **wtransport issue #311 status:** A time crate version update was filed as breaking the build against wtransport 0.7.1 on 2026-06-12. Verify whether this is resolved before Phase 2 begins.
- **999.7 current state:** The two researchers reached slightly different conclusions. The reconciled position based on PROJECT.md and `docs/999.7-SECURITY.md`: the Phase-19 prefilter (`osc_prefilter` in `TerminalState::advance`) is in place and is the correct fix; v1.4's task is adversarial re-verification (run the named test, fuzz with higher `max_len`, audit the prefilter against all OSC categories) plus confirming the bound generalises. Frame Phase 5 accordingly: re-verify and regression-gate, not net-new implementation — unless the re-verification reveals a gap, in which case a gap-closure plan is created mid-phase.
- **Mode B (Envoy proxy) test environment:** If the roadmapper includes Mode B as a stretch goal, note that a working Envoy configuration with `allow_extended_connect: true` must be provisioned in a lab environment. Do not assume a production Envoy deployment is available.

---

## Sources

### Primary (HIGH confidence)

- `crates/nosh-proto/src/messages.rs` — 18 `Message` variants (discriminants 0-17), append-only invariant, `ChannelType` enum
- `crates/nosh-server/src/server.rs` — `build_server_config`, `handle_connection`, `run_session`, `run_reattach_session`, `send_burst`, `build_state_diff`; all use `quinn::Connection` by concrete type (no trait exists)
- `crates/nosh-server/src/channel.rs` — `ChannelEvent::Stream(quinn::SendStream, quinn::RecvStream)` — the surface area to abstract
- `crates/nosh-auth/src/verifier.rs` — `HostKeyVerifier`, `AuthorizedKeysVerifier`; TOFU silent-record path; SPKI pinning
- `crates/nosh-server/src/registry.rs` — `SequencedOutputBuffer`, `SessionRegistry`; transport-agnostic
- `docs/999.7-SECURITY.md` — OSC accumulation OOM analysis, Phase-16 mitigation-was-wrong finding, regression test reference
- `docs/999.1-SECURITY.md` — Pre-auth security review; residual risks
- `.planning/PROJECT.md` — v1.4 scope, 999.x backlog
- https://crates.io/api/v1/crates/wtransport/0.7.1/dependencies — quinn ^0.11.6, rustls ^0.23.23, tokio ^1.28.1, no h3 dep, upstream quinn not a fork
- https://docs.rs/wtransport/latest/wtransport/connection/struct.Connection.html — `open_bi`, `accept_bi`, `send_datagram`, `receive_datagram`, `max_datagram_size`, `quic_connection()` API confirmed
- https://docs.rs/wtransport/latest/wtransport/config/struct.ServerConfigBuilder.html — `with_custom_tls(TlsServerConfig)` confirmed
- https://community.nginx.org/t/http3-webtransport-webtransport-support-in-nginx/5500 — nginx maintainer: no near-term plan to support WebTransport proxying
- https://github.com/haproxy/haproxy/issues/2256 — HAProxy WebTransport feature request open since Aug 2023; no assignee, no milestone
- https://www.envoyproxy.io/docs/envoy/latest/intro/arch_overview/http/http3 — Envoy WebTransport via `allow_extended_connect`: labelled "work-in-progress," not stable, not covered by security team

### Secondary (MEDIUM confidence)

- https://github.com/rustls/rustls/issues/1877 — ring + aws-lc-rs dual-activation panic mechanism confirmed
- https://github.com/BiagioFesta/wtransport/issues/285 — `finish()` hangs on unidirectional streams — open; prefer bidi streams in nosh-transport
- https://github.com/BiagioFesta/wtransport/issues — Issue #311: time crate build failure (2026-06-12) — open; verify before using 0.7.1 in a release build
- RFC 9266 "Channel Bindings for TLS 1.3" — `tls-exporter` channel binding mechanism; rustls API surface must be verified at Phase 3 implementation time
- RFC 9297 "HTTP Datagrams and the Capsule Protocol" — Quarter Stream ID overhead in WebTransport datagrams (~8-20 bytes)
- dgl.cx/2023/09/ansi-terminal-security — 10+ terminal escape-sequence CVEs (2022-2023); DECRQSS injection, OSC 52 clipboard read, title DoS patterns
- https://francoismichel.github.io/ssh3-spec/draft-michel-remote-terminal-http3.html — SSH3/IETF draft confirming proxy gap as an open problem in the space
- https://github.com/w3c/webtransport/issues/525 — W3C WebTransport proxy issue documents the proxy limitation as an open problem

---
*Research completed: 2026-06-13*
*Ready for roadmap: yes*
