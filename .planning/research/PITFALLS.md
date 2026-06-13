# Domain Pitfalls: nosh v1.4 (M7) WebTransport + Inner Auth + Security Hardening

**Domain:** Adding WebTransport-over-HTTP/3 reverse-proxy mode, inner SSH-key mutual auth, migration handover behind a QUIC-terminating proxy, and a security hardening pass to a working QUIC mobility shell with session persistence, predictive echo, channel multiplexing, and scrollback sync.
**Researched:** 2026-06-13
**Confidence:** HIGH — grounded in the actual codebase (messages.rs, verifier.rs, server.rs, terminal.rs), first-party security reviews (999.1-SECURITY.md, 999.7-SECURITY.md), and the design brief. WebTransport crate compatibility verified against docs.rs/crate/wtransport/latest.

---

## Summary

Five of the pitfalls below are security-critical or silent-corruption class. The inner-handshake channel-binding gap is the most dangerous: skip it and a trusted proxy becomes a MITM with no protocol-visible signal. The wire-format discriminant-stability pitfall is the second: the `Message` enum already has 18 append-only variants (discriminants 0–17) and adding any new variant for the inner-handshake protocol in a non-tail position silently corrupts every existing live connection. The live 999.7 OSC-OOM bug (Phase-16 mitigation reasoning was found incorrect) is an availability DoS that hits multi-user deployments hardest and must be addressed before the server is internet-exposed. The wtransport/quinn crypto-provider conflict is the most likely build-time blocker on day one. The TOFU prompt is the most likely UX footgun: a blocking yes/no prompt that the user cannot paste a fingerprint into trains the wrong habit.

The pitfalls are ordered: security-critical first, then silent-corruption, then build/integration failures, then operational.

---

## Critical Pitfalls

### Pitfall WT-1: wtransport pulls a different rustls crypto feature, panicking at runtime

**What goes wrong:**

The nosh workspace pins `quinn = "0.11.9"` with `features = ["runtime-tokio", "rustls-ring"]` — the `rustls-ring` feature selects `ring` as the rustls crypto provider. `wtransport 0.7.1` depends on `quinn = "^0.11.6"` with default features disabled, and `rustls = "^0.23.23"` also with default features disabled — it does not pin a crypto provider. If either wtransport or any of its transitive dependencies activates `rustls-aws-lc-rs` as a feature, Cargo will enable both `ring` and `aws-lc-rs` backends simultaneously. Rustls panics at runtime when two `CryptoProvider`s are both registered: `install_default()` returns `Err` if called a second time, and code that calls `CryptoProvider::get_default().unwrap()` panics. This does not produce a compile error; the panic surfaces during the first TLS handshake attempt.

**Why it happens:**

Cargo feature unification: features from all crates that depend on a package are merged. If any path in the dependency graph activates `rustls`'s `aws-lc-rs` feature (which is the new default in some rustls configurations), it is silently additive to the existing `ring` activation.

**How to avoid:**

When adding `wtransport` to the workspace, add it with `default-features = false` and explicitly specify the same crypto provider as the rest of the workspace. In `Cargo.toml`:

```toml
wtransport = { version = "0.7", default-features = false, features = ["runtime-tokio", "ring"] }
```

If `wtransport` does not expose a `ring` feature gate of its own (verify this at add-time), add a direct `rustls = { version = "0.23", features = ["ring"], default-features = false }` workspace dependency that forces feature unification toward `ring`. Run `cargo tree -f "{p} {f}" | grep rustls` to confirm only `ring` appears, not `aws-lc-rs`.

**Warning signs:**

Thread 'main' panicked at 'called `Result::unwrap()` on an `Err` value: AlreadyInstalled' during the first connection attempt, or any `CryptoProvider::install_default()` returning `Err`. A `cargo tree --features` that shows `rustls` with both `aws-lc-rs` and `ring` in its feature set.

**Phase to address:** The phase that adds `wtransport` to the workspace (first WebTransport phase, before any networking code is written). Resolve the dependency conflict before writing any code.

---

### Pitfall WT-2: WebTransport datagram size is smaller than raw QUIC — the state-diff encoder assumes raw MTU

**What goes wrong:**

The nosh datagram encoder (`encode_datagram` in `nosh-proto/src/datagram.rs`) uses `conn.max_datagram_size()` from the raw quinn `Connection` to cap `StateDiff` payload sizes. Over raw QUIC on UDP/443, this returns approximately 1200–1350 bytes depending on path MTU. Over WebTransport, the same QUIC path has additional overhead from:

- The HTTP/3 QUIC stream framing (QUIC STREAM frame header: variable-length)
- The HTTP/3 DATA frame header (type + length varint)
- The WebTransport DATAGRAM capsule header (Quarter Stream ID varint, up to 8 bytes)

Per RFC 9297, an HTTP Datagram over QUIC carries a Quarter Stream ID prefix before the payload. This consumes 1–8 bytes of the available QUIC DATAGRAM frame payload. The net effect is that `max_datagram_size()` reported by the WebTransport session is smaller than the raw quinn value — by approximately 8–20 bytes depending on the session stream ID encoding. If the state-diff encoder relies on the raw `Connection::max_datagram_size()` rather than the WebTransport session's datagram size API, it will produce payloads that are slightly too large. Quinn silently drops datagrams that exceed `max_datagram_size` with `SendDatagramError::TooLarge`.

**Why it happens:**

`wtransport` wraps quinn's `Connection` in its `Session` type. The `Session` may expose a separate `max_datagram_payload_size()` that accounts for the capsule overhead. Developers used to calling `conn.max_datagram_size()` on a raw `quinn::Connection` will naturally use the same call on the wrapped type, but the values differ.

**How to avoid:**

When constructing the WebTransport transport adapter, route all datagram size queries through the `wtransport::Session` API rather than the underlying quinn `Connection`. Write a test that encodes a `StateDiff` at the WT session's reported max datagram size, sends it via the WT datagram API, and asserts it arrives without error. Do not assume the raw QUIC MTU figure applies inside a WebTransport session.

**Warning signs:**

State-diff datagrams sent over WebTransport are silently dropped; the client renders stale state indefinitely. `SendDatagramError::TooLarge` returned from the WT datagram send call. The terminal appears frozen while the control stream (reliable) is still alive. The issue is intermittent when near the MTU boundary and consistent when the path MTU is low (e.g. PPPoE with 1480-byte Ethernet frames, reducing QUIC DATAGRAM space to ~1380 bytes before WT overhead).

**Phase to address:** WebTransport integration phase, before datagram plumbing is wired. Write the MTU sizing unit test first.

---

### Pitfall WT-3: Inner-handshake channel binding is missing — the proxy becomes a transparent MITM

**What goes wrong (security-critical — prioritise this pitfall):**

The current auth design puts mutual SSH-key authentication inside the TLS 1.3 handshake (the outer QUIC connection). The `AuthorizedKeysVerifier` and `HostKeyVerifier` verify each other's SPKI during `CertificateVerify` in TLS. Behind a WebTransport proxy, the proxy terminates the outer QUIC/TLS connection. The nosh server establishes a new inner QUIC/TLS connection to the actual nosh server daemon. The proxy sees all bytes flowing over the WebTransport tunnel.

When the inner-auth handshake is a simple challenge-response SSH-key signature without channel binding, the following attack is possible:

1. An adversary controls the proxy (or compromises it).
2. The client connects to the proxy (outer TLS — terminated at the proxy, establishing a `tls-exporter` value C1).
3. The proxy connects to the real nosh server (inner TLS — establishing a different `tls-exporter` value C2).
4. The client sends its inner-auth challenge-response in plaintext over the WebTransport tunnel.
5. The proxy relays the client's challenge to the real server and relays the server's challenge to the client.
6. The client signs the server's challenge (bound only to the challenge bytes, not to the outer TLS session). The proxy relays the signature.
7. The server authenticates the client successfully — to the wrong outer session.
8. A second client can now be attached to the server session that the first client authenticated.

This is the classic confused-deputy attack on inner-auth without channel binding. The outer TLS provides confidentiality from the network, but the proxy is trusted for confidentiality and is simultaneously the attacker.

**Why it happens:**

Application-level handshakes naturally sign only the application-visible challenge, not the outer transport session. The TLS 1.3 `tls-exporter` mechanism (RFC 9266) exists specifically to bind application-level auth to a specific TLS session, preventing this attack. Without it, the inner handshake is portable across outer sessions.

**How to avoid:**

The inner-auth challenge must incorporate the outer TLS session's exported keying material (`tls-exporter`) as a nonce component. Specifically:

1. Export keying material from the outer TLS 1.3 session (the WebTransport connection) using `rustls::ExportedKeyingMaterial` (or the equivalent quinn/rustls API). The label should be nosh-specific (e.g. `"nosh-inner-auth-v1"`), the context empty.
2. The challenge the server sends to the client (and the challenge the client sends to the server, for mutual auth) must include this exported key material as a non-negotiable field.
3. The client's SSH-key signature covers `challenge || tls_exported_material`, not `challenge` alone.
4. The server verifies the signature against the same exported material from its own outer TLS session.

If the proxy is honest, client and server see the same exported material (they share the same TLS session from the proxy's perspective — but this is the case only if the proxy is not intercepting; a MITM proxy will have different exported values on each leg, causing verification failure). This provides binding.

The implementation must also bind the server's inner-auth response to the same exported material, for symmetric protection.

**Warning signs:**

Inner-auth handshake that signs only `challenge_bytes` (a CSPRNG nonce) without any outer-session material. An inner-auth implementation that works identically whether connected directly or through a proxy — this means it has no proxy-binding. A confused-deputy attack is invisible at the protocol level unless binding is checked.

**Phase to address:** Inner-auth design phase (the first phase that defines the inner-handshake wire protocol). Channel binding must be designed in before any implementation begins. It cannot be retrofitted without a wire-breaking protocol change.

---

### Pitfall WT-4: Inner-auth challenge replay — nonce reuse or predictable nonces enable signature replay

**What goes wrong:**

The inner-auth handshake is a challenge-response exchange outside TLS (or inside a TLS whose purpose is tunnelling, not auth). The server generates a challenge; the client signs it. If the challenge is short, predictable, or reused across sessions, an attacker who observes one successful handshake can replay the client's signature in a future session against the same challenge. Even with unique nonces, if the challenge is not bound to the session (see Pitfall WT-3), the replay can be directed at a different session.

A subtler form: the client sends its own challenge to the server for server-auth. If that challenge has lower entropy than the server's challenge — for example, a monotonic counter rather than CSPRNG bytes — the client-to-server auth is weaker and may be predictable.

**Why it happens:**

Inner-auth implementations reuse challenge generation patterns from simpler systems (e.g. a 32-bit counter, a timestamp, a hash of the connection address). These look random but have low entropy or are guessable.

**How to avoid:**

Both challenges (client→server and server→client) must be 32 bytes of CSPRNG output (use `rand::rngs::OsRng` or `ring::rand::SystemRandom`). They must be single-use: the server must not accept the same challenge twice within a session or across sessions for the same identity. The challenge must include the outer `tls-exporter` material (see Pitfall WT-3) so replays across sessions are impossible even if the nonce reused. Do not derive the challenge from the connection address, timestamp, or any observable value.

**Warning signs:**

A challenge field shorter than 32 bytes. A challenge derived deterministically from any connection-observable value. No monotonic-use check for challenges on the server side. An inner-auth that passes unit tests when the server is restarted between each test (single-use is not verified).

**Phase to address:** Inner-auth design phase — challenge generation must be specified with entropy requirements before implementation.

---

### Pitfall WT-5: Inner-auth downgrade — the client accepts direct QUIC when WebTransport was expected

**What goes wrong:**

If the client is configured to use the WebTransport mode (reverse-proxy path), but the server also accepts raw QUIC connections on the same port, a MITM or misconfiguration can cause the client to connect directly without going through the proxy. The client performs the outer TLS auth (SPKI-pinned against `known_hosts`) directly with the server, bypassing the proxy's ACLs, firewall rules, and access logging. The inner-auth layer never runs because the outer TLS auth succeeds and there is no inner-auth enforcement on the raw path.

This also manifests as a configuration error: the operator intends the server to be reachable only via the proxy but forgets to firewall the direct QUIC port, leaving it open.

**Why it happens:**

The nosh server currently accepts raw QUIC connections. When WebTransport mode is added, the server may be left running both listeners without a clear mode switch or firewall enforcement. The client has no in-protocol mechanism to verify it connected through the expected proxy topology.

**How to avoid:**

The server should have an explicit `--mode` or configuration flag: `raw-quic` (current), `webtransport` (new), or `both` (explicitly opt-in, not default). In `webtransport` mode, the server should not accept raw QUIC connections at all — only WebTransport upgrades. Document the required firewall rule (block raw UDP/443 from the internet, allow only from the proxy IP) in the operator guide. The threat model document (SEC-01) must cover this misconfiguration.

**Warning signs:**

Server accepts connections on the raw QUIC path even when configured for WebTransport. Client successfully authenticates directly when the proxy is down. No `--mode` flag exists.

**Phase to address:** WebTransport integration phase (server mode flag), and SEC-01 threat model document (misconfiguration coverage).

---

### Pitfall WF-1: Message discriminant corruption — new inner-auth variants inserted at non-tail positions

**What goes wrong (silent-corruption — security-critical):**

The `nosh-proto` `Message` enum currently has 18 variants at discriminants 0–17 (postcard encodes enums by source-order position). The v1.3 codebase has two `// APPEND-ONLY — do NOT insert or reorder` comments and a `message_discriminant_order_is_stable` test. The inner-auth handshake for WebTransport mode will require new message types: at minimum an `InnerAuthChallenge` (server→client), `InnerAuthResponse` (client→server), and `InnerAuthOk`/`InnerAuthErr` pair. The design pressure is to add these near the top of the enum (logically they come "before" session open) or near the `Reattach`/`ReattachErr` variants (they are conceptually related). Placing them anywhere but after `ScrollbackCredit` (discriminant 17) will shift all following discriminants and silently corrupt every old client that tries to decode a message from a new server — `PtyData` will decode as `InnerAuthChallenge`, `Reattach` will decode as something else, and sessions will silently misbehave or hang.

**Why it happens:**

A new developer sees `Reattach` at discriminant 5 and thinks "inner auth is also a form of session open, so it belongs near Reattach" — and inserts it at position 5. The compiler accepts it. The discriminant stability test catches it only if it is running in CI and the expected values are still hardcoded. If the test was not updated to include the new expected discriminants, it will catch the shift only if a NEW variant happened to land at an old variant's expected byte — easily missed in a busy PR.

**How to avoid:**

All inner-auth message variants MUST be appended after `ScrollbackCredit` (currently discriminant 17). The `message_discriminant_order_is_stable` test must be updated to include the new variants with their expected discriminant bytes before any new variant is merged. The append-only comment block must be extended to name `ScrollbackCredit` as the current tail. Consider extracting the inner-auth messages into a separate `InnerAuthMessage` enum encoded on a separate channel, so they never pollute the main `Message` enum and the discriminant stability invariant is localised. This is the cleaner long-term design.

**Warning signs:**

A PR that inserts any `Message` variant between two existing variants. A PR that adds variants without updating `message_discriminant_order_is_stable`. Any live connection where one peer is on a new build and the other is on the old build and messages are decoded as the wrong variant (manifests as unexpected disconnects, garbled session output, or the session hanging immediately after auth).

**Phase to address:** Inner-auth design phase (before any new Message variants are defined). The discriminant stability test update must be part of the same commit as the new variants.

---

### Pitfall MH-1: Session-fixation via reattach-token theft across the proxy boundary

**What goes wrong:**

The existing cold-reattach protocol uses a 16-byte CSPRNG token (single-use, rotated on every successful reattach) that is bound to the SSH identity via the TLS handshake. In the raw QUIC topology, the reattach token is transmitted only inside the TLS-encrypted QUIC stream — a passive attacker cannot read it, and an active attacker cannot use it without also owning the SSH private key (the TLS handshake requires the SSH key for the new connection's `CertificateVerify`).

In the WebTransport topology, the outer TLS is terminated at the proxy. The proxy sees the cleartext WebTransport tunnel. If the inner-auth handshake is not complete before the reattach token is transmitted, or if the token is transmitted over the WebTransport session before the inner auth is proven, the proxy (or any component that can read the WT tunnel) can read the reattach token and use it to steal the session on a different raw-QUIC or WebTransport connection.

**Why it happens:**

The temptation is to reuse the existing `Reattach` message unchanged in the WebTransport path, since the session-resume logic is the same. But the security invariant of the existing token — "only someone with the SSH private key can use this token" — relies on the outer TLS for confidentiality. Inside a WT tunnel, the outer TLS is at the proxy, not end-to-end.

**How to avoid:**

The reattach token MUST NOT be transmitted in the WebTransport session until the inner-auth handshake is complete and the inner session is cryptographically authenticated. The protocol sequencing must be:

1. WebTransport outer session established.
2. Inner-auth handshake runs (mutual SSH-key challenge-response with outer TLS binding).
3. Only after `InnerAuthOk`, the client may send `Reattach` or `SessionOpen`.

Additionally, for the WebTransport path, the reattach token should be bound not only to the SSH identity but also to the outer TLS session's exported key material (same as the inner-auth binding in Pitfall WT-3), so a stolen token cannot be replayed on a different outer session.

**Warning signs:**

The client sends `Reattach` before an `InnerAuthOk` is received. The server accepts `Reattach` before the inner-auth state machine reaches `Authenticated`. A test that replays a captured `Reattach` token on a different WT session and succeeds.

**Phase to address:** Inner-auth design phase (state-machine sequencing). Must be designed in; cannot be patched after the fact without a protocol change.

---

### Pitfall MH-2: Double-attach race — two WebTransport clients reattach to the same orphaned session simultaneously

**What goes wrong:**

An orphaned session can be in a `Reconnecting` state for up to the idle timeout (300 s). In the raw QUIC topology, two simultaneous reattach attempts from different connections are arbitrated by the `SessionRegistry` — the second one loses because the registry marks the session as active on the first successful attach. In the WebTransport topology, two clients behind different proxy connections can simultaneously send `Reattach` with the same token. The token is single-use, but if the server's token invalidation is not atomic (check-then-invalidate is not a single critical section), both clients can win the check and both proceed to the reattach session — resulting in two live sessions driving the same PTY, which produces corrupted output for both.

**Why it happens:**

The `SessionRegistry` may use a `Mutex`-guarded `HashMap`, which is correct for the raw QUIC path (each connection runs in its own tokio task and the mutex serialises token checks). Over WebTransport, if the server-side WT session handling spawns a separate tokio task per WT session (the likely design), two tasks can contend on the registry lock. The bug is that "check token validity" and "invalidate token" must be a single atomic operation inside the lock.

**How to avoid:**

The token check-and-invalidate in `SessionRegistry::try_reattach` must be a single `HashMap::remove` call inside the mutex guard — remove the token from the map, returning it if present. If `remove` returns `None`, the token was already used. This is already the correct pattern for the raw QUIC path; confirm it holds when the WebTransport path is wired in. Write a test that fires two concurrent reattach attempts with the same token and asserts only one succeeds.

**Warning signs:**

Two clients successfully reattaching to the same session. `SessionRegistry::try_reattach` that calls `get` (to check) then `remove` (to invalidate) in two separate steps — classic TOCTOU. Intermittent test failure in a concurrent-reattach test.

**Phase to address:** Migration handover phase. The registry atomicity must be confirmed (not assumed) before the WebTransport reattach path is wired in.

---

### Pitfall SEC-1: OSC-OOM live bug — 999.7 mitigation is in place but must survive M7 changes

**What goes wrong:**

The `docs/999.7-SECURITY.md` documents that the Phase-16 mitigation reasoning was incorrect: `OSC_52_MAX_BYTES` and `MAX_TITLE_BYTES` run in `osc_dispatch` (after vte has already buffered the full sequence), not before vte accumulates. A multi-chunk giant OSC sequence grows vte's internal `osc_raw` `Vec<u8>` without bound until OOM. Phase 19 Plan 03 added an `osc_prefilter` in `TerminalState::advance` with a 1 MiB cap and a parser resync on overflow. This was the correct fix.

The OOM bug is closed for v1.3. The risk for v1.4 is regression: if any M7 phase modifies `TerminalState::advance`, the channel layer, or adds a new OSC type to `TerminalControlPayload`, the prefilter logic (shadow state `in_osc` / `osc_byte_count`) may be invalidated. Specifically:

- Adding a new OSC category that the prefilter does not account for (e.g. OSC 1337 for iTerm2 image protocol) passes through the prefilter uncapped if the prefilter does not recognise the sequence as an OSC.
- Changes to the multi-chunk delivery path (e.g. the new WebTransport reliable stream delivering PTY output in larger chunks) change the chunk boundaries that the prefilter sees, potentially breaking the `in_osc`/`osc_byte_count` shadow state if it assumes chunk-aligned OSC sequences.

The fuzz target `osc_accumulation` exists but its `max_len` default (4 096) is too small to catch multi-MiB accumulation. This is documented in 999.1-SECURITY.md §4 footnote 1.

**How to avoid:**

Before the 999.7 re-check phase ships, run the named regression test (`oversized_multi_chunk_osc_is_bounded_then_resyncs`) and the fuzz target with increased max_len (e.g. `LIBFUZZER_MAX_LEN=2097152`) to confirm the prefilter still holds. Any change to `TerminalState::advance` or the OSC dispatch path must re-run this test as a mandatory gate. The 999.7 re-check phase should audit the prefilter against all OSC categories nosh handles, not just OSC 52 and OSC 0/2.

**Warning signs:**

`TerminalState::advance` changes that do not re-run `oversized_multi_chunk_osc_is_bounded_then_resyncs`. Adding a new `TerminalControlPayload` variant without checking whether its corresponding OSC sequence is pre-filtered. Server RSS growing under a session that emits large OSC sequences.

**Phase to address:** 999.7 OSC OOM re-check phase (dedicated). The re-check must be adversarial — probe the specific claim that the prefilter shadow state is correct for all OSC sequence boundaries, not just the single test case.

---

### Pitfall SEC-2: Terminal escape injection from a malicious server — client trust boundary

**What goes wrong:**

The 999.2 client trust-boundary backlog covers this class. In the current architecture, the server sends `PtyData` (raw PTY bytes) and the client re-emits them directly to the local terminal (stdout). A malicious or compromised server can send arbitrary ANSI/VT sequences to the client terminal, including:

- **OSC 52 write:** inject content into the client's clipboard without user interaction (already gated by the server's OSC 52 passthrough; but the client re-emits the forwarded payload without sanitisation of the `selection` field).
- **OSC 8 hyperlinks:** inject a link to a `file://` or `shell:` URI that executes code when the user clicks it in a terminal that supports clickable links (e.g. iTerm2, Windows Terminal).
- **Title injection (OSC 2):** set the terminal window title to a command that looks like it comes from the shell — used in social engineering.
- **Clipboard poisoning via OSC 52 followed by a crafted prompt:** classic clipboard-injection attack; the user pastes a malicious command thinking it came from a legitimate session.
- **`\r` + overwrite:** the server sends a line ending in `\r` (carriage return without newline), followed by content that overwrites the displayed prompt — the user sees a fake prompt but the clipboard or terminal title contains something different.

The attack surface is particularly wide in the WebTransport topology because the proxy may be internet-facing and the nosh server may be trusted to relay bytes from arbitrary applications running in the session.

**Why it happens:**

Remote shell protocols traditionally trust the server fully — the server is the user's own machine. nosh's mobility model changes this: the server may be a shared or cloud-hosted machine, and the connection goes through an internet-facing proxy. A compromised application on the server has a direct byte channel to the client terminal.

**How to avoid:**

The 999.2 phase must audit each category of outbound data from server to client and apply the minimum trust for each:

- `PtyData`: cannot be sanitised without breaking legitimate terminal apps. Define an explicit "malicious server" threat model: nosh trusts the server to the same degree as SSH. Document this boundary in SEC-01.
- `TerminalControl(Clipboard)`: the server-side filter (`osc_dispatch` drops the query form) already exists. Confirm the client does not re-emit the selection field raw — it must validate the selection is a known value (`c`, `p`, `s`, etc.) before forwarding.
- `TerminalControl(Title)`: the title is already bounded to 1 KiB. Confirm the client strips `\x1b`, `\x07`, `\r`, `\n` from the title before re-emitting — a title containing an OSC terminator can break out of the title sequence on some terminals.
- OSC 8 hyperlinks: explicitly decide whether to pass through or strip. If the server signals `OSC 8 ; ; <URL> ST`, confirm the URL scheme whitelist (allow `https://`, deny `file://`, `shell:`, `ms-appx:`).

**Warning signs:**

`TerminalControl(Clipboard{selection: ..})` re-emitted without validating the `selection` bytes. `TerminalControl(Title{title: ..})` re-emitted without stripping escape bytes. Any OSC 8 sequence arriving from the server that is not explicitly handled. The client trust-boundary analysis in 999.2 not covering the `TerminalControl` forwarding path.

**Phase to address:** 999.2 client trust-boundary hardening phase. The SEC-01 threat model must define the "malicious server" boundary explicitly.

---

### Pitfall SEC-3: TOFU prompt fatigue — silent accept or non-blocking prompt trains the wrong habit

**What goes wrong:**

SEC-02 requires an interactive TOFU fingerprint-confirm prompt on first contact. The failure mode is a prompt that:

1. Does not display the fingerprint in a human-verifiable form (SHA-256 hex or randomart — not a raw base64 dump that no one reads).
2. Defaults to "accept" on empty input (pressing Enter once accepts the key — the SSH pattern that most users follow without reading).
3. Is non-blocking (fires in the background while the session starts, so the user cannot interrupt the session if they reject the key).
4. Does not pause the session until the user responds (the terminal starts rendering PTY output over the fingerprint prompt, making it unreadable).

All four failure modes produce the same behavioural outcome: the user accepts every key without verifying it, which provides no security benefit over silent TOFU (the v1.3 current behaviour).

**Why it happens:**

Developers model the prompt on the SSH client's `Are you sure you want to continue connecting (yes/no)?` pattern, which also defaults to explicit `yes`, but SSH users have trained to type `yes` reflexively. The goal is to make verification easy, not to add friction that is immediately bypassed.

**How to avoid:**

The prompt must:
- Display the fingerprint as SHA-256 hex (matches `ssh-keygen -l -E sha256` output so users can verify against a known value) and optionally randomart.
- Require the user to type `yes` explicitly — no empty-input default.
- Block session establishment until the user responds — no PTY output until the prompt is answered.
- On rejection, close the connection cleanly (not leave a half-open session).
- Be suppressed (no prompt) if `StrictHostKeyChecking=yes` equivalent is configured, returning an error instead.

The prompt must be rendered to the terminal before any PTY output is received, which means the session open sequence must not send `SessionOpen` until after the TOFU prompt resolves.

**Warning signs:**

Prompt that accepts empty input (just Enter). PTY output arriving before the prompt has been answered. The fingerprint displayed as a raw base64 string. The prompt rendered in a way that is overwritten by PTY output during the session.

**Phase to address:** SEC-02 interactive TOFU phase.

---

## Technical Debt Patterns

| Shortcut | Immediate Benefit | Long-term Cost | When Acceptable |
|----------|-------------------|----------------|-----------------|
| Reuse `Message` enum for inner-auth messages | No new enum, simpler implementation | Every inner-auth protocol change risks discriminant-shift bugs on the main protocol wire | Never — extract inner-auth to a separate `InnerAuthMessage` enum on a separate channel |
| Skip channel binding in inner auth | Simpler inner-auth implementation | Proxy MITM is undetectable; inner auth provides no security over no auth | Never — channel binding is the entire point of the inner handshake |
| Trust `X-Forwarded-For` for rate limiting | Simple implementation | XFF is trivially spoofable by any client; rate limits and IP-based caps are bypassed | Never for security decisions; only for non-security logging |
| Inner auth without nonce freshness guarantee | Reuse existing reattach token as challenge | Replay attacks become possible; prior observed signatures can be replayed if the nonce is predictable | Never |
| Emit raw `TerminalControl` payload to terminal without sanitisation | Simpler client forwarding code | Malicious server can inject OSC sequences that poison the clipboard or execute code | Never for OSC 8 URLs; acceptable for OSC 52 data if selection field is validated |
| Silent TOFU (auto-accept, no prompt) | No UX friction | The entire TOFU security model is bypassed; provides the same security as no host key verification | Never for new connections; acceptable only when `StrictHostKeyChecking=no` is explicitly set |
| Defer OSC-OOM 999.7 re-check to a later milestone | Less scope this milestone | The OOM is a live post-auth DoS on multi-user servers, especially relevant after internet exposure | Never — must be re-verified before internet exposure |

---

## Integration Gotchas

| Integration | Common Mistake | Correct Approach |
|-------------|----------------|------------------|
| `wtransport` + `quinn` in same workspace | Using raw `Connection::max_datagram_size()` inside a WT session for payload sizing | Use the WT `Session` API's datagram size method, which accounts for capsule overhead |
| `wtransport` + `rustls-ring` | Adding `wtransport` without specifying `default-features = false`, pulling in `aws-lc-rs` alongside `ring` | Add with `default-features = false` and pin the crypto provider explicitly |
| Inner-auth + existing `HostKeyVerifier` | Reusing the outer-TLS `HostKeyVerifier` for the inner-auth server identity check | The inner-auth server check needs a different code path — the outer TLS is at the proxy, the inner auth verifies the nosh server daemon's key against `known_hosts` using the inner challenge |
| Reattach token + WT session | Transmitting the reattach token before inner auth completes | Strict sequencing: `InnerAuthOk` → `SessionOpen`/`Reattach`; the token must never travel before inner auth is complete |
| `X-Forwarded-For` + rate limiting | Keying the pre-auth half-open cap on the XFF header value | Key the cap on the actual source IP of the QUIC connection (the proxy's IP); use XFF only for logging, never for security caps |
| `wtransport` session SETTINGS negotiation | Starting to send nosh data before the WT SETTINGS exchange (`SETTINGS_WT_ENABLED`) is complete | The `wtransport` crate handles SETTINGS internally; wait for `ServerConnection::accept_session()` to complete before starting the nosh session pump |
| Inner-auth + `Reattach` sequencing | Sending `Reattach` on a new WT session before the inner auth state machine reaches `Authenticated` | State machine must enforce: `Unauthenticated` → `ChallengeExchanged` → `Authenticated`, and only in `Authenticated` state accept `SessionOpen` or `Reattach` |

---

## Performance Traps

| Trap | Symptoms | Prevention | When It Breaks |
|------|----------|------------|----------------|
| Inner-auth blocking the tokio event loop | Every new WT session stalls; CPU pegged during handshake bursts | The inner-auth challenge signature (ssh-agent call) is synchronous — wrap in `tokio::task::spawn_blocking` as in the existing TLS `AgentSigner` path | Under > 10 simultaneous new WT sessions |
| Datagram bursting over WT sending too large | State-diff datagrams silently dropped; terminal frozen | Cap burst payload to WT session's `max_datagram_payload_size()`, not raw QUIC MTU | Any time path MTU is near 1280 bytes (minimum QUIC MTU) |
| Pre-auth half-open cap applies per-proxy-IP, not per-client | A single proxy IP exhausts the 64-slot cap for all clients | The cap must be per-original-client-IP (from XFF, validated from a trusted proxy) or per-proxy-IP with a much larger limit | More than 64 concurrent new connections from behind a single proxy |
| Channel multiplexing control-stream contention | Control stream backs up with `InnerAuthChallenge` messages during handshake, blocking `ChannelOpen`/`ChannelAccept` | Inner-auth messages on a separate stream (or early in the session before the control stream is multiplexed), not mixed with channel management messages | Immediately — during handshake, control stream is not yet accepting channel frames |

---

## Security Mistakes

| Mistake | Risk | Prevention |
|---------|------|------------|
| Inner auth without `tls-exporter` channel binding | Proxy MITM: proxy relays auth exchange, authenticates to real server on behalf of untrusted client | Mandatory: challenge must include `tls-exporter` material from the outer TLS session (RFC 9266 `tls-exporter` channel binding) |
| Reattach token transmitted before inner auth | Proxy reads token, attaches to server session without SSH key | Strict state-machine ordering: token transmitted only after `InnerAuthOk` |
| Rate limiting on `X-Forwarded-For` | Attacker spoofs XFF header, bypasses rate limit or exhausts server memory | Cap on actual source IP (proxy IP); XFF for logging only |
| TOFU auto-accept (no fingerprint prompt) | MITM on first connect goes undetected | Blocking fingerprint prompt displaying SHA-256 hex; explicit `yes` required |
| `OSC_ACCUMULATION_MAX` prefilter not verified after M7 changes | Multi-MiB OSC sequence OOMs server, disrupts all sessions | Re-run `oversized_multi_chunk_osc_is_bounded_then_resyncs` after any `TerminalState::advance` change |
| `Message` inner-auth variants inserted before discriminant 17 | Old client/server decodes wrong message type; session silently corrupts or hangs | Append-only rule enforced by `message_discriminant_order_is_stable` CI test |
| `TerminalControl(Clipboard)` selection field not validated on client | Malicious server sets an unexpected selection designator, triggering unintended clipboard operations | Validate selection against known values (`c`, `p`, `s`, etc.) before re-emitting |
| `TerminalControl(Title)` containing escape bytes re-emitted raw | Title containing `\x1b` breaks out of the title sequence, injecting further OSC commands | Strip `\x1b`, `\x07`, `\r`, `\n` from the title before re-emitting |
| WebTransport mode server also accepts raw QUIC | Proxy ACLs and logging bypassed; attacker connects directly | Server `--mode webtransport` must reject raw QUIC; firewall raw UDP/443 except from proxy IP |
| Inner auth nonce derived from observable values | Nonce predictable; attacker pre-computes challenge response or replays | Both challenges must be 32 bytes of CSPRNG output, validated single-use on the server |

---

## UX Pitfalls

| Pitfall | User Impact | Better Approach |
|---------|-------------|-----------------|
| TOFU prompt displayed after PTY output starts | User cannot read the fingerprint; accepts blindly | Block `SessionOpen` until TOFU resolves; render prompt to stderr/tty before any PTY output |
| TOFU prompt with default "yes" on empty input | User presses Enter reflexively; key is never verified | Require explicit `yes` string; treat empty input as "no" |
| Fingerprint shown as raw base64 | User has no way to verify; copies nothing useful from `ssh-keygen -l` output | Display SHA-256 hex in `sha256:...` format matching `ssh-keygen -l -E sha256` |
| Migration handover shows the connection-lost banner | User sees "connection lost, reconnecting" on every Wi-Fi→mobile handover | WT migration handover should be silent (same as raw QUIC migration) if the reconnect completes within the keep-alive window |
| Inner-auth failure indistinguishable from network failure | User retries indefinitely, not knowing the server key changed | Inner-auth failure (key mismatch) must produce a clear distinct error, not a generic "connection failed" |

---

## "Looks Done But Isn't" Checklist

- [ ] **WebTransport datagram sizing:** Datagram encoder uses WT session's `max_datagram_payload_size()` — not raw `conn.max_datagram_size()`. Verified with a test that sends at maximum WT payload size with no `TooLarge` error.
- [ ] **Inner-auth channel binding:** The challenge signed by the client includes the `tls-exporter` material from the outer TLS session. Verified: a signature over `challenge_only` (no exporter material) fails server verification.
- [ ] **Inner-auth nonce freshness:** Both client→server and server→client challenges are 32 bytes of CSPRNG output. Verified: the same challenge cannot be accepted twice (server rejects replayed nonce).
- [ ] **Reattach sequencing:** The server state machine rejects `Reattach` and `SessionOpen` messages received before `InnerAuthOk`. Verified: sending `Reattach` before inner auth is complete produces a clean error close, not a hang.
- [ ] **Double-attach race:** Concurrent reattach with the same token — only one succeeds. Verified: a test fires two simultaneous `Reattach` with the same token and asserts only one session is active.
- [ ] **Discriminant stability after new variants:** `message_discriminant_order_is_stable` test includes all new inner-auth variants with their expected discriminant bytes. Verified: the test fails if any variant is inserted before the expected tail.
- [ ] **OSC-OOM re-verification:** `oversized_multi_chunk_osc_is_bounded_then_resyncs` passes after all M7 changes to `TerminalState::advance`. Fuzz target re-run with increased `max_len`.
- [ ] **TOFU prompt blocks session open:** No PTY output reaches the client before the TOFU prompt resolves. Verified: sending `SessionOpen` without user confirmation is rejected at the client, not at the server.
- [ ] **Server mode gate:** Server in `webtransport` mode rejects raw QUIC connections. Verified: a raw quinn client cannot connect when the server is in WT-only mode.
- [ ] **`TerminalControl` sanitisation:** Clipboard `selection` field validated against known values; `Title` field stripped of escape bytes before re-emission. Verified by unit tests with adversarial inputs.
- [ ] **XFF handling:** Pre-auth half-open cap is keyed on source IP (proxy IP), not XFF value. Rate-limiting that uses XFF uses only the rightmost trusted proxy entry. Verified: a client that spoofs an XFF header is not able to exhaust rate limits or bypass caps.

---

## Recovery Strategies

| Pitfall | Recovery Cost | Recovery Steps |
|---------|---------------|----------------|
| Crypto-provider conflict (WT-1) | LOW | Remove conflicting feature from `wtransport` dep; `cargo clean`; rebuild |
| Datagram size mismatch (WT-2) | LOW | Update encoder to use WT session MTU API; existing tests catch size regressions |
| Missing channel binding (WT-3) | HIGH — wire-breaking protocol change | Redesign inner-auth to include `tls-exporter` field in challenge; bump inner-auth protocol version; all deployed clients must update |
| Discriminant shift (WF-1) | HIGH — all connections from mismatched versions are broken | Revert the insertion; append the variant at the tail; re-deploy; add discriminant test to CI |
| Reattach token before inner auth (MH-1) | HIGH — live sessions may be hijacked | Add inner-auth state-machine enforcement; rotate all outstanding reattach tokens via a forced full re-auth on all sessions |
| OSC OOM regression (SEC-1) | MEDIUM — availability DoS, not data loss | Re-apply the prefilter; re-run regression test; patch deployed servers |
| TOFU auto-accept shipped to users | MEDIUM — past sessions may be MITM'd; no way to retroactively verify | Force `known_hosts` reset and re-TOFU on next connection; add a `--rehash-host-keys` command |

---

## Pitfall-to-Phase Mapping

| Pitfall | Prevention Phase | Verification |
|---------|------------------|--------------|
| WT-1: crypto provider conflict | Phase that adds `wtransport` to workspace (day one) | `cargo tree -f "{p} {f}" \| grep rustls` shows only `ring`, not `aws-lc-rs` |
| WT-2: WT datagram MTU | WebTransport integration phase | Unit test: encode at WT max payload, send, assert no `TooLarge` error |
| WT-3: inner-auth channel binding | Inner-auth design phase (before implementation) | Test: signature over `challenge_only` fails; signature over `challenge||tls_exporter` passes |
| WT-4: inner-auth nonce replay | Inner-auth design phase | Test: replaying a captured challenge-response on a new session fails |
| WT-5: inner-auth downgrade | WebTransport integration phase (server mode flag) | Test: raw quinn client cannot connect in WT-only mode |
| WF-1: discriminant corruption | Inner-auth design phase (new Message variants) | `message_discriminant_order_is_stable` CI test updated and passing with new discriminant values |
| MH-1: reattach token theft | Migration handover phase | Test: `Reattach` before `InnerAuthOk` is rejected; token not in WT stream before auth |
| MH-2: double-attach race | Migration handover phase | Test: concurrent same-token reattach — exactly one session active |
| SEC-1: OSC-OOM regression | 999.7 re-check phase | `oversized_multi_chunk_osc_is_bounded_then_resyncs` passes; fuzz target re-run |
| SEC-2: terminal escape injection | 999.2 client trust-boundary phase | Adversarial `TerminalControl` inputs with escape bytes, malicious URLs, and unknown selection fields rejected or sanitised |
| SEC-3: TOFU fatigue | SEC-02 interactive TOFU phase | Manual test: empty input at TOFU prompt does not accept the key; session blocked until explicit `yes` |

---

## Sources

- `.planning/PROJECT.md` — v1.4 scope, 999.x backlog, 999.7 finding. HIGH confidence.
- `docs/999.7-SECURITY.md` — OSC accumulation OOM analysis, Phase-16 mitigation-was-wrong finding, regression test reference. HIGH confidence.
- `docs/999.1-SECURITY.md` — Pre-auth security review; residual risks; HTTP/3 reverse-proxy topology noted as out-of-scope for that review. HIGH confidence.
- `crates/nosh-proto/src/messages.rs` — `Message` enum with 18 variants (discriminants 0–17), append-only invariant, `ChannelType` enum. HIGH confidence — first-party source.
- `crates/nosh-auth/src/verifier.rs` — `HostKeyVerifier` and `AuthorizedKeysVerifier`; TLS-handshake-based auth; SPKI pinning. HIGH confidence.
- `crates/nosh-proto/src/transport.rs` — Transport config, datagram buffer sizes, keep-alive. HIGH confidence.
- `CLAUDE.md` — Security invariants (env sanitisation, `SSH_AUTH_SOCK`, discriminant stability, noecho invariant). HIGH confidence.
- `docs.rs/crate/wtransport/latest/source/Cargo.toml` — wtransport 0.7.1 depends on `quinn ^0.11.6`, `rustls ^0.23.23`, `default-features = false`. HIGH confidence — verified at source.
- RFC 9266 "Channel Bindings for TLS 1.3" (`tls-exporter` binding) — MEDIUM confidence (standard, but rustls API surface for keying material export must be verified at implementation time).
- RFC 9297 "HTTP Datagrams and the Capsule Protocol" — Quarter Stream ID overhead in WebTransport datagrams. MEDIUM confidence.
- `github.com/rustls/rustls/issues/1877` — ring + aws-lc-rs dual-activation panic. MEDIUM confidence.
- Terminal escape injection: Trail of Bits blog (MCP/ANSI injection, 2025); InfosecMatter terminal escape injection reference. MEDIUM confidence — attack vectors verified; specific nosh mitigations are first-party design.
- Trust on first use / TOFU UX: HandWiki; SSH fingerprint verification research (arxiv 2208.08846). MEDIUM confidence — UX failure modes well-documented.

---
*Pitfalls research for: nosh v1.4 M7 WebTransport + Inner Auth + Security Hardening*
*Researched: 2026-06-13*
