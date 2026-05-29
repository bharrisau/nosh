# Phase 2: SSH-Key Mutual Auth - Discussion Log

> **Audit trail only.** Not consumed by downstream agents. Decisions live in 02-CONTEXT.md.

**Date:** 2026-05-29
**Phase:** 02-ssh-key-mutual-auth
**Mode:** discuss (default, interactive — inline via /gsd:autonomous --interactive)
**Areas discussed:** Host-key trust/TOFU, Key & file locations, Algorithm scope, Pre-auth DoS limits

## Pre-discussion scouting
Read the Phase 1 auth seam: `crates/nosh-auth/src/verifier.rs` (PlaceholderServerVerifier — real sig delegation, accepts any cert), `crates/nosh-server/src/server.rs` (rcgen localhost cert, with_no_client_auth), `crates/nosh-client/src/client.rs` (placeholder verifier, with_no_client_auth). Framed gray areas against the actual swap needed.

## Questions & Answers

### Host-key trust / TOFU
- Options: Auto-TOFU hard-fail mismatch / SSH-style prompt / Strict no-TOFU
- **Selected:** Auto-TOFU, hard-fail on mismatch (record+proceed silently on first contact)

### Key & file locations
- Options: Mirror OpenSSH paths / nosh-specific config dir
- **Selected:** Mirror OpenSSH paths (client ssh-agent + --identity + ~/.ssh/known_hosts; server host-key file + ~/.ssh/authorized_keys; all flag-overridable)

### Algorithm scope
- Options: Ed25519 only / Ed25519+ECDSA / Ed25519+ECDSA+RSA
- **Selected:** Ed25519 only (sidesteps the flagged RSA ssh-agent SHA-2 risk; matches AUTH-03 + success criterion)

### Pre-auth DoS limits
- Options: Defaults configurable (~64 / ~5s) / Conservative (~16 / ~3s) / Claude's discretion
- **Selected:** Defaults, configurable (~64 concurrent half-open, ~5s auth timeout)

## Roadmapper caveats folded as inputs (not blockers)
- RSA ssh-agent SHA-2 flag uncertainty → resolved by Ed25519-only decision
- verify_tls13_signature / CertificateVerify signing round-trip → flagged as riskiest; validate agent-signing round-trip early

## Deferred Ideas
- RSA/ECDSA support, RFC 7250 RPK, host-key rotation, authorized_keys options enforcement — later. PTY/session — Phase 3.
