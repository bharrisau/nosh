# Milestones

## v1.0 M0-M2 Architecture-Validation Spike (Shipped: 2026-05-29)

**Phases completed:** 3 phases, 11 plans

**Delivered:** A single QUIC connection on UDP/443 carries a live interactive shell, authenticated entirely from the user's existing SSH-key identity, on Linux — proving the architecture's three foundational bets end-to-end.

**Key accomplishments:**

- QUIC transport skeleton: quinn endpoints over UDP/443 with TLS 1.3, shared `nosh/0` ALPN, a reliable bidirectional stream and RFC 9221 datagrams coexisting on one connection, plus keep-alive against the idle timeout (TRANS-01..05).
- SSH-key mutual auth wired into the TLS handshake: custom rustls SPKI-pinning verifiers (client TOFU `known_hosts`, server `authorized_keys`), real `CertificateVerify` signature verification (never stubbed), and pre-auth DoS hardening (half-open cap + auth-completion timeout) (AUTH-01..05).
- ssh-agent signing: the TLS `CertificateVerify` is produced via the ssh-agent socket so the private key is never loaded directly; live Ed25519 agent handshake passes (AUTH-04).
- PTY session core: real `portable-pty` login shell over the authenticated connection — bidirectional I/O, raw-mode RAII restore, ~40 ms coalesced resize, signal forwarding, deny-by-default env sanitization, `SessionClose{exit_code}` propagation, clean QUIC close, and a structured server-side `Session` struct as the M3 reattach seam (SESS-01..11).
- Milestone audit passed 21/21 requirements, 3/3 phases, 3/3 integration seams, 1/1 E2E flow; build/test/clippy reproduced clean (27 tests pass + 3 `#[ignore]`-gated live tests).

**Stats:** ~3,460 LOC Rust across crates `nosh-proto`, `nosh-auth`, `nosh-server`, `nosh-client`. Timeline: 2026-05-29 (single-day spike). Tag: v1.0.

**Known-by-design limitations (M3+ seams, not blockers):** `Session.identity` not yet threaded from the authenticated peer cert; datagrams carry no session traffic (enablement only); cold reattach not implemented; single-account server (no privilege drop); Ed25519-only (RFC 7250 RPK deferred). Two items (SESS-03 SIGKILL restore, SESS-06 Ctrl-C) are human-verified live (not headless-automatable) and recorded PASSED.

---
