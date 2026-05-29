# Phase 4: Identity Threading - Context

**Gathered:** 2026-05-30
**Status:** Ready for planning

<domain>
## Phase Boundary

Wire the authenticated peer's SSH identity (the Ed25519 public key proven during the TLS mutual handshake) into the server-side `Session.identity`, on every new connection, before any session work begins. This fills the deliberate v1.0 seam (`Session.identity` is currently always `None`). It is the prerequisite key that Phase 5 (session persistence, keyed/capped per identity) and Phase 6 (reattach authorization bound to identity) depend on.

In scope: extracting the peer key after handshake, threading it into `Session`, enforcing it as a hard invariant, and logging it for audit. NOT in scope: persistence, orphaning, reattach, per-identity caps, or any use of the identity beyond storing + logging it (those are Phases 5-6).
</domain>

<decisions>
## Implementation Decisions

### Identity field strictness
- **D-01:** Change `Session.identity` from `Option<NoshPublicKey>` to a non-optional `NoshPublicKey`. The "every session belongs to an authenticated identity" invariant becomes a compile-time guarantee — a `Session` cannot be constructed without a verified identity.
- **D-02:** `session::open(...)`'s `identity` parameter changes from `Option<NoshPublicKey>` to `NoshPublicKey` to match. The single current call site (`server.rs:215`, today passing `None`) is updated to pass the extracted key.
- **D-03:** Update the field doc comment in `session.rs` (currently "`None` for this spike … noted M3 seam") to reflect that identity is now always the authenticated peer key.

### Missing-identity behavior
- **D-04:** Client-cert auth is mandatory (the `AuthorizedKeysVerifier` rejects any connection without a pinned client cert), so a resolved connection should ALWAYS have a parseable peer identity. If extraction nonetheless fails (no `peer_identity`, empty cert chain, or non-Ed25519/malformed SPKI), the server REJECTS the connection: close with a structured QUIC application close code and emit a loud `error!` log. It MUST NOT start a session. An unauthenticated session must be impossible.
- **D-05:** This rejection happens in `handle_connection` immediately after the handshake resolves (after `incoming.await`, server.rs:154) and before `accept_bi`/`run_session` — extract identity as early as possible (avoids Pitfall #11: capturing identity before the handshake completes).

### Identity in logs / audit
- **D-06:** Add an OpenSSH-style `SHA256:<base64>` fingerprint of the authenticated key to the per-session tracing span (alongside the existing `session_id` / `peer` / `username` fields at `server.rs:219`).
- **D-07:** Add a small `fingerprint()` helper to `NoshPublicKey` (SHA256 over the raw key / standard OpenSSH fingerprint form). The raw private/public key bytes are NEVER logged — only the fingerprint.

### Claude's Discretion
- Exact mechanism for exposing the SPKI→key parse: either make `parse_ed25519_from_spki` (`verifier.rs:218`) `pub(crate)`/expose a public `nosh_key_from_spki` in `nosh-auth`, or reuse `extract_spki_from_cert` + `NoshPublicKey` construction. Planner/executor picks the cleanest reuse — do not duplicate the SPKI-parsing logic.
- The structured close code value for D-04 (reuse `CLOSE_PROTOCOL` or add a dedicated auth-failure code) — implementer's choice, keep it consistent with the existing `CLOSE_OK`/`CLOSE_PROTOCOL` convention in `server.rs`.
- Exact fingerprint encoding details (padding/no-padding base64) — match OpenSSH `SHA256:` convention.
</decisions>

<specifics>
## Specific Ideas

- The peer key extraction must mirror what the server-side `AuthorizedKeysVerifier` already does at `verifier.rs:166-168` (`extract_spki_from_cert` → `parse_ed25519_from_spki`) — the SAME key that was checked against `authorized_keys` is the one threaded into the session, so identity-in-session is provably the authorized identity (no second, divergent parse path).
</specifics>

<canonical_refs>
## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### Requirements & roadmap
- `.planning/REQUIREMENTS.md` — IDENT-01 (the requirement this phase delivers)
- `.planning/ROADMAP.md` §"Phase 4: Identity Threading" — goal + success criteria
- `.planning/research/SUMMARY.md` §"Phase 1: Identity Threading" and §"Critical Pitfalls" #3 — the architectural rationale and the non-optional-field recommendation
- `.planning/research/ARCHITECTURE.md` — identity-threading section (peer_identity downcast → SPKI → NoshPublicKey path, with file:line citations)
- `.planning/research/PITFALLS.md` — Pitfall #11 (identity captured before handshake completes; cert vs SPKI confusion)

### Code touchpoints (verified this session)
- `crates/nosh-server/src/server.rs:145-182` — `handle_connection` (where to extract identity, post-handshake); `:215` — the `session::open(..., None)` call site to update; `:219` — the session span to add fingerprint to
- `crates/nosh-server/src/session.rs:115-127` — `Session` struct + `identity` field/doc; `:205-260` — `session::open` signature + construction
- `crates/nosh-auth/src/verifier.rs:166-168, 218-229` — existing `extract_spki_from_cert` + `parse_ed25519_from_spki` (the parse path to reuse/expose)
- `crates/nosh-auth/src/keys.rs:38-83, 172` — `NoshPublicKey` (where `fingerprint()` helper lands) + `extract_spki_from_cert`
</canonical_refs>

<code_context>
## Existing Code Insights

### Reusable Assets
- `nosh_auth::keys::extract_spki_from_cert` (`keys.rs:172`, public): cert → SPKI DER. Reuse directly.
- `parse_ed25519_from_spki` (`verifier.rs:218`, currently private): SPKI DER → `NoshPublicKey`. Expose (or provide an equivalent public wrapper in `nosh-auth`) rather than duplicating.
- `NoshPublicKey` (`keys.rs:38`): has `from_raw`, `key32`, `spki_der`, `to_openssh_line`; needs a new `fingerprint()` helper for D-07.
- `quinn::Connection::peer_identity()`: returns the peer cert chain after the handshake (downcast to `Vec<CertificateDer<'static>>`).

### Established Patterns
- Structured QUIC close codes already exist: `CLOSE_OK = 0`, `CLOSE_PROTOCOL = 1` (`server.rs:136-138`) — follow this for D-04.
- The server already downcasts `handshake_data()` for ALPN logging (`server.rs:167-172`) — the same post-handshake point is where identity extraction belongs.
- Per-session `tracing::info_span!` at `server.rs:219` is the place to add the fingerprint field.

### Integration Points
- `handle_connection` (server.rs) extracts identity → passes it through `run_session` → into `session::open`. `run_session`'s signature gains an `identity: NoshPublicKey` parameter.
- The change is additive to the message protocol (no `nosh-proto` changes needed for Phase 4).
</code_context>

<deferred>
## Deferred Ideas

- Using the identity as a registry/persistence key, per-identity session caps — Phase 5.
- Reattach authorization comparing the reconnecting identity against the orphaned session's identity — Phase 6.
- Multi-account servers / privilege drop per identity — out of scope for v1.1 (noted in PROJECT.md).
- Non-Ed25519 identities (RSA/ECDSA) — out of scope; v1.1 stays Ed25519-only.

None of these are implemented or scaffolded in Phase 4 — identity is only stored + logged here.
</deferred>

---

*Phase: 04-identity-threading*
*Context gathered: 2026-05-30*
