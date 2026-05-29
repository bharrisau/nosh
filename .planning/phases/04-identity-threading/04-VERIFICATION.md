---
phase: 04-identity-threading
verified: 2026-05-30T00:00:00Z
status: passed
score: 3/3 roadmap success criteria verified; 7/7 locked decisions met (D-07 remediated)
reverification: opus
reverification_of: 2026-05-30 initial pass (gsd-verifier inline, status=passed)
overrides_applied: 0
remediation:
  applied: 2026-05-30 (gsd-code-fixer, sonnet)
  commits: [4deaae1, 429fa01, 90c826c, 0c37178]
  notes: >-
    D-07 fingerprint now uses ssh_key::PublicKey::fingerprint(Sha256) over the SSH
    wire blob; golden test asserts SHA256:kmYcvdi2GkPeWxB6XLjrZB8JHsy2Hm8luHMFp9GMvqk
    for the all-zeros Ed25519 key — independently confirmed equal to
    `ssh-keygen -l -E sha256`. Warnings also closed: duplicate SPKI parser removed
    (single keys::nosh_key_from_spki), identity-equality + CLOSE_AUTH-path tests added,
    NoshPublicKey Debug redacted. cargo test --workspace: 34 passed, 0 failed, 3 ignored.
re_verification:
  previous_status: passed
  previous_score: 3/3
  gaps_closed: ["D-07 fingerprint format (remediated post-re-verification)"]
  gaps_remaining: []
  regressions: []
  new_findings:
    - "D-07 fingerprint does not match OpenSSH SHA256 format — hashes raw 32 bytes, not the SSH wire blob (keys.rs:87-97). Prior verification accepted it on format-shape evidence only."
gaps:
  - truth: "D-07: NoshPublicKey::fingerprint() returns the standard OpenSSH SHA256 fingerprint (must match `ssh-keygen -l -E sha256`)"
    status: failed
    reason: >-
      fingerprint() hashes the raw 32-byte Ed25519 key (self.key32) instead of the
      OpenSSH SSH wire-format public-key blob
      (length-prefixed "ssh-ed25519" || length-prefixed raw32). For a real key,
      ssh-keygen produces SHA256:FbiRL5wzZ2P85+2RCsWw/zTv6OP299vaxrIaTKRGuEY while
      nosh produces SHA256:m5fHMlH4RtT0Bu5C+Ce0EDuK+M/0RunUb+JYakc020s — a different
      value. D-07 and CONTEXT.md require the OpenSSH form. Audit logs / Phase 6
      reattach diagnostics keyed on this fingerprint will not match any operator's
      `ssh-keygen` output.
    artifacts:
      - path: "crates/nosh-auth/src/keys.rs"
        issue: "fingerprint() (lines 87-97) hashes self.key32 (32 raw bytes); must hash the SSH wire blob"
      - path: "crates/nosh-auth/src/keys.rs"
        issue: "Unit test fingerprint_format (lines 254-265) only checks shape (SHA256: prefix, 43 chars, no '='); never compares to a known ssh-keygen value, so the wrong hashing input passed CI"
    missing:
      - "Hash the SSH wire-format blob: sshstring(\"ssh-ed25519\") || sshstring(key32). The bytes are already reachable via ssh_key::public::Ed25519PublicKey (same blob that to_openssh_line base64-encodes)."
      - "Add a golden-vector test: assert fingerprint() of a known key equals the ssh-keygen -l -E sha256 output for that key."
deferred: []
human_verification: []
---

# Phase 4: Identity Threading — Opus Re-Verification Report

**Phase Goal:** Every server-side session carries the authenticated peer's SSH identity as a non-optional field, enforced by the type system.
**Verified:** 2026-05-30 (opus re-verification, EXTRA SCRUTINY)
**Status:** GAPS_FOUND
**Re-verification:** Yes — re-examining a prior inline pass (status=passed) on a stronger model.

> The initial verification marked this phase PASSED (3/3). That assessment is
> correct for the three ROADMAP success criteria. This opus re-run found one
> locked-decision defect (D-07, fingerprint format) that the prior pass missed
> because it accepted the fingerprint on format-shape evidence rather than a
> golden vector. The defect is real and observable. The three ROADMAP success
> criteria themselves remain genuinely met.

## Goal Achievement

### ROADMAP Success Criteria (the roadmap contract)

| # | Truth | Status | Evidence |
|---|-------|--------|---------|
| 1 | `Session.identity` is a non-optional `NoshPublicKey` field — compiler rejects constructing a `Session` without a verified identity | ✓ VERIFIED | `session.rs:120` `pub identity: NoshPublicKey` (no `Option`). `session::open` (session.rs:201-208) takes `identity: NoshPublicKey`. Struct literal at session.rs:255-263 has no default for `identity`; passing `None` is a type error. |
| 2 | After a successful TLS handshake, `peer_identity()` is downcast and `extract_spki_from_cert` is called before any session message is read; the connection is rejected (not silently defaulted) on extraction failure | ✓ VERIFIED | `extract_peer_identity` (server.rs:401-409) does `peer_identity()?.downcast::<Vec<CertificateDer>>().ok()?` → `certs.first()?` → `extract_spki_from_cert(leaf)?` → `nosh_key_from_spki`. Called at server.rs:175, after `incoming.await` resolves (154-164) and before `accept_bi()` (194). `None` branch (177-181): `tracing::error!` + `conn.close(CLOSE_AUTH, ...)` + `return Ok(())` — no session started. |
| 3 | All existing handshake tests still pass with no assertion changes | ✓ VERIFIED | `cargo test --workspace`: 32 passed, 0 failed, 3 ignored (1 agent unit, 1 agent integration `agent_ed25519_handshake_live`, 1 slow `idle_survival_60s`). AUTH (6), SESS (6), transport (4), nosh-auth unit (11), proto (4), nosh-server unit (1). No assertion edits in auth.rs/session.rs/transport.rs. |

**ROADMAP score: 3/3.**

### Locked Decisions (CONTEXT.md D-01..D-07)

| Decision | Status | Evidence |
|----------|--------|---------|
| D-01 `Session.identity` non-optional | ✓ MET | session.rs:120 |
| D-02 `session::open` takes `NoshPublicKey` (call site updated) | ✓ MET | session.rs:207; call site server.rs:234 passes `identity` (was `None`) |
| D-03 field doc updated (no more "None for this spike" language) | ✓ MET | session.rs:117-119 reads "Always present — a `Session` cannot be constructed without a verified identity (D-01)" |
| D-04 reject (CLOSE_AUTH + loud error!) on extraction failure; no session | ✓ MET | server.rs:177-181 |
| D-05 extract immediately after handshake, before `accept_bi`/`run_session` | ✓ MET | server.rs:175 before 194/199 |
| D-06 fingerprint in per-session tracing span | ✓ MET | server.rs:238-245, `identity = %fingerprint` |
| **D-07 fingerprint is the standard OpenSSH SHA256 form; raw key never logged** | ✗ **FAILED (format)** / ✓ (no-log) | Raw key is never logged (only `fingerprint()` reaches a span; no `?identity` anywhere — grep confirms). BUT the fingerprint value is wrong: it hashes the raw 32 bytes, not the OpenSSH SSH wire blob. See Gaps. |

## EXTRA-SCRUTINY Findings (what a thin plan might have skipped)

### 1. Is the threaded identity provably the SAME key the verifier checked? — YES (behaviorally), but via a DUPLICATED parse path

- `AuthorizedKeysVerifier::verify_client_cert` (verifier.rs:166-168): `extract_spki_from_cert(end_entity)` → `parse_ed25519_from_spki`.
- `extract_peer_identity` (server.rs:407-408): `extract_spki_from_cert(certs.first())` → `nosh_key_from_spki`.
- Both run `extract_spki_from_cert` on the leaf cert. In rustls/quinn the peer chain is end-entity-first, so `certs.first()` IS the `end_entity` the verifier validated. `nosh_key_from_spki` (keys.rs:194-205) and `parse_ed25519_from_spki` (verifier.rs:218-229) are **byte-for-byte identical** logic. So the stored identity is provably the authorized key. ✓
- **However, CONTEXT.md "Claude's Discretion" explicitly said "do not duplicate the SPKI-parsing logic."** The executor created `nosh_key_from_spki` as a verbatim copy of `parse_ed25519_from_spki` rather than exposing the existing function. There are now three call sites across two identical implementations (verifier.rs:63, verifier.rs:168, keys.rs:194). **Not a behavior gap today, but a maintenance footgun** — a future fix to one parser (e.g. to accept a different SPKI encoding) could silently diverge the verifier's view of identity from the session's view, which is exactly the "no second divergent parse path" property the phase was meant to guarantee. WARNING.

### 2. Ordering / TOCTOU — clean

`incoming.await` (server.rs:158) resolves only after the full TLS 1.3 handshake **including** the mandatory client-cert verification (`client_auth_mandatory()=true`, verifier.rs:152). Identity extraction (175) is strictly after that and strictly before `accept_bi` (194). No window where a session or stream is created pre-identity. ✓ (Pitfall #11 avoided.)

### 3. Could `extract_peer_identity` succeed with a wrong/empty/non-Ed25519/malformed cert? — NO

- Empty chain → `certs.first()?` returns `None` → CLOSE_AUTH. ✓
- Multiple certs → takes leaf (`.first()`), correct. ✓
- Non-Ed25519 SPKI → `nosh_key_from_spki` length/prefix checks return `None`. ✓
- Malformed DER → `extract_spki_from_cert` errors → `.ok()?` → `None`. ✓
- All four covered by `None` → CLOSE_AUTH. (Unit-tested at the `nosh_key_from_spki` level: keys.rs:240-251. NOT exercised through `extract_peer_identity` end-to-end — see finding 6.)

### 4. Fingerprint format — WRONG (see D-07 gap)

OpenSSH SHA256 fingerprints hash the **SSH wire-format public-key blob**, not the raw key. Verified against `ssh-keygen -l -E sha256` on a freshly generated Ed25519 key:
- `ssh-keygen`: `SHA256:FbiRL5wzZ2P85+2RCsWw/zTv6OP299vaxrIaTKRGuEY`
- `nosh fingerprint()` (hash over raw 32 bytes): `SHA256:m5fHMlH4RtT0Bu5C+Ce0EDuK+M/0RunUb+JYakc020s`

These differ. D-07 requires the OpenSSH form. **The raw key is never logged** — that half of D-07 holds — but the format requirement fails. Severity is moderate: the fingerprint is audit/log-only this phase (no security wiring depends on it), but Phase 6 reattach diagnostics and any operator cross-referencing logs against `ssh-keygen` will be misled.

### 5. CLOSE_AUTH collision / reachability — clean

`CLOSE_OK=0`, `CLOSE_PROTOCOL=1`, `CLOSE_AUTH=2` (server.rs:137-142) are distinct and follow the established convention. `CLOSE_AUTH` is reached only via the `None` arm (177-181), which `return`s immediately — not bypassable. No other code path uses code 2. ✓

### 6. Rejection-path test coverage — GAP (coverage, not behavior)

- The verifier-level rejection (unknown key, forged CertificateVerify) IS tested: auth.rs:50-70 `unknown_client_key_rejected`, auth.rs:143-164 `forged_certificate_verify_rejected`. These prove an unauthorized peer never gets a session — which structurally also means `extract_peer_identity` never has to reject a valid-but-unauthorized peer.
- **No test asserts the positive threading invariant** (that `Session.identity` equals the client's authenticated key) — it is verified only by the type system + code inspection. **No test exercises the `extract_peer_identity` `None` branch / CLOSE_AUTH** directly; it is effectively dead-defensive code (D-04 itself says it "should never happen"). The `nosh_key_from_spki` rejection branches are unit-tested (keys.rs:240-251) but not through the server helper. WARNING — acceptable for a defensive invariant, but the happy-path identity-equality assertion is a notable coverage hole given this is the phase's whole point.

### 7. Latent: `NoshPublicKey` derives `Debug` exposing `key32`

keys.rs:37 `#[derive(..., Debug)]` would print raw key bytes if anyone `?`-logs a `NoshPublicKey`. No current call site does (grep clean), so D-07's "never logged" holds today, but a custom `Debug` redacting to the fingerprint would harden against a future `tracing::debug!(?identity)`. INFO.

## Required Artifacts

| Artifact | Expected | Status | Details |
|----------|----------|--------|---------|
| `crates/nosh-auth/src/keys.rs` | `fingerprint()` + `nosh_key_from_spki()` | ⚠️ PRESENT, fingerprint WRONG | `nosh_key_from_spki` (194-205) correct; `fingerprint` (87-97) hashes wrong input |
| `crates/nosh-auth/src/lib.rs` | `nosh_key_from_spki` exported | ✓ VERIFIED | line 23 |
| `crates/nosh-server/src/session.rs` | `Session.identity: NoshPublicKey`; `open` takes it | ✓ VERIFIED | 120, 207, 255-263 |
| `crates/nosh-server/src/server.rs` | CLOSE_AUTH, `extract_peer_identity`, extraction placement, fingerprint span | ✓ VERIFIED | 142, 401-409, 175-181, 238-245 |
| `crates/nosh-auth/Cargo.toml` | `sha2`, `base64` deps | ✓ VERIFIED | both direct deps |

## Key Link Verification

| From | To | Via | Status |
|------|-----|-----|--------|
| `handle_connection` | `extract_peer_identity` | direct call (server.rs:175) before `accept_bi` (194) | ✓ WIRED |
| `extract_peer_identity` | `extract_spki_from_cert` + `nosh_key_from_spki` | server.rs:407-408 | ✓ WIRED |
| `run_session` | `session::open` | `identity` param forwarded (server.rs:199→234) | ✓ WIRED |
| `run_session` | tracing span | `sess.identity.fingerprint()` (238→244) | ⚠️ WIRED but emits a non-OpenSSH value (D-07) |
| verifier parse path | server extraction parse path | identical logic, two copies | ⚠️ duplicated (CONTEXT discretion violated) |

## Behavioral Spot-Checks

| Behavior | Command | Result | Status |
|----------|---------|--------|--------|
| Full workspace suite | `cargo test --workspace` | 32 passed, 0 failed, 3 ignored | ✓ PASS |
| `Session.identity` non-optional | read session.rs:120 | `NoshPublicKey`, no `Option` | ✓ PASS |
| Extraction before `accept_bi` | read server.rs:175 vs 194 | ordered correctly | ✓ PASS |
| OpenSSH fingerprint match | `ssh-keygen -l -E sha256` vs nosh hash-of-raw | values DIFFER | ✗ FAIL |
| `nosh_key_from_spki` rejects bad SPKI | keys.rs:240-251 tests | pass | ✓ PASS |
| Raw key logged anywhere? | grep logging macros for `?identity`/`key32` | none | ✓ PASS |

## Requirements Coverage

| Requirement | Description | Status | Evidence |
|-------------|-------------|--------|---------|
| IDENT-01 | Server threads authenticated peer SSH identity (SPKI from TLS handshake) into `Session.identity` on every new connection, before any session message is processed | ✓ SATISFIED | SC1+SC2 met; threading is provably the authorized key. (IDENT-01 says nothing about fingerprint format — the D-07 defect does not block IDENT-01 itself.) |

## Anti-Patterns Found

| File | Line | Pattern | Severity | Impact |
|------|------|---------|----------|--------|
| keys.rs | 87-97 | fingerprint hashes raw key, not OpenSSH wire blob | 🛑 Blocker (D-07) | Audit fingerprints won't match `ssh-keygen`/operator expectations |
| keys.rs | 254-265 | `fingerprint_format` test checks only shape, no golden vector | ⚠️ Warning | Let the wrong hashing input pass CI |
| keys.rs / verifier.rs | 194-205 / 218-229 | duplicated SPKI parser (CONTEXT said do not duplicate) | ⚠️ Warning | Divergence risk between verifier's and session's notion of identity |
| keys.rs | 37 | `Debug` derive prints `key32` | ℹ️ Info | Latent key-leak footgun if ever `?`-logged |
| — | — | TBD/FIXME/XXX/TODO/HACK/PLACEHOLDER scan of all 4 files | clean | none |

## Gaps Summary

The phase **achieves its core goal**: the authenticated peer's Ed25519 identity is threaded into a compile-time-mandatory `Session.identity`, extracted strictly post-handshake and pre-session, with a hard CLOSE_AUTH rejection on any extraction failure, and it is provably the same key the verifier pinned. All three ROADMAP success criteria and decisions D-01..D-06 are genuinely met; tests are green with no assertion changes.

**One locked decision (D-07) fails on its format requirement.** `NoshPublicKey::fingerprint()` computes SHA256 over the raw 32-byte key instead of the OpenSSH SSH wire-format blob, so the logged `SHA256:...` value does not match `ssh-keygen -l -E sha256`. The shape-only unit test masked it. This is audit-log-only in Phase 4 (no security path depends on it), but it is a real, observable deviation from a locked decision and will mislead Phase 6 reattach diagnostics and operators.

**Two warnings:** (a) the SPKI parser was duplicated despite an explicit "do not duplicate" instruction, reintroducing the divergent-parse-path risk the phase was meant to eliminate; (b) there is no test asserting the positive identity-equality invariant or exercising the CLOSE_AUTH path.

Recommended: fix `fingerprint()` to hash the wire blob + add a golden-vector test before relying on the fingerprint in Phase 6. The duplicate-parser and Debug-derive items can be folded into the same cleanup.

---

_Re-verified (opus): 2026-05-30_
_Verifier: Claude Opus (gsd-verifier, extra-scrutiny re-run)_
