# Phase 4: Identity Threading — Research

**Phase:** 4 — Identity Threading
**Researched:** 2026-05-30
**Status:** Complete

## RESEARCH COMPLETE

---

## Summary

Phase 4 is a tightly scoped, low-risk refactor. Every touchpoint has already been
catalogued with file:line precision in CONTEXT.md. The RESEARCH confirms the
implementation path and adds compiler-behavior notes and a validation architecture.

---

## 1. What Exists Today

### Session.identity (the seam to fill)

`crates/nosh-server/src/session.rs:115-127` — `Session` struct:

```
pub identity: Option<NoshPublicKey>,
```

Docstring explicitly marks it as "None for this spike — noted M3 seam." This is
the deliberate placeholder the v1.1 work fills.

`session::open` signature (`session.rs:205-260`) accepts `identity: Option<NoshPublicKey>`
and passes it straight into the struct. The single call site is `server.rs:215`:

```
session::open(&passwd, &term, cols, rows, &client_env, None)
```

### Peer identity extraction path (already working in the verifier)

`crates/nosh-auth/src/verifier.rs:166-168` — `AuthorizedKeysVerifier::verify_client_cert`:

```rust
let spki = keys::extract_spki_from_cert(end_entity)…;
let presented = parse_ed25519_from_spki(&spki)…;
if self.authorized.contains(&presented) { Ok(…) }
```

`parse_ed25519_from_spki` is at `verifier.rs:218-229` — currently **private to the
module**. It validates the 44-byte SPKI prefix and extracts the raw 32-byte key
into a `NoshPublicKey`.

`extract_spki_from_cert` is at `keys.rs:172` — already **`pub`**. Exported from
`nosh_auth` lib.rs.

### quinn::Connection::peer_identity()

Returns `Option<Arc<dyn Any>>` after the handshake completes. For rustls-backed
connections (which is all nosh connections), this downcasts to
`Vec<CertificateDer<'static>>` — the client's certificate chain. The first
element `[0]` is the leaf cert.

The existing code in `server.rs:167-172` already downcasts `handshake_data()` for
ALPN logging, confirming the downcast pattern works in this codebase.

**Important:** `peer_identity()` returns `None` if `client_auth_mandatory()` is
false. Because `AuthorizedKeysVerifier::client_auth_mandatory()` returns `true`,
a successfully-authenticated connection ALWAYS has a peer identity. A `None` here
means something went catastrophically wrong post-handshake — should never happen
in the happy path but must be handled defensively (D-04).

### QUIC close codes

`server.rs:136-138`:
```rust
const CLOSE_OK: u32 = 0;
const CLOSE_PROTOCOL: u32 = 1;
```

D-04 asks for a close code on identity-extraction failure. The cleanest choice is
to add `const CLOSE_AUTH: u32 = 2;` (auth/identity failure, distinct from a
protocol framing violation). Alternatively, reuse `CLOSE_PROTOCOL` since identity
extraction failure IS a protocol violation from the server's perspective.
CONTEXT.md leaves this to implementer discretion.

---

## 2. Exact Change Set

### Change 1 — Expose `parse_ed25519_from_spki` from `nosh-auth`

**File:** `crates/nosh-auth/src/verifier.rs`

Change `fn parse_ed25519_from_spki` at line 218 from private to
`pub(crate)`. Then re-export it from `nosh-auth`'s `lib.rs` as a public
function, OR provide a thin public wrapper in `keys.rs` (e.g.,
`pub fn nosh_key_from_spki(spki: &[u8]) -> Option<NoshPublicKey>`). Either path
is valid — the goal is that `nosh-server` can call it without duplicating the
SPKI-parsing logic.

The CONTEXT.md "Claude's Discretion" note confirms: "Planner/executor picks the
cleanest reuse — do not duplicate the SPKI-parsing logic."

**Recommended:** Add `pub fn nosh_key_from_spki(spki: &[u8]) -> Option<NoshPublicKey>`
in `keys.rs` wrapping the existing logic (keeps verifier.rs's internal function
private, exposes a clean surface in the keys module where the SPKI infrastructure
already lives). Export from `lib.rs`.

### Change 2 — Add `fingerprint()` to `NoshPublicKey` (D-07)

**File:** `crates/nosh-auth/src/keys.rs`

Add to `impl NoshPublicKey`:
```rust
pub fn fingerprint(&self) -> String {
    use sha2::{Digest, Sha256};
    let hash = Sha256::new().chain_update(&self.key32).finalize();
    format!("SHA256:{}", base64::engine::general_purpose::STANDARD_NO_PAD.encode(hash))
}
```

This matches OpenSSH `SHA256:` fingerprint format (SHA256 over the raw 32-byte
Ed25519 key material, base64 without padding). Note: `sha2` and `base64` must be
added to `nosh-auth`'s `Cargo.toml` if not already transitive deps. Check the
lockfile — `sha2` is almost certainly already present via `rustls`/`ring`.

**Crate availability:** `ring` (already a dep via rustls) provides SHA-256 via
`ring::digest`. Alternatively, `sha2` (RustCrypto, likely already in the tree).
The implementer should check the existing Cargo.lock before adding new deps.

**Export:** `fingerprint()` is a method on `NoshPublicKey`; no extra lib.rs export
needed.

### Change 3 — `Session.identity` → non-optional (D-01)

**File:** `crates/nosh-server/src/session.rs`

1. Change field declaration from `pub identity: Option<NoshPublicKey>` to
   `pub identity: NoshPublicKey`.
2. Update the doc comment (D-03): remove the "None for this spike — noted M3 seam"
   text; replace with "The authenticated peer's SSH identity, proven during the
   TLS mutual handshake."
3. Update `Session` struct initialization in `session::open` to assign
   `identity` directly (not `Some(identity)`).

### Change 4 — `session::open` signature (D-02)

**File:** `crates/nosh-server/src/session.rs`

Change `pub fn open(…, identity: Option<NoshPublicKey>) -> …` to
`pub fn open(…, identity: NoshPublicKey) -> …`.

The call site in `server.rs:215` is the ONLY call site — the compiler will
immediately flag it as soon as the `Option` is removed, forcing the implementer
to supply the real key before the code compiles.

### Change 5 — Extract identity in `handle_connection` and reject on failure (D-04, D-05)

**File:** `crates/nosh-server/src/server.rs`

After `drop(permit)` (line ~162), before `conn.accept_bi()`:

```rust
// D-04/D-05: extract the authenticated peer identity immediately after the
// handshake, before any session work. If extraction fails (should never happen
// on an AuthorizedKeysVerifier-enforced connection), close with auth error.
let peer_identity = match extract_peer_identity(&conn) {
    Some(k) => k,
    None => {
        tracing::error!(%peer, "connection passed auth but peer identity could not be extracted — closing");
        conn.close(CLOSE_AUTH.into(), b"peer identity extraction failed");
        return Ok(());
    }
};
```

Add a helper function:

```rust
/// Extract the `NoshPublicKey` from the peer's TLS client cert after handshake.
fn extract_peer_identity(conn: &quinn::Connection) -> Option<NoshPublicKey> {
    let certs: Vec<CertificateDer<'static>> = conn
        .peer_identity()?
        .downcast::<Vec<CertificateDer<'static>>>()
        .ok()
        .filter(|v| !v.is_empty())
        .map(|v| *v)?;
    let spki = nosh_auth::keys::extract_spki_from_cert(certs.first()?)
        .ok()?;
    nosh_auth::keys::nosh_key_from_spki(&spki)
}
```

Add `const CLOSE_AUTH: u32 = 2;` alongside `CLOSE_OK`/`CLOSE_PROTOCOL`.

### Change 6 — Thread identity through `run_session` and `session::open` (D-02)

**File:** `crates/nosh-server/src/server.rs`

1. `run_session` gains a parameter: `identity: NoshPublicKey`.
2. The call site `run_session(conn, peer, send, recv, shell_override)` becomes
   `run_session(conn, peer, identity, send, recv, shell_override)`.
3. Inside `run_session`, pass `identity` to `session::open(…, identity)` (no
   longer `None`).

### Change 7 — Add fingerprint to the session span (D-06)

**File:** `crates/nosh-server/src/server.rs`

At the `tracing::info_span!` call (`server.rs:219`):

```rust
let fingerprint = sess.identity.fingerprint();
let span = tracing::info_span!(
    "session",
    %session_id,
    %peer,
    username = %username,
    identity = %fingerprint,   // D-06: OpenSSH-style SHA256: fingerprint
);
```

The raw key is NEVER logged — only the fingerprint (D-07 invariant).

---

## 3. Compiler-Enforced Correctness

Making `Session.identity` non-optional is the key correctness guarantee. Because
`session::open` now requires a `NoshPublicKey` (not `Option<NoshPublicKey>`), any
call site that passes the old `None` will fail to compile. There is exactly one
call site; fixing it requires supplying the real extracted key. The type system
enforces D-01 mechanically.

**Test compatibility (success criterion 3):** All existing tests (`auth.rs`,
`session.rs`) go through `common::spawn_server_with_shell` which calls
`run_accept_loop` → `handle_connection` → `run_session` → `session::open`. After
the refactor, this path will extract and pass the real peer identity on every
connection, so the tests don't need to change — they just start working with a
real identity instead of `None`. The test assertions don't inspect
`Session.identity`, so they remain unchanged.

---

## 4. Validation Architecture

### Unit tests to add

1. **`nosh_key_from_spki` roundtrip** — in `keys.rs` tests: construct a known
   Ed25519 SPKI via `ed25519_spki_der`, call `nosh_key_from_spki`, assert the
   returned key's `key32` matches the input.
2. **`NoshPublicKey::fingerprint` format** — assert the result starts with
   `SHA256:` and the base64 portion is 43 characters (SHA256 = 32 bytes,
   base64-no-pad = 43 chars).
3. **Non-optional field compilation** — the compiler rejects `session::open(…,
   None)` after the change. This is enforced at compile time, not a test.

### Integration test to add

**`sess_identity_threaded`** — in `nosh-client/tests/session.rs` (or a new
`identity.rs`):

1. Start a server + complete mutual auth handshake (standard `connect_session_server` harness).
2. After the connection is established, run a session.
3. The test cannot directly inspect `Session.identity` from outside the server
   binary, but it can verify the server didn't close early with `CLOSE_AUTH` —
   a successful session proves identity extraction succeeded.

A more direct test: expose a `get_session_identity` helper via test-support that
returns the `NoshPublicKey` from the most recently opened session; assert it equals
the client key used to connect.

**Simpler approach (preferred for Phase 4 scope):** The success criteria only
require that existing tests pass with no changes (criterion 3). The type-system
enforcement (criterion 1) and the extraction code path (criterion 2) are verified
by compile-time + the fact that the tests complete successfully.

---

## 5. Dependencies / No New Crates Needed

- `sha2` for `fingerprint()`: check if it's already in `Cargo.lock`. If not, add
  `sha2 = "0.10"`. The `base64` crate is also needed; check the lockfile.
  Alternatively, use `ring::digest::SHA256` (already a dep via rustls/ring).
- All other changes use existing types (`NoshPublicKey`, `CertificateDer`,
  `quinn::Connection`) and existing functions (`extract_spki_from_cert`).
- No `nosh-proto` changes — the message protocol is untouched.
- No client-side changes — the client sends its cert during the TLS handshake;
  the server-side identity extraction is entirely server-internal.

---

## 6. Risk Assessment

| Risk | Likelihood | Mitigation |
|------|-----------|------------|
| `peer_identity()` returns None on authenticated connection | Very Low | Server closes with CLOSE_AUTH (D-04); tested defensively |
| SHA-256 dep not in lockfile | Low | Check Cargo.lock; `ring` already present via rustls |
| Existing tests break due to signature mismatch | None | Tests don't inspect Session.identity; no assertion changes needed |
| `parse_ed25519_from_spki` exposure breaks verifier.rs encapsulation | Negligible | Expose as a public `nosh_key_from_spki` in keys.rs instead |

**Overall risk: LOW.** This is a 7-touch refactor across 2 crates (nosh-auth,
nosh-server), all in existing modules with clear touchpoints. No protocol changes.

---

*Researcher: Phase 4 analysis based on direct codebase inspection*
*Phase dir: .planning/phases/04-identity-threading/*
