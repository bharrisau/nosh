# Technology Stack — nosh v1.4 (M7) Additions

**One-line summary:** Add `wtransport` 0.7.1 as a new `nosh-transport` crate that wraps WebTransport-over-HTTP/3; it uses the same `quinn ^0.11.6` and `rustls ^0.23.23` the workspace already pins, so there is no version conflict. No second QUIC library, no second async runtime.

**Researched:** 2026-06-13
**Confidence:** HIGH for wtransport version and dependency graph (verified against crates.io 0.7.1 manifest and docs.rs); HIGH for the quinn/rustls coexistence answer (dependency versions confirmed); MEDIUM for proxy ecosystem state (nginx/HAProxy open issues confirmed, Envoy work-in-progress confirmed, alternative approaches verified); HIGH for wtransport API surface (Connection, Endpoint, with_custom_tls — verified against docs.rs 0.7.1).

---

## The Single Biggest Stack Risk — quinn/rustls Coexistence

This is answered first because the milestone brief flags it as the critical question.

**Verdict: no conflict. wtransport 0.7.1 uses the same quinn and rustls the workspace already pins.**

Evidence (from the published `wtransport/Cargo.toml` at crates.io, version 0.7.1, published 2026-04-26):

```toml
quinn    = "^0.11.6"   # lower bound 0.11.6; workspace pins 0.11.9 — satisfied
rustls   = "^0.23.23"  # lower bound 0.23.23; workspace pins 0.23.40 — satisfied
tokio    = "^1.28.1"   # lower bound 1.28.1; workspace uses 1.52.x — satisfied
```

`wtransport` uses the **published upstream quinn crate**, not a fork. Its `wtransport-proto` workspace member is an internal frame-parsing crate, not a quinn replacement. There is no `h3` dependency — wtransport implements HTTP/3 framing directly on top of quinn.

Because `quinn 0.11.9` satisfies `^0.11.6`, Cargo will resolve a single copy of quinn and a single copy of rustls across the whole workspace. Adding `wtransport` to the workspace does not pull a second incompatible copy of either.

The `quic_connection()` method on `wtransport::Connection` returns `&quinn::Connection` (available when the `quinn` feature flag is enabled), so nosh can reach through to the underlying quinn connection if needed for diagnostics, migration events, or transport stats — the abstraction is not opaque.

---

## Recommended Additions

### New crate: `nosh-transport`

The right packaging decision is to add a new workspace crate `nosh-transport` that owns the WebTransport mode. This keeps the existing `nosh-server` and `nosh-client` QUIC code unchanged and lets both transport modes coexist cleanly behind a trait boundary.

```
nosh-transport (new) — wraps wtransport; exposes the same nosh-proto channel model
  ├── depends on: wtransport, nosh-proto, tokio
  └── used by: nosh-server (optional feature "webtransport"), nosh-client
```

### New workspace dependencies

| Crate | Version | Purpose | Why |
|-------|---------|---------|-----|
| `wtransport` | 0.7.1 | WebTransport-over-HTTP/3 session layer | The only mature, actively maintained, async-native Rust WebTransport implementation. Uses upstream quinn 0.11.x and rustls 0.23.x — no version conflict. Exposes `open_bi`/`accept_bi`/`open_uni`/`accept_uni` and `send_datagram`/`receive_datagram`, which map directly onto nosh-proto's existing channel model. Server and client both get `with_custom_tls(rustls::ServerConfig)` / `with_custom_tls(rustls::ClientConfig)` escape hatches for full rustls control. |

No other new crates are required for the WebTransport transport layer itself.

### Supporting library additions (inner auth layer)

The outer QUIC+TLS terminates at the proxy. nosh's SSH-key handshake must move inside the WebTransport tunnel as an application-layer protocol. The handshake itself reuses existing crates (`ssh-key`, `ssh-agent-client-rs`, `ed25519-dalek`) — no new auth crates are needed. What is needed is a small inner-auth wire protocol on top of the WebTransport control stream.

| Component | Implementation | Notes |
|-----------|---------------|-------|
| Inner auth wire format | New `nosh-proto` variants (appended in discriminant order) | `InnerAuthChallenge`, `InnerAuthResponse` messages on the control stream before any session traffic |
| Server-side client key check | Existing `nosh-auth` `AuthorizedKeys::verify` logic | Reuse as-is — the inner auth is the same SPKI-pinning check, just moved from TLS layer to app layer |
| Client-side server key check | Existing `nosh-auth` `KnownHosts::verify` logic | Same TOFU/known_hosts logic; SEC-02 (interactive fingerprint prompt) is also part of this milestone |
| Signing | Existing `ssh-agent-client-rs 1.1.2` | No change — the agent client is already used for TLS signing; reuse for inner auth signing |

---

## wtransport 0.7.1 API Surface — Integration Map

### Server side (nosh-server / nosh-transport)

```rust
// 1. Build config — supply a full rustls::ServerConfig via with_custom_tls.
//    The outer TLS here is the proxy's cert (or a self-signed cert if nosh is direct).
//    In proxy mode: proxy terminates TLS, so nosh does NOT do mTLS at the outer layer.
let tls_server_config: rustls::ServerConfig = /* system-cert or self-signed via rcgen */;
let config = ServerConfig::builder()
    .with_bind_default(443)
    .with_custom_tls(tls_server_config)
    .build();

// 2. Create endpoint and accept sessions.
let endpoint = Endpoint::server(config)?;
loop {
    if let Some(incoming) = endpoint.accept().await {
        let session_request = incoming.await?;
        // Check URL path (e.g. "/nosh") before accepting.
        let conn: Connection = session_request.accept().await?;
        tokio::spawn(handle_session(conn));
    }
}

// 3. In handle_session — the nosh-proto channel model maps directly:
//    - Control channel:  conn.open_bi() / conn.accept_bi()  (reliable ordered)
//    - Shell output:     conn.open_bi()                     (reliable ordered)
//    - Scrollback:       conn.open_uni() / conn.accept_uni() (reliable ordered)
//    - State-sync diff:  conn.send_datagram() / conn.receive_datagram()  (RFC 9221 equivalent)
```

**Datagram note:** `max_datagram_size()` must be checked before sending (same as `quinn::Connection::max_datagram_size()`). `receive_datagram()` is the correct method name (not `read_datagram`).

### Client side (nosh-client)

```rust
// In proxy mode: supply a rustls::ClientConfig with native certs (the proxy presents a
// real cert). The inner SSH-key auth runs after the WebTransport session is established.
let config = ClientConfig::builder()
    .with_native_certs()   // or with_custom_tls(rustls::ClientConfig) for pinning
    .build()?;

let endpoint = Endpoint::client(config)?;
let conn: Connection = endpoint.connect(ConnectOptions::new("https://proxy.example.com/nosh")).await?;
// Proceed to inner auth on the control stream, then session traffic as normal.
```

### TLS integration with the proxy topology

In the reverse-proxy topology:

- The **outer TLS** is terminated by the proxy. nosh's `ServerConfig` must present a **valid CA-signed cert** (Let's Encrypt, etc.) that the proxy and browsers/clients will accept. `rcgen` is already in the workspace for self-signed certs; for proxy mode, the operator manages the cert at the proxy — nosh itself just needs to bind and accept the proxied WebTransport connection.
- The **inner auth** runs after the WebTransport session is established: nosh sends an `InnerAuthChallenge` message on the control stream, the client responds with an `InnerAuthResponse` (SSH key + signature), and the server verifies against `authorized_keys` / `known_hosts` using the existing `nosh-auth` logic.
- `with_custom_tls(rustls::ServerConfig)` / `with_custom_tls(rustls::ClientConfig)` provides the rustls escape hatch for injecting any rustls config — including custom `ClientCertVerifier` or `ServerCertVerifier` implementations — if needed for advanced scenarios.

---

## Reverse-Proxy Ecosystem — What Actually Works Today

This is the second critical question. The answer is more constrained than the design brief implies.

### nginx — NOT suitable (WebTransport not supported)

nginx 1.25.0+ supports HTTP/3 downstream termination, but it **does not proxy WebTransport connections to an upstream backend**. When nginx terminates HTTP/3 it re-proxies the request as HTTP/1.1 to the upstream — the WebTransport session dies at the proxy. There is no near-term plan from the nginx maintainers to implement WebTransport proxying (confirmed in the nginx community forum).

**Workaround:** nginx can sit in front of nosh if it is configured to pass raw UDP through without QUIC termination — but this defeats the entire reverse-proxy topology (QUIC's routing key is the connection ID, not the 5-tuple; L4 passthrough breaks under IP change). This is explicitly the wrong answer per the design brief.

### HAProxy — NOT suitable (WebTransport not implemented)

An open GitHub issue (#2256, raised August 2023) requests WebTransport support. No implementation has been assigned, no milestone, no pull requests. HAProxy is not suitable for the proxy-forwarding role.

### Envoy — Partially supported (work-in-progress, not stable)

Envoy has experimental WebTransport support via `allow_extended_connect` in the HTTP/3 protocol options. The Envoy docs explicitly label this as "work-in-progress," not covered by the security team, not stable. It may be viable for internal/testing deployments but carries API breakage risk and is not production-safe by Envoy's own guidance.

### Cloudflare Tunnel — Not confirmed for WebTransport

Cloudflare Tunnel uses QUIC between `cloudflared` and Cloudflare's edge, but WebTransport passthrough from the edge to an origin is not officially documented or confirmed as supported.

### The correct deployment model for this milestone

Given the state of the proxy ecosystem, the v1.4 milestone must be explicit about the supported deployment topology:

**Mode A — nosh runs WebTransport directly, no proxy (most practical now):** nosh binds UDP/443, presents its own cert, runs WebTransport natively. The "reverse-proxy topology" becomes a future integration path, not a v1.4 requirement. Clients connect directly; the SSH-key inner auth runs inside the WebTransport session.

**Mode B — Envoy as the proxy (experimental, internal use only):** Envoy with `allow_extended_connect: true` forwards the WebTransport CONNECT request to a backend nosh WebTransport endpoint. Inner SSH-key auth runs inside the forwarded session. This is viable for a lab demo but not production-safe; the Envoy WebTransport API may change without notice.

**What to build in the phase plan:** implement `nosh-transport` for Mode A (direct WebTransport) first, then verify Mode B with Envoy as a stretch goal. Document both as explicitly separate operating modes with different cert requirements.

---

## Migration Handover in the WebTransport Topology

QUIC connection migration works at the transport layer between client and the QUIC endpoint. In direct mode (Mode A), nosh's WebTransport endpoint IS that QUIC endpoint — migration works exactly as in the existing QUIC-direct path.

In proxy mode (Mode B), the proxy terminates QUIC, so end-to-end QUIC migration is broken. Migration handover must be implemented at the application layer:

- When the client detects a path change (e.g. its local address changes), it closes the WebTransport connection and opens a new one to the proxy.
- nosh's existing 1-RTT cold-reattach protocol (the `SessionResume` control message with sequence number) handles this transparently — the client reattaches the orphaned server-side session over the new WebTransport session.
- No new reattach logic is needed; the existing reattach path already survives QUIC connection replacement. The migration handover is a policy decision ("on network change, reconnect and reattach") not a protocol change.

**What NOT to build:** do not attempt to bridge QUIC connection IDs across the proxy — that is L4 passthrough, which is explicitly ruled out. The 1-RTT reattach is the correct answer for the proxy topology.

---

## Alternatives Considered

| Category | Recommended | Alternative | Why Not |
|----------|-------------|-------------|---------|
| WebTransport crate | `wtransport` 0.7.1 | `web-transport-quinn` 0.11.9 | `web-transport-quinn` is explicitly a single-session-owns-the-whole-QUIC-connection design with no HTTP/3 multiplexing support; it targets WASM/browser clients and has a different ergonomic model. `wtransport` provides a higher-level server API (session request accept/reject, URL routing) that maps more naturally to a server-side deployment. Both use quinn 0.11 + rustls 0.23. |
| WebTransport crate | `wtransport` 0.7.1 | `h3` + `h3-webtransport` | As of 2026-06-13 the `h3-webtransport` crate is still being moved into a separate crate and is not ready for production use. wtransport is more complete and maintained. |
| Reverse proxy | Direct WebTransport (Mode A first) | nginx | nginx has no WebTransport proxy support; terminating at nginx kills the session |
| Reverse proxy | Envoy (Mode B, experimental) | HAProxy | HAProxy has no WebTransport support, not even experimental |
| Inner auth wire | New `nosh-proto` variants on control stream | Separate auth stream | Reusing the control stream (channel 0) keeps the auth sequenced before any shell traffic; no new stream type needed for a simple challenge-response |

---

## What NOT to Add

| Avoid | Why | Use Instead |
|-------|-----|-------------|
| A second QUIC library (`s2n-quic`, `quiche`) | wtransport already uses quinn; adding a second QUIC implementation would cause runtime conflicts and bloat | wtransport wrapping quinn |
| A second async runtime | wtransport and quinn both require tokio; adding async-std or smol would cause executor conflicts | tokio only, as now |
| `rustls-native-certs` as a new dep | wtransport pulls it in as a direct dep already — it will be in the lockfile | Use wtransport's `with_native_certs()` builder method |
| Pinning `wtransport` to `0.6.x` | 0.7.1 (2026-04-26) is current; a breaking build issue with the `time` crate was reported against 0.7.x (issue #311, 2026-06-12) — watch the issue tracker before release | 0.7.1, verify the time crate fix is resolved before cutting a release |
| L4 UDP passthrough behind nginx | QUIC connection ID routing makes this incorrect; the connection migrates using connection IDs not the 5-tuple — a UDP loadbalancer will misdirect post-migration packets | WebTransport as the outer tunnel (or direct mode) |
| mTLS at the WebTransport outer layer in proxy mode | The proxy terminates TLS; nosh is not the TLS termination point in proxy mode | Inner SSH-key auth on the control stream |
| `rcgen` as a new dep | Already in the workspace (used in M1/M2 for self-signed cert pinning) | No version change needed; rcgen 0.14.8 is compatible with wtransport's rcgen 0.14.5 constraint |

---

## Version Compatibility Summary

| Package | Current workspace | wtransport 0.7.1 requires | Compatible? |
|---------|-------------------|--------------------------|-------------|
| `quinn` | 0.11.9 | `^0.11.6` | Yes — 0.11.9 satisfies ^0.11.6 |
| `rustls` | 0.23.40 | `^0.23.23` | Yes — 0.23.40 satisfies ^0.23.23 |
| `tokio` | 1.52.x | `^1.28.1` | Yes — 1.52.x satisfies ^1.28.1 |
| `rcgen` | 0.14.8 | `^0.14.5` (optional) | Yes — 0.14.8 satisfies ^0.14.5 |
| `bytes` | 1.x | `^1.4.0` | Yes |
| `thiserror` | 2.x | `^2.0.3` | Yes |
| `tracing` | 0.1.x | `^0.1.37` | Yes |

**There are no version conflicts.** Cargo will resolve a single copy of quinn and rustls across the whole workspace.

---

## Cargo.toml sketch for `nosh-transport`

```toml
[package]
name    = "nosh-transport"
version = "0.1.0"
edition = "2021"

[dependencies]
wtransport  = { version = "0.7.1", features = ["quinn"] }
nosh-proto  = { path = "../nosh-proto" }
tokio       = { version = "1", features = ["macros", "io-util"] }
tracing     = "0.1"
thiserror   = "2"

# rcgen is already in the workspace; add only if nosh-transport needs to generate
# self-signed certs directly (for direct mode with no external cert).
# rcgen = { version = "0.14", optional = true }
```

The `quinn` feature flag on `wtransport` is what exposes `Connection::quic_connection()` for reaching through to the underlying `quinn::Connection`.

---

## Known Issues to Watch

| Issue | Severity | Status |
|-------|----------|--------|
| wtransport #285: `finish()` on a unidirectional stream sometimes hangs indefinitely | Medium — affects uni streams but nosh uses bidi streams for most channels | Open, under investigation. Prefer bidi streams in nosh-transport; avoid relying on uni-stream `finish()` |
| wtransport #311: `time` crate version update breaks build (2026-06-12) | High — build failure if unresolved | Open. Verify this is resolved before pulling 0.7.1 into the release build |
| Envoy WebTransport marked "work-in-progress" by Envoy project | High for Mode B — API may break | Confirmed. Do not depend on Envoy for production deployments; use Mode A (direct) as the primary path |
| nginx: no WebTransport proxy support, no near-term plan | Blocking for nginx-fronted deployments | Confirmed from nginx community forum. nginx is not a viable proxy for this milestone |

---

## Sources

| URL | What it verified | Confidence |
|-----|-----------------|------------|
| https://crates.io/api/v1/crates/wtransport/0.7.1/dependencies | quinn ^0.11.6, rustls ^0.23.23, tokio ^1.28.1, no h3 dep, upstream quinn not a fork | HIGH |
| https://docs.rs/wtransport/latest/wtransport/config/struct.ServerConfigBuilder.html | `with_custom_tls(TlsServerConfig)` accepts `rustls::ServerConfig` alias | HIGH |
| https://docs.rs/wtransport/latest/wtransport/config/struct.ClientConfigBuilder.html | `with_custom_tls(TlsClientConfig)` accepts `rustls::ClientConfig` alias; `with_native_certs()`, `with_server_certificate_hashes()` | HIGH |
| https://docs.rs/wtransport/latest/wtransport/connection/struct.Connection.html | `open_bi`, `open_uni`, `accept_bi`, `accept_uni`, `send_datagram`, `receive_datagram`, `close`, `rtt`, `remote_address`, `max_datagram_size`, `quic_connection()` (quinn feature) | HIGH |
| https://docs.rs/wtransport/latest/wtransport/endpoint/struct.Endpoint.html | `Endpoint::server(ServerConfig)`, `Endpoint::client(ClientConfig)`, `accept()`, `connect()`, `reload_config()` | HIGH |
| https://community.nginx.org/t/http3-webtransport-webtransport-support-in-nginx/5500 | nginx maintainer: "We currently do not have near term plan to support it" | HIGH |
| https://github.com/haproxy/haproxy/issues/2256 | HAProxy WebTransport feature request open since Aug 2023; no assignee, no milestone | HIGH |
| https://www.envoyproxy.io/docs/envoy/latest/intro/arch_overview/http/http3 | Envoy WebTransport via `allow_extended_connect`: labelled "work-in-progress," not stable, not covered by security team | HIGH |
| https://github.com/BiagioFesta/wtransport/issues/285 | `finish()` hangs on unidirectional streams — open, under investigation | HIGH |
| https://github.com/BiagioFesta/wtransport/issues | Issue #311: time crate build failure (2026-06-12) — open | HIGH |
| https://crates.io/api/v1/crates/web-transport-quinn | web-transport-quinn 0.11.9 (2026-04-07): also uses quinn ^0.11, rustls ^0.23; single-session-owns-QUIC design | MEDIUM |
| https://github.com/hyperium/h3/discussions/189 | h3-webtransport not yet a published production crate | MEDIUM |

---
*Stack research for: nosh v1.4 (M7) — WebTransport-over-HTTP/3 additions*
*Researched: 2026-06-13*
