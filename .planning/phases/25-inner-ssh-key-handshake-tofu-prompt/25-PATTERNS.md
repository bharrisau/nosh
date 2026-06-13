# Phase 25: Inner SSH-Key Handshake + TOFU Prompt — Pattern Map

**Mapped:** 2026-06-13
**Files analysed:** 9 (4 new, 5 modified)
**Analogs found:** 9 / 9

---

## File Classification

| New/Modified File | Role | Data Flow | Closest Analog | Match Quality |
|-------------------|------|-----------|----------------|---------------|
| `crates/nosh-proto/src/messages.rs` | model (wire format) | request-response | same file (Phase 21/22 append-only extension) | exact — same file, same pattern |
| `crates/nosh-proto/src/codec.rs` | model (discriminant stability test) | request-response | same file (codec.rs:276-312 stability test) | exact — same file, same test pattern |
| `crates/nosh-auth/src/keys.rs` | utility | CRUD | same file (`AuthorizedKeysVerifier::verify_client_cert` at line 171) | exact — extraction from same method |
| `crates/nosh-auth/src/verifier.rs` | middleware | request-response | same file (`HostKeyVerifier::verify_server_cert` TOFU branch) | exact — replacing lines 80-88 |
| `crates/nosh-server/src/inner_auth.rs` | service (NEW) | request-response | `crates/nosh-server/src/server.rs` (`handle_connection` + `run_session`) | role-match |
| `crates/nosh-client/src/inner_auth.rs` | service (NEW) | request-response | `crates/nosh-client/src/client.rs` (`build_client_config` + `connect`) | role-match |
| `crates/nosh-server/src/wt_transport.rs` | middleware (modified) | request-response | same file (`handle_connection_wt` lines 390-467) | exact — replacing the stub block |
| `crates/nosh-client/src/main.rs` | controller (modified) | request-response | same file (WT connect path lines 1354-1374) | exact — inserting inner auth after connect_wt |
| `crates/nosh-server/src/main.rs` | config/wiring (modified) | request-response | same file (WT arm lines 175-185) | exact — threading authorized+host-key into run_wt_accept_loop |

---

## Pattern Assignments

### `crates/nosh-proto/src/messages.rs` (model, wire format, MODIFIED)

**Analog:** Same file — Phase 21 append at line 219, Phase 22 append at line 273.

**Pattern: Append-only section header** (lines 213-219 for Phase 21 precedent):
```rust
    // ── Phase 21: Channel Multiplexing Foundation ────────────────────────────
    //
    // These variants are appended AFTER `TerminalControl` (discriminant 9) to
    // preserve the postcard discriminant order of all existing variants.
    // Inserting or reordering is NOT backward-compatible. The
    // discriminant-stability test in codec.rs (message_discriminant_order_is_stable)
    // enforces this invariant.
    // APPEND-ONLY from here.
```

**Pattern: Fieldless opaque-failure variant** (`ReattachErr` at line 174):
```rust
    /// Server → client on ANY reattach failure. FIELDLESS and UNIFORM — there
    /// is deliberately no reason code or distinguishing field (D-07). Unknown
    /// token, expired token, wrong SSH identity, active/reconnecting session:
    /// ALL map to this identical variant. This is the no-oracle invariant:
    /// an attacker cannot distinguish "session exists but wrong key" from
    /// "session does not exist".
    ///
    /// INVARIANT: this variant MUST remain fieldless forever. Adding a
    /// reason field would create a session-existence oracle.
    ReattachErr,
```

**Pattern: variant_name() extension** (lines 400-423):
```rust
    pub fn variant_name(&self) -> &'static str {
        match self {
            // ... existing arms ...
            // Phase 22 scrollback variants:
            Message::ScrollbackRequest { .. } => "ScrollbackRequest",
            Message::ScrollbackPage { .. } => "ScrollbackPage",
            Message::ScrollbackCredit { .. } => "ScrollbackCredit",
        }
    }
```
New arms to add AFTER `ScrollbackCredit`:
```rust
            // Phase 25 inner-auth variants:
            Message::InnerAuthChallenge { .. } => "InnerAuthChallenge",
            Message::InnerAuthResponse { .. } => "InnerAuthResponse",
            Message::InnerAuthComplete { .. } => "InnerAuthComplete",
            Message::InnerAuthFail => "InnerAuthFail",
```

**New variants to append after `ScrollbackCredit` (line 337):**
```rust
    // ── Phase 25: Inner SSH-key handshake — WebTransport only ────────────────
    //
    // Appended AFTER `ScrollbackCredit` (discriminant 17). Inserting or
    // reordering is NOT backward-compatible. The discriminant-stability test
    // in codec.rs (message_discriminant_order_is_stable) enforces this invariant.
    // APPEND-ONLY from here.

    /// Server → client: challenge (discriminant 18). D-03.
    InnerAuthChallenge {
        server_nonce: [u8; 32],
        server_spki: Vec<u8>,
        ekm: [u8; 32],
    },
    /// Client → server: response (discriminant 19). D-03.
    InnerAuthResponse {
        client_nonce: [u8; 32],
        client_spki: Vec<u8>,
        client_sig: [u8; 64],
    },
    /// Server → client: mutual auth complete (discriminant 20). D-03.
    InnerAuthComplete {
        server_sig: [u8; 64],
    },
    /// Either direction: inner auth failed, FIELDLESS (discriminant 21). D-04.
    InnerAuthFail,
```

---

### `crates/nosh-proto/src/codec.rs` (model, discriminant test, MODIFIED)

**Analog:** Same file — `message_discriminant_order_is_stable` test at lines 276-312.

**Pattern: Stability test case array** (lines 280-302):
```rust
    fn message_discriminant_order_is_stable() {
        use crate::messages::{ChannelType, TerminalControlPayload};
        use postcard::to_allocvec;

        let cases: &[(u8, Message)] = &[
            // ... existing entries 0-14 ...
            // Phase 22: Scrollback Sync — discriminants 15–17 (append-only after ChannelClose):
            (15, Message::ScrollbackRequest { channel_id: 2, from_line: 0, count: 256 }),
            (16, Message::ScrollbackPage {
                channel_id: 2, from_line: 0, total_available: 0,
                epoch_at_snapshot: 0, lines: vec![] }),
            (17, Message::ScrollbackCredit { channel_id: 2, bytes: 0 }),
        ];
```
Extend by appending after the `(17, ...)` entry:
```rust
            // Phase 25: Inner SSH-key handshake — discriminants 18–21 (append-only after ScrollbackCredit):
            (18, Message::InnerAuthChallenge {
                server_nonce: [0u8; 32], server_spki: vec![], ekm: [0u8; 32] }),
            (19, Message::InnerAuthResponse {
                client_nonce: [0u8; 32], client_spki: vec![], client_sig: [0u8; 64] }),
            (20, Message::InnerAuthComplete { server_sig: [0u8; 64] }),
            (21, Message::InnerAuthFail),
```

**Pattern: Fieldless variant size test** (model: `ChannelReject` size assertion at lines 383-389):
```rust
        // ChannelReject carries ONLY channel_id — no reason field.
        let reject = Message::ChannelReject { channel_id: 0 };
        let encoded = postcard::to_allocvec(&reject).expect("encode ChannelReject");
        // discriminant byte (1) + channel_id varint for 0 (1 byte) = 2 bytes total.
        assert_eq!(
            encoded.len(), 2,
            "ChannelReject must encode as exactly 2 bytes (discriminant + zero channel_id); \
             a reason field would increase this"
        );
```
Add analogous test for `InnerAuthFail`:
```rust
        // InnerAuthFail is fieldless — discriminant byte only = 1 byte.
        let fail = Message::InnerAuthFail;
        let encoded = postcard::to_allocvec(&fail).expect("encode InnerAuthFail");
        assert_eq!(
            encoded.len(), 1,
            "InnerAuthFail must encode as exactly 1 byte (discriminant only); \
             a fields would create an oracle (D-04)"
        );
```

---

### `crates/nosh-auth/src/keys.rs` (utility, MODIFIED — extract `check_authorized_key`)

**Analog:** Same file — `AuthorizedKeysVerifier::verify_client_cert` at `crates/nosh-auth/src/verifier.rs` lines 166-178.

**Current inline check** (`verifier.rs:166-178`):
```rust
    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, Error> {
        let spki = keys::extract_spki_from_cert(end_entity)
            .map_err(|_| Error::InvalidCertificate(CertificateError::BadEncoding))?;
        let presented = keys::nosh_key_from_spki(&spki)
            .ok_or(Error::InvalidCertificate(CertificateError::BadEncoding))?;

        if self.authorized.contains(&presented) {   // ← extract this check
            Ok(ClientCertVerified::assertion())
        } else {
            Err(Error::InvalidCertificate(
                CertificateError::ApplicationVerificationFailure,
            ))
        }
    }
```

**Pattern to add to `keys.rs`** (after `nosh_key_from_spki`, following the same doc-comment style):
```rust
/// Check whether `key` appears in `authorized`. Pure function reusable by both
/// the TLS verifier and the inner SSH-key handshake (Phase 25, D-07).
///
/// The inner-auth server calls this after parsing `client_spki` from the
/// `InnerAuthResponse` to gate session access (D-05 state machine gate).
pub fn check_authorized_key(key: &NoshPublicKey, authorized: &[NoshPublicKey]) -> bool {
    authorized.contains(key)
}
```

**Public export in `nosh-auth/src/lib.rs`** (append after line 32):
```rust
pub use keys::{
    check_authorized_key, load_authorized_keys, load_host_key,
    nosh_key_from_spki, NoshPublicKey, ED25519_SPKI_LEN,
};
```

---

### `crates/nosh-auth/src/verifier.rs` (middleware, MODIFIED — replace silent TOFU)

**Analog:** Same file — the TOFU branch at lines 80-88 is the target of replacement.

**Current silent TOFU path (lines 80-88) — REPLACED by Phase 25:**
```rust
            None => {
                // D-01: TOFU — record and proceed silently.
                let _guard = self.tofu_lock.lock().unwrap();
                keys::record_known_host(&self.known_hosts, &self.host, &presented)
                    .map_err(|e| Error::General(format!("known_hosts write failed: {e}")))?;
                tracing::info!(host = %self.host, "TOFU: recorded new host key");
                Ok(ServerCertVerified::assertion())
            }
```

**Struct addition pattern** (modelled on the existing `HostKeyVerifier` struct at lines 27-33):
```rust
pub struct HostKeyVerifier {
    known_hosts: PathBuf,
    host: String,
    provider: Arc<CryptoProvider>,
    tofu_lock: Mutex<()>,
    tofu_policy: TofuPolicy,  // NEW — Phase 25
}
```

**New `TofuPolicy` enum** (insert before `HostKeyVerifier`):
```rust
/// Controls how `HostKeyVerifier` handles an unknown server host key.
///
/// Phase 25 replaces the former `Silent` auto-record with `Interactive`
/// (SEC-02: blocking prompt, explicit `yes` required).
pub enum TofuPolicy {
    /// v1.3 behaviour — record silently without prompting. Retained for test use.
    /// MUST NOT be the default in production (SEC-02).
    Silent,
    /// SEC-02: blocking prompt to stderr/stdin. `block_in_place` required because
    /// this runs inside the TLS verifier callback on a tokio thread (Pitfall 4).
    Interactive,
    /// Future: trust a specific fingerprint without prompting (WT-UX-02, deferred).
    TrustKey(crate::keys::NoshPublicKey),
}
```

**Replacement TOFU branch with `TofuPolicy` dispatch:**
```rust
            None => {
                let _guard = self.tofu_lock.lock().unwrap();
                match &self.tofu_policy {
                    TofuPolicy::Silent => {
                        // v1.3 silent path — test-only; production uses Interactive.
                        keys::record_known_host(&self.known_hosts, &self.host, &presented)
                            .map_err(|e| Error::General(format!("known_hosts write failed: {e}")))?;
                        tracing::info!(host = %self.host, "TOFU: recorded new host key (silent mode)");
                    }
                    TofuPolicy::Interactive => {
                        // SEC-02: blocking prompt. block_in_place is safe: nosh-server
                        // uses new_multi_thread() (Pitfall 4). Test with flavor="multi_thread".
                        tokio::task::block_in_place(|| {
                            prompt_and_record(&self.known_hosts, &self.host, &presented)
                        })
                        .map_err(|e| Error::General(e.to_string()))?;
                    }
                    TofuPolicy::TrustKey(trusted) => {
                        if *trusted != presented {
                            return Err(Error::General(format!(
                                "host key mismatch for {} — pinned key does not match presented key",
                                self.host
                            )));
                        }
                        keys::record_known_host(&self.known_hosts, &self.host, &presented)
                            .map_err(|e| Error::General(format!("known_hosts write failed: {e}")))?;
                    }
                }
                Ok(ServerCertVerified::assertion())
            }
```

**`prompt_and_record` helper pattern** (uses the same `keys::` call style as the existing silent path, plus blocking stdin):
```rust
fn prompt_and_record(
    known_hosts: &std::path::Path,
    host: &str,
    key: &crate::keys::NoshPublicKey,
) -> anyhow::Result<()> {
    use std::io::{BufRead, Write};
    use std::io::IsTerminal as _;

    let fingerprint = key.fingerprint();
    let stderr = std::io::stderr();
    let mut out = stderr.lock();
    writeln!(out, "The authenticity of host '{host}' can't be established.")?;
    writeln!(out, "ED25519 key fingerprint is {fingerprint}.")?;

    if !std::io::stdin().is_terminal() {
        // D-10: no-TTY fails closed. Print fingerprint, reference future flag.
        writeln!(out, "Host key verification failed: stdin is not a TTY.")?;
        writeln!(out, "Use --trust-key <fingerprint> to accept a specific key non-interactively (future flag).")?;
        anyhow::bail!("cannot prompt for TOFU confirmation: stdin is not a TTY");
    }

    write!(out, "Are you sure you want to continue connecting (yes/no)? ")?;
    out.flush()?;
    drop(out);

    let line = std::io::stdin()
        .lock()
        .lines()
        .next()
        .ok_or_else(|| anyhow::anyhow!("stdin closed during TOFU prompt"))??;
    if line.trim() != "yes" {
        anyhow::bail!("Host key not accepted; connection refused.");
    }
    keys::record_known_host(known_hosts, host, key)
        .with_context(|| format!("record_known_host for {host}"))?;
    tracing::info!(host, "TOFU: user accepted and recorded new host key");
    Ok(())
}
```

---

### `crates/nosh-server/src/inner_auth.rs` (service, NEW)

**Analog:** `crates/nosh-server/src/server.rs` — `handle_connection` function (lines 522-650) for the state-machine dispatch pattern; `crates/nosh-auth/src/signer.rs` for the `spawn_blocking` signing pattern.

**Imports pattern** (modelled on `server.rs` imports + `wt_transport.rs`):
```rust
use std::sync::Arc;

use anyhow::Context;
use sha2::{Digest, Sha256};

use nosh_auth::keys::{self, check_authorized_key, NoshPublicKey};
use nosh_auth::signer::RawEd25519Signer;
use nosh_proto::transport_trait::{NoshSendStream, NoshRecvStream};
use nosh_proto::{Message, read_message_ns, write_message_ns};
```

**CSPRNG nonce pattern** (modelled on `signer.rs:247-249` `getrandom_seed`):
```rust
fn csprng_nonce() -> [u8; 32] {
    let mut nonce = [0u8; 32];
    getrandom::getrandom(&mut nonce).expect("getrandom failed");
    nonce
}
```

**Transcript hash pattern** (per RESEARCH.md Pattern 1):
```rust
fn transcript_client_signs(
    ekm: &[u8; 32],
    server_nonce: &[u8; 32],
    client_nonce: &[u8; 32],
    server_spki: &[u8],
) -> [u8; 32] {
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

**EKM extraction pattern** (per RESEARCH.md Channel-Binding section):
```rust
// In run_inner_auth_server, transport must expose export_keying_material.
// Option A: NoshTransport trait method (recommended).
// Option B: accept &quinn::Connection directly alongside the boxed transport.
let mut ekm = [0u8; 32];
transport.export_keying_material(&mut ekm, b"nosh-inner-auth-v1", b"")?;
// Or if passing raw conn:
// conn.export_keying_material(&mut ekm, b"nosh-inner-auth-v1", b"")?;
```

**`spawn_blocking` signing pattern** (from RESEARCH.md + signer.rs convention):
```rust
// Wrap blocking sign() to avoid starving the tokio thread (Pitfall 7).
let sig: [u8; 64] = tokio::task::spawn_blocking({
    let signer = Arc::clone(&host_signer);
    let transcript = transcript_bytes;
    move || signer.sign(&transcript)
}).await
    .context("spawn_blocking for signing")??;
```

**State machine + message send/recv pattern** (from `handle_connection_wt` lines 422-466):
```rust
// State machine: Unauthenticated → ChallengeExchanged → Authenticated (D-05).
// Messages sent/received via write_message_ns / read_message_ns.
write_message_ns(&mut *control_send, &Message::InnerAuthChallenge { ... }).await?;
match read_message_ns(&mut *control_recv).await? {
    Message::InnerAuthResponse { client_nonce, client_spki, client_sig } => {
        // ... verify, check authorized_keys, sign, send InnerAuthComplete
    }
    other => {
        tracing::warn!(frame = other.variant_name(), "unexpected frame during inner auth");
        write_message_ns(&mut *control_send, &Message::InnerAuthFail).await?;
        anyhow::bail!("inner auth protocol error");
    }
}
```

**Function signature** (based on server.rs `handle_connection` at lines 522-549):
```rust
/// Run the inner SSH-key mutual authentication handshake for WebTransport sessions.
/// Returns the authenticated client identity on success, or closes and errors on failure.
///
/// The state machine enforces Unauthenticated → ChallengeExchanged → Authenticated;
/// `SessionOpen`/`Reattach` are only accepted after this returns Ok (D-05, MH-1).
pub(crate) async fn run_inner_auth_server(
    transport: &dyn nosh_proto::transport_trait::NoshTransport,
    control_send: &mut dyn NoshSendStream,
    control_recv: &mut dyn NoshRecvStream,
    authorized: &[NoshPublicKey],
    host_signer: Arc<dyn RawEd25519Signer>,
) -> anyhow::Result<NoshPublicKey>
```

---

### `crates/nosh-client/src/inner_auth.rs` (service, NEW)

**Analog:** `crates/nosh-client/src/client.rs` — `build_client_config` (lines 95-127) for the `HostKeyVerifier` + `known_hosts` pattern; `crates/nosh-auth/src/verifier.rs` for the TOFU prompt flow.

**Imports pattern** (modelled on `client.rs` imports lines 1-21):
```rust
use std::path::Path;
use std::sync::Arc;

use anyhow::Context;
use sha2::{Digest, Sha256};

use nosh_auth::keys::{self, check_authorized_key, lookup_known_host, record_known_host, NoshPublicKey};
use nosh_auth::signer::RawEd25519Signer;
use nosh_proto::transport_trait::{NoshSendStream, NoshRecvStream};
use nosh_proto::{Message, read_message_ns, write_message_ns};
```

**TOFU prompt pattern** (inline in inner_auth_client, bypassing TLS verifier — clean blocking I/O):
```rust
// D-08: blocking TOFU prompt. No PTY output until resolved.
// This runs at the application level (not inside TLS verifier) — no block_in_place needed.
fn prompt_tofu_or_fail(host: &str, fingerprint: &str) -> anyhow::Result<bool> {
    use std::io::{BufRead, Write};
    use std::io::IsTerminal as _;
    let stderr = std::io::stderr();
    let mut out = stderr.lock();
    writeln!(out, "The authenticity of host '{host}' can't be established.")?;
    writeln!(out, "ED25519 key fingerprint is {fingerprint}.")?;
    if !std::io::stdin().is_terminal() {
        // D-10: non-TTY fails closed.
        writeln!(out, "Host key verification failed: stdin is not a TTY.")?;
        writeln!(out, "Use --trust-key <fingerprint> (future flag) for non-interactive use.")?;
        return Ok(false);
    }
    write!(out, "Are you sure you want to continue connecting (yes/no)? ")?;
    out.flush()?;
    drop(out);
    let line = std::io::stdin().lock().lines().next()
        .ok_or_else(|| anyhow::anyhow!("stdin closed"))??;
    Ok(line.trim() == "yes")
}
```

**Function signature** (parallel to `run_inner_auth_server`):
```rust
/// Run the inner SSH-key mutual authentication handshake (client side).
/// Returns Ok(()) on successful mutual auth, Err on any failure.
/// TOFU prompt for unknown server keys blocks here (D-08).
pub(crate) async fn run_inner_auth_client(
    transport: &dyn nosh_proto::transport_trait::NoshTransport,
    control_send: &mut dyn NoshSendStream,
    control_recv: &mut dyn NoshRecvStream,
    known_hosts: &Path,
    host: &str,
    client_signer: Arc<dyn RawEd25519Signer>,
) -> anyhow::Result<()>
```

---

### `crates/nosh-server/src/wt_transport.rs` (middleware, MODIFIED)

**Analog:** Same file — `handle_connection_wt` function lines 390-467.

**Current stub block to REPLACE** (lines 401-412):
```rust
    // Inner-auth gate (T-24-03-E: test-support bypass must not reach release builds).
    #[cfg(any(test, feature = "test-support"))]
    let skip_inner_auth = true;
    #[cfg(not(any(test, feature = "test-support")))]
    let skip_inner_auth = false;

    if !skip_inner_auth {
        // Phase 25 fills in the real inner SSH-key handshake.
        // Release builds reject any connection lacking inner auth.
        tracing::warn!(%peer, "WebTransport inner auth not yet implemented; closing connection");
        conn.close(1, b"inner-auth-not-implemented");
        return Ok(());
    }

    // test-support path: bypass inner auth.
    tracing::warn!(%peer, "INNER AUTH BYPASSED — test-support mode; MUST NOT appear in release builds");
    let peer_identity = nosh_auth::NoshPublicKey::from_raw([0u8; 32]);
```

**Replacement pattern** (based on `server.rs` `handle_connection` lines 522-560, adapted for WT):
```rust
    // Accept first bidi stream — this is the control stream for the inner auth
    // handshake AND the subsequent session pump (D-05 state machine gate).
    let (mut send, mut recv) = match conn.accept_bi().await {
        Ok(pair) => pair,
        Err(e) => { tracing::warn!(%peer, "accept_bi failed: {e}"); return Ok(()); }
    };

    // Inner-auth gate — production always runs this; test-support bypasses.
    #[cfg(not(any(test, feature = "test-support")))]
    let peer_identity = {
        match crate::inner_auth::run_inner_auth_server(
            &*conn,
            &mut *send,
            &mut *recv,
            &authorized,
            host_signer.clone(),
        ).await {
            Ok(identity) => identity,
            Err(e) => {
                tracing::warn!(%peer, "inner auth failed: {e:#}");
                conn.close(1, b"inner-auth-failed");
                return Ok(());
            }
        }
    };
    #[cfg(any(test, feature = "test-support"))]
    let peer_identity = nosh_auth::NoshPublicKey::from_raw([0u8; 32]);
```

**Function signature change** — `handle_connection_wt` needs `authorized` and `host_signer` passed in (currently only takes `transport, registry, shell`). The analog for this pattern is `server.rs::run_accept_loop` passing `host_key_path` and `authorized_keys_path` into `handle_connection` (lines 522-549):
```rust
pub(crate) async fn handle_connection_wt(
    conn: Box<dyn NoshTransport>,
    registry: Arc<SessionRegistry>,
    shell_override: Option<String>,
    authorized: Arc<Vec<nosh_auth::NoshPublicKey>>,          // NEW
    host_signer: Arc<dyn nosh_auth::RawEd25519Signer>,       // NEW
) -> anyhow::Result<()>
```

And `run_wt_accept_loop` signature grows correspondingly (modelled on `run_accept_loop` in server.rs):
```rust
pub async fn run_wt_accept_loop(
    endpoint: wtransport::Endpoint<Server>,
    registry: Arc<SessionRegistry>,
    limits: AuthLimits,
    shell_override: Option<String>,
    authorized: Arc<Vec<nosh_auth::NoshPublicKey>>,          // NEW
    host_signer: Arc<dyn nosh_auth::RawEd25519Signer>,       // NEW
) -> anyhow::Result<()>
```

---

### `crates/nosh-client/src/main.rs` (controller, MODIFIED)

**Analog:** Same file — WT connect path lines 1354-1374 (the `connect_wt` call site).

**Current WT connect path** (lines 1354-1374):
```rust
        #[cfg(feature = "webtransport")]
        if args.webtransport {
            // ── WebTransport connect path ──────────────────────────────────────
            let url = format!("https://{}:{}{}", args.host, args.port, args.wt_path);
            let wt_config = nosh_client::wt_transport::build_wt_client_config();
            let conn = match nosh_client::wt_transport::connect_wt(wt_config, &url).await {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!("webtransport connect failed: {e}");
                    // ... backoff / continue ...
                }
            };
```

**Pattern to INSERT after `connect_wt` returns `conn`** (based on the existing ALPN-assert pattern in `client.rs:connect` lines 210-219):
```rust
            // Inner auth on the control stream (D-02: WT path only).
            // Accept the control stream first, then run inner auth before session dispatch.
            let (mut ctrl_send, mut ctrl_recv) = match conn.open_bi().await {
                Ok(pair) => pair,
                Err(e) => { /* backoff */ }
            };
            if let Err(e) = nosh_client::inner_auth::run_inner_auth_client(
                &*conn,
                &mut *ctrl_send,
                &mut *ctrl_recv,
                &known_hosts,
                &args.host,
                identity.signer.clone(),
            ).await {
                tracing::warn!("inner auth failed: {e:#}");
                // fatal if known-host mismatch; transient otherwise
                if is_fatal_connect_error(&e) { exit_code = 1; break; }
                /* backoff / continue */
            }
            // ctrl_send / ctrl_recv are now the authenticated control stream;
            // pass them into fresh_session / reattach_session instead of opening
            // a new accept_bi() (server already opened it from its side).
```

**Note for planner:** Because the server's `handle_connection_wt` now accepts the control stream first (for inner auth), the client must `open_bi()` (not wait for `accept_bi()`). The stream is then passed into `fresh_session`/`reattach_session` instead of those functions opening a new stream internally. This requires a small signature change to the session pump entry points (or the inner-auth functions can return the control stream pair for onward use).

---

### `crates/nosh-server/src/main.rs` (config/wiring, MODIFIED)

**Analog:** Same file — Native arm lines 155-173 (loads host_key + authorized_keys, passes to endpoint builder).

**Current WT arm** (lines 175-185):
```rust
        #[cfg(feature = "webtransport")]
        TransportMode::Webtransport => {
            let cert = args.cert.context("--cert is required for --mode webtransport")?;
            let key = args.key.context("--key is required for --mode webtransport")?;
            tracing::info!(
                %addr,
                cert = %cert.display(),
                "nosh-server listening (WebTransport/HTTP3, outer TLS from operator cert)"
            );
            let endpoint = nosh_server::wt_transport::make_wt_endpoint(addr, &cert, &key)?;
            nosh_server::wt_transport::run_wt_accept_loop(endpoint, registry, limits, args.shell).await
        }
```

**Replacement (threading authorized + host_signer in):**
```rust
        #[cfg(feature = "webtransport")]
        TransportMode::Webtransport => {
            let cert = args.cert.context("--cert is required for --mode webtransport")?;
            let key = args.key.context("--key is required for --mode webtransport")?;
            // Load host key + authorized_keys for inner auth (D-07).
            let host_key_path = match args.host_key {
                Some(p) => p,
                None => default_host_key()?,
            };
            let authorized_keys_path = match args.authorized_keys {
                Some(p) => p,
                None => default_authorized_keys()?,
            };
            let host_key = nosh_auth::keys::load_host_key(&host_key_path)?;
            let host_signer: Arc<dyn nosh_auth::RawEd25519Signer> =
                Arc::new(nosh_auth::InProcessEd25519Signer::from_ssh_private(&host_key)?);
            let authorized = Arc::new(nosh_auth::load_authorized_keys(&authorized_keys_path)?);
            tracing::info!(%addr, cert = %cert.display(), "nosh-server listening (WebTransport/HTTP3)");
            let endpoint = nosh_server::wt_transport::make_wt_endpoint(addr, &cert, &key)?;
            nosh_server::wt_transport::run_wt_accept_loop(
                endpoint, registry, limits, args.shell, authorized, host_signer,
            ).await
        }
```
**Analog for `load_host_key` + `InProcessEd25519Signer::from_ssh_private` pattern:** `server.rs` `build_server_config` lines 73-89 (the native-QUIC server does the same thing).

---

## Shared Patterns

### EKM extraction — `NoshTransport::export_keying_material`

**Decision (RESEARCH.md open question 1, Claude's Discretion):** Add to `NoshTransport` trait with a default `Err` impl.

**Source to add to** `crates/nosh-proto/src/transport_trait.rs` (after `is_closed` default impl, lines 127-130):
```rust
    /// Export keying material from the TLS session (RFC 9266 / RFC 5705).
    ///
    /// Used by the inner SSH-key handshake (Phase 25, D-01) to bind the
    /// inner auth to the outer TLS session. Only the WebTransport transport
    /// implements this; native-QUIC transport can also implement it.
    ///
    /// Returns `Err` by default — transports that do not support EKM
    /// (e.g. test doubles) return an error; inner auth skips EKM-based
    /// binding on those paths.
    fn export_keying_material(
        &self,
        output: &mut [u8; 32],
        label: &[u8],
        context: &[u8],
    ) -> anyhow::Result<()> {
        let _ = (output, label, context);
        anyhow::bail!("export_keying_material not supported by this transport")
    }
```

**WebTransport impl** (in `nosh-server/src/wt_transport.rs` and `nosh-client/src/wt_transport.rs`, inside the `NoshTransport` impl block):
```rust
    fn export_keying_material(
        &self,
        output: &mut [u8; 32],
        label: &[u8],
        context: &[u8],
    ) -> anyhow::Result<()> {
        // quic_connection() is gated on the "quinn" feature — confirmed active
        // in workspace Cargo.toml:35 (features = [..., "quinn"]).
        self.0.quic_connection()
            .export_keying_material(output, label, context)
            .map_err(|e| anyhow::anyhow!("export_keying_material failed: {e:?}"))
    }
```

### write_message_ns / read_message_ns (control stream I/O)

**Source:** `crates/nosh-proto/src/transport_trait.rs` lines 222-273.

Both inner_auth modules use these for all message I/O on the control stream — the same pattern as every other session pump in the codebase:
```rust
write_message_ns(&mut *control_send, &Message::InnerAuthChallenge { ... }).await?;
let msg = read_message_ns(&mut *control_recv).await?;
```

### spawn_blocking for signing

**Source:** Pattern established in `crates/nosh-auth/src/signer.rs` (AgentSigner blocks on Unix socket I/O). Applied consistently in both `inner_auth.rs` files:
```rust
let sig: [u8; 64] = tokio::task::spawn_blocking({
    let signer = Arc::clone(&signer);
    let transcript = transcript_bytes;
    move || signer.sign(&transcript)
}).await
    .context("signing task panicked")?
    .context("signing failed")?;
```

### D-07 no-token-logging discipline

**Source:** `crates/nosh-proto/src/messages.rs` lines 395-423 (`variant_name()`).

Applies to inner auth: `client_sig` and `server_sig` bytes in `InnerAuthResponse`/`InnerAuthComplete` MUST NOT be logged. Log only `variant_name()`. New arms in `variant_name()` must NOT format sig bytes in the match arm body.

### Error pattern: close + return on auth failure

**Source:** `crates/nosh-server/src/wt_transport.rs` lines 406-412 and `server.rs` `handle_connection` lines 522-560.

All auth failure paths in the server follow the same close-and-return pattern:
```rust
conn.close(CLOSE_PROTOCOL, b"inner-auth-failed");
return Ok(());
```

Never `Err(...)` — this is a per-connection failure, not a fatal server error.

---

## NoshTransport Trait — `export_keying_material` Planner Decision Point

The research identified two options for how inner_auth functions reach `export_keying_material`:

**Option A (recommended by research):** Add `export_keying_material` to `NoshTransport` with a default `Err` impl. The WebTransport impl delegates to `quic_connection().export_keying_material(...)`. Both `run_inner_auth_server` and `run_inner_auth_client` receive `transport: &dyn NoshTransport` and call `transport.export_keying_material(...)`.

**Option B:** Pass a `&quinn::Connection` reference alongside the boxed transport. Inner_auth functions have two parameters: `transport: &dyn NoshTransport` and `quic_conn: &quinn::Connection`. Requires the call site (in `handle_connection_wt`) to hold the raw `wtransport::Connection` before boxing it into `WtransportTransport`.

Option A is cleaner and the `NoshTransport` pattern already has precedent for default-impl methods (`rtt()` returns `Duration::ZERO`, `is_closed()` returns `false` — lines 114-130 of `transport_trait.rs`). **Planner should document the choice in PLAN.md**.

---

## Transcript Layout Constants

**RESEARCH.md open question 2 (Claude's Discretion):** Define the label constants in `nosh-proto` so both `nosh-server/src/inner_auth.rs` and `nosh-client/src/inner_auth.rs` import the same values:

```rust
// In crates/nosh-proto/src/lib.rs or a new inner_auth_consts.rs:
pub const INNER_AUTH_LABEL_CLIENT: &[u8] = b"nosh-inner-auth-v1\0";
pub const INNER_AUTH_LABEL_SERVER: &[u8] = b"nosh-inner-auth-v1-server\0";
pub const INNER_AUTH_EKM_LABEL: &[u8] = b"nosh-inner-auth-v1";
pub const INNER_AUTH_EKM_CONTEXT: &[u8] = b"";
```

Alternatively, inline identical literals in both `inner_auth.rs` files (simpler, no new pub items). Either is correct; the test for EKM equality (`assert_eq!(locally_derived, ekm_from_server)`) catches a divergence.

---

## Files with No Analog (New Territory)

| File | Role | Data Flow | Reason |
|------|------|-----------|--------|
| `crates/nosh-server/src/inner_auth.rs` | service | request-response | First application-level mutual-auth handshake; closest analog is TLS-layer auth in `handle_connection` but the pattern is adapted for message-level protocol |
| `crates/nosh-client/src/inner_auth.rs` | service | request-response | Same — client side of the above; blocking TOFU prompt is also new at this layer |

Both files have strong pattern guidance from existing code (signer traits, known_hosts ops, write/read_message_ns, spawn_blocking). They are "new wiring, not new crypto" as RESEARCH.md notes.

---

## Metadata

**Analog search scope:** `crates/nosh-proto/src/`, `crates/nosh-auth/src/`, `crates/nosh-server/src/`, `crates/nosh-client/src/`
**Files read:** 10 source files
**Pattern extraction date:** 2026-06-13
