//! Server-side inner SSH-key handshake state machine (Phase 25 / D-01..D-07).
//!
//! `run_inner_auth_server` drives the `Unauthenticated → ChallengeExchanged →
//! Authenticated` protocol (D-05 state machine) over a bidirectional control
//! stream that has already been accepted by `handle_connection_wt`. On success
//! it returns the authenticated `NoshPublicKey` (the client's Ed25519 identity).
//!
//! ## Security invariants
//!
//! - **D-01 / WT-3 channel binding**: the 32-byte RFC 9266 EKM derived from the
//!   outer TLS session is folded into both signed transcripts. A terminating proxy
//!   running the handshake on a different TLS leg derives different EKM bytes and
//!   cannot produce a matching client signature.
//! - **D-04 no-oracle**: every failure path sends the SAME fieldless
//!   `InnerAuthFail` variant and returns `Err(..)`. The CALLER (`handle_connection_wt`)
//!   closes the connection. Reason details are logged locally only (fingerprint /
//!   variant name — never nonce, signature, or key bytes).
//! - **D-05 state machine**: the client signature is verified before the
//!   `authorized_keys` lookup (Pitfall 5 fixed-order). No session frame is
//!   accepted before this function returns `Ok(key)`.
//! - **D-06 / WT-4**: the 32-byte server nonce is generated fresh via CSPRNG on
//!   every call (single-use, never cached).
//! - **Pitfall 7 / T-25-02-DOS**: the host-key signing operation is synchronous
//!   (`RawEd25519Signer::sign`). On a tokio-backed executor this MUST be wrapped
//!   in `spawn_blocking` to avoid starving other tasks on the event loop.

use std::sync::Arc;

use sha2::{Digest, Sha256};
use tracing::{info, warn};

use nosh_auth::keys::{check_authorized_key, nosh_key_from_spki, verify_ed25519_spki, ed25519_spki_der};
use nosh_auth::{NoshPublicKey, RawEd25519Signer};
use nosh_proto::transport_trait::{NoshRecvStream, NoshSendStream, NoshTransport};
use nosh_proto::{
    write_message_ns, read_message_ns, Message,
    INNER_AUTH_EKM_CONTEXT, INNER_AUTH_EKM_LABEL,
    INNER_AUTH_LABEL_CLIENT, INNER_AUTH_LABEL_SERVER,
};

/// Generate a fresh 32-byte CSPRNG nonce (D-06, WT-4).
///
/// Single-use: called once per `run_inner_auth_server` invocation. Never cached,
/// never reused.
fn csprng_nonce() -> [u8; 32] {
    let mut nonce = [0u8; 32];
    getrandom::getrandom(&mut nonce).expect("getrandom failed — OS entropy unavailable");
    nonce
}

/// Build the client-side transcript hash.
///
/// `SHA-256(INNER_AUTH_LABEL_CLIENT || ekm || server_nonce || client_nonce || server_spki)`
///
/// Field order MUST match the client-side construction exactly (Pitfall 1).
fn client_transcript(
    ekm: &[u8; 32],
    server_nonce: &[u8; 32],
    client_nonce: &[u8; 32],
    server_spki: &[u8],
) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(INNER_AUTH_LABEL_CLIENT);
    h.update(ekm);
    h.update(server_nonce);
    h.update(client_nonce);
    h.update(server_spki);
    h.finalize().into()
}

/// Build the server-side transcript hash.
///
/// `SHA-256(INNER_AUTH_LABEL_SERVER || ekm || server_nonce || client_nonce || client_spki)`
///
/// The distinct `INNER_AUTH_LABEL_SERVER` prevents cross-role transcript
/// substitution (Pitfall 3 / D-01).
fn server_transcript(
    ekm: &[u8; 32],
    server_nonce: &[u8; 32],
    client_nonce: &[u8; 32],
    client_spki: &[u8],
) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(INNER_AUTH_LABEL_SERVER);
    h.update(ekm);
    h.update(server_nonce);
    h.update(client_nonce);
    h.update(client_spki);
    h.finalize().into()
}

/// Send `InnerAuthFail` and return `Err` — the failure helper.
///
/// D-04: fieldless, uniform. The caller is responsible for closing the
/// connection after receiving `Err`. We never close the connection here.
async fn fail(
    send: &mut dyn NoshSendStream,
    reason: &str,
) -> anyhow::Error {
    // Best-effort send — ignore errors (connection may already be closing).
    let _ = write_message_ns(send, &Message::InnerAuthFail).await;
    anyhow::anyhow!("inner auth failed: {reason}")
}

/// Run the server side of the inner SSH-key mutual auth handshake (D-05).
///
/// ## State machine
///
/// ```text
/// Unauthenticated
///   → (1) export EKM
///   → (2) generate server_nonce
///   → (3) build server_spki
///   → (4) send InnerAuthChallenge
/// ChallengeExchanged
///   → (5) receive InnerAuthResponse
///   → (6) verify client signature over client_transcript
///   → (7) parse + check client_spki → NoshPublicKey
///   → (8) check_authorized_key
///   → (9) compute server_transcript
///   → (10) spawn_blocking sign → send InnerAuthComplete
/// Authenticated
///   → return Ok(client_key)
/// ```
///
/// ## Failure contract
///
/// On ANY failure: send `InnerAuthFail` (fieldless — D-04), return `Err`.
/// The caller (`handle_connection_wt`) MUST close the connection. Do not
/// close it here — the caller owns the connection lifetime.
pub(crate) async fn run_inner_auth_server(
    transport: &dyn NoshTransport,
    control_send: &mut dyn NoshSendStream,
    control_recv: &mut dyn NoshRecvStream,
    authorized: &[NoshPublicKey],
    host_signer: Arc<dyn RawEd25519Signer>,
) -> anyhow::Result<NoshPublicKey> {
    // Step 1: export 32-byte EKM from the outer TLS session (D-01 / WT-3).
    // The EKM binds the inner transcript to the exact outer TLS connection so
    // a proxy on a different TLS leg derives different EKM → client sig fails.
    let mut ekm = [0u8; 32];
    transport
        .export_keying_material(&mut ekm, INNER_AUTH_EKM_LABEL, INNER_AUTH_EKM_CONTEXT)
        .map_err(|e| anyhow::anyhow!("export_keying_material failed: {e:#}"))?;

    // Step 2: generate fresh server nonce (D-06 / WT-4 — single-use CSPRNG).
    let server_nonce = csprng_nonce();

    // Step 3: build server SPKI from the host public key.
    let server_spki = ed25519_spki_der(&host_signer.public_key32());

    // Step 4: send InnerAuthChallenge. The ekm field lets the client bind its
    // signed transcript to the outer TLS session (D-01).
    write_message_ns(
        control_send,
        &Message::InnerAuthChallenge {
            server_nonce,
            server_spki: server_spki.clone(),
            ekm,
        },
    )
    .await?;

    // Step 5: receive InnerAuthResponse.
    let (client_nonce, client_spki, client_sig) = match read_message_ns(control_recv).await {
        Ok(Message::InnerAuthResponse {
            client_nonce,
            client_spki,
            client_sig,
        }) => {
            // Validate client_sig length (must be exactly 64 bytes — Vec<u8> in wire).
            if client_sig.len() != 64 {
                return Err(fail(control_send, "client_sig wrong length").await);
            }
            (client_nonce, client_spki, client_sig)
        }
        Ok(other) => {
            // Wrong frame type — expected InnerAuthResponse, got something else.
            // Log only the variant name (D-07 no-token-logging).
            warn!(
                frame = other.variant_name(),
                "inner auth: expected InnerAuthResponse as first frame"
            );
            return Err(fail(control_send, "unexpected first frame").await);
        }
        Err(e) => {
            warn!(error = %e, "inner auth: failed to read InnerAuthResponse");
            return Err(fail(control_send, "read error").await);
        }
    };

    // Step 6: verify client signature BEFORE authorized_keys lookup (Pitfall 5 —
    // fixed verification order prevents timing oracle on key existence).
    let client_transcript_hash = client_transcript(&ekm, &server_nonce, &client_nonce, &server_spki);

    // client_sig is exactly 64 bytes (validated above).
    let sig_arr: [u8; 64] = client_sig
        .as_slice()
        .try_into()
        .expect("length already validated to be 64");

    if !verify_ed25519_spki(&client_spki, &client_transcript_hash, &sig_arr) {
        // D-04: same InnerAuthFail for sig failure and key-not-found.
        return Err(fail(control_send, "client sig verify failed").await);
    }

    // Step 7: parse client SPKI into a NoshPublicKey.
    let client_key = match nosh_key_from_spki(&client_spki) {
        Some(k) => k,
        None => {
            return Err(fail(control_send, "client_spki parse failed").await);
        }
    };

    // Step 8: check client key against authorized_keys.
    if !check_authorized_key(&client_key, authorized) {
        // D-04: same InnerAuthFail as sig failure.
        return Err(fail(control_send, "client key not in authorized_keys").await);
    }

    // Log only the fingerprint (D-07 — never log key bytes, nonce, or sig).
    info!(
        client_fp = %client_key.fingerprint(),
        "inner auth: client key authorized; signing server transcript"
    );

    // Step 9: compute server_transcript.
    let server_transcript_hash =
        server_transcript(&ekm, &server_nonce, &client_nonce, &client_spki);

    // Step 10: sign the server transcript in a spawn_blocking task (Pitfall 7 /
    // T-25-02-DOS — RawEd25519Signer::sign is synchronous; blocking the tokio
    // event loop is not acceptable on a busy server).
    let signer = Arc::clone(&host_signer);
    let server_sig = tokio::task::spawn_blocking(move || signer.sign(&server_transcript_hash))
        .await
        .map_err(|e| anyhow::anyhow!("spawn_blocking join error: {e:#}"))??;

    // Send InnerAuthComplete with the server's signature.
    write_message_ns(
        control_send,
        &Message::InnerAuthComplete {
            server_sig: server_sig.to_vec(),
        },
    )
    .await?;

    // Authenticated: return the client's identity.
    info!(
        client_fp = %client_key.fingerprint(),
        "inner auth: mutual authentication complete"
    );
    Ok(client_key)
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use nosh_auth::InProcessEd25519Signer;

    /// Helper: generate a fresh Ed25519 signer for tests.
    ///
    /// `InProcessEd25519Signer::generate()` is `#[cfg(test)]`-gated in nosh-auth,
    /// so we replicate the same seed-from-getrandom pattern directly here.
    fn make_signer() -> Arc<dyn RawEd25519Signer> {
        let mut seed = [0u8; 32];
        getrandom::getrandom(&mut seed).expect("getrandom");
        let key = ed25519_dalek::SigningKey::from_bytes(&seed);
        Arc::new(InProcessEd25519Signer::new(key))
    }

    /// TC-01: transcript byte order is deterministic.
    ///
    /// Two calls with the same inputs must produce identical hashes.
    #[test]
    fn transcript_is_deterministic() {
        let ekm = [0xaau8; 32];
        let sn = [0x01u8; 32];
        let cn = [0x02u8; 32];
        let spki = vec![0x03u8; 44];

        let h1 = client_transcript(&ekm, &sn, &cn, &spki);
        let h2 = client_transcript(&ekm, &sn, &cn, &spki);
        assert_eq!(h1, h2, "transcript must be deterministic for identical inputs");
    }

    /// TC-02: EKM sensitivity — changing the EKM byte yields a different hash.
    ///
    /// This is the core WT-3 invariant: a proxy deriving different EKM from a
    /// different outer TLS session will see a different client_transcript and
    /// the client signature will NOT verify. This test is the unit-level proof
    /// that the EKM is actually mixed into the hash.
    #[test]
    fn transcript_sensitive_to_ekm() {
        let ekm_a = [0xaau8; 32];
        let ekm_b = [0xbbu8; 32];
        let sn = [0x01u8; 32];
        let cn = [0x02u8; 32];
        let spki = vec![0x03u8; 44];

        let h_a = client_transcript(&ekm_a, &sn, &cn, &spki);
        let h_b = client_transcript(&ekm_b, &sn, &cn, &spki);
        assert_ne!(
            h_a, h_b,
            "transcript must differ when EKM differs (WT-3 channel binding)"
        );
    }

    /// TC-03: client and server transcripts are distinct labels.
    ///
    /// A server_transcript signature MUST NOT verify against client_transcript
    /// because the domain-separation labels differ (Pitfall 3 / D-01).
    #[test]
    fn client_and_server_transcripts_differ() {
        let ekm = [0xaau8; 32];
        let sn = [0x01u8; 32];
        let cn = [0x02u8; 32];
        let spki = vec![0x03u8; 44];

        let h_client = client_transcript(&ekm, &sn, &cn, &spki);
        let h_server = server_transcript(&ekm, &sn, &cn, &spki);
        assert_ne!(
            h_client, h_server,
            "client and server transcripts must differ (cross-role transcript attack prevention)"
        );
    }

    /// TC-04: transcript is sensitive to nonce and SPKI fields.
    ///
    /// A change to server_nonce, client_nonce, or spki must change the hash.
    #[test]
    fn transcript_sensitive_to_all_fields() {
        let ekm = [0xaau8; 32];
        let sn = [0x01u8; 32];
        let cn = [0x02u8; 32];
        let spki = vec![0x03u8; 44];

        let base = client_transcript(&ekm, &sn, &cn, &spki);

        // Different server_nonce.
        let mut sn2 = sn;
        sn2[0] ^= 0xff;
        assert_ne!(base, client_transcript(&ekm, &sn2, &cn, &spki), "server_nonce change must change hash");

        // Different client_nonce.
        let mut cn2 = cn;
        cn2[0] ^= 0xff;
        assert_ne!(base, client_transcript(&ekm, &sn, &cn2, &spki), "client_nonce change must change hash");

        // Different SPKI.
        let mut spki2 = spki.clone();
        spki2[12] ^= 0xff;
        assert_ne!(base, client_transcript(&ekm, &sn, &cn, &spki2), "spki change must change hash");
    }

    /// TC-05: csprng_nonce produces 32 bytes and two calls produce different results.
    ///
    /// The probability of a collision is 2^-256 — practically zero.
    #[test]
    fn csprng_nonce_unique() {
        let n1 = csprng_nonce();
        let n2 = csprng_nonce();
        // Two successive nonces must not be equal (birthday bound is negligible at 32 bytes).
        assert_ne!(n1, n2, "successive CSPRNG nonces must differ");
        assert_eq!(n1.len(), 32, "nonce must be 32 bytes");
    }

    /// TC-06: the `server_transcript` hash uses `client_spki`, not `server_spki`.
    ///
    /// Verifies that `server_transcript` mixes the client's SPKI (not the server's)
    /// as per the wire-format spec.
    #[test]
    fn server_transcript_uses_client_spki() {
        let ekm = [0xaau8; 32];
        let sn = [0x01u8; 32];
        let cn = [0x02u8; 32];
        let client_spki = vec![0x04u8; 44]; // distinct from server_spki below
        let server_spki = vec![0x05u8; 44];

        let h_with_client = server_transcript(&ekm, &sn, &cn, &client_spki);
        let h_with_server = server_transcript(&ekm, &sn, &cn, &server_spki);
        // Different SPKIs → different server transcripts.
        assert_ne!(
            h_with_client, h_with_server,
            "server_transcript must mix the supplied SPKI (client's SPKI per spec)"
        );
    }

    /// TC-07: full happy-path signing round-trip over the transcript.
    ///
    /// Simulate the server transcript sign + verify cycle that run_inner_auth_server
    /// performs. Proves the crypto plumbing is correct before the wire tests.
    #[test]
    fn server_transcript_sign_verify_roundtrip() {
        let signer = make_signer();
        let ekm = [0x11u8; 32];
        let server_nonce = csprng_nonce();
        let client_nonce = csprng_nonce();
        let client_spki = ed25519_spki_der(&[0x22u8; 32]);

        let tx_hash = server_transcript(&ekm, &server_nonce, &client_nonce, &client_spki);
        let sig = signer.sign(&tx_hash).expect("sign must succeed");

        let server_spki = ed25519_spki_der(&signer.public_key32());
        assert!(
            verify_ed25519_spki(&server_spki, &tx_hash, &sig),
            "server transcript signature must verify with the server's SPKI"
        );
    }
}
