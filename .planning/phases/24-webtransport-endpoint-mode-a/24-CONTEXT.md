# Phase 24: WebTransport Endpoint + Mode A - Context

**Gathered:** 2026-06-13 (batched all-phase discussion v1.4)
**Status:** Ready for planning

<domain>
## Phase Boundary

A nosh server can listen on UDP/443 as a **direct WebTransport-over-HTTP/3 endpoint (Mode A — no proxy)** and carry a fully interactive shell over it: datagram state-sync, predictive echo, and reliable control/scrollback channels, all over the `wtransport` session via the Phase 23 transport trait. Includes downgrade protection. Inner SSH-key auth is NOT wired here (Phase 25) — this phase proves the WebTransport pump end-to-end with a test-only auth stub gated out of release builds.
</domain>

<decisions>
## Implementation Decisions

### Crate integration (research-resolved)
- **D-01:** Add `wtransport = { version = "0.7.1", default-features = false, features = ["self-signed", "ring"] }` (confirm exact feature names at impl) — `default-features = false` + explicit `ring` to prevent the rustls crypto-provider feature-unification panic (ring + aws-lc-rs). Gate `cargo tree -f "{p} {f}" | grep rustls` showing only `ring` is SC#4.
- **D-02:** **Pin `time = "=0.3.47"`** at the workspace level. `wtransport` issue #311 (filed 2026-06-12, still open, no 0.7.2) — a newer `time` release breaks the build via rcgen→time coherence. This pin fixes both wtransport and the existing rcgen 0.14.8 usage. (See re-ask trigger — re-verify at phase start in case #311 is resolved by then.)
- **D-03:** Datagram MTU sizing uses `wtransport::Connection::max_datagram_size()` — **not** `max_datagram_payload_size()` (that method does not exist). `max_datagram_size()` already subtracts the WebTransport capsule (Quarter Stream ID) overhead, so the returned value is the directly-usable payload budget. (Corrects SUMMARY.md's guessed name — verified against live source.) This is the WebTransport wrapper's `NoshTransport::max_datagram_size` impl; it must NOT pass through quinn's raw value.

### Outer TLS / certificate (Mode A) — user decision
- **D-04:** The WebTransport outer TLS presents a **real CA-signed certificate (Let's Encrypt)**, loaded from operator-configured cert + key file paths (PEM). nosh does **not** implement ACME — certificate acquisition/renewal is an external sidecar process (certbot/lego/caddy-as-cert-tool); nosh just reads the files. ACME-in-nosh is explicitly out of scope (see Deferred).
- **D-05:** Security does **not** rest on the outer chain. The outer CA cert gives a genuine HTTPS/HTTP-3 server identity (browser-likeness, proxy-friendliness); the **inner SSH-key handshake (Phase 25) is the authoritative end-to-end mutual auth**. The client performs standard outer-TLS validation, but a compromised/terminating proxy is contained by inner auth. This composition MUST be stated in SEC-01 (Phase 27).
- **D-06:** Native QUIC mode is **unchanged** — it keeps the v1.0 self-signed-cert SPKI-pinning model. The CA cert is a WebTransport-outer-layer concern only.

### Transport mode model — user decision
- **D-07:** **One transport per process.** A `--mode native|webtransport` flag selects the listener. A server started `--mode webtransport` **rejects raw-QUIC (non-WebTransport) connection attempts** (downgrade protection, WT-05). To serve both, run two processes. (Chosen over dual-listen and same-port auto-detect for the cleanest security boundary.)
- **D-08:** Client gets a matching `--webtransport` flag to dial in WebTransport mode.

### Claude's Discretion
- **Listen port:** default UDP/443, with a configurable `--port`. Document the privilege requirement (bind 443 needs root or `setcap CAP_NET_BIND_SERVICE` / a sidecar) — don't silently fail. Planner picks the flag name.
- Exact `wtransport::ServerConfigBuilder` chain (`with_bind_address` + `with_custom_tls(rustls::ServerConfig)` to inject the CA cert) — verify the builder API at impl (re-ask trigger below). The `with_custom_tls` escape hatch is how the loaded CA cert reaches the WebTransport server config.
- Test-only auth stub: gate behind a `test-support` cargo feature (the v1.3 pattern), NOT `#[cfg(test)]` — so integration tests in `nosh-client` can see it; release builds must reject any connection lacking inner auth.
- `open_bi`'s `OpeningBiStream` double-await asymmetry is absorbed inside the WebTransport `NoshTransport` wrapper (per Phase 23 D-spec).
</decisions>

<specifics>
## Specific Ideas

- This phase's win condition: `nosh-server --mode webtransport` ↔ `nosh-client --webtransport` delivers a live shell with keystrokes/output/resize, and datagram sync + predictive echo + scrollback all behave identically to native QUIC (SC#1/#2). Prove the pump first; auth in Phase 25.
- Avoid relying on `wtransport` unidirectional-stream `finish()` (open upstream issue #285 — hangs); prefer bidi streams, which nosh already uses for channels.
</specifics>

<canonical_refs>
## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### Phase scope & decisions
- `.planning/ROADMAP.md` §"Phase 24" — goal + 5 success criteria
- `.planning/REQUIREMENTS.md` — WT-02, WT-03, WT-05
- `.planning/research/SUMMARY.md` §"Phase 2: WebTransport Endpoint + Outer TLS Wiring" + §"Recommended Stack"

### Stack & API (read before integrating wtransport)
- `.planning/research/STACK.md` — wtransport 0.7.1 dependency graph, version coexistence proof, `with_custom_tls` escape hatch, known issues #285/#311
- `.planning/research/PITFALLS.md` — WT-1 (crypto-provider unification), WT-2 (datagram MTU), WT-5 (downgrade / mode flag)
- Verified API facts (this session's research): method is `Connection::max_datagram_size()`; pin `time = "=0.3.47"` for #311; `open_bi`→`OpeningBiStream` double-await; `with_custom_tls(rustls::ServerConfig/ClientConfig)`

### Architecture
- `.planning/research/ARCHITECTURE.md` §"WebTransport endpoint + outer TLS" — `WtransportServerConnection`/`WtransportClientConnection`, `build_wt_server_config`, `run_wt_accept_loop`
- `crates/nosh-auth/src/` — existing cert/key loading to reuse for the outer config; `CLAUDE.md` note "never use PKI chains for host verification" applies to native QUIC, NOT the WebTransport outer CA cert (documented exception, D-05)

### Topology rationale
- `CLAUDE.md` §"Topology note that constrains the design" — why WebTransport + inner auth, why not L4 passthrough
- `INIT.md` §12 / M7 sections — WebTransport reverse-proxy topology
</canonical_refs>

<code_context>
## Existing Code Insights

### Reusable Assets
- Phase 23's `NoshTransport` trait — the WebTransport connection implements it; the session pump is reused unchanged.
- `nosh-auth` cert/key handling and `rustls::ServerConfig`/`ClientConfig` construction — reused to build the outer TLS config (with the CA cert instead of self-signed).
- `test-support` cargo-feature pattern from v1.3 Phase 21 for the test-only auth stub.

### Established Patterns
- `with_custom_tls` lets the existing rustls configs flow into wtransport verbatim — minimal new auth code in this phase.
- Single quinn stream per channel (v1.3) maps onto wtransport bidi streams 1:1.

### Integration Points
- WebTransport `NoshTransport` impl plugs into the Phase 23 seam; `--mode`/`--webtransport` flags on both binaries; the outer rustls config consumes the CA cert files.
</code_context>

<deferred>
## Deferred Ideas

- **ACME / automatic certificate management inside nosh** — out of scope; operator runs an external cert sidecar (user decision). Could be a future QoL item but not this milestone.
- **Mode B (Envoy-fronted proxy deployment)** — Future stretch (WT-STRETCH-01); not a committed phase.

## Pending Decisions — Re-Ask Before Planning

1. **wtransport #311 / `time` pin status** — *Dependency: external (upstream wtransport).*
   RE-ASK TRIGGER (research, autonomous): at Phase 24 planning start, re-check whether wtransport issue #311 is resolved / a 0.7.2+ is published. If resolved, drop or relax the `time = "=0.3.47"` pin (D-02). If still open, keep the pin. Researcher/planner verifies; not a user question unless the pin causes a conflict elsewhere.
2. **Exact `wtransport::ServerConfigBuilder` API** — *Dependency: external (wtransport 0.7.x surface at impl time).*
   RE-ASK TRIGGER (research): confirm the `with_bind_address` + `with_custom_tls` builder chain and feature-flag names against docs.rs for the resolved version before writing endpoint code (SUMMARY flagged this as the Phase-24 research item).
</deferred>

---

*Phase: 24-webtransport-endpoint-mode-a*
*Context gathered: 2026-06-13*
