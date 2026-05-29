---
phase: 02
phase_name: ssh-key-mutual-auth
date: 2026-05-29
depth: deep
review_focus: security-critical (SSH-key mutual auth)
files_reviewed: 6
status: findings
findings:
  critical: 0
  warning: 1
  info: 2
  total: 3
---

# Code Review: Phase 02 — SSH-Key Mutual Auth

Security-critical review of the custom rustls verifiers, ssh-agent signer, SPKI
pinning, TOFU, Ed25519-only enforcement, and the pre-auth DoS cap.

## Files reviewed

- crates/nosh-auth/src/verifier.rs
- crates/nosh-auth/src/signer.rs
- crates/nosh-auth/src/keys.rs
- crates/nosh-auth/src/lib.rs
- crates/nosh-client/src/client.rs
- crates/nosh-server/src/server.rs

## Security checklist (all PASS)

- **Real signature verification (no stubs).** Both `HostKeyVerifier` and
  `AuthorizedKeysVerifier` delegate `verify_tls12_signature` /
  `verify_tls13_signature` to rustls' real `verify_tls12_signature` /
  `verify_tls13_signature` with `provider.signature_verification_algorithms`.
  No no-op returns of `HandshakeSignatureValid::assertion()`. A MITM presenting
  a pinned SPKI but signing the transcript with another key is rejected here.
  (verifier.rs:87-105, 167-185)
- **SPKI pinning compares the full key.** `parse_ed25519_from_spki` requires
  `spki.len() == 44`, validates the fixed 12-byte RFC 8410 prefix, and copies
  the trailing 32 bytes into a fixed `[u8;32]`. `NoshPublicKey` equality is
  derived `[u8;32]` array equality — no truncation, no substring/prefix match.
  Server uses `Vec::contains` (full equality); client uses `==`. (verifier.rs:194-205,
  keys.rs:37-40)
- **authorized_keys / known_hosts parsing is strict.** authorized_keys parses
  each non-comment line via `PublicKey::from_openssh` and rejects non-Ed25519;
  options/comments don't cause a wrong key to match because matching is on the
  parsed key bytes, not on raw line text (D-07). known_hosts matches the host
  token in column 1 exactly (`h == host`), then parses the key column. (keys.rs:85-147)
- **CertificateVerify signed raw — no double-hash/reconstruction.**
  `Ed25519HandshakeSigner::sign` passes rustls' `message` straight to the inner
  `RawEd25519Signer`. The ssh-agent path returns `ssh_key::Signature::as_bytes()`
  and `try_into::<[u8;64]>()` enforces an exact 64-byte raw Ed25519 signature,
  rejecting any wire-format/wrong-length response. (signer.rs:60-70, 279-290)
- **TOFU records only on genuinely-unknown hosts; hard-fail on mismatch.**
  `lookup_known_host` returning `Some(pinned)` with `pinned != presented`
  returns `Error::General(...)` and does NOT overwrite. Recording happens only
  in the `None` branch, under `tofu_lock`. (verifier.rs:62-85)
- **Ed25519-only enforced cleanly.** `from_ssh_public`, `AgentSigner::new`,
  `load_host_key`, `InProcessEd25519Signer::from_ssh_private`, and
  `lookup_known_host` all use `.ed25519()` and `bail!`/`context`/skip on `None`
  — no `unwrap` panics, no silent accept of other key types.

## Findings

### WR-01 (Warning) — Pre-auth semaphore permit held for the entire session, not just the handshake
**File:** crates/nosh-server/src/server.rs:94-119

The semaphore permit (`max_concurrent`, default 64) is acquired in the accept
loop and moved into the per-connection task as `let _permit = permit;`, so it is
released only when the whole connection (handshake **and** the authenticated
echo session) ends. The stated intent (D-13 / FOOTGUN-3) is a cap on concurrent
*pre-auth* handshakes to bound half-open state. As written, 64 long-lived
*authenticated* sessions exhaust the pool and cause all new connections —
including new auth attempts — to be `refuse()`d. This is an availability/DoS
consideration, not an auth bypass (no unauthenticated connection is ever
admitted). The behaviour is documented in the inline comment, so it may be
intentional for the spike; flagging because the variable name and D-13 framing
imply a pre-auth-only cap. If a pre-auth-only cap is desired, drop the permit
once `incoming.await` resolves (handshake complete) before running the session.
No code change applied — needs a product decision, not a safe mechanical fix.

### INFO-01 — `getrandom_seed` reads /dev/urandom directly and panics on failure
**File:** crates/nosh-auth/src/signer.rs:111-116

`InProcessEd25519Signer::generate()` seeds from `/dev/urandom` and `expect()`s on
open/read failure. This is only used for tests and the agent-unavailable
fallback, and the comment scopes it to the Linux spike target, so it is
acceptable here. For portability/production, prefer the `getrandom` crate or
`OsRng` so non-Linux targets and sandboxed environments without `/dev/urandom`
are handled, and a CSPRNG failure surfaces as an error rather than a panic.

### INFO-02 — known_hosts written world-readable with no atomicity
**File:** crates/nosh-auth/src/keys.rs:151-166

`record_known_host` appends with default file permissions and no temp-file +
rename. known_hosts holds only public keys, so confidentiality is not at risk,
and the `tofu_lock` plus `O_APPEND` make concurrent appends within one process
safe. A crash mid-write could leave a partial line, and cross-process TOFU has
no locking, but for the daemon/spike model this is low impact. Noted for
hardening, not a blocker.

## Verdict

No critical or auth-bypass issues. All six security-critical invariants in the
review charter hold. The single Warning (WR-01) is an availability/DoS design
question that requires a product decision rather than a mechanical fix; the two
Info items are hardening notes. Build, tests, and clippy are green.
