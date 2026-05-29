# Phase 2 Research: SSH-Key Mutual Auth

**Researched:** 2026-05-29
**Confidence:** HIGH — all crate APIs verified against installed source (ssh-key 0.6.7, ssh-agent-client-rs 1.1.2, rcgen 0.14.8, rustls 0.23.40, ed25519-dalek 2.2.0).

This document records the *verified* API surface and the locked implementation
shape, building on `.planning/research/STACK.md` and `PITFALLS.md` (already
authoritative for the broader stack). It is intentionally Phase-2-specific.

## Crate APIs (verified from source)

### ssh-agent-client-rs 1.1.2
- `Client::connect(path: &Path) -> Result<Client>` — connects to a Unix socket (use `$SSH_AUTH_SOCK`).
- `Client::sign(&mut self, key: impl Into<Identity>, data: &[u8]) -> Result<ssh_key::Signature>`.
  - `Identity: From<ssh_key::PublicKey>` and `From<&PublicKey>`.
  - For Ed25519 the agent signs the **raw `data`** with no pre-hashing (EdDSA hashes internally). This is exactly what TLS 1.3 Ed25519 CertificateVerify requires (sign the constructed input directly). Returned `Signature::as_bytes()` is the 64-byte raw Ed25519 signature.
- `Client::list_identities(&mut self) -> Result<Vec<PublicKey>>` — used to pick the default identity when `--identity` is not given and exactly one key is present.
- API is **synchronous + blocking** → call from within `tokio::task::spawn_blocking` or accept a brief block. We wrap agent calls in `spawn_blocking` at cert-mint time (client connect path), so the blocking I/O never lands on the rustls handshake task.

### ssh-key 0.6.7 (features: `ed25519`, `std`)
- `PublicKey::from_openssh(&str)` / `read_openssh_file` — parse `authorized_keys`/identity pubkeys.
- `PrivateKey::read_openssh_file(path)` — parse the server host key file. `.public_key()` yields the matching `PublicKey`.
- `KeyData::Ed25519(Ed25519PublicKey)`; `Ed25519PublicKey(pub [u8; 32])` — the raw 32-byte key.
- `authorized_keys::AuthorizedKeys` and `known_hosts` modules exist; for the spike we parse line-by-line and match on key only (CONTEXT D-07, deferred options).
- `Signature::as_bytes()`, `Signature::algorithm()`.
- `Ed25519PublicKey -> ed25519_dalek::VerifyingKey` via `TryFrom` (used in the in-process fallback signer + tests).

### rcgen 0.14.8
- `pub trait PublicKeyData { fn der_bytes(&self) -> &[u8]; fn algorithm(&self) -> &'static SignatureAlgorithm; }` — `der_bytes()` returns the **raw** public key bytes (32 for Ed25519); rcgen wraps them into SPKI using `algorithm()`.
- `pub trait SigningKey: PublicKeyData { fn sign(&self, msg: &[u8]) -> Result<Vec<u8>>; }`.
- `pub static PKCS_ED25519: SignatureAlgorithm` (in `rcgen::PKCS_ED25519`).
- `CertificateParams::self_signed(&impl SigningKey) -> Result<Certificate>`; `Certificate` → `CertificateDer` via `into()`/`.der()`.
- **Decision:** implement a custom `rcgen::SigningKey` whose `der_bytes()` = the SSH Ed25519 raw key and whose `sign()` delegates to the agent (client) or the host key (server). This mints a **genuinely self-signed Ed25519 cert whose SPKI IS the SSH public key** (CONTEXT D-09). The cert self-signature is valid, but our pinning verifiers do not depend on it — they pin on SPKI and verify the live `CertificateVerify` (CONTEXT D-10).

### rustls 0.23.40
- Client side (unchanged from Phase 1 seam): `ServerCertVerifier` with REAL `verify_tls13_signature` delegation kept; only `verify_server_cert` becomes SPKI-pinning + TOFU.
- Server side: `ServerConfig::builder().with_client_cert_verifier(Arc<dyn ClientCertVerifier>)` replaces `with_no_client_auth()`. `ClientCertVerifier` requires `verify_client_cert` (SPKI vs authorized_keys), real `verify_tls12/tls13_signature` delegation, `root_hint_subjects() -> &[]` (empty), and `supported_verify_schemes() -> [ED25519]`.
- Client identity (the CertificateVerify signature) is produced by rustls calling `Signer::sign(message)` on the resolved client cert's key. rustls verifies it against the cert SPKI = SSH key. The `message` passed to `Signer::sign` is the **fully constructed** TLS 1.3 CertificateVerify input (`0x20`×64 ‖ context ‖ `0x00` ‖ transcript-hash, RFC 8446 §4.4.3) — rustls builds it; we must NOT reconstruct it (PITFALL 8). The agent signs it raw.
- Client cert is supplied via `ClientConfig::...with_client_auth_cert(certs, key)` where `key` is an `Arc<dyn rustls::sign::SigningKey>` — our `AgentSigningKey` whose `choose_scheme` returns an `AgentSigner` for `SignatureScheme::ED25519`.

## SPKI extraction in the verifiers
The peer presents an X.509 `CertificateDer`. We parse it (x509-parser, already a transitive dep via rcgen, added directly) to pull the `SubjectPublicKeyInfo`, then compare the contained Ed25519 raw key (or the full SPKI DER) against the pinned SSH key's SPKI. Ed25519 SPKI is the fixed 44-byte DER `30 2A 30 05 06 03 2B 65 70 03 21 00 ‖ key32`. We build the pinned SPKI from the SSH key and compare SPKI-to-SPKI; the signature methods stay REAL (delegate to CryptoProvider) — never stubbed (PITFALL 5).

## TOFU / known_hosts (CONTEXT D-01/D-02/D-05)
- Default path `~/.ssh/known_hosts`, overridable. On unknown host: append a line `[<host>]:<port> ssh-ed25519 <b64>` (we use the connect server name) and proceed. On a recorded entry mismatch: hard-fail (return `Err`), do not overwrite.
- For the spike we key TOFU on the server-name string passed to `connect()` (matches OpenSSH host keying granularity at the level this spike needs).

## ssh-agent round-trip (riskiest — validated EARLY, CONTEXT/INIT)
- An ephemeral agent is spawned inside the integration test: `ssh-agent -a <tmp.sock>`, `ssh-keygen -t ed25519 -N "" -f key`, `SSH_AUTH_SOCK=<sock> ssh-add key`.
- A focused unit/integration test signs a known 32-byte input via the agent and verifies the 64-byte Ed25519 signature with `ed25519-dalek` against the key — proving the signer path before the full handshake is built on top.
- **Fallback:** if `ssh-agent`/`ssh-keygen` are unavailable at runtime, an in-process `InProcessEd25519SigningKey` exercises the identical `rustls::sign::SigningKey`/`Signer` + rcgen `SigningKey` code path with an `ed25519-dalek` key, and the live-agent test is `#[ignore]`-gated + reported as a human-verification item.

## Pre-auth DoS cap (CONTEXT D-13, FOOTGUN-3)
- A `tokio::sync::Semaphore` with `max_concurrent_handshakes` (default 64) permits acquired in the accept loop *before* `incoming.await`; permit released when the connection finishes auth or is dropped.
- `tokio::time::timeout(auth_timeout, incoming.await)` (default 5s); on elapse, the half-open connection is dropped (its `Incoming`/`Connecting` future is cancelled), freeing state. Both values are flag-overridable on the server binary.

## Ed25519-only guard (CONTEXT D-12)
Any non-Ed25519 SSH key (host key, identity, authorized_keys entry) is rejected at load time with a clear error: `unsupported key type {alg} (Ed25519 only in this milestone)`.
