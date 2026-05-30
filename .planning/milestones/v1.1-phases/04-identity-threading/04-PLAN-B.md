---
id: 04-B
title: "Thread authenticated identity into Session — non-optional field, handshake extraction, fingerprint logging"
wave: 2
depends_on:
  - 04-A
files_modified:
  - crates/nosh-server/src/session.rs
  - crates/nosh-server/src/server.rs
autonomous: true
requirements:
  - IDENT-01
---

# Plan B: nosh-server — Make Session.identity Non-Optional and Thread the Peer Key

## Objective

Wire the authenticated peer SSH identity (extracted from the TLS handshake) into `Session.identity` as a non-optional `NoshPublicKey` field — enforced by the type system. After this plan:

- `Session.identity` is `NoshPublicKey` (not `Option<NoshPublicKey>`); the compiler rejects constructing a `Session` without it.
- `handle_connection` extracts the peer key immediately after handshake resolution, before `accept_bi` or any session work; a failed extraction closes the connection with `CLOSE_AUTH`.
- The per-session tracing span includes the `SHA256:` fingerprint (from `NoshPublicKey::fingerprint()`) — raw key bytes are never logged.
- All existing integration tests continue to pass unchanged.

## Prerequisites

Plan A must be complete: `nosh_auth::nosh_key_from_spki` and `NoshPublicKey::fingerprint()` must be available.

## Context

- D-01: `Session.identity: NoshPublicKey` (non-optional)
- D-02: `session::open` parameter changes from `Option<NoshPublicKey>` to `NoshPublicKey`
- D-03: update the field doc comment (remove "None for this spike — noted M3 seam")
- D-04: reject connection if identity extraction fails (CLOSE_AUTH, `error!` log)
- D-05: extract identity after `incoming.await` completes (post-handshake, pre-`accept_bi`)
- D-06: add fingerprint field to session tracing span
- D-07: only the fingerprint is logged, never raw key bytes

---

## Task B-1: Make `Session.identity` non-optional (D-01, D-03)

<read_first>
- crates/nosh-server/src/session.rs (full file — read before editing)
</read_first>

<action>
In `crates/nosh-server/src/session.rs`, make the following changes to the `Session` struct:

1. Change the `identity` field from:
   ```
   pub identity: Option<NoshPublicKey>,
   ```
   to:
   ```
   pub identity: NoshPublicKey,
   ```

2. Update the doc comment on `identity` from:
   ```
   /// The authenticated SSH identity (Phase 2). `None` for this spike: the
   /// connection handler does not yet surface the peer cert key (noted M3 seam).
   ```
   to:
   ```
   /// The authenticated peer's SSH identity, proven during the TLS mutual
   /// handshake. Always present — a `Session` cannot be constructed without
   /// a verified identity (D-01).
   ```

3. In `session::open`'s function signature, change the `identity` parameter from:
   ```
   identity: Option<NoshPublicKey>,
   ```
   to:
   ```
   identity: NoshPublicKey,
   ```

4. In `session::open`'s `Session { ... }` construction (inside the function body), change:
   ```
   identity,
   ```
   The field assignment already works (`identity: identity` or shorthand `identity`) — ensure it's a direct assignment, not `Some(identity)`.
</action>

<acceptance_criteria>
- `crates/nosh-server/src/session.rs` field `identity` is declared as `pub identity: NoshPublicKey` (no `Option<>`)
- The doc comment no longer contains "None for this spike" or "M3 seam" language
- `session::open` signature has `identity: NoshPublicKey` (no `Option<>`)
- `cargo check -p nosh-server` exits with errors at the single call site in `server.rs` (expected — that call site still passes `None`; it will be fixed in Task B-2)
</acceptance_criteria>

---

## Task B-2: Add `CLOSE_AUTH` constant and `extract_peer_identity` helper to server.rs

<read_first>
- crates/nosh-server/src/server.rs (full file — read before editing; focus on lines 136-145 for CLOSE_* constants and lines 143-182 for handle_connection)
</read_first>

<action>
In `crates/nosh-server/src/server.rs`:

1. Add a new close code constant alongside `CLOSE_OK` and `CLOSE_PROTOCOL` (around line 136):
   ```rust
   /// QUIC application close code for peer identity extraction failure (should
   /// never happen on an AuthorizedKeysVerifier-enforced connection — D-04).
   const CLOSE_AUTH: u32 = 2;
   ```

2. Add the following `use` import at the top of the file (with the existing imports):
   ```rust
   use rustls::pki_types::CertificateDer;
   ```
   (If `CertificateDer` is already imported via `quinn::crypto::rustls::HandshakeData` or another path, check before adding a duplicate import.)

3. Add a private helper function `extract_peer_identity` near the bottom of `server.rs` (before `clean_exit`):
   ```rust
   /// Extract the `NoshPublicKey` from the peer's TLS client cert after the
   /// handshake completes. Returns `None` if the peer has no identity, the
   /// downcast fails, or the cert is not a valid Ed25519 SPKI.
   ///
   /// Used by `handle_connection` to enforce D-04/D-05: identity is extracted
   /// before any session work, and the connection is closed if extraction fails.
   fn extract_peer_identity(conn: &quinn::Connection) -> Option<nosh_auth::NoshPublicKey> {
       let certs = conn
           .peer_identity()?
           .downcast::<Vec<CertificateDer<'static>>>()
           .ok()?;
       let leaf = certs.first()?;
       let spki = nosh_auth::keys::extract_spki_from_cert(leaf).ok()?;
       nosh_auth::nosh_key_from_spki(&spki)
   }
   ```
</action>

<acceptance_criteria>
- `crates/nosh-server/src/server.rs` contains `const CLOSE_AUTH: u32 = 2;`
- `crates/nosh-server/src/server.rs` contains a function `fn extract_peer_identity(conn: &quinn::Connection) -> Option<nosh_auth::NoshPublicKey>`
- `extract_peer_identity` calls `conn.peer_identity()`, downcasts to `Vec<CertificateDer<'static>>`, calls `nosh_auth::keys::extract_spki_from_cert`, then `nosh_auth::nosh_key_from_spki`
- `cargo check -p nosh-server` compiles these additions without errors (the `session::open(…, None)` call site error from Task B-1 may still be present — that's fixed in B-3)
</acceptance_criteria>

---

## Task B-3: Extract identity in `handle_connection` and close on failure (D-04, D-05)

<read_first>
- crates/nosh-server/src/server.rs (the `handle_connection` function, lines ~143-182)
</read_first>

<action>
In `handle_connection`, after `drop(permit)` (which releases the pre-auth semaphore permit after the handshake resolves) and before the `conn.accept_bi()` call, insert:

```rust
// D-04/D-05: extract the authenticated peer identity immediately after the
// handshake completes — before any session work. AuthorizedKeysVerifier
// enforces client auth, so a resolved connection must always have a parseable
// peer identity. If extraction nonetheless fails, close with CLOSE_AUTH and
// log an error. An unauthenticated session is impossible.
let peer_identity = match extract_peer_identity(&conn) {
    Some(k) => k,
    None => {
        tracing::error!(%peer, "connection passed auth but peer identity could not be extracted — closing");
        conn.close(CLOSE_AUTH.into(), b"peer identity extraction failed");
        return Ok(());
    }
};
```

Then update the call to `run_session` to pass `peer_identity`:
Change:
```rust
run_session(conn, peer, send, recv, shell_override).await
```
to:
```rust
run_session(conn, peer, peer_identity, send, recv, shell_override).await
```
</action>

<acceptance_criteria>
- `handle_connection` calls `extract_peer_identity(&conn)` after `drop(permit)` and before `conn.accept_bi()`
- On `None` result: `conn.close(CLOSE_AUTH.into(), b"peer identity extraction failed")` is called and the function returns `Ok(())`
- On `None` result: `tracing::error!` is emitted (not `warn!` or `info!`)
- `run_session` call in `handle_connection` now passes `peer_identity` as a parameter
- `cargo check -p nosh-server` reports an error on `run_session`'s signature mismatch (expected — fixed in B-4)
</acceptance_criteria>

---

## Task B-4: Update `run_session` signature and thread identity into `session::open` (D-02)

<read_first>
- crates/nosh-server/src/server.rs (the `run_session` function signature and its call to `session::open`)
</read_first>

<action>
In `crates/nosh-server/src/server.rs`:

1. Update `run_session`'s signature to accept `identity: nosh_auth::NoshPublicKey`:

Change:
```rust
async fn run_session(
    conn: quinn::Connection,
    peer: SocketAddr,
    mut send: quinn::SendStream,
    mut recv: quinn::RecvStream,
    shell_override: Option<String>,
) -> anyhow::Result<()> {
```
to:
```rust
async fn run_session(
    conn: quinn::Connection,
    peer: SocketAddr,
    identity: nosh_auth::NoshPublicKey,
    mut send: quinn::SendStream,
    mut recv: quinn::RecvStream,
    shell_override: Option<String>,
) -> anyhow::Result<()> {
```

2. Inside `run_session`, update the `session::open` call (currently passing `None`):

Change:
```rust
let (mut sess, reader, writer) =
    session::open(&passwd, &term, cols, rows, &client_env, None).context("open session")?;
```
to:
```rust
let (mut sess, reader, writer) =
    session::open(&passwd, &term, cols, rows, &client_env, identity).context("open session")?;
```
</action>

<acceptance_criteria>
- `run_session` has `identity: nosh_auth::NoshPublicKey` as its third parameter (after `peer: SocketAddr`)
- `session::open` is called with `identity` (not `None`) as the last argument
- `cargo check -p nosh-server` exits 0 (all type errors from B-1 through B-4 are resolved)
- `cargo build -p nosh-server` exits 0
</acceptance_criteria>

---

## Task B-5: Add fingerprint to the per-session tracing span (D-06, D-07)

<read_first>
- crates/nosh-server/src/server.rs (the `run_session` function, the `tracing::info_span!` call — around the block where `session_id` and `username` are extracted)
</read_first>

<action>
In `run_session`, find the block where the session span is created. It currently looks like:

```rust
let session_id = sess.session_id;
let username = sess.username.clone();
let span = tracing::info_span!("session", %session_id, %peer, username = %username);
```

Change it to:

```rust
let session_id = sess.session_id;
let username = sess.username.clone();
let fingerprint = sess.identity.fingerprint();
let span = tracing::info_span!(
    "session",
    %session_id,
    %peer,
    username = %username,
    identity = %fingerprint,
);
```

The `fingerprint` variable holds the `SHA256:...` string. Raw key bytes are never logged (D-07 invariant: `fingerprint()` only outputs the hash, not the key material).
</action>

<acceptance_criteria>
- `run_session` calls `sess.identity.fingerprint()` and stores the result in `fingerprint`
- The `tracing::info_span!` call includes `identity = %fingerprint` as a field
- The raw `sess.identity.key32()` bytes are NOT logged anywhere in `run_session` or `handle_connection`
- `cargo build -p nosh-server` exits 0
- `cargo test --workspace` exits 0 (all existing tests pass — no assertion changes needed)
</acceptance_criteria>

---

## Task B-6: Verify full test suite passes (success criterion 3)

<read_first>
- crates/nosh-client/tests/auth.rs (the AUTH-01..05 tests)
- crates/nosh-client/tests/session.rs (the SESS-01..10 tests)
</read_first>

<action>
Run the full test suite to confirm success criterion 3 ("All existing handshake tests still pass with no changes to their assertions"):

```bash
cargo test --workspace
```

If any test fails, diagnose from the output. The most likely failure modes:
- A test constructs a `Session` directly (not via `session::open`) — update to pass a real `NoshPublicKey` from the test harness.
- A test checks `sess.identity.is_none()` — update to reflect the new non-optional type (but per CONTEXT.md, no tests inspect `Session.identity`, so this should not occur).

The tests use `common::spawn_server_with_shell` → `run_accept_loop` → `handle_connection` → `run_session` → `session::open`. After the refactor, this path extracts and threads the real peer identity on every connection. The tests don't inspect `Session.identity`, so they pass without assertion changes.
</action>

<acceptance_criteria>
- `cargo test --workspace` exits 0
- All AUTH-01..05 tests in `nosh-client/tests/auth.rs` pass
- All SESS-01..10 tests in `nosh-client/tests/session.rs` pass
- Zero new compiler warnings introduced (existing warnings are pre-existing)
</acceptance_criteria>

---

## Verification

```bash
cargo build --workspace
cargo test --workspace
```

Both exit 0. The compiler rejects `session::open(…, None)` — the type-system invariant is enforced. The `tracing` span for each session shows `identity=SHA256:...`.

<must_haves>
## Truths that must hold

- `Session.identity` is `pub identity: NoshPublicKey` — the compiler rejects constructing a `Session` without a verified identity (D-01, success criterion 1)
- `handle_connection` calls `extract_peer_identity(&conn)` after the handshake (`incoming.await`) and before `conn.accept_bi()` — identity is extracted as early as possible (D-05, success criterion 2)
- If `extract_peer_identity` returns `None`, the connection is closed with `CLOSE_AUTH` and `tracing::error!` is emitted; no session is started (D-04)
- `session::open` accepts `NoshPublicKey` (not `Option<NoshPublicKey>`) — the single call site in `server.rs` passes the extracted key (D-02)
- The per-session `tracing::info_span!` includes `identity = %fingerprint` where `fingerprint` is the `SHA256:` string (D-06); raw key bytes are never in the log output (D-07)
- `cargo test --workspace` exits 0 with no test assertion changes — all AUTH and SESS tests continue to pass (success criterion 3)
</must_haves>
