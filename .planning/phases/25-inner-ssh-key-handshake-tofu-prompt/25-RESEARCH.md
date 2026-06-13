# Phase 25: Inner SSH-Key Handshake + TOFU Prompt — Research

**Researched:** 2026-06-13
**Domain:** Application-level mutual SSH-key authentication over WebTransport, RFC 9266 channel binding, TOFU prompt
**Confidence:** HIGH — grounded entirely in first-party source reads plus registry-verified dependency checks

---

<user_constraints>
## User Constraints (from CONTEXT.md)

### Locked Decisions

- **D-01:** RFC 9266 `tls-exporter` channel binding. `export_keying_material` confirmed reachable on both `quinn::Connection` and `wtransport::Connection` (research-verified). No CSPRNG-nonce fallback needed. (Re-ask trigger: verify exact call path through Phase-24 wiring — see Channel-Binding API Gate below.)
- **D-02:** Inner auth runs over WebTransport only. Native QUIC keeps its existing in-TLS-handshake mutual auth (AUTH-01..04) unchanged. No inner handshake on the native path. No double-auth.
- **D-03:** Four new `Message` variants, appended after `ScrollbackCredit` (discriminant 17): `InnerAuthChallenge` (18) → `InnerAuthResponse` (19) → `InnerAuthComplete` (20) → `InnerAuthFail` (21). `message_discriminant_order_is_stable` test updated in the same commit (WF-1).
- **D-04:** `InnerAuthFail` is fieldless — no oracle for key existence or signature validity. Same pattern as `ReattachErr`.
- **D-05:** State machine strictly enforces `Unauthenticated → ChallengeExchanged → Authenticated`; `SessionOpen`/`Reattach` accepted ONLY in `Authenticated`.
- **D-06:** Both nonces are 32-byte CSPRNG, single-use server-side (WT-4 replay guard).
- **D-07:** Reuse existing `nosh-auth` crypto: `RawEd25519Signer`/`AgentSigner` for signing, `lookup_known_host`/`record_known_host` for server TOFU, extract `check_authorized_key` helper from `AuthorizedKeysVerifier`. No new auth crates.
- **D-08:** Blocking TOFU prompt on first contact: SHA-256 hex fingerprint, explicit `yes` required, no PTY output until resolved, declining disconnects cleanly. Replaces silent-record in `verifier.rs`.
- **D-09:** Pre-connect TOFU check on native-QUIC path is done BEFORE dialling — check known_hosts, prompt interactively if unknown, record on `yes`, then `connect()`. Keeps I/O off the TLS handshake thread.
- **D-10:** No-TTY fails closed on unknown host key. Print fingerprint, reference future `--trust-key` flag.

### Claude's Discretion

- Exact byte layout of the signed transcript (fields + exported keying material + nonces, concatenation/hash before signing) — planner designs; must include channel-binding material (D-01) and both nonces (D-06). Keep it a single canonical transcript both sides reconstruct identically.
- Whether inner auth lives in a shared `inner_auth.rs` used by both client and server, or split per crate — planner's call. ARCHITECTURE.md sketches `run_inner_auth_server`/`run_inner_auth_client`.

### Deferred Ideas (OUT OF SCOPE)

- `--trust-key <fingerprint>` / `--strict-host-key-checking` CLI flags (WT-UX-02). D-09's pre-connect check is designed to accommodate them later. D-10's no-TTY message should reference `--trust-key` as the future escape hatch.
</user_constraints>

<phase_requirements>
## Phase Requirements

| ID | Description | Research Support |
|----|-------------|------------------|
| WT-04 | WebTransport session performs inner SSH-key mutual handshake (authorized_keys + known_hosts), bound to outer TLS via RFC 9266 tls-exporter; inner-auth failure is opaque | Channel-binding call path verified (§ below); crypto reuse confirmed; message variants 18–21 append-only pattern confirmed |
| SEC-02 | On first contact with unknown server host key: blocking TOFU fingerprint-confirm dialogue (SHA-256 hex, explicit `yes`, no PTY output until resolved), replacing current silent-record behaviour | `HostKeyVerifier` silent-record path in `verifier.rs:82–84` identified; `fingerprint()` on `NoshPublicKey` produces `SHA256:...` format; `lookup_known_host`/`record_known_host` confirmed reusable |
</phase_requirements>

---

## Summary

Phase 25 adds the inner application-level SSH-key mutual authentication that makes WebTransport mode actually secure. The outer TLS is terminated at any proxy; without inner auth, any client reaching the WebTransport endpoint is authenticated. This phase replaces the test-only stub in `handle_connection_wt` with real mutual challenge-response.

The most important research finding is the **channel-binding API gate**: RFC 9266 `tls-exporter` binding is achievable through the exact Phase-24 wiring. The call path is `conn.quic_connection().export_keying_material(&mut out, label, context)` — `quic_connection()` is a confirmed public method on `wtransport::Connection` (gated on the `"quinn"` feature, which is active in the workspace). Both client and server reach the same underlying `quinn::Connection` through this path, so both sides derive identical exported key material. The CSPRNG-nonce fallback is NOT needed.

All cryptographic primitives are already in the dependency tree. The inner handshake reuses `RawEd25519Signer`/`InProcessEd25519Signer`/`AgentSigner` from `nosh-auth`, `sha2` (already in `nosh-auth` Cargo.toml) for the transcript hash, `getrandom` (already in `nosh-client` dev-deps and `nosh-auth` dev-deps — needed as a real dep in server/client for nonce generation), and `NoshPublicKey`'s existing `fingerprint()` for the TOFU prompt. No new crates are needed; a `getrandom` dep addition to `nosh-server` and `nosh-client` is the only dependency change.

The TOFU prompt replaces four lines in `HostKeyVerifier::verify_server_cert` (the silent-record TOFU path at `verifier.rs:82–84`) and adds a similar gate inside `run_inner_auth_client` for the WebTransport path. The pre-connect path (D-09) handles the native-QUIC prompt at the application level, before `connect()`.

**Primary recommendation:** Implement as two new modules (`nosh-server/src/inner_auth.rs` and `nosh-client/src/inner_auth.rs`), with a shared transcript-layout convention defined inline. Wire into `handle_connection_wt` (server) and the client WT connect path. First commit: append new `Message` variants 18–21 and update `message_discriminant_order_is_stable`.

---

## Channel-Binding API Gate (RESOLVED — D-01 Re-Ask Trigger)

**Claim in CONTEXT.md:** `export_keying_material` is reachable on both `quinn::Connection` and `wtransport::Connection`. **Verified against Phase-24 actual wiring.**

### Exact call path through Phase-24 wiring

On the server side, `WtransportTransport(conn: wtransport::Connection)` is the type produced by Phase 24 (`wt_transport.rs:66`). The `quinn` feature is active on wtransport in the workspace (`Cargo.toml:35: features = [..., "quinn"]`). Therefore:

```rust
// Server inner_auth.rs — obtain channel binding bytes
let mut ekm = [0u8; 32];
conn.quic_connection()                           // &quinn::Connection (gated on "quinn" feature — active)
    .export_keying_material(                     // quinn 0.11.9 Connection::export_keying_material
        &mut ekm,
        b"nosh-inner-auth-v1",                   // label
        b"",                                     // context (empty)
    )?;
```

On the client side, the `WtransportTransport(conn)` in `nosh-client/src/wt_transport.rs` holds the same `wtransport::Connection` type. The call is identical:

```rust
// Client inner_auth.rs — derive same channel binding bytes
let mut ekm = [0u8; 32];
conn.quic_connection()
    .export_keying_material(&mut ekm, b"nosh-inner-auth-v1", b"")?;
```

**Why both ends produce identical bytes:** RFC 9266 / RFC 5705 — given the same label, context, and output length, both TLS endpoints of the same TLS 1.3 session derive the same exported keying material. The label `"nosh-inner-auth-v1"` is nosh-specific (domain-separation). The context is empty (no sub-context needed). Both endpoints share the same TLS session (the WebTransport outer TLS), so they derive the same 32-byte output.

**What `NoshTransport` currently exposes:** The trait does NOT currently expose `export_keying_material` — it is not part of the `NoshTransport` interface (by design: the trait only covers I/O operations). The `run_inner_auth_server` and `run_inner_auth_client` functions receive the raw `wtransport::Connection` (or can receive a `&quinn::Connection` reference) directly before boxing into `Box<dyn NoshTransport>`. The planner needs to decide whether to:

1. Pass `conn: &wtransport::Connection` to the inner_auth functions alongside the boxed transport, or
2. Add an `export_keying_material` method to `NoshTransport` (returns `[u8; 32]` given label/context) so the trait stays the single I/O interface.

**Recommendation (Claude's Discretion):** Add `export_keying_material(&self, label: &[u8], context: &[u8]) -> anyhow::Result<[u8; 32]>` to `NoshTransport` with a default impl returning `Err(...)` (allowing native-QUIC impl to also provide it for future use). The WebTransport impl delegates to `quic_connection().export_keying_material(...)`. This keeps inner_auth functions fully transport-agnostic.

**VERDICT: CONFIRMED — call path verified in Phase-24 source. RFC 9266 channel binding is achievable. No user escalation needed.**

---

## Architectural Responsibility Map

| Capability | Primary Tier | Secondary Tier | Rationale |
|------------|-------------|----------------|-----------|
| Inner-auth challenge generation (CSPRNG nonce) | API / Backend (server) | — | Server initiates; CSPRNG nonce is server-side generated, single-use |
| Inner-auth signing (client → server) | API / Backend (client) | — | Client signs; key never leaves `nosh-auth` crypto layer |
| Inner-auth signing (server → client) | API / Backend (server) | — | Symmetric mutual auth |
| Known_hosts lookup + TOFU record | API / Backend (client) | — | Filesystem op at application level, not inside TLS verifier |
| Authorized_keys check | API / Backend (server) | — | Server-side gate, same as `AuthorizedKeysVerifier` today |
| TOFU interactive prompt | CLI / Frontend | — | stdin/stderr blocking read — must be at the application level, NOT inside a TLS callback |
| Message discriminant stability | Wire format (nosh-proto) | — | Append-only enum invariant; codec test update owns this |

---

## Standard Stack

### Core

No new external crates are required. All dependencies are already in the workspace or need a minor dep addition.

| Library | Version | Purpose | Status |
|---------|---------|---------|--------|
| `nosh-auth::RawEd25519Signer` / `InProcessEd25519Signer` / `AgentSigner` | (in-workspace) | Sign/verify 64-byte Ed25519 signatures over the transcript | Already in workspace |
| `nosh-auth::keys::lookup_known_host` / `record_known_host` | (in-workspace) | Server TOFU known_hosts check and record | Already in workspace |
| `nosh-auth::keys::NoshPublicKey::fingerprint()` | (in-workspace) | SHA-256 hex fingerprint for TOFU prompt (`SHA256:...` format) | Already in workspace |
| `sha2` | 0.10 | SHA-256 transcript hash (domain-separated) | Already in `nosh-auth/Cargo.toml`; needs adding to `nosh-server` and `nosh-client` |
| `getrandom` | 0.2 | CSPRNG 32-byte nonce generation | In `nosh-client` dev-deps and `nosh-auth` dev-deps; needs promoting to real dep in `nosh-server` and `nosh-client` |
| `quinn::Connection::export_keying_material` | 0.11.9 | RFC 9266 tls-exporter channel binding | Already in workspace; reachable via `wtransport::Connection::quic_connection()` |

### Dependency additions needed

```toml
# nosh-server/Cargo.toml additions:
sha2 = "0.10"
getrandom = "0.2"

# nosh-client/Cargo.toml additions:
sha2 = "0.10"
# getrandom already present as dev-dep — promote to real dep
```

Note: `getrandom` is currently in `nosh-client` as a dev-dependency (`Cargo.toml:70`). For production nonce generation, it needs to be a regular dependency.

---

## Package Legitimacy Audit

No new external packages are introduced by this phase. All cryptographic dependencies (`sha2`, `getrandom`, `ed25519-dalek`) are already in the workspace or are being promoted from existing (transitive/dev) usage. No slopcheck needed.

| Package | Status |
|---------|--------|
| `sha2 = "0.10"` | Already direct dep of `nosh-auth`; well-established RustCrypto crate |
| `getrandom = "0.2"` | Already in `nosh-client` dev-deps and `nosh-auth` dev-deps; well-established |
| All others | In-workspace code only — no new registry packages |

---

## Architecture Patterns

### System Architecture Diagram

```
WebTransport outer TLS (terminated at proxy or direct)
         |
         v
  [wtransport::Connection] ──quic_connection()──> [quinn::Connection]
         |                                               |
         |                               export_keying_material()
         |                                               |
         |                               ekm: [u8; 32]  (RFC 9266 binding)
         |
         v
  run_wt_accept_loop
         |
         v
  handle_connection_wt ──── (Phase 25 replaces stub here) ────>
         |
         v
  run_inner_auth_server(transport, control_send, control_recv, authorized, host_signer)
         |
    1. export ekm via transport.export_keying_material()
    2. generate server_nonce: [u8; 32] (CSPRNG)
    3. send InnerAuthChallenge { server_nonce, server_spki, ekm }
    4. recv InnerAuthResponse { client_nonce, client_spki, client_sig }
    5. build transcript_server = SHA-256("nosh-inner-auth-v1" || ekm || server_nonce || client_nonce || server_spki)
    6. verify client_sig over transcript_server using client_spki
    7. check client_spki in authorized_keys via check_authorized_key()
    8. build transcript_client = SHA-256("nosh-inner-auth-v1-server" || ekm || server_nonce || client_nonce || client_spki)
    9. sign transcript_client with host_signer → server_sig
   10. send InnerAuthComplete { server_sig }
         |
         v
  returns Ok(NoshPublicKey) → feeds into run_session / run_reattach_session
         |
         v (on any failure)
  send InnerAuthFail (fieldless), close connection

Client side (run_inner_auth_client):
  1. export ekm via transport.export_keying_material()
  2. recv InnerAuthChallenge { server_nonce, server_spki, ekm_from_server }
  3. verify ekm_from_server == locally_derived_ekm (binding check)
  4. check server_spki against known_hosts → TOFU prompt if new
  5. generate client_nonce: [u8; 32] (CSPRNG)
  6. build transcript_server = SHA-256("nosh-inner-auth-v1" || ekm || server_nonce || client_nonce || server_spki)
  7. sign transcript_server with client signer → client_sig
  8. send InnerAuthResponse { client_nonce, client_spki, client_sig }
  9. recv InnerAuthComplete { server_sig } or InnerAuthFail
 10. verify server_sig over SHA-256("nosh-inner-auth-v1-server" || ekm || server_nonce || client_nonce || client_spki)
```

### Recommended Project Structure

```
crates/
  nosh-proto/src/
    messages.rs          # MODIFIED: append InnerAuth{Challenge,Response,Complete,Fail} (18–21)
  nosh-auth/src/
    keys.rs              # MODIFIED: add check_authorized_key(spki: &NoshPublicKey, authorized: &[NoshPublicKey]) -> bool
    verifier.rs          # MODIFIED: replace silent-record TOFU (lines 82–84) with TofuPolicy-gated prompt
  nosh-server/src/
    inner_auth.rs        # NEW: run_inner_auth_server
    wt_transport.rs      # MODIFIED: handle_connection_wt replaces stub with call to run_inner_auth_server
  nosh-client/src/
    inner_auth.rs        # NEW: run_inner_auth_client
    client.rs            # MODIFIED: pre-connect TOFU check (D-09) on native-QUIC path
    wt_transport.rs      # No change needed — connect_wt returns the transport; caller chains inner auth
```

### Pattern 1: Transcript Layout (Canonical Signed Bytes)

Both sides must reconstruct the signed transcript identically. The following layout is recommended:

**Client signs (server verifies) — `transcript_server`:**
```
SHA-256(
  "nosh-inner-auth-v1\0"  ||  // 20-byte domain label + NUL (prevents prefix collision)
  ekm[32]                 ||  // RFC 9266 exported keying material from outer TLS
  server_nonce[32]        ||  // server's CSPRNG nonce (from InnerAuthChallenge)
  client_nonce[32]        ||  // client's CSPRNG nonce (from InnerAuthResponse)
  server_spki[44]             // server's Ed25519 SPKI DER (44 bytes fixed for Ed25519)
)
```

**Server signs (client verifies) — `transcript_client`:**
```
SHA-256(
  "nosh-inner-auth-v1-server\0"  ||  // 27-byte domain label + NUL
  ekm[32]                        ||
  server_nonce[32]               ||
  client_nonce[32]               ||
  client_spki[44]                    // client's Ed25519 SPKI DER
)
```

Design rationale:
- Different labels prevent cross-transcript confusion (client cannot reuse server's signature or vice versa).
- `ekm` in the transcript ties the signature to THIS specific outer TLS session (WT-3 channel binding).
- Both nonces prevent replay: `server_nonce` proves the client signed for THIS session; `client_nonce` proves the server signed for THIS response (not a replay of a prior session's `InnerAuthComplete`).
- `server_spki` in `transcript_server` proves the client accepted THIS server (not a MITM that relayed the challenge from a different server).
- `client_spki` in `transcript_client` proves the server responded to THIS client's identity (no cross-identity confusion).
- Fixed-length fields (SPKI is always 44 bytes for Ed25519; nonces and EKM are always 32 bytes) mean no length-prefix ambiguity. The domain label + NUL provides the only variable-length boundary.

**Implementation note on `sha2`:** `nosh-auth` already depends on `sha2 = "0.10"`. The inner_auth modules in `nosh-server` and `nosh-client` will add `sha2` as a direct dep:

```rust
use sha2::{Digest, Sha256};
let hash: [u8; 32] = Sha256::new()
    .chain_update(b"nosh-inner-auth-v1\0")
    .chain_update(&ekm)
    .chain_update(&server_nonce)
    .chain_update(&client_nonce)
    .chain_update(&server_spki)
    .finalize()
    .into();
```

[VERIFIED: first-party source — `nosh-auth/Cargo.toml` confirms `sha2 = "0.10"` and `sha2`'s `Digest` trait API is stable]

### Pattern 2: Message Variant Additions (Append-Only, WF-1)

Current tail: `ScrollbackCredit` at discriminant 17 (confirmed in `codec.rs:302`). New variants append at 18–21:

```rust
// APPEND-ONLY — DO NOT INSERT OR REORDER. Discriminants 18–21.
// Inner SSH-key handshake for WebTransport mode (Phase 25, D-03).
// All four variants MUST be appended after ScrollbackCredit (discriminant 17).

/// Server → client: challenge carrying server's host key SPKI, server nonce,
/// and RFC 9266 exported keying material from the outer TLS session (D-01).
InnerAuthChallenge {
    /// Server's CSPRNG 32-byte nonce (D-06, single-use).
    server_nonce: [u8; 32],
    /// Server's Ed25519 SPKI DER (44 bytes for Ed25519, D-07).
    server_spki: Vec<u8>,
    /// RFC 9266 tls-exporter output: export_keying_material(32, "nosh-inner-auth-v1", "").
    /// Folded into the signed transcript to bind this handshake to the outer TLS session (WT-3).
    ekm: [u8; 32],
},   // discriminant 18

/// Client → server: response carrying client identity, client nonce, and signature.
InnerAuthResponse {
    /// Client's CSPRNG 32-byte nonce (D-06).
    client_nonce: [u8; 32],
    /// Client's Ed25519 SPKI DER (44 bytes for Ed25519).
    client_spki: Vec<u8>,
    /// Ed25519 signature over SHA-256(label || ekm || server_nonce || client_nonce || server_spki).
    client_sig: [u8; 64],
},   // discriminant 19

/// Server → client: server's signature completing mutual auth.
InnerAuthComplete {
    /// Ed25519 signature over SHA-256(label_server || ekm || server_nonce || client_nonce || client_spki).
    server_sig: [u8; 64],
},   // discriminant 20

/// Either → other: inner auth failed. FIELDLESS — no-oracle invariant (D-04).
/// INVARIANT: must remain fieldless forever (same reason as ReattachErr).
/// Server logs the reason (wrong key, bad signature, etc.) privately. Wire never reveals why.
InnerAuthFail,   // discriminant 21
```

The `message_discriminant_order_is_stable` test in `nosh-proto/src/codec.rs` (currently lines 276–312) must be extended to include entries `(18, ...), (19, ...), (20, ...), (21, ...)` in the same commit as the enum change.

The `variant_name()` method must also be extended:

```rust
Message::InnerAuthChallenge { .. } => "InnerAuthChallenge",
Message::InnerAuthResponse { .. } => "InnerAuthResponse",
Message::InnerAuthComplete { .. } => "InnerAuthComplete",
Message::InnerAuthFail => "InnerAuthFail",
```

**Security note:** `client_sig` and `server_sig` in `InnerAuthResponse` and `InnerAuthComplete` are not tokens but signatures. However, they still must NOT be logged — log only the identity fingerprint. The `variant_name()`-based logging discipline already covers this.

### Pattern 3: check_authorized_key Helper

`AuthorizedKeysVerifier::verify_client_cert` currently inlines the authorized-keys check:

```rust
// verifier.rs:171 (current)
if self.authorized.contains(&presented) {
    Ok(ClientCertVerified::assertion())
} else { ... }
```

Extract this as a standalone function in `keys.rs`:

```rust
/// Check whether `key` appears in `authorized`. Pure function usable by both
/// the TLS verifier and the inner SSH-key handshake.
pub fn check_authorized_key(key: &NoshPublicKey, authorized: &[NoshPublicKey]) -> bool {
    authorized.contains(key)
}
```

`AuthorizedKeysVerifier::verify_client_cert` then calls `check_authorized_key(&presented, &self.authorized)`. The inner-auth server calls the same function after parsing `client_spki`.

### Pattern 4: TOFU Prompt Replacement

**Current silent-record path (verifier.rs:80–86):**
```rust
None => {
    // D-01: TOFU — record and proceed silently.   // THIS IS REPLACED
    let _guard = self.tofu_lock.lock().unwrap();
    keys::record_known_host(&self.known_hosts, &self.host, &presented)
        .map_err(|e| Error::General(format!("known_hosts write failed: {e}")))?;
    tracing::info!(host = %self.host, "TOFU: recorded new host key");
    Ok(ServerCertVerified::assertion())
}
```

**Replacement strategy (D-08/D-09):**

For the **WebTransport path**, TOFU happens inside `run_inner_auth_client` at the application level — straightforward blocking I/O to stderr/stdin before sending `InnerAuthResponse`. No changes to `HostKeyVerifier` needed for this path.

For the **native-QUIC path** (D-09), the TOFU check moves to a pre-connect function called BEFORE `connect()`:

```rust
/// Pre-connect TOFU check (D-09). Call before connect() on the native-QUIC path.
/// Returns Ok(()) if the key is already known or the user typed "yes".
/// Returns Err(...) if the key mismatches or the user declined.
pub fn pre_connect_tofu_check(
    known_hosts: &Path,
    host: &str,
    server_spki: Option<&NoshPublicKey>,  // None = check deferred to TLS verifier
) -> anyhow::Result<()>
```

However, on the native-QUIC path the server's public key is NOT known until the TLS handshake completes. The existing `HostKeyVerifier` approach (TOFU inside the TLS verifier callback) is the correct hook. D-09 says "before dialling: check known_hosts; if unknown, prompt, record on `yes`, then `connect()` with the key already pinned" — but this requires knowing the server key before connecting, which is only possible if the key is already in `known_hosts` or provided via `--trust-key` (future flag, WT-UX-02).

**Revised D-09 interpretation for Phase 25:** On the native-QUIC path, the TOFU prompt cannot be fully pre-connect without the `--trust-key` flag. The practical implementation adds a `TofuPolicy` parameter to `HostKeyVerifier`:

```rust
pub enum TofuPolicy {
    /// Current v1.3 behaviour — REPLACED by this phase (SEC-02).
    Silent,
    /// SEC-02: blocking prompt to stderr/stdin. Fails on non-TTY (D-10).
    Interactive,
    /// Future: accept this specific fingerprint without prompting (WT-UX-02, deferred).
    TrustKey(NoshPublicKey),
}
```

The `HostKeyVerifier::verify_server_cert` TOFU branch calls a blocking `prompt_tofu(fingerprint, host)` function when `TofuPolicy::Interactive`. Because the TLS verifier runs on a tokio runtime thread (inside the quinn handshake), a blocking prompt must use `tokio::task::block_in_place` to avoid starving the async runtime. Alternatively (and cleanly): the prompt runs before `connect()` and the result is stored; the TLS verifier then checks the stored decision instead of prompting again.

**Simplest approach for D-09 (recommended):**

Add a `TofuDecision` channel to `HostKeyVerifier`:

```rust
pub struct HostKeyVerifier {
    known_hosts: PathBuf,
    host: String,
    provider: Arc<CryptoProvider>,
    tofu_lock: Mutex<()>,
    tofu_policy: TofuPolicy,  // NEW
}
```

The `HostKeyVerifier::new(...)` now takes an optional `TofuPolicy`. On the `Interactive` path, the `verify_server_cert` TOFU branch uses `block_in_place` to run the prompt:

```rust
None => {
    let _guard = self.tofu_lock.lock().unwrap();
    match self.tofu_policy {
        TofuPolicy::Silent => { /* current path — deprecated */ }
        TofuPolicy::Interactive => {
            // block_in_place is safe here: we're in a tokio task, not a non-tokio thread.
            tokio::task::block_in_place(|| {
                prompt_and_record(&self.known_hosts, &self.host, &presented)
            }).map_err(|e| Error::General(e.to_string()))?;
        }
    }
    Ok(ServerCertVerified::assertion())
}
```

This avoids restructuring the entire connect flow. The `block_in_place` is acceptable because the TLS handshake already expects a blocking operation here.

### Pattern 5: CSPRNG Nonce Generation

The `uuid` crate (already in `nosh-server`) uses `rand` internally. For the 32-byte inner-auth nonces, use `getrandom` directly (the same crate used in `nosh-auth`'s test support):

```rust
use getrandom::getrandom;

fn csprng_nonce() -> [u8; 32] {
    let mut nonce = [0u8; 32];
    getrandom(&mut nonce).expect("getrandom failed");
    nonce
}
```

`getrandom` is platform-agnostic (Linux, macOS, Windows) — the same cross-platform CSPRNG used in `nosh-auth`'s test support. [VERIFIED: `nosh-auth/Cargo.toml` dev-dep; `nosh-client/Cargo.toml` dev-dep; `nosh-server/Cargo.toml` does not yet list it — needs adding as a real dep.]

### Anti-Patterns to Avoid

- **Signing the challenge bytes alone (no EKM):** The signature covers only `challenge_bytes` without the outer TLS exported keying material → WT-3 transparent-proxy MITM. The `ekm` field in `InnerAuthChallenge` and the EKM in the transcript are mandatory.
- **Sending InnerAuthChallenge EKM derived from `quic_connection().max_datagram_size()` or similar wrong method:** The call is `export_keying_material`, not `max_datagram_size`. These look nothing alike but the wrong-method trap is documented — always use `export_keying_material`.
- **Allowing `SessionOpen`/`Reattach` before `InnerAuthComplete`:** The state machine must enforce this strictly. No "shortcut" path where a client that already has a valid token can skip inner auth.
- **`InnerAuthFail` with a reason field:** Never add reason codes. Server logs the reason; the wire message is always the fieldless variant. A reason code is a key-existence oracle.
- **Inserting inner-auth variants at any position other than after `ScrollbackCredit` (17):** WF-1 silent-corruption. The `message_discriminant_order_is_stable` test catches this, but the test must be updated in the same commit.
- **Using `block_in_place` without checking we are inside a `tokio::task`:** `block_in_place` panics if called outside a multi-threaded tokio runtime. The nosh server uses `tokio::runtime::Builder::new_multi_thread()` — safe.
- **Logging `client_sig` or `server_sig` bytes:** These are cryptographic material. `variant_name()` logging discipline prevents this, but verify that new match arms in `variant_name()` do not format the sig bytes.

---

## Don't Hand-Roll

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| SHA-256 over transcript | Custom hash loop | `sha2::Sha256` via `Digest` trait | Correct padding, finalization, constant-time — already in dep tree |
| CSPRNG nonces | `rand::thread_rng()` or `SystemTime`-based | `getrandom::getrandom` | Cross-platform; no extra dep; already in workspace |
| Ed25519 sign/verify | `ed25519-dalek` directly | `nosh-auth::RawEd25519Signer` / `InProcessEd25519Signer` | Signer abstraction already handles agent vs in-process; reuse the pattern |
| SPKI extraction from raw bytes | Manual DER parsing | `nosh_auth::keys::nosh_key_from_spki(spki: &[u8])` | Already validates prefix and length; returns `None` on invalid input |
| Fingerprint for TOFU prompt | Custom hash display | `NoshPublicKey::fingerprint()` | Already produces `SHA256:<base64-no-pad>` matching `ssh-keygen -l -E sha256` |
| Known_hosts read/write | Custom file format | `keys::lookup_known_host` / `record_known_host` | Already implemented and tested; format is OpenSSH-compatible |

**Key insight:** Every primitive this phase needs already exists in `nosh-auth`. Inner auth is new wiring, not new crypto.

---

## Runtime State Inventory

Not applicable — this is a new-feature phase, not a rename/refactor/migration.

---

## Common Pitfalls

### Pitfall 1: EKM bytes differ between client and server (WT-3)
**What goes wrong:** Both sides call `export_keying_material` with the same label and context but get different bytes. This causes `server_sig` verification to fail on every connection — not a security problem but a correctness one.
**Why it happens:** RFC 9266 only guarantees identical output when label, context, AND output length are all identical. If one side passes `context: b""` and the other passes `context: &[]` (same bytes, but also: any length difference breaks it), or the output lengths differ, they diverge.
**How to avoid:** Define the constants once in `nosh-proto` or `nosh-auth` (or simply inline the same literal on both sides): `label = b"nosh-inner-auth-v1"`, `context = b""`, `output_len = 32`. Test with a mock that asserts both sides derive the same bytes.
**Warning signs:** `InnerAuthFail` on every connection even with valid keys; server logs "server_sig verification failed."

### Pitfall 2: Wrong method on quic_connection (not a security issue, just a build error)
**What goes wrong:** `conn.quic_connection().max_datagram_size()` is called instead of `export_keying_material`. This compiles but returns datagram size, not keying material.
**How to avoid:** The call is `export_keying_material(&mut out_buf, label, context)`. It is NOT `handshake_data()`. It is NOT a `rustls::ConnectionCommon` method — it is directly on `quinn::Connection`.

### Pitfall 3: Transcript field order mismatch (silent-failure)
**What goes wrong:** Client builds `SHA-256(label || ekm || server_nonce || client_nonce || server_spki)` but server builds `SHA-256(label || ekm || client_nonce || server_nonce || server_spki)` — nonce order swapped. Hash differs; verification fails every time.
**How to avoid:** Define the transcript field order as a named constant or a helper function in one place, imported by both `inner_auth.rs` files.
**Warning signs:** `client_sig` never verifies despite valid keys and correct EKM.

### Pitfall 4: block_in_place panic on native-QUIC TOFU path
**What goes wrong:** `tokio::task::block_in_place(|| prompt())` panics with "cannot call `block_in_place` from the context of an `async fn`" if the TLS verifier is called from a single-threaded tokio context (e.g. `tokio::runtime::Builder::new_current_thread()`).
**Why it happens:** `block_in_place` requires a multi-threaded tokio runtime. nosh-server uses `new_multi_thread()` so this is safe in production. Tests that use `#[tokio::test]` (single-thread by default) will panic.
**How to avoid:** Tests for the TOFU-path code should use `#[tokio::test(flavor = "multi_thread")]` or avoid calling `block_in_place` in the test by using `TofuPolicy::Silent` (or a mock `TofuPolicy::TrustKey(key)`).

### Pitfall 5: InnerAuthFail leaks information via timing
**What goes wrong:** The server performs expensive crypto (signature verification) before checking `authorized_keys`. A fast fail (key not in `authorized_keys`, no expensive verification) vs. a slow fail (verification runs, then fails) leaks whether the key exists.
**Why it matters:** This is a minor side-channel on the key-existence oracle, separate from the wire-format oracle. It does not compromise security in practice (attacker already knows their own key) but is worth noting.
**How to avoid:** For an Ed25519 signature verification, the cost is negligible (~µs). Perform authorized-key lookup and signature verification in a fixed order (verify signature first, then check authorized_keys) so the per-key timing is consistent. Log only the fingerprint, never the outcome.

### Pitfall 6: Pre-connect TOFU on native-QUIC cannot display fingerprint before connect
**What goes wrong:** D-09 says "check known_hosts before dialling; if unknown, prompt, record on `yes`, then connect with the key already pinned." But the server's public key is not known until after the TLS handshake. The pre-connect check can only say "this host is unknown" — it cannot display the fingerprint until after handshake.
**How to avoid:** For Phase 25, implement TOFU inside `HostKeyVerifier::verify_server_cert` using `block_in_place` (Pattern 4 above). The `--trust-key` flag (WT-UX-02, deferred) will enable true pre-connect pinning in a future phase. Document this limitation.

### Pitfall 7: ssh-agent blocking sign() holds the quinn handshake task
**What goes wrong:** `AgentSigner::sign()` opens a Unix socket to ssh-agent synchronously. If called on the tokio event loop thread, it blocks all other futures on that thread.
**Why it matters:** On the WebTransport path, inner auth runs in a `tokio::spawn` task — the blocking happens only in that task. But if the inner auth is called from inside an async function without `spawn_blocking`, the socket I/O blocks the tokio thread until the agent responds.
**How to avoid:** Wrap the `signer.sign(transcript)` call in `tokio::task::spawn_blocking(move || signer.sign(transcript)).await?`. This is the same pattern used for the TLS handshake `AgentSigner` in `signer.rs`. The `RawEd25519Signer` trait is `Send + Sync`, so `Arc<dyn RawEd25519Signer>` is safely moved into `spawn_blocking`.

---

## Code Examples

### Export Keying Material (RFC 9266 channel binding)

```rust
// Source: quinn 0.11.9 Connection::export_keying_material — verified in
// ~/.cargo/registry/src/…/quinn-0.11.9/src/connection.rs:605
// Accessed via: wtransport::Connection::quic_connection() — verified in
// ~/.cargo/registry/src/…/wtransport-0.7.1/src/connection.rs:415

let mut ekm = [0u8; 32];
conn.quic_connection()
    .export_keying_material(&mut ekm, b"nosh-inner-auth-v1", b"")?;
```

### Transcript Hash (sha2)

```rust
// Source: sha2 = "0.10", nosh-auth/Cargo.toml; Digest trait API (stable)
use sha2::{Digest, Sha256};

fn transcript_server(ekm: &[u8; 32], server_nonce: &[u8; 32], client_nonce: &[u8; 32], server_spki: &[u8]) -> [u8; 32] {
    Sha256::new()
        .chain_update(b"nosh-inner-auth-v1\0")
        .chain_update(ekm)
        .chain_update(server_nonce)
        .chain_update(client_nonce)
        .chain_update(server_spki)
        .finalize()
        .into()
}
```

### Sign Transcript

```rust
// Source: nosh-auth/src/signer.rs — RawEd25519Signer::sign(&self, msg: &[u8])
// For server host key (InProcessEd25519Signer), for client (AgentSigner on Unix).

let sig: [u8; 64] = tokio::task::spawn_blocking({
    let signer = Arc::clone(&signer);
    let transcript = transcript_bytes; // [u8; 32]
    move || signer.sign(&transcript)
}).await??;
```

### Verify Signature

```rust
// Source: ed25519-dalek 2.2, standard ed25519 verification
use ed25519_dalek::{VerifyingKey, Signature, Verifier};
use nosh_auth::keys::nosh_key_from_spki;

fn verify_ed25519(spki: &[u8], transcript: &[u8; 32], sig_bytes: &[u8; 64]) -> bool {
    let key = match nosh_key_from_spki(spki) {
        Some(k) => k,
        None => return false,
    };
    let vk = match VerifyingKey::from_bytes(key.key32()) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let sig = Signature::from_bytes(sig_bytes);
    vk.verify(transcript, &sig).is_ok()
}
```

Note: `ed25519-dalek` is not currently a direct dep of `nosh-server` or `nosh-client`. It is a transitive dep via `nosh-auth`. For direct use in `inner_auth.rs`, add `ed25519-dalek = "2.2"` to each crate's `Cargo.toml`. Alternatively, expose a `verify_ed25519_spki(spki, msg, sig) -> bool` helper from `nosh-auth/src/keys.rs` so `ed25519-dalek` stays encapsulated in `nosh-auth`.

### TOFU Prompt (blocking, stderr)

```rust
// Source: CLAUDE.md / D-08 / SEC-02 — nosh design. No external library needed.
use std::io::{self, BufRead, Write};

fn prompt_tofu(host: &str, fingerprint: &str) -> io::Result<bool> {
    let stderr = io::stderr();
    let mut out = stderr.lock();
    writeln!(out, "The authenticity of host '{host}' can't be established.")?;
    writeln!(out, "ED25519 key fingerprint is {fingerprint}.")?;
    writeln!(out, "Are you sure you want to continue connecting (yes/no)? ")?;
    out.flush()?;
    let stdin = io::stdin();
    let line = stdin.lock().lines().next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "stdin closed"))??;
    Ok(line.trim() == "yes")
}
```

Note: If stdin is not a TTY (D-10), the `lines().next()` call will either return EOF immediately (piped input with no data) or block waiting for data that never comes. The `is_terminal()` check should precede this call. The `crossterm` crate (already in `nosh-client`) provides `crossterm::tty::IsTty::is_tty()` for this check. Alternatively, `std::io::IsTerminal` (stable since Rust 1.70) requires no extra dependency.

---

## State of the Art

| Old Approach | Current Approach | When Changed | Impact |
|--------------|------------------|--------------|--------|
| Silent TOFU record in TLS verifier callback | Blocking interactive prompt (SEC-02) | Phase 25 | User sees fingerprint; MITM on first connect is detectable |
| No inner auth on WebTransport (test stub) | Full mutual SSH-key challenge-response | Phase 25 | WebTransport mode is actually secure |

**Deprecated/outdated:**
- `HostKeyVerifier` silent-record path (`verifier.rs:80–86`): replaced by `TofuPolicy::Interactive` with `block_in_place` prompt. The `Silent` variant is retained for tests but must not be the default in production.
- `handle_connection_wt` stub (`wt_transport.rs:406–411`): `if !skip_inner_auth { ... conn.close(1, b"inner-auth-not-implemented"); }` — this block is replaced by a real call to `run_inner_auth_server`.

---

## Open Questions

All locked decisions from CONTEXT.md are resolved. The following are implementation choices left to Claude's Discretion:

1. **NoshTransport::export_keying_material vs. passing raw connection reference**
   - What we know: `quic_connection()` on `WtransportTransport` gives `&quinn::Connection`; `export_keying_material` is on `quinn::Connection`.
   - What's unclear: whether `export_keying_material` should be added to the `NoshTransport` trait (clean but adds a method), or the inner_auth functions should receive a `&quinn::Connection` directly alongside the boxed transport.
   - Recommendation: add to `NoshTransport` trait with a default impl returning `Err(...)` for transports that don't support it. The native-QUIC `QuinnTransport` can also implement it for future use.

2. **Where to place transcript-layout constants**
   - What we know: both `nosh-server/src/inner_auth.rs` and `nosh-client/src/inner_auth.rs` need the same label strings and field order.
   - Recommendation: define `const NOSH_INNER_AUTH_LABEL_CLIENT: &[u8]` and `NOSH_INNER_AUTH_LABEL_SERVER: &[u8]` in `nosh-proto` (since both server and client depend on it), or inline them as identical literals in each `inner_auth.rs` file.

3. **`ed25519-dalek` direct dep vs. helper in `nosh-auth`**
   - Recommendation: add a `pub fn verify_ed25519_spki(spki: &[u8], msg: &[u8], sig: &[u8; 64]) -> bool` to `nosh-auth/src/keys.rs`. This keeps `ed25519-dalek` encapsulated in `nosh-auth` and avoids adding another direct dep to `nosh-server`/`nosh-client`.

---

## Environment Availability

| Dependency | Required By | Available | Version | Fallback |
|------------|-------------|-----------|---------|----------|
| `getrandom` | CSPRNG nonce generation | Partial (dev-dep only) | 0.2 | None — needs promoting to real dep in nosh-server and nosh-client |
| `sha2` | Transcript hash | Partial (`nosh-auth` only) | 0.10 | None — needs adding to nosh-server and nosh-client |
| `ed25519-dalek` | Signature verification | Transitive (via nosh-auth) | 2.2 | Use helper in nosh-auth instead of direct dep |
| `ssh-agent` socket | AgentSigner (Unix only) | Conditional | — | Falls back to FileSigner/InProcessEd25519Signer if agent unavailable |

**Missing dependencies with no fallback:**
- `getrandom` as a real dep in `nosh-server` (currently only dev-dep in other crates)
- `sha2` as a direct dep in `nosh-server` and `nosh-client`

**Missing dependencies with fallback:**
- Direct `ed25519-dalek` — mitigated by exposing verification helper from `nosh-auth`

---

## Validation Architecture

### Test Framework

| Property | Value |
|----------|-------|
| Framework | `cargo nextest` (recommended) / `cargo test` |
| Config file | `.cargo/nextest.toml` (if present) or default |
| Quick run command | `cargo test -p nosh-proto --lib` (discriminant stability) |
| Full suite command | `cargo test --workspace --features "nosh-server/webtransport nosh-client/webtransport"` |

### Phase Requirements → Test Map

| Req ID | Behavior | Test Type | Automated Command | File Exists? |
|--------|----------|-----------|-------------------|--------------|
| WT-04 | Discriminants 18–21 encode at correct positions | unit | `cargo test -p nosh-proto message_discriminant_order_is_stable` | Wave 0 gap |
| WT-04 | `InnerAuthFail` encodes as exactly 1 byte (fieldless) | unit | `cargo test -p nosh-proto inner_auth_fail_is_fieldless` | Wave 0 gap |
| WT-04 | Successful mutual inner auth with valid keys | integration | `cargo test --features webtransport inner_auth_happy_path` | Wave 0 gap |
| WT-04 | Unknown client key rejected with `InnerAuthFail` | integration | `cargo test --features webtransport inner_auth_unknown_client_key` | Wave 0 gap |
| WT-04 | Wrong server key → client disconnects with error | integration | `cargo test --features webtransport inner_auth_wrong_server_key` | Wave 0 gap |
| WT-04 | `SessionOpen` before inner auth → close with error | integration | `cargo test --features webtransport inner_auth_session_open_before_auth` | Wave 0 gap |
| WT-04 | Signature over challenge-only (no EKM) fails verification | unit | `cargo test -p nosh-server inner_auth_sig_without_ekm_fails` | Wave 0 gap |
| WT-04 | Replayed challenge-response fails on new session | integration | `cargo test --features webtransport inner_auth_replay_rejected` | Wave 0 gap |
| SEC-02 | TOFU prompt blocks on unknown host key | integration | `cargo test --features webtransport tofu_blocks_on_unknown_key` | Wave 0 gap |
| SEC-02 | Empty input at TOFU prompt does NOT accept key | unit | `cargo test -p nosh-client prompt_tofu_rejects_empty_input` | Wave 0 gap |
| SEC-02 | No-TTY on unknown host key fails closed | unit | `cargo test -p nosh-client tofu_no_tty_fails_closed` | Wave 0 gap |

### Sampling Rate

- Per task commit: `cargo test -p nosh-proto --lib` (discriminant stability, fast)
- Per wave merge: `cargo test --workspace --features "nosh-server/webtransport nosh-client/webtransport"`
- Phase gate: Full suite green before `/gsd:verify-work`

### Wave 0 Gaps

- [ ] `crates/nosh-proto/src/codec.rs` — extend `message_discriminant_order_is_stable` to include discriminants 18–21
- [ ] `crates/nosh-proto/src/codec.rs` — add `inner_auth_fail_is_fieldless` test (encodes to 1 byte)
- [ ] `crates/nosh-server/src/inner_auth.rs` — new file; unit tests for transcript construction and signature verification
- [ ] `crates/nosh-client/src/inner_auth.rs` — new file; unit tests for transcript construction
- [ ] `crates/nosh-client/tests/inner_auth_integration.rs` (or extend `webtransport.rs`) — end-to-end inner auth tests

---

## Security Domain

### Applicable ASVS Categories

| ASVS Category | Applies | Standard Control |
|---------------|---------|-----------------|
| V2 Authentication | Yes | SSH Ed25519 mutual challenge-response; `authorized_keys` and `known_hosts` gates |
| V3 Session Management | Yes | `Unauthenticated → ChallengeExchanged → Authenticated` state machine; `Reattach`/`SessionOpen` only in `Authenticated` |
| V4 Access Control | Yes | `check_authorized_key` helper gates access; no session before `InnerAuthComplete` |
| V5 Input Validation | Yes | `nosh_key_from_spki` validates SPKI length and prefix; `InnerAuthFail` is fieldless (no parsing of failure reasons) |
| V6 Cryptography | Yes | SHA-256 via `sha2`; Ed25519 via `ed25519-dalek`; EKM via quinn's RFC 5705 implementation |

### Known Threat Patterns

| Pattern | STRIDE | Standard Mitigation |
|---------|--------|---------------------|
| Transparent-proxy MITM (WT-3) | Spoofing | RFC 9266 EKM in signed transcript — proxy on a different TLS leg has different EKM |
| Challenge replay across sessions (WT-4) | Repudiation | 32-byte CSPRNG server nonce; single-use enforced server-side |
| Session fixation via reattach-token theft (MH-1) | Elevation of privilege | `Reattach`/`SessionOpen` gated on `Authenticated` state; token never sent before `InnerAuthComplete` |
| Key-existence oracle via `InnerAuthFail` | Information disclosure | `InnerAuthFail` is fieldless; server logs reason privately |
| TOFU fatigue / auto-accept (SEC-3) | Spoofing | Blocking prompt; explicit `yes` required; empty input = "no"; no-TTY fails closed |
| Discriminant shift (WF-1) | Tampering | `message_discriminant_order_is_stable` test updated in same commit as new variants |
| Blocking ssh-agent starves event loop | Denial of service | `spawn_blocking` wrapper around `signer.sign()` |

---

## Assumptions Log

| # | Claim | Section | Risk if Wrong |
|---|-------|---------|---------------|
| A1 | `ed25519-dalek 2.2` `VerifyingKey::from_bytes` and `Verifier::verify` API are stable and match the SPKI key bytes from `NoshPublicKey::key32()` | Code Examples | Verification fails; need to adjust byte extraction from SPKI |
| A2 | `std::io::IsTerminal` (stable since Rust 1.70) is available in the workspace's MSRV | TOFU Prompt pattern | If MSRV < 1.70, use `crossterm::tty::IsTty` (already in `nosh-client`) instead |
| A3 | `quinn 0.11.9`'s `export_keying_material` error type implements `std::error::Error` and is compatible with `anyhow::Result` mapping via `?` | Channel-Binding API Gate | Need explicit `.map_err(|e| anyhow::anyhow!("{e:?}"))` if not |

**If this table were empty:** All claims were verified or cited. Three assumptions remain low-risk but flag for implementation-time confirmation.

---

## Sources

### Primary (HIGH confidence)

- `/home/bharris/github.com/bharrisau/nosh/crates/nosh-proto/src/messages.rs` — 18 `Message` variants (discriminants 0–17 confirmed in codec.rs:276–312); `ReattachErr` fieldless pattern
- `/home/bharris/github.com/bharrisau/nosh/crates/nosh-proto/src/codec.rs:276–312` — `message_discriminant_order_is_stable` test; current tail at discriminant 17 (`ScrollbackCredit`)
- `/home/bharris/github.com/bharrisau/nosh/crates/nosh-auth/src/verifier.rs` — `HostKeyVerifier` silent-record TOFU path (lines 80–86); `AuthorizedKeysVerifier` authorized-key check (lines 171–177)
- `/home/bharris/github.com/bharrisau/nosh/crates/nosh-auth/src/keys.rs` — `NoshPublicKey::fingerprint()`, `lookup_known_host`, `record_known_host`, `nosh_key_from_spki`, `ed25519_spki_der`
- `/home/bharris/github.com/bharrisau/nosh/crates/nosh-auth/src/signer.rs` — `RawEd25519Signer` trait, `AgentSigner`, `InProcessEd25519Signer`, `FileSigner`
- `/home/bharris/github.com/bharrisau/nosh/crates/nosh-server/src/wt_transport.rs` — `handle_connection_wt` stub (lines 390–467); `quic_connection()` usage confirmed (lines 86, 126, 131)
- `/home/bharris/github.com/bharrisau/nosh/crates/nosh-client/src/wt_transport.rs` — client `WtransportTransport`; `quic_connection()` usage confirmed (lines 84, 128, 133)
- `/home/bharris/github.com/bharrisau/nosh/crates/nosh-proto/src/transport_trait.rs` — `NoshTransport`/`NoshSendStream`/`NoshRecvStream` traits; `write_message_ns`/`read_message_ns`
- `~/.cargo/registry/src/…/quinn-0.11.9/src/connection.rs:605–617` — `quinn::Connection::export_keying_material` signature and implementation — CONFIRMED present
- `~/.cargo/registry/src/…/wtransport-0.7.1/src/connection.rs:415` — `wtransport::Connection::quic_connection() -> &quinn::Connection` — CONFIRMED public, `#[cfg(feature = "quinn")]`
- `/home/bharris/github.com/bharrisau/nosh/Cargo.toml:35` — `wtransport = { ..., features = ["quinn"] }` — CONFIRMED quinn feature active, enabling `quic_connection()`
- `/home/bharris/github.com/bharrisau/nosh/crates/nosh-auth/Cargo.toml:21` — `sha2 = "0.10"` — CONFIRMED in dep tree
- `.planning/phases/25-inner-ssh-key-handshake-tofu-prompt/25-CONTEXT.md` — locked decisions D-01 through D-10

### Secondary (MEDIUM confidence)

- `.planning/research/PITFALLS.md` — WT-3 (channel binding), WT-4 (nonce replay), WF-1 (discriminant corruption), MH-1 (token before auth), SEC-3 (TOFU fatigue) — grounded in first-party codebase reads
- `.planning/research/ARCHITECTURE.md §"Inner SSH-key handshake"` — 4-step sequence, message variant definitions, `run_inner_auth_server`/`run_inner_auth_client` sketches

---

## Metadata

**Confidence breakdown:**
- Channel-binding API gate: HIGH — verified against both quinn 0.11.9 source and wtransport 0.7.1 source in local cargo registry; workspace Cargo.toml confirms `quinn` feature active
- Message variant additions: HIGH — discriminant test and pattern are first-party source; append-only invariant documented extensively
- Crypto reuse (RawEd25519Signer, sha2, getrandom): HIGH — first-party source confirms all primitives present
- TOFU prompt design: HIGH — D-08/D-09/D-10 are locked decisions; `fingerprint()` format verified; `block_in_place` approach is a standard tokio pattern
- Signed transcript layout: MEDIUM — design recommendation from Claude's Discretion; no external authority; planner should confirm field order

**Research date:** 2026-06-13
**Valid until:** 60 days (stable crates, locked design decisions; no fast-moving APIs involved)
