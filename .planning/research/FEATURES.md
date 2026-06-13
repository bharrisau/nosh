# Feature Research: nosh v1.4 (M7 Remote Access over HTTP/3 + Security Hardening)

**Domain:** Internet-exposed roaming remote shell with WebTransport reverse-proxy topology and client trust-boundary security hardening
**Researched:** 2026-06-13
**Confidence:** HIGH — grounded in the existing nosh codebase (v1.3), verified prior art (ET, Mosh, SSH3 draft), wtransport 0.7.1 API docs, and the CVE research corpus on terminal escape-sequence attacks. WebTransport proxy maturity is MEDIUM (real constraint, well-documented limitation).

---

## Executive Summary

v1.4 adds four capability areas to an already-shipped QUIC remote shell. Each has a different character:

**WebTransport mode** is the headline transport change — nosh runs as a WebTransport client inside an HTTP/3 tunnel established to a proxy (e.g. nginx), with the SSH-key handshake repeated as an inner auth layer inside the tunnel. This is the only way to survive a QUIC-terminating proxy; L4 UDP passthrough is not viable because QUIC's routing key is the connection ID, not the 5-tuple. The new crate is `wtransport` (0.7.1 as of April 2026 — not yet declared production-stable by its authors, which is a risk to flag).

**Migration handover behind a proxy** is a derived requirement: once QUIC connection-ID migration is broken at the proxy boundary, roaming must fall back to an application-layer reattach. Nosh already has 1-RTT cold reattach from v1.1 — this feature is wiring that machinery into the WebTransport mode, not inventing a new one.

**Security design pass** covers two distinct things: (1) a threat-model document (SEC-01) for the new internet-exposed topology, which is a written artefact, not a code change; and (2) an interactive TOFU fingerprint-confirm prompt (SEC-02), which is a UX addition to the existing `HostKeyVerifier` path in `nosh-auth`. The current code silently records new host keys at TOFU time (`tracing::info!` only) — SEC-02 makes this interactive. Known-hosts pinning and host-key-mismatch hard-fail already exist and are correct.

**Client trust-boundary hardening** (SEC-04/999.2) is the most technically nuanced security area: protecting the nosh client against a malicious or compromised server that sends dangerous terminal escape sequences. The CVE research corpus (10+ CVEs in 2022-2023 alone: CVE-2022-45872, CVE-2022-44702, CVE-2022-47583, CVE-2023-39726) shows this is an active, real attack surface. Nosh's architecture — server-side terminal model, structured datagram protocol — provides a structural advantage here that byte-stream shells (SSH, ET) do not have.

**OSC OOM re-check** (999.7) is a bounded investigation: confirm (or fix) that vte's OSC accumulation buffer is bounded before the terminal model dispatch, because Phase 16's mitigation reasoning was found incorrect in the 999.1 review.

---

## Feature Landscape

### Area 1: WebTransport-over-HTTP/3 Reverse-Proxy Mode

#### Table Stakes

| Feature | Why Expected | Complexity | Notes |
|---------|--------------|------------|-------|
| Establish a WebTransport session to an HTTP/3 proxy via Extended CONNECT | Without this, nosh cannot be reached behind any QUIC-terminating HTTP/3 proxy (nginx 1.25+, Caddy). The proxy terminates QUIC; nosh must live inside an HTTP/3 tunnel, not beside it | HIGH | Uses `wtransport` client API: `Endpoint::client()` → `connect(url)` → `open_bi()`/`send_datagram()`. The session URL encodes the host, path, and port. Wire format is identical to native QUIC mode — the WebTransport session is a carrier, not a protocol change |
| Inner SSH-key mutual auth handshake inside the WebTransport session | The proxy's TLS terminates the outer QUIC trust chain; the inner auth re-establishes mutual identity between nosh client and server. Without this, a malicious proxy could impersonate either party | HIGH | Reuses the existing `nosh-auth` machinery: `AuthorizedKeysVerifier` and `HostKeyVerifier`, but now applied to an inner TLS-like handshake over the WebTransport reliable stream. This is ET's outer-transport/inner-handshake model. The inner handshake must run before any session data flows |
| Datagram and stream channels both work inside the WebTransport session | The nosh protocol uses RFC 9221-style datagrams for state-sync and reliable streams for control/scrollback. Both must function inside the WebTransport carrier | MEDIUM | WebTransport natively supports both datagrams and bidirectional streams, mirroring the native QUIC API. `wtransport` exposes the same `send_datagram`/`open_bi`/`accept_bi` surface. Verify that `wtransport` 0.7.1 datagram support is stable and the MTU budget is sane |
| Proxy-mode flag on the nosh command line | Operators need a way to invoke WebTransport mode vs native QUIC mode. The URL scheme distinguishes them: `https://` (WebTransport) vs raw QUIC | LOW | A `--proxy <url>` flag or URL-scheme detection. On the server side, `nosh-server` may need a listener mode that accepts WebTransport connections rather than (or in addition to) native QUIC |
| Server-side WebTransport listener mode | The nosh server behind the proxy must accept WebTransport sessions from the proxy, not raw QUIC from the client | HIGH | The server runs `wtransport`'s `ServerConfig` / `Endpoint::server()`. The proxy connects to it as a WebTransport client on behalf of the nosh client. Requires TLS configuration on the server side for the proxy→server leg |

#### Differentiators

| Feature | Value Proposition | Complexity | Notes |
|---------|-------------------|------------|-------|
| Transparent protocol negotiation: WebTransport vs native QUIC based on target URL | Users do not need to know which topology applies; they give a URL and nosh picks the right mode. `nosh://host:443` = native QUIC; `https://proxy/nosh` = WebTransport | MEDIUM | URL-scheme dispatch in `nosh-client`. When the target is a `https://` URL, open a WebTransport session; otherwise use native quinn. The inner protocol is identical once established |
| Inner session token bound to the SSH identity (proxy cannot replay it to a different identity) | The inner auth token / session reattach token is signed by the user's SSH key and scoped to the session ID. A compromised proxy cannot lift the token and present it to a different nosh server | MEDIUM | Extend the existing reattach token (v1.1) to include the WebTransport session endpoint as a binding factor. Analogous to how Kerberos service tickets are name-bound |
| Connection continuity is preserved through proxy restarts via reattach (not migration) | When the proxy restarts, the outer WebTransport session drops. Nosh detects this and triggers a 1-RTT cold reattach to re-establish the session — same as a client network change in native mode | MEDIUM | Falls out of the migration-handover feature below. The user experience is the same reconnecting banner that already exists from v1.2 |

#### Anti-Features

| Anti-Feature | Why Requested | Why Problematic | Alternative |
|--------------|---------------|-----------------|-------------|
| L4 UDP passthrough through the proxy | Seems simpler: let the proxy forward raw UDP packets to the nosh server | QUIC's routing key is the connection ID, not the 5-tuple. A NAT or proxy that routes by IP:port will break connection migration and may misroute packets from different clients to the same backend | WebTransport inside HTTP/3; the proxy does stateful session routing by WebTransport session ID |
| Running nosh over TCP/TLS through an HTTP/1.1 or HTTP/2 proxy | Widely supported, simpler to configure | TCP cannot carry RFC 9221 datagrams; head-of-line blocking kills the predictive-echo latency model; session migration is impossible | WebTransport over HTTP/3 is the correct answer; if HTTP/3 is unavailable, nosh cannot provide its differentiated UX |
| Terminating the inner SSH-key auth at the proxy | Operators sometimes want the proxy to handle auth (e.g. SSO) | The proxy becomes a trusted intermediary that can impersonate either party. A compromised proxy has full session access. nosh's security model is end-to-end identity | Inner auth always runs between nosh client and server; the proxy is explicitly untrusted for identity |
| Self-signed TLS on the proxy→server leg accepted silently | Simpler for internal deployments | Opens a MITM opportunity on the inner leg. The proxy is not the same trust boundary as the user's SSH key | The server's host key on the proxy→server leg must be pinned (known\_hosts on the proxy config, or the proxy presents a CA-signed cert and nosh validates it) |
| Building a complete HTTP/3 stack from scratch | Avoids the `wtransport` dependency | `wtransport` 0.7.1 is the only serious Rust WebTransport implementation; building an alternative is months of work for no benefit | Accept `wtransport` as a dependency; monitor its maturity status; the production-not-ready caveat is a risk, not a blocker |

---

### Area 2: Migration Handover Behind a Proxy

#### Table Stakes

| Feature | Why Expected | Complexity | Notes |
|---------|--------------|------------|-------|
| IP change detected and nosh reconnects automatically (same UX as native QUIC migration) | This is nosh's headline roaming differentiator. Users who deploy nosh behind a proxy expect the same "Wi-Fi to cellular, session continues" experience. If proxy mode degrades to "session dies on IP change", the feature is broken | MEDIUM | The outer WebTransport session will drop when the client's IP changes (the proxy has a connection-ID-based session to the old IP). Nosh must detect the drop and trigger a 1-RTT cold reattach immediately, before the reconnecting banner timeout |
| Reattach token survives proxy topology and re-establishes the server-side session | The server-side session (PTY, scrollback, state) must persist across the reattach, identical to native-QUIC cold reattach | LOW | Falls out of v1.1 session persistence + v1.1 cold reattach machinery. The reattach token is identity-scoped, not transport-scoped. No new server-side changes needed; the client-side changes are in the WebTransport connection manager |
| Reconnecting banner displayed while reattach is in progress | Users must not think the session has died — they need the "Reconnecting…" notice that already exists from v1.2 QoL pack | LOW | Already shipped in v1.2 (Phase 16). Verify the banner is triggered by WebTransport-mode disconnects as well as native-QUIC disconnects |
| Reconnect timeout with abort-and-error on failure | After a configurable timeout (default: existing value from v1.2), nosh gives up and exits cleanly rather than hanging indefinitely | LOW | Already exists from v1.2. Confirm it works in WebTransport mode |

#### Differentiators

| Feature | Value Proposition | Complexity | Notes |
|---------|-------------------|------------|-------|
| Zero-keystroke loss during reattach: the SequencedOutputBuffer replays missed output | Nosh already has this from v1.1 (`BackedReader`-style sequence-numbered output). Behind a proxy the same mechanism applies — the client provides its last-seen sequence number in the reattach message and the server replays the gap | LOW | Existing mechanism; test it in WebTransport mode specifically |
| Predictive echo continues operating during the brief reattach window | After the WebTransport session drops, the client already has a speculative view from the predictor. The predictor can continue showing speculative output while the reattach is in progress — the user sees no freeze | MEDIUM | The predictor runs client-side and is independent of the transport. Confirm the reconnecting state does not tear down the predictor state |

#### Anti-Features

| Anti-Feature | Why Requested | Why Problematic | Alternative |
|--------------|---------------|-----------------|-------------|
| 0-RTT reattach to reduce reconnect latency | "1-RTT is slow" | 0-RTT was deliberately deferred in v1.1 (the latency gain is dwarfed by Wi-Fi/DHCP bring-up; 0-RTT introduces replay-safety burden). Nothing has changed that makes this worth revisiting in v1.4 | Keep 1-RTT; measure before reconsidering |
| Custom keep-alive pings at the WebTransport level to detect proxy restarts faster | Faster detection = faster reattach | The WebTransport session already has QUIC keep-alive semantics. Adding an application-layer ping duplicates this with no benefit and adds complexity | Rely on QUIC/WebTransport idle timeout and the existing reconnect machinery |
| Mosh-style per-IP UDP port negotiation as a fallback | Used by Mosh when WebTransport is unavailable | Requires inbound server port range (Mosh's acknowledged firewall problem); one of the reasons nosh was built to replace Mosh | UDP/443 via WebTransport is the answer; no fallback to Mosh-style port negotiation |

---

### Area 3: Security Design Pass

#### Table Stakes

#### SEC-01: Threat-Model Document

| Feature | Why Expected | Complexity | Notes |
|---------|--------------|------------|-------|
| Written threat-model document covering the internet-exposed topology | Before v1.4 ships a proxy mode that exposes nosh to the public internet, there must be a written analysis of assets, attacker capabilities, trust boundaries, and mitigations. This is the standard gate for any internet-exposed authentication system | MEDIUM | The document is not a code change; it is a structured artefact. Format: assets (session state, SSH identity, server secrets), trust boundaries (nosh client ↔ proxy, proxy ↔ nosh server, outer TLS, inner auth), attacker capabilities (network adversary, compromised proxy, malicious server, brute-force), mitigations (inner auth, known\_hosts pinning, env sanitization, pre-auth cap, OSC OOM bound). The document should also include a residual-risk section noting what is NOT mitigated (e.g. compromise of the ssh-agent, physical access to the client) |
| Trust boundary analysis specific to the proxy topology | The proxy introduces a new intermediary that terminates outer TLS. The document must explicitly address what the proxy can and cannot do: it can observe session timing and data volumes, it cannot impersonate either party because inner auth pins identities end-to-end | MEDIUM | Cover: (1) what a compromised proxy can observe; (2) what a compromised proxy cannot do; (3) why L4 passthrough is ruled out; (4) why the inner auth is mandatory |
| Attacker capability model for an internet-exposed endpoint | nosh on UDP/443 is reachable from the public internet. The document must model: network adversary (passive eavesdrop, active MITM), unauthenticated attacker (pre-auth DoS), authenticated attacker (malicious client), and compromised server (malicious server sending bad terminal sequences to the client) | MEDIUM | The pre-auth DoS hardening from v1.0 (concurrent half-open cap, auth-completion timeout) already exists; document it as a shipped mitigation. The malicious-server case is the one that motivates SEC-04 below |

#### SEC-02: Interactive TOFU Fingerprint-Confirm Prompt

| Feature | Why Expected | Complexity | Notes |
|---------|--------------|------------|-------|
| On first contact with a host not in known\_hosts, display the host key fingerprint and prompt "Are you sure?" before recording it | This is what SSH does. The current nosh code silently records new host keys with only a `tracing::info!` log line. For an internet-exposed tool, silent TOFU is a usability security gap — a MITM on first contact goes unnoticed by the user | LOW | The prompt wording should match SSH: show the fingerprint in SHA-256 base64 format (same as `ssh-keygen -l -E sha256`), name the host, and ask `yes/no/[fingerprint]`. Implement in `HostKeyVerifier::verify_server_cert` or at the call site in `nosh-client/src/client.rs` |
| Hard-fail with clear error message on host-key mismatch | Already implemented (D-02 in `verifier.rs`: "host key mismatch for {} — known_hosts pins a different key (aborting)"). Verify the message is surfaced to the user as a human-readable error, not just a log line | LOW | Check the error propagation path from `HostKeyVerifier` up through the quinn handshake failure to the client's stderr output |
| `--trust-key <fingerprint>` flag to automate TOFU in scripting contexts | Operators running nosh from scripts or CI pipelines need a way to pre-authorise a key without interactive prompting | LOW | Accept a SHA-256 fingerprint on the command line; compare it to the presented key; if it matches, record without prompting. If it mismatches, hard-fail. This is equivalent to `ssh -o "StrictHostKeyChecking=accept-new"` with a fingerprint check |
| `--strict-host-key-checking` flag to refuse all TOFU (known hosts only) | High-security environments may want to disable TOFU entirely — every host must be pre-populated in known\_hosts | LOW | A flag that makes the `None` branch (key not in known\_hosts) a hard failure rather than a TOFU record. Equivalent to SSH's `StrictHostKeyChecking=yes` |

#### Differentiators

| Feature | Value Proposition | Complexity | Notes |
|---------|-------------------|------------|-------|
| Fingerprint displayed in both SHA-256 base64 and SHA-256 hex for comparison | Some server admin tools display hex, some display base64. Showing both avoids the user having to convert | LOW | Two lines in the TOFU prompt |
| Threat-model document links to specific code mitigations (file + line or REQ-ID) | Makes the document a living artefact that can be reviewed alongside code changes rather than a standalone Word doc | LOW | Add code references in the mitigation table: "pre-auth cap: `nosh-server/src/server.rs` AUTH-05" |

#### Anti-Features

| Anti-Feature | Why Requested | Why Problematic | Alternative |
|--------------|---------------|-----------------|-------------|
| Auto-accept TOFU by default (current silent behaviour) | "SSH does it, users expect it" | SSH's auto-accept is the canonical example of a security UX failure. The nosh UX goal is to be better than SSH, not to replicate its mistakes. A MITM on first contact is silent with auto-accept | Interactive prompt on first contact; `--trust-key` for automation |
| Storing known\_hosts in a database rather than the OpenSSH flat-file format | Richer querying, faster lookup | Breaks compatibility with OpenSSH's `ssh-keygen -l` tooling for fingerprint inspection; adds a dependency; the flat-file format is sufficient for nosh's scale | Keep the OpenSSH flat-file format; `ssh-key` crate already parses and writes it |
| SSH CA certificates as an alternative to TOFU | Enterprises may prefer CA-based host authentication | Out of scope for MVP; SSH CA → X.509 mapping is explicitly deferred in PROJECT.md | Document as a future extension |

---

### Area 4: Client Trust-Boundary Hardening (SEC-04 / 999.2)

This area protects the nosh client from a malicious or compromised nosh server. The server is an attacker — authenticated but adversarial. Nosh's server-side terminal model architecture (which parses VT sequences on the server and sends structured cell-diff datagrams to the client) already provides a partial structural defence compared to byte-stream shells (SSH, ET), which send raw VT bytes to the client terminal emulator. This advantage must be systematically enumerated and any gaps in the structured-datagram path must be closed.

#### Table Stakes

| Feature | Why Expected | Complexity | Notes |
|---------|--------------|------------|-------|
| Datagram path carries only structured cell-diffs — no raw VT sequences | The nosh state-sync datagram (`StateDiff`) carries cell data (character, attributes, position) in a defined wire format, not raw terminal bytes. A malicious server cannot inject arbitrary VT sequences via the datagram path because the client's `emit_diff` function renders cell structs, not raw bytes | LOW | This is structural — the architecture already provides it. Verify that `screen.rs`'s `emit_diff` does not pass raw bytes from the datagram to the local terminal without interpretation. Document as a shipped mitigation in SEC-01 |
| OSC sequences on the reliable stream path are bounded before vte dispatch | `vte` accumulates OSC bytes in an unbounded internal buffer until the terminator arrives. An `ESC]52;c;<100MB>` sequence from a malicious server allocates 100 MB server-side (already noted in 999.7) and potentially client-side too. The fix is an application-level byte-count gate before feeding bytes to `vte` | MEDIUM | This is the 999.7 requirement. The gate must be applied on both the server side (before the server-side `vte` parse in `terminal.rs`) and the client side (before the client-side `vte` parse in `screen.rs`, if any raw VT bytes flow there). Determine which path applies in v1.3 |
| OSC 52 (clipboard write) is the only clipboard-affecting OSC permitted and it is bounded | OSC 52 clipboard write is a legitimate feature (shipped in v1.2). OSC sequences that read the clipboard (OSC 52 with a `?` parameter) should be rejected client-side — they allow a malicious server to exfiltrate clipboard contents by triggering a response from the local terminal | MEDIUM | The server-side terminal model should: (a) pass OSC 52 clipboard-write through to the client; (b) never respond to or forward OSC 52 clipboard-read requests (the `?` parameter). The client should similarly never generate a clipboard-read response to a server-issued request |
| Window title is set from OSC 0/1/2 but bounded in length | Title manipulation (OSC 0/1/2) is legitimate. Unbounded title strings can cause a performance DoS on some platforms (Windows SetWindowText has been shown to cause system-wide lag with very long strings). Cap title length | LOW | A 256-byte cap on OSC 0/1/2 content before passing to the local terminal's title-setting API. Already documented in the cyberark research |
| DCS/PM/APC sequences are silently discarded | Device Control Strings, Privacy Messages, and Application Program Commands are rarely used for legitimate shell interaction and have been the source of several CVEs (ReGIS graphics injection, Sixel overflow). Nosh's architecture should discard them before reaching the client terminal | LOW | The server-side vte `Perform` trait's `dcs_hook`/`dcs_put`/`dcs_unhook` implementations should be no-ops. If raw VT bytes reach the client, the client-side vte handler must also no-op these |
| Dangerous DECRQSS (Request Status String) is never echoed | DECRQSS (ESC P $ q <query> ESC \\) exploits were the source of CVE-2022-45872 (iTerm2) and CVE-2022-47583 (mintty). These CVEs allow a server to craft a DECRQSS request that, when echoed back, injects arbitrary sequences. Nosh must never echo DECRQSS responses from the server back to the client terminal | LOW | No-op the `dcs_hook` handler for DCS strings matching the DECRQSS pattern. Since nosh does not report DECRQSS, the attack surface is already reduced; document this |

#### Differentiators

| Feature | Value Proposition | Complexity | Notes |
|---------|-------------------|------------|-------|
| Structured datagram path as the primary defence: server parses VT, client renders structs | Nosh's architecture inherently limits the blast radius of a malicious server compared to byte-stream shells. A malicious server can only send what the `StateDiff` wire format allows: cells with character + attribute data. It cannot send arbitrary VT escape sequences through the datagram path | LOW | Document this architectural advantage explicitly in SEC-01. It is a genuine differentiator from SSH/ET/Mosh (Mosh's SSP model provides the same benefit on the state-sync path; SSH/ET do not) |
| Per-OSC byte-count gate on all OSC sequences, not just OSC 52 | Rather than enumerating every dangerous OSC code, a generic byte-count cap (e.g. 16 KB per OSC sequence) bounds the worst-case allocation from any OSC type before the terminator is reached | MEDIUM | Applied as a wrapper around the bytes fed to `vte`. This is the correct architectural answer to 999.7 and generalises to future OSC codes |
| Resize-flood protection: rate-limit server-issued resize requests | A malicious server could send thousands of resize messages per second, triggering SIGWINCH repeatedly. Cap at a sensible rate (e.g. one resize per 100 ms) | LOW | The resize coalescing (~40 ms) from v1.0 (SESS-04/05) provides partial protection; add an explicit rate-limit cap to make it a security property, not just a UX one |

#### Anti-Features

| Anti-Feature | Why Requested | Why Problematic | Alternative |
|--------------|---------------|-----------------|-------------|
| Passing raw VT bytes from the server to the local client terminal | Simpler, removes the server-side terminal model requirement | Allows a malicious server to inject arbitrary terminal sequences, including the CVE classes documented above (DECRQSS injection, OSC 52 read, title DoS, resize flood). This is the SSH/ET model's fundamental limitation | Keep the structured datagram path; any VT bytes that reach the client must have been parsed and re-emitted by nosh's own structured render path |
| Restricting to a hard-coded allowlist of terminal sequences | Seems tighter than a generic cap | An allowlist must be maintained against every new terminal feature; it becomes stale and causes compatibility breaks | A byte-count cap on OSC + no-op on DCS/PM/APC + the structural datagram advantage is the right defence-in-depth |
| Implementing a full sandboxed terminal emulator on the client | Maximum isolation | nosh is explicitly not a terminal emulator (PROJECT.md Out of Scope). The client renders structured cell diffs, not arbitrary VT | The structured datagram path is the correct scope limit |

---

### Area 5: OSC OOM Bound Re-check (999.7)

| Feature | Why Expected | Complexity | Notes |
|---------|--------------|------------|-------|
| Confirm vte OSC accumulation is bounded before dispatch in server-side terminal model | Phase 16's mitigation reasoning for 999.7 was found incorrect in the 999.1 review. The actual risk: vte accumulates OSC bytes internally until `osc_dispatch` is called; application-level caps in `osc_dispatch` fire only after vte has already allocated the full buffer | MEDIUM | Investigation-first: trace the byte path from PTY output through `vte::Parser::advance()` to `osc_dispatch`. Determine whether vte's internal accumulation is bounded (check the vte 0.15.0 source for `osc_raw` buffer management). If unbounded, add a pre-gate: count bytes fed to the parser and abort the OSC with a synthetic terminator after N bytes (e.g. 64 KB). This must be done before `vte::Parser::advance()`, not inside `osc_dispatch` |
| Confirm or fix the same path on the client side if raw VT bytes reach the client's vte instance | If the client has a vte instance parsing raw bytes (e.g. for the reconnecting overlay or the scrollback render), the same vulnerability applies there | LOW | In v1.3, determine whether `screen.rs` uses vte for anything. If it does, the same pre-gate must apply |

---

### Area 6: Interactive Guided UAT Clearing

| Feature | Why Expected | Complexity | Notes |
|---------|--------------|------------|-------|
| Conversational UAT walkthrough, one item at a time, confirm before moving on | The v1.4 milestone includes carried-forward UAT items (Phase 19 Windows alt-screen re-test, 999.3 rendering pack, 999.4 Windows predictive-echo, Windows CI green confirm) plus the new M7 remote-access path. These must be validated interactively, not handed over as a document | LOW | Process requirement, not a code feature. The walkthrough covers: (1) Windows alt-screen re-test (vim over nosh Windows client → Linux server); (2) 999.3 rendering quality; (3) 999.4 `read -s` predictive-echo fix on Windows; (4) green `build-windows` and `cargo audit` CI; (5) WebTransport proxy mode end-to-end live test; (6) TOFU prompt on first contact; (7) SEC-04 hardening acceptance |
| WebTransport proxy mode end-to-end live test as a UAT gate | The headline v1.4 feature must be live-validated (not just unit-tested) before the milestone closes | MEDIUM | Requires a test environment with nginx 1.25+ (QUIC+HTTP/3) or Caddy as the proxy, a nosh server behind it, and a nosh client connecting through the proxy. The UAT must confirm: session establishment, TOFU prompt, scrollback, predictive echo, and a simulated IP change triggering reattach |

---

## Feature Dependencies

```
WebTransport mode (Area 1)
    └──requires──> Inner SSH-key auth (Area 1, table stakes)
    └──requires──> wtransport 0.7.1 crate
    └──enables──>  Migration handover behind proxy (Area 2)

Migration handover (Area 2)
    └──requires──> 1-RTT cold reattach (SHIPPED v1.1)
    └──requires──> Reconnecting banner (SHIPPED v1.2)
    └──no new server-side changes needed

SEC-01 threat model (Area 3)
    └──requires──> WebTransport mode design decisions (Area 1)
    └──requires──> SEC-04 hardening decisions (Area 4)
    └──documents──> pre-auth cap (SHIPPED v1.0)
    └──documents──> env sanitization (SHIPPED v1.0)

SEC-02 TOFU prompt (Area 3)
    └──requires──> HostKeyVerifier (SHIPPED v1.0, nosh-auth/src/verifier.rs)
    └──modifies──> TOFU silent-record path (currently line ~82 of verifier.rs)

SEC-04 client hardening (Area 4)
    └──requires──> structured datagram path (SHIPPED v1.2/v1.3)
    └──requires──> 999.7 OSC OOM fix (Area 5) — must land before SEC-04 is complete

999.7 OSC OOM (Area 5)
    └──requires──> vte 0.15.0 investigation (nosh-server/src/terminal.rs)
    └──independent of all other v1.4 areas (can land in any phase order)

UAT clearing (Area 6)
    └──requires──> All Areas 1–5 complete
    └──requires──> WebTransport proxy test environment
    └──includes──> Carried-forward Windows UAT items (Phase 19 re-test, 999.3, 999.4)
```

### Dependency Notes

- **Migration handover requires cold reattach (v1.1):** The 1-RTT cold reattach in v1.1 is the foundation for proxy-mode migration handover. No new server-side state is required. The client-side WebTransport connection manager must trigger the existing reattach path when the WebTransport session drops.
- **SEC-04 hardening requires the structured datagram path (v1.2/v1.3):** The primary defence against a malicious server is the structured `StateDiff` datagram format that replaced raw VT byte passthrough. This was built in v1.2 (Phase 11). SEC-04 documents and extends it — it does not build a new mechanism.
- **999.7 should land before SEC-04 sign-off:** 999.7 is an OOM vulnerability on an authenticated-user code path. SEC-04 cannot honestly be declared complete if the OSC OOM vector is still open. They are in the same milestone; 999.7 should be an early phase.
- **Inner auth requires nosh-auth changes but reuses the verifier logic:** The inner auth handshake in WebTransport mode must re-run SPKI pinning. The existing `HostKeyVerifier` and `AuthorizedKeysVerifier` structs are reused; the change is wiring them into the WebTransport session setup, not rewriting them.

---

## MVP Definition

### Launch With (v1.4)

All of the following are required for milestone sign-off:

- WebTransport-over-HTTP/3 mode with inner SSH-key mutual auth — without this the milestone's goal is not achieved
- Migration handover via 1-RTT reattach behind a proxy — without this, WebTransport mode is not roaming-tolerant
- SEC-01 threat-model document — required gate for an internet-exposed release
- SEC-02 interactive TOFU fingerprint-confirm prompt — required for any internet-exposed first-contact UX
- 999.7 OSC OOM fix — an open authenticated-user OOM vector cannot be shipped in an internet-exposed milestone
- SEC-04 client trust-boundary hardening — at minimum: OSC byte-count gate, OSC 52 read rejection, DCS/PM/APC no-op, resize rate-limit
- Interactive UAT clearing covering both carried-forward items and the new M7 path

### Add After Validation (v1.x)

- `--trust-key <fingerprint>` and `--strict-host-key-checking` CLI flags (useful, not blocking)
- SSH CA certificate support for host verification (deferred by design)
- Pageant / Windows ssh-agent integration for the Windows client (deferred from v1.1)
- Port forwarding and agent forwarding channels (mux types already declared; implementation is M5+)

### Future Consideration (v2+)

- NAT hole-punch/relay — explicitly deferred to a later milestone
- 0-RTT reattach — measure first, implement only if profiling shows the gain
- Browser/web client — HTTP/3 framing leaves the door open; not this milestone
- Native Windows server (ConPTY) — M6
- macOS support — deferred

---

## Feature Prioritisation Matrix

| Feature | User Value | Implementation Cost | Priority |
|---------|------------|---------------------|----------|
| WebTransport session establishment + inner auth | HIGH | HIGH | P1 |
| Migration handover via reattach behind proxy | HIGH | MEDIUM | P1 |
| SEC-01 threat-model document | HIGH | MEDIUM | P1 |
| 999.7 OSC OOM bound fix | HIGH | MEDIUM | P1 |
| SEC-02 interactive TOFU prompt | HIGH | LOW | P1 |
| SEC-04: OSC byte-count gate | HIGH | MEDIUM | P1 |
| SEC-04: OSC 52 read rejection | HIGH | LOW | P1 |
| SEC-04: DCS/PM/APC no-op | MEDIUM | LOW | P1 |
| SEC-04: resize rate-limit | MEDIUM | LOW | P1 |
| Proxy-mode CLI flag / URL-scheme dispatch | HIGH | LOW | P1 |
| Server-side WebTransport listener mode | HIGH | HIGH | P1 |
| Interactive UAT clearing | HIGH | MEDIUM | P1 |
| `--trust-key` / `--strict-host-key-checking` flags | MEDIUM | LOW | P2 |
| Transparent protocol negotiation by URL scheme | MEDIUM | MEDIUM | P2 |
| Inner session token proxy binding | MEDIUM | MEDIUM | P2 |
| Fingerprint displayed in both SHA-256 formats | LOW | LOW | P2 |

---

## Competitor Feature Analysis

| Feature | SSH / OpenSSH | Mosh | Eternal Terminal | nosh v1.3 | nosh v1.4 target |
|---------|---------------|------|------------------|-----------|------------------|
| HTTP/3 proxy mode | No (TCP only) | No (custom UDP) | No (TCP) | No | Yes — WebTransport inner auth |
| Roaming through proxy | No | No | No | No | Yes — 1-RTT reattach |
| TOFU fingerprint prompt | Yes (interactive) | Uses SSH for initial auth | Uses SSH for initial auth | Silent record only | Yes (interactive prompt) |
| Threat-model document | Published (NIST, RFC 4251) | Published in Mosh paper | Limited | None yet | Yes (SEC-01) |
| Client protection against malicious server terminal sequences | No — raw bytes to terminal | Partial — SSP structured state sync, but passes some OSC | No — raw bytes | Partial — structured datagram, OSC 52 shipped | Yes — byte-count gate + OSC 52 read reject + DCS no-op |
| OSC OOM protection | No application-level limit (terminal emulator limits) | N/A | No | Incomplete (999.7) | Yes — pre-vte byte-count gate |
| Resize rate-limit | No explicit limit | Implicit via SSP send rate | No explicit limit | 40 ms coalescing only | Yes — explicit rate-limit cap |

---

## Sources

- nosh codebase: `nosh-auth/src/verifier.rs` (HostKeyVerifier TOFU path, D-01/D-02 comments) — HIGH confidence (read directly)
- nosh codebase: `nosh-client/src/client.rs` (ClientIdentity, HostKeyVerifier wiring) — HIGH confidence (read directly)
- nosh PROJECT.md and ROADMAP.md — HIGH confidence (primary project artefacts)
- `wtransport` 0.7.1: [GitHub — BiagioFesta/wtransport](https://github.com/BiagioFesta/wtransport) — MEDIUM confidence (not declared production-stable by authors; API verified as of April 2026)
- `wtransport` authentication discussion: [Issue #244 — wtransport](https://github.com/BiagioFesta/wtransport/discussions/244) — MEDIUM confidence (confirms inner-auth is application responsibility)
- Eternal Terminal BackedReader/BackedWriter: [eternalterminal.dev/howitworks](https://eternalterminal.dev/howitworks/) — HIGH confidence (official site)
- SSH3 / Remote Terminal over HTTP/3: [draft-michel-remote-terminal-http3](https://francoismichel.github.io/ssh3-spec/draft-michel-remote-terminal-http3.html) — MEDIUM confidence (IETF draft, not standardised; confirms proxy gap)
- ANSI terminal security CVE research (10 CVEs): [dgl.cx/2023/09/ansi-terminal-security](https://dgl.cx/2023/09/ansi-terminal-security) — HIGH confidence (primary research, widely cited, CVEs verified)
- CyberArk: [Don't Trust This Title](https://www.cyberark.com/resources/threat-research-blog/dont-trust-this-title-abusing-terminal-emulators-with-ansi-escape-characters) — HIGH confidence (specific attack patterns and CVE classes)
- Terminal Escape Injection: [infosecmatter.com/terminal-escape-injection](https://www.infosecmatter.com/terminal-escape-injection/) — MEDIUM confidence (secondary, practitioner-level)
- W3C WebTransport reverse proxy issue: [w3c/webtransport#525](https://github.com/w3c/webtransport/issues/525) — HIGH confidence (documents the proxy limitation as an open problem)
- nginx QUIC/HTTP/3 support: [nginx.org/en/docs/quic.html](https://nginx.org/en/docs/quic.html) — HIGH confidence (official nginx docs; confirms HTTP/3 termination, no WebTransport upstream proxy)
- Wikipedia TOFU: [Trust on first use](https://en.wikipedia.org/wiki/Trust_on_first_use) — HIGH confidence
- SSH host key validation UX: [blog.g3rt.nl](https://blog.g3rt.nl/ssh-host-key-validation-strict-yet-user-friendly.html) — MEDIUM confidence

---

*Feature research for: nosh v1.4 M7 Remote Access over HTTP/3 + Security Hardening*
*Researched: 2026-06-13*
