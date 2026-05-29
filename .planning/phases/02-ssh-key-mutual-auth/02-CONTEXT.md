# Phase 2: SSH-Key Mutual Auth - Context

**Gathered:** 2026-05-29
**Status:** Ready for planning

<domain>
## Phase Boundary

Mutual authentication derived from existing SSH keys, enforced inside the TLS 1.3 handshake. An unknown client key is rejected during the handshake before any session code runs; the client pins the server host key against `known_hosts` (TOFU); the client's `CertificateVerify` is signed via `ssh-agent` (private key never handled directly); concurrent unauthenticated/half-open connections are capped with an auth-completion timeout. Builds directly on the Phase 1 quinn/rustls wiring. No PTY/session yet (Phase 3). Covers AUTH-01..05.

</domain>

<decisions>
## Implementation Decisions

### Host-key trust / TOFU
- **D-01:** Auto-TOFU. On first contact with an unknown server host key, record the fingerprint to `known_hosts` and proceed silently — no interactive prompt (works non-interactively; matches the success criterion).
- **D-02:** On mismatch against a recorded `known_hosts` entry, hard-fail (abort the connection) — do not prompt, do not overwrite.
- **D-03:** Server-side, an unknown client key (not in `authorized_keys`) is rejected during the TLS handshake via the `ClientCertVerifier` — no session/connection-accept code runs for it.

### Key & file locations (mirror OpenSSH)
- **D-04:** Client signing identity comes from **ssh-agent** (AUTH-04, locked). Which identity is selectable via `--identity` (path to a public key, or a default agent key when only one is present). The agent produces the `CertificateVerify` signature; the private key is never read.
- **D-05:** Client pins server host keys to `~/.ssh/known_hosts` by default (OpenSSH known_hosts format), overridable by flag.
- **D-06:** Server reads its **host private key from a file** (daemon model — not via agent), default a nosh host-key path, overridable via `--host-key`. The server signs its own handshake `CertificateVerify` directly with this host key.
- **D-07:** Server authorizes client keys against `~/.ssh/authorized_keys` by default (OpenSSH authorized_keys format), overridable by flag.
- **D-08:** All paths are flag-overridable; defaults reuse the user's existing OpenSSH trust files — reusing existing SSH identity is the project's core value.

### Cert/identity mechanism (from research — confirmed approach)
- **D-09:** Self-signed-cert key-pinning (NOT RFC 7250 RPK — deferred per PROJECT decision). Each side wraps its SSH **public** key in an ephemeral self-signed X.509 cert whose SubjectPublicKeyInfo IS the SSH public key. Verification pins on the SPKI bytes (compare against `known_hosts`/`authorized_keys` entries), not PKI path validation.
- **D-10:** Replace Phase 1's `PlaceholderServerVerifier::verify_server_cert` with SSH-key SPKI pinning + TOFU. Keep the existing real `verify_tls12_signature`/`verify_tls13_signature` delegation unchanged. Add a server-side `ClientCertVerifier` (the server changes from `with_no_client_auth()` to requiring + verifying client certs).
- **D-11:** The client `CertificateVerify` signature MUST be computed by ssh-agent over the correctly-constructed TLS 1.3 CertificateVerify input (`0x20`×64 ‖ context-string ‖ `0x00` ‖ transcript-hash, per RFC 8446 §4.4.3 — research PITFALL). Wire ssh-agent into rustls via a custom `SigningKey`/`Signer` whose `sign()` delegates to the agent (synchronous trait → use `spawn_blocking`).

### Algorithm scope
- **D-12:** Ed25519 **only** for this spike — both client identity and server host key. Maps directly to `SignatureScheme::ED25519`; sidesteps the unverified `ssh-agent-client-rs` RSA SHA-2 flag risk. ECDSA and RSA are explicitly deferred to a later milestone. A non-Ed25519 key should fail with a clear "unsupported key type (Ed25519 only in this milestone)" error, not a confusing handshake failure.

### Pre-auth DoS limits
- **D-13:** Cap concurrent unauthenticated/half-open connections (default ~64) and enforce an auth-completion timeout (default ~5s); both overridable by flag. Connections that don't complete auth within the timeout are closed. Bounds memory against an unauthenticated flood (research: CVE-2024-22189 pattern).

### Claude's Discretion
- Exact ssh-agent client crate usage (`ssh-agent-client-rs` per research) and the `SigningKey`/`Signer` struct names.
- How the ephemeral self-signed cert is minted around the SSH public key (rcgen vs hand-built), and how the SSH host private key file is loaded/parsed (`ssh-key` crate).
- known_hosts/authorized_keys parsing (use `ssh-key`'s support where possible); handling of `authorized_keys` options/comments (can ignore options for the spike — match on key only).
- Precise default file paths for the nosh host key; flag names beyond those above.
- Whether the agent `sign()` runs on a dedicated thread vs `spawn_blocking` (research notes hardware keys can be slow).
- Error message wording and exact DoS limit constants.

</decisions>

<specifics>
## Specific Ideas

- Keep Phase 1's real signature-verification delegation — only `verify_server_cert` and the client-auth side change. The smaller the diff, the better.
- The two roadmapper-flagged caveats are inputs here, not blockers: (1) `ssh-agent-client-rs` 1.1.2 RSA SHA-2 flag exposure is sidestepped by the Ed25519-only decision (D-12); (2) the `verify_tls13_signature` / CertificateVerify signing round-trip is the riskiest part — validate the agent-signing round-trip early (a focused test that a real ssh-agent Ed25519 signature verifies against the pinned SPKI) before declaring the phase done.

</specifics>

<canonical_refs>
## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### Phase scope & prior decisions
- `.planning/PROJECT.md` — auth decisions (cert-pinning first, RPK deferred; ssh-agent signing; mutual/symmetric)
- `.planning/REQUIREMENTS.md` — AUTH-01..05 acceptance criteria
- `.planning/phases/01-quic-transport-skeleton/01-CONTEXT.md` — Phase 1 decisions / the placeholder-verifier seam

### Existing code to modify (the seam)
- `crates/nosh-auth/src/verifier.rs` — `PlaceholderServerVerifier`; replace `verify_server_cert`, keep signature delegation
- `crates/nosh-server/src/server.rs` — `build_server_config()`: currently `with_no_client_auth()` + rcgen `localhost` cert; add client-cert verifier + host-key cert
- `crates/nosh-client/src/client.rs` — `build_client_config()`: add client-cert/identity + ssh-agent signing

### Research (stack & gotchas)
- `.planning/research/STACK.md` — ssh-agent→rustls `SigningKey`/`Signer` path, `ssh-key` / `ssh-agent-client-rs` versions, cert-pinning vs RPK recommendation
- `.planning/research/PITFALLS.md` — custom verifier must not no-op signatures; TLS 1.3 CertificateVerify input format (RFC 8446 §4.4.3); RSA SHA-2 agent flags; ECDSA curve check; pre-auth memory cap (CVE-2024-22189)

No project-external specs beyond the above.

</canonical_refs>

<code_context>
## Existing Code Insights

### Reusable Assets
- `nosh-auth::PlaceholderServerVerifier` — its `verify_tls12_signature`/`verify_tls13_signature` (real CryptoProvider delegation) and `supported_verify_schemes` carry over unchanged; only `verify_server_cert` is replaced.
- `nosh-server::build_server_config()` and `nosh-client::build_client_config()` — the rustls→quinn wiring (`QuicServerConfig`/`QuicClientConfig::try_from`) is established; Phase 2 swaps the verifier/cert/auth pieces inside them.
- rcgen is already a dependency (used for the Phase 1 self-signed cert).

### Established Patterns
- `anyhow::Result` for fallible config builders; `tracing` for instrumentation; ring CryptoProvider installed as the process default.
- ALPN constant + transport config builder live in `nosh-proto`.

### Integration Points
- Server accept loop (`nosh-server`) is where the pre-auth cap + auth-timeout (D-13) hook in.
- `nosh-auth` gains the real verifiers + the ssh-agent `SigningKey`; both `nosh-server` and `nosh-client` depend on it.

</code_context>

<deferred>
## Deferred Ideas

- RSA and ECDSA key support (and the ssh-agent SHA-2 flag handling) — later milestone (D-12).
- RFC 7250 raw public keys — deferred per PROJECT decision; revisit when rustls issue #2257 status is confirmed.
- Host-key rotation as a signed object — M1+ per INIT.md §12, not this spike.
- `authorized_keys` options enforcement (from=, command=, etc.) — out of scope; match on key only.
- PTY / session / shell — Phase 3.

</deferred>

---

*Phase: 02-ssh-key-mutual-auth*
*Context gathered: 2026-05-29*
