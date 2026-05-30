# Phase 4: Identity Threading - Discussion Log

> **Audit trail only.** Do not use as input to planning, research, or execution agents.
> Decisions captured in CONTEXT.md — this log preserves the discussion.

**Date:** 2026-05-30
**Phase:** 04-identity-threading
**Mode:** discuss (interactive, via /gsd:autonomous --interactive)
**Areas discussed:** Identity field strictness, Missing-identity behavior, Identity in logs/audit

## Area selection

Presented 3 gray areas (multiSelect). User selected all three to discuss:
- Identity field strictness
- Missing-identity behavior
- Identity in logs/audit

## Questions & decisions

### Identity field strictness
- **Options presented:** Non-optional now (recommended) / Keep Option for now
- **User selected:** Non-optional now
- **Outcome:** D-01/D-02/D-03 — `Session.identity` and `session::open`'s param become non-optional `NoshPublicKey`; compile-time invariant that every session is authenticated.

### Missing-identity behavior
- **Options presented:** Reject & close cleanly (recommended) / Panic the connection task
- **User selected:** Reject & close cleanly
- **Outcome:** D-04/D-05 — on (theoretically impossible) extraction failure, close with a structured QUIC code + loud error log, before any session starts; extract as early as possible after handshake.

### Identity in logs / audit
- **Options presented:** Yes — SHA256 fingerprint (recommended) / No fingerprint logging
- **User selected:** Yes — SHA256 fingerprint
- **Outcome:** D-06/D-07 — add OpenSSH-style `SHA256:` fingerprint to the session span; add a `fingerprint()` helper on `NoshPublicKey`; raw key never logged.

## Claude's discretion (noted in CONTEXT.md)
- Exact mechanism to expose the SPKI→key parse (expose `parse_ed25519_from_spki` vs public wrapper) — reuse, don't duplicate.
- Structured close-code value for the reject path.
- Fingerprint base64 encoding details (match OpenSSH).

## Deferred ideas
- Identity as persistence/registry key + per-identity caps → Phase 5.
- Reattach identity comparison → Phase 6.
- Multi-account / privilege drop, non-Ed25519 identities → out of scope for v1.1.

## Scope creep redirected
None — discussion stayed within the identity-threading boundary.
