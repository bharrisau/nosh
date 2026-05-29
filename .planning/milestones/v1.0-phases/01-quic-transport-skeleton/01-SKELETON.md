# Walking Skeleton — nosh

**Phase:** 1
**Generated:** 2026-05-29

## Capability Proven End-to-End

A `nosh-client` process connects to a `nosh-server` process over a single QUIC/UDP connection (TLS 1.3, shared ALPN), echoes bytes over a reliable bidirectional stream, round-trips an RFC 9221 datagram, does both concurrently without interference, and the connection survives 60 seconds of idle — exercising the entire transport stack the rest of the project builds on.

## Architectural Decisions

| Decision | Choice | Rationale |
|---|---|---|
| Transport | quinn 0.11.9 (QUIC, tokio runtime, rustls-ring) | Only mature async QUIC in Rust; RFC 9221 datagrams built in; rustls backend is the ssh-agent-signing seam for Phase 2 |
| TLS | rustls 0.23.x with the `ring` CryptoProvider | TLS 1.3; custom verifier traits are the Phase 2 SSH-key cert-pinning seam |
| Dev cert | rcgen 0.14 ephemeral self-signed + honest placeholder `ServerCertVerifier` (delegates real signature verification) | Keeps skeleton honest (no MITM hole); Phase 2 swaps the verifier in place |
| Wire codec | postcard 1.x behind a single `Message` enum + one codec module in `nosh-proto` | Compact Rust-to-Rust; isolated so postcard→prost is a one-file swap |
| Async runtime | tokio multi-thread | quinn's required runtime |
| Directory layout | Cargo workspace, crates under `crates/` (nosh-proto, nosh-auth, nosh-server, nosh-client) | Locked decision D-01; standing it up now avoids a Phase 2/3 restructure |
| CLI / config | clap 4 derive; `--addr`/`--port`, default port 4433 | Unprivileged dev/CI default; UDP/443 is the documented production target |
| Logging | `tracing` + `tracing-subscriber` | Structured async logging; the demo is eyeballed via tracing output |

## Stack Touched in Phase 1

- [x] Project scaffold — Cargo workspace, 4 crates, shared `[workspace.dependencies]`, builds + tests run
- [x] "Routing" (transport equivalent) — QUIC endpoint on each side, accept loop, ALPN-negotiated connection
- [x] "Data layer" (transport equivalent) — one real reliable-stream round-trip AND one real datagram round-trip on the same connection
- [x] "UI" (operator equivalent) — two runnable binaries; client drives the round-trips and prints results; server logs accept/echo via tracing
- [x] Deployment — documented local two-binary run command (`cargo run -p nosh-server` + `cargo run -p nosh-client`)

## Out of Scope (Deferred to Later Slices)

- SSH-key mutual auth, `authorized_keys`/`known_hosts`, cert-pinning verifiers, ssh-agent signing — Phase 2 (the placeholder verifier is the seam)
- PTY allocation, shell spawn, stdin/stdout pumping, env sanitization, resize, exit-code propagation — Phase 3
- RFC 7250 raw public keys, connection migration / roaming, predictive-echo datagram state-sync, channel multiplexing — later milestones
- protobuf/prost wire format — only if a non-Rust peer or published spec appears (D-04)

## Subsequent Slice Plan

Each later phase adds one vertical slice without altering these architectural decisions:

- Phase 2 (M1 Auth): replace the placeholder verifier with SSH-key cert-pinning `ServerCertVerifier`/`ClientCertVerifier`; add `nosh-auth` agent signing; client-cert auth in the server config.
- Phase 3 (M2 Session): server allocates a PTY and spawns the login shell; client raw-mode terminal; stdin/stdout over the reliable stream; `SessionClose` exit-code frame (the `Message` variant created this phase).
