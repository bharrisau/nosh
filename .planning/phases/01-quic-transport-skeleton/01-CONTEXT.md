# Phase 1: QUIC Transport Skeleton - Context

**Gathered:** 2026-05-29
**Status:** Ready for planning

<domain>
## Phase Boundary

Deliver a `nosh-server` and `nosh-client` that establish a single QUIC connection over UDP (quinn + rustls, TLS 1.3, shared ALPN), exchange bytes over a reliable bidirectional stream (echo round-trip), send/receive RFC 9221 datagrams on the same connection, prove streams and datagrams coexist without interference, and stay alive through interactive idle (keep-alive vs idle-timeout). No SSH auth (Phase 2), no PTY/session (Phase 3). Covers TRANS-01..05.

</domain>

<decisions>
## Implementation Decisions

### Workspace / crate layout
- **D-01:** Scaffold the full multi-crate Cargo workspace now — do NOT defer the split. Crates:
  - `nosh-proto` — shared wire types and the message codec (lib)
  - `nosh-auth` — stub/empty crate now; Phase 2 fills it with the rustls cert-pinning verifiers + ssh-agent signing
  - `nosh-server` — server binary
  - `nosh-client` — client binary
  Rationale: research recommends this layout; standing it up now avoids a restructure in Phase 2/3.

### Wire framing & serialization
- **D-02:** Define a real message codec in `nosh-proto` now (not raw bytes). Length-delimited frames carrying a `Message` enum serialized with **postcard** (compact serde binary, Rust-to-Rust).
- **D-03:** The codec MUST be isolated behind the single `Message` type / one codec module in `nosh-proto` so the serialization format is a one-file swap later.
- **D-04:** Migration path is **postcard → protobuf (prost)**, NOT cap'n proto. Promote to protobuf only when one of protobuf's real advantages becomes live: (a) a non-Rust peer exists (e.g. the M7 WebTransport/browser direction), or (b) we publish a stable, independently-implementable wire spec needing field-number evolution. Cap'n Proto is explicitly rejected — its zero-copy win is irrelevant for small control frames and its API is heavier.
- **D-05:** For the M0 echo proof, the reliable-stream echo and datagram round-trip may use the `Message` codec or raw bytes as convenient, but the `Message` enum must exist with room for future control frames (include a `SessionClose { exit_code, reason }` variant now as the first real message type, per the "cheap now" research item).

### Dev port & cert seam
- **D-06:** Listen address/port is configurable via flag (`--addr` / `--port`), **default 4433** so dev/CI run unprivileged. UDP/443 is documented as the production target, not the dev default.
- **D-07:** Phase 1 uses an **rcgen ephemeral self-signed certificate** plus a clearly-marked **placeholder** TLS verifier (e.g. a dev verifier named to flag it as temporary). It MUST be structured as the seam Phase 2 replaces with the real SSH-key cert-pinning `ServerCertVerifier`/`ClientCertVerifier`. The placeholder verifier must still delegate real signature verification to the CryptoProvider where applicable — do NOT ship an all-stubs `SkipServerVerification` that no-ops `verify_tls13_signature` (research PITFALL: that's a MITM hole even at the skeleton stage; keep the skeleton honest so Phase 2's swap is minimal).

### Proof / verification approach
- **D-08:** Phase 1 is proven by **both**: (a) automated integration tests (client + server in-process, or spawned, exercising handshake, stream echo, datagram round-trip, concurrent coexistence, and 60s idle-survival) as the verification gate; **and** (b) a runnable two-binary demo (`nosh-server` + `nosh-client`) that a human can eyeball.

### Claude's Discretion
- ALPN protocol string value (a stable `nosh`-prefixed constant in `nosh-proto`).
- Exact keep-alive interval and `max_idle_timeout` values (must satisfy "60s idle does not drop" — keep-alive interval comfortably below idle timeout).
- `datagram_receive_buffer_size` value (must be `Some(_)` so datagrams are enabled; research flags `None` = silently disabled).
- tokio task/loop structure for accept + stream/datagram pumps.
- Whether the demo binaries share a CLI arg parser; logging via `tracing`.
- Postcard frame length-prefix width/endianness.

</decisions>

<specifics>
## Specific Ideas

- User reasoned through the codec choice explicitly: rejected cap'n proto (zero-copy wasted on tiny control frames), considered protobuf for schema/cross-language/versioning, and chose postcard for the spike with protobuf as the named graduation path. The "why" matters — don't silently pick a different format.
- Keep the skeleton's TLS honest (no fully-stubbed verifier) specifically so Phase 2's SSH-key verifier swap is a small, clean diff.

</specifics>

<canonical_refs>
## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### Project & milestone scope
- `INIT.md` §6 (transport & session design), §9 (Rust stack), §13 (research notes) — transport bets and stack starting points
- `.planning/PROJECT.md` — milestone scope (M0–M2 spike, Linux-only) and Key Decisions
- `.planning/REQUIREMENTS.md` — TRANS-01..05 acceptance criteria

### Research (verified stack & gotchas)
- `.planning/research/STACK.md` — verified crate versions (quinn 0.11.9, rustls 0.23.x, rcgen), quinn DATAGRAM API (`TransportConfig::datagram_receive_buffer_size`, `Connection::send_datagram`/`read_datagram`/`max_datagram_size`), quinn↔rustls wiring (`QuicServerConfig`/`QuicClientConfig::try_from`)
- `.planning/research/ARCHITECTURE.md` — workspace layout (nosh-proto/nosh-auth/nosh-server/nosh-client), stream-vs-datagram split, deferred seams
- `.planning/research/PITFALLS.md` — datagrams silently disabled until receive buffer set; ALPN mandatory (QUIC error 0x178 on mismatch); 30s default idle timeout + keep-alive disabled by default; do NOT no-op `verify_tls13_signature`

No project-external specs beyond the above — requirements are captured in the docs listed.

</canonical_refs>

<code_context>
## Existing Code Insights

### Reusable Assets
- None — greenfield. This phase creates the Cargo workspace from scratch.

### Established Patterns
- None yet. Patterns established here (workspace layout, `nosh-proto` Message/codec, tracing instrumentation, placeholder-verifier seam) become the conventions for Phases 2–3.

### Integration Points
- `nosh-auth` is created as a stub this phase; Phase 2 wires its verifiers into the quinn/rustls config built here.
- The `Message` enum + codec in `nosh-proto` is where Phase 3's session control frames (resize, SessionClose) plug in.

</code_context>

<deferred>
## Deferred Ideas

- Protobuf/`.proto` wire spec — only if a non-Rust peer or published spec appears (D-04). Out of scope for the spike.
- Real SSH-key auth / cert-pinning verifiers — Phase 2.
- PTY, shell I/O, resize, exit-code propagation — Phase 3.
- Connection migration, keep-alive tuning for roaming, datagram state-sync — M3/M4.

</deferred>

---

*Phase: 01-quic-transport-skeleton*
*Context gathered: 2026-05-29*
