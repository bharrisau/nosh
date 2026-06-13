//! WebTransport inner SSH-key mutual authentication — client side (Phase 25,
//! Plan 03).
//!
//! [`run_inner_auth_client`] runs the 10-step handshake over the control stream
//! that the client opens right after `connect_wt` returns:
//!
//! 1. Opens a bidi control stream (`conn.open_bi()`).
//! 2. Derives the local 32-byte RFC 9266 EKM binding from the outer TLS session.
//! 3. Reads `InnerAuthChallenge { server_nonce, server_spki, ekm }` from the
//!    server.
//! 4. Asserts `ekm_from_server == locally_derived_ekm` (D-01 binding check).
//!    Fails BEFORE any signing if the EKM does not match — a proxy on a
//!    different TLS leg cannot pass this check.
//! 5. Parses `server_key` from `server_spki` (None → Err).
//! 6. TOFU: looks up `host` in `known_hosts`; if present, asserts equality
//!    (hard error on mismatch, no prompt); if absent, calls
//!    [`prompt_tofu_or_fail`] — fails closed on no-TTY or declined input.
//! 7. Generates a 32-byte CSPRNG `client_nonce`.
//! 8. Builds `client_spki` from the signer's public key.
//! 9. Signs `SHA-256(INNER_AUTH_LABEL_CLIENT || ekm || server_nonce ||
//!    client_nonce || server_spki)` via `spawn_blocking` (ssh-agent is blocking).
//! 10. Sends `InnerAuthResponse`, reads `InnerAuthComplete { server_sig }`,
//!     verifies the server signature over
//!     `SHA-256(INNER_AUTH_LABEL_SERVER || ekm || server_nonce || client_nonce ||
//!     client_spki)`.
//!
//! Returns `Ok((send, recv))` — the SAME authenticated control stream pair,
//! ready for the caller to use for `SessionOpen` / `Reattach` dispatch.
//!
//! # Security properties
//!
//! - EKM binding (D-01): ensures the signed transcript is tied to THIS
//!   specific outer TLS session. A relay/proxy cannot transplant the handshake
//!   to a different TLS session.
//! - Mutual auth: both the server's host key and the client's identity key are
//!   verified. No session proceeds without BOTH keys authenticating.
//! - TOFU / known_hosts (SEC-02, D-08): first-contact shows a blocking prompt;
//!   subsequent contacts verify the pinned key. Empty input and no-TTY fail
//!   closed (D-10).
//! - No-PTY output before TOFU resolved (D-08): [`prompt_tofu_or_fail`] writes
//!   only to `stderr`.

use std::path::Path;
use std::sync::Arc;

use anyhow::Context;
use sha2::{Digest, Sha256};

use nosh_auth::keys::{
    ed25519_spki_der, lookup_known_host, nosh_key_from_spki, record_known_host, verify_ed25519_spki,
};
use nosh_auth::signer::RawEd25519Signer;
use nosh_proto::transport_trait::{NoshRecvStream, NoshSendStream};
use nosh_proto::{
    read_message_ns, write_message_ns, Message, INNER_AUTH_EKM_CONTEXT, INNER_AUTH_EKM_LABEL,
    INNER_AUTH_LABEL_CLIENT, INNER_AUTH_LABEL_SERVER,
};

/// Generate a 32-byte CSPRNG nonce. Panics if `getrandom` fails (OS entropy
/// source unavailable — treated as an unrecoverable system error).
fn csprng_nonce() -> [u8; 32] {
    let mut nonce = [0u8; 32];
    getrandom::getrandom(&mut nonce).expect("getrandom failed");
    nonce
}

/// Build the client-signed transcript:
/// `SHA-256(INNER_AUTH_LABEL_CLIENT || ekm || server_nonce || client_nonce ||
/// server_spki)`.
///
/// Both sides reconstruct this hash independently; any field-order divergence
/// would cause the server's verification to fail (Pitfall 3 guard).
fn transcript_client_signs(
    ekm: &[u8; 32],
    server_nonce: &[u8; 32],
    client_nonce: &[u8; 32],
    server_spki: &[u8],
) -> [u8; 32] {
    Sha256::new()
        .chain_update(INNER_AUTH_LABEL_CLIENT)
        .chain_update(ekm)
        .chain_update(server_nonce)
        .chain_update(client_nonce)
        .chain_update(server_spki)
        .finalize()
        .into()
}

/// Build the server-signed transcript (client verifies):
/// `SHA-256(INNER_AUTH_LABEL_SERVER || ekm || server_nonce || client_nonce ||
/// client_spki)`.
fn transcript_server_signs(
    ekm: &[u8; 32],
    server_nonce: &[u8; 32],
    client_nonce: &[u8; 32],
    client_spki: &[u8],
) -> [u8; 32] {
    Sha256::new()
        .chain_update(INNER_AUTH_LABEL_SERVER)
        .chain_update(ekm)
        .chain_update(server_nonce)
        .chain_update(client_nonce)
        .chain_update(client_spki)
        .finalize()
        .into()
}

/// Run the inner SSH-key mutual authentication handshake (client side).
///
/// Opens a control stream, performs the 10-step mutual auth protocol, and
/// returns the authenticated `(send, recv)` stream pair on success. The
/// caller uses this pair for all subsequent `SessionOpen` / `Reattach`
/// dispatch — no second stream is opened.
///
/// # Arguments
///
/// - `conn` — the WebTransport connection; used to open the control stream and
///   to derive the RFC 9266 EKM binding.
/// - `known_hosts` — path to the `known_hosts` file (client-side TOFU store).
/// - `host` — the server hostname (used as the lookup/record key in
///   `known_hosts`; should match the SNI).
/// - `client_signer` — the client's Ed25519 signing key (may be an
///   `AgentSigner` — blocking, so signing is wrapped in `spawn_blocking`).
///
/// # Errors
///
/// Returns `Err` on:
/// - Any transport error (stream open, read, write).
/// - EKM mismatch (`ekm_from_server != locally_derived_ekm`).
/// - Malformed `server_spki` (cannot parse to `NoshPublicKey`).
/// - Known-host mismatch (pinned key differs from presented key).
/// - TOFU declined or no-TTY (D-08 / D-10).
/// - Client signing failure.
/// - Server-completion signature verification failure.
/// - Server sent `InnerAuthFail` or an unexpected message variant.
pub async fn run_inner_auth_client(
    conn: &dyn nosh_proto::transport_trait::NoshTransport,
    known_hosts: &Path,
    host: &str,
    client_signer: Arc<dyn RawEd25519Signer>,
) -> anyhow::Result<(Box<dyn NoshSendStream>, Box<dyn NoshRecvStream>)> {
    // Step 1: open the control bidi stream.
    // The server calls `accept_bi()` on this same stream from its side.
    let (mut send, mut recv) = conn
        .open_bi()
        .await
        .context("open inner-auth control stream")?;

    // Step 2: derive the local RFC 9266 EKM binding from the outer TLS session.
    // Both client and server derive identical bytes (same TLS session, same
    // label+context+length). A proxy on a different TLS leg gets different bytes
    // and the challenge's `ekm` field will not match → connection refused before
    // any signature is produced (D-01).
    let mut ekm = [0u8; 32];
    conn.export_keying_material(&mut ekm, INNER_AUTH_EKM_LABEL, INNER_AUTH_EKM_CONTEXT)
        .context("derive RFC 9266 EKM for inner-auth binding")?;

    // Step 3: receive the server's challenge.
    let (server_nonce, server_spki_vec, ekm_from_server) =
        match read_message_ns(&mut *recv).await? {
            Message::InnerAuthChallenge {
                server_nonce,
                server_spki,
                ekm: ekm_from_server,
            } => (server_nonce, server_spki, ekm_from_server),
            other => {
                anyhow::bail!(
                    "inner auth: expected InnerAuthChallenge, got {}",
                    other.variant_name()
                );
            }
        };

    // Step 4: D-01 binding check — client verifies the server's EKM matches
    // the locally-derived EKM. Fails BEFORE any signing.
    // A transparent proxy on a different TLS leg cannot forge this check.
    if ekm_from_server != ekm {
        anyhow::bail!(
            "inner auth: EKM mismatch (D-01 binding check failed) — \
             possible transparent proxy on a different TLS session"
        );
    }

    // Step 5: parse the server's SPKI.
    let server_key = nosh_key_from_spki(&server_spki_vec)
        .ok_or_else(|| anyhow::anyhow!("inner auth: server SPKI is malformed or not Ed25519"))?;

    // Step 6: TOFU / known_hosts check.
    match lookup_known_host(known_hosts, host)
        .with_context(|| format!("known_hosts lookup for {host}"))?
    {
        Some(pinned) => {
            // Hard error on mismatch — same semantics as HostKeyVerifier (no prompt).
            if pinned != server_key {
                anyhow::bail!(
                    "inner auth: host key mismatch for {host} — \
                     known_hosts pins a different key (aborting; possible MITM)"
                );
            }
            // Pinned key matches presented key — continue.
        }
        None => {
            // First contact: show the blocking TOFU prompt (D-08).
            let accepted = prompt_tofu_or_fail(host, &server_key.fingerprint())
                .context("TOFU prompt for inner auth")?;
            if !accepted {
                anyhow::bail!(
                    "inner auth: host key for {host} not accepted; connection declined"
                );
            }
            record_known_host(known_hosts, host, &server_key)
                .with_context(|| format!("record known host {host}"))?;
        }
    }

    // Step 7: generate a 32-byte CSPRNG client nonce.
    let client_nonce = csprng_nonce();

    // Step 8: build client SPKI DER from the signer's public key.
    let client_spki_vec = ed25519_spki_der(&client_signer.public_key32());

    // Step 9: sign the EKM-bound client transcript.
    // Transcript = SHA-256(INNER_AUTH_LABEL_CLIENT || ekm || server_nonce ||
    //              client_nonce || server_spki).
    // Wrap in spawn_blocking: AgentSigner is synchronous (D-07 / Pitfall 7).
    let transcript = transcript_client_signs(&ekm, &server_nonce, &client_nonce, &server_spki_vec);
    let signer_clone = Arc::clone(&client_signer);
    let client_sig: [u8; 64] = tokio::task::spawn_blocking(move || signer_clone.sign(&transcript))
        .await
        .context("client signing task panicked")?
        .context("client signing failed")?;

    // Send the response.
    // Message::InnerAuthResponse::client_sig is Vec<u8> (postcard can't encode [u8;64]).
    write_message_ns(
        &mut *send,
        &Message::InnerAuthResponse {
            client_nonce,
            client_spki: client_spki_vec.clone(),
            client_sig: client_sig.to_vec(),
        },
    )
    .await
    .context("send InnerAuthResponse")?;

    // Step 10: read the server's completion message.
    let server_sig_vec = match read_message_ns(&mut *recv).await? {
        Message::InnerAuthComplete { server_sig } => server_sig,
        Message::InnerAuthFail => {
            anyhow::bail!("inner auth: server rejected our credentials (InnerAuthFail)");
        }
        other => {
            anyhow::bail!(
                "inner auth: expected InnerAuthComplete or InnerAuthFail, got {}",
                other.variant_name()
            );
        }
    };

    // Validate the server sig is exactly 64 bytes (Message field is Vec<u8>).
    let server_sig: &[u8; 64] = server_sig_vec
        .as_slice()
        .try_into()
        .map_err(|_| anyhow::anyhow!("inner auth: server_sig length is not 64 bytes"))?;

    // Verify the server's completion signature over:
    // SHA-256(INNER_AUTH_LABEL_SERVER || ekm || server_nonce || client_nonce ||
    //         client_spki).
    let server_transcript =
        transcript_server_signs(&ekm, &server_nonce, &client_nonce, &client_spki_vec);
    if !verify_ed25519_spki(&server_spki_vec, &server_transcript, server_sig) {
        anyhow::bail!(
            "inner auth: server completion signature verification failed \
             (server cannot prove ownership of the host key)"
        );
    }

    // Mutual auth complete. Return the authenticated control stream pair.
    // The caller uses send/recv for SessionOpen / Reattach dispatch — no
    // second stream is opened (D-05 state machine: Authenticated state).
    Ok((send, recv))
}

/// Show the OpenSSH-style blocking TOFU prompt for an unknown server host key.
///
/// Writes to `stderr` only — no PTY/stdout output until resolved (D-08).
///
/// # No-TTY fail-closed (D-10)
///
/// Before prompting, checks `std::io::stdin().is_terminal()`. In a
/// non-interactive context (piped input, CI, automation) the function returns
/// `Ok(false)` immediately without prompting, printing the fingerprint and a
/// reference to the future `--trust-key` flag.
///
/// # Accepted input
///
/// Only the literal string `yes` (after trimming whitespace) is accepted.
/// Empty input, `no`, any other string, or EOF all return `Ok(false)`.
///
/// The test helper [`parse_yes`] is provided separately so unit tests can
/// exercise the yes/no parsing without a TTY.
///
/// This function now delegates to the shared `nosh_auth::keys::prompt_host_key_accept`
/// to maintain a single canonical TOFU prompt implementation (WR-02).
pub fn prompt_tofu_or_fail(host: &str, fingerprint: &str) -> anyhow::Result<bool> {
    nosh_auth::keys::prompt_host_key_accept(host, fingerprint)
}

/// Parse a yes/no prompt response.
///
/// Returns `true` only for the literal string `"yes"` (after trimming
/// whitespace). Empty strings, `"no"`, or anything else returns `false`.
///
/// Extracted as a separate function so unit tests can exercise the parse
/// logic without a TTY (D-08 acceptance criterion: `prompt_tofu_rejects_empty_input`).
///
/// This function now delegates to the shared `nosh_auth::keys::parse_yes`
/// to maintain a single canonical parse implementation (WR-02).
pub fn parse_yes(input: &str) -> bool {
    nosh_auth::keys::parse_yes(input)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── D-08 / SEC-02: prompt parse tests ─────────────────────────────────────

    /// Empty input MUST NOT be accepted as "yes" (D-08 / SEC-02 TOFU fatigue guard).
    #[test]
    fn prompt_tofu_rejects_empty_input() {
        assert!(!parse_yes(""), "empty string must NOT be accepted");
        assert!(!parse_yes("  "), "whitespace-only must NOT be accepted");
        assert!(!parse_yes("\t"), "tab must NOT be accepted");
    }

    #[test]
    fn prompt_tofu_accepts_only_yes() {
        assert!(parse_yes("yes"), "literal 'yes' must be accepted");
        assert!(parse_yes("  yes  "), "trimmed 'yes' must be accepted");
    }

    #[test]
    fn prompt_tofu_rejects_non_yes() {
        for input in &["no", "YES", "Yes", "y", "ok", "true", "1", "affirmative"] {
            assert!(!parse_yes(input), "'{input}' must NOT be accepted");
        }
    }

    // ── Transcript-order determinism test ─────────────────────────────────────
    //
    // Verifies that the transcript helper functions produce stable, order-dependent
    // output. Swapping any two fields must change the hash (Pitfall 3 guard).

    /// Transcript with swapped nonce order must produce a DIFFERENT hash.
    #[test]
    fn transcript_client_signs_is_order_sensitive() {
        let ekm = [1u8; 32];
        let server_nonce = [2u8; 32];
        let client_nonce = [3u8; 32];
        let server_spki = vec![4u8; 44];

        let t1 = transcript_client_signs(&ekm, &server_nonce, &client_nonce, &server_spki);
        // Swap server_nonce and client_nonce — must produce a different hash.
        let t2 = transcript_client_signs(&ekm, &client_nonce, &server_nonce, &server_spki);
        assert_ne!(
            t1, t2,
            "swapping nonce order must change the client-signed transcript hash"
        );
    }

    /// Server transcript with swapped nonce order must produce a DIFFERENT hash.
    #[test]
    fn transcript_server_signs_is_order_sensitive() {
        let ekm = [1u8; 32];
        let server_nonce = [2u8; 32];
        let client_nonce = [3u8; 32];
        let client_spki = vec![5u8; 44];

        let t1 = transcript_server_signs(&ekm, &server_nonce, &client_nonce, &client_spki);
        let t2 = transcript_server_signs(&ekm, &client_nonce, &server_nonce, &client_spki);
        assert_ne!(
            t1, t2,
            "swapping nonce order must change the server-signed transcript hash"
        );
    }

    /// Client and server transcripts must produce DIFFERENT hashes (domain separation).
    ///
    /// If the labels were the same, a client-transcript signature could be
    /// replayed as a server-transcript signature (cross-role confusion attack).
    #[test]
    fn client_and_server_transcripts_are_distinct() {
        let ekm = [1u8; 32];
        let server_nonce = [2u8; 32];
        let client_nonce = [3u8; 32];
        let shared_spki = vec![6u8; 44]; // same bytes to isolate the label difference

        let client_transcript =
            transcript_client_signs(&ekm, &server_nonce, &client_nonce, &shared_spki);
        let server_transcript =
            transcript_server_signs(&ekm, &server_nonce, &client_nonce, &shared_spki);

        assert_ne!(
            client_transcript, server_transcript,
            "client and server transcripts must differ (distinct domain-separation labels)"
        );
    }

    /// Changing the EKM must change both transcript hashes (channel-binding test).
    #[test]
    fn different_ekm_produces_different_transcripts() {
        let ekm1 = [0xAAu8; 32];
        let ekm2 = [0xBBu8; 32];
        let server_nonce = [2u8; 32];
        let client_nonce = [3u8; 32];
        let server_spki = vec![4u8; 44];

        assert_ne!(
            transcript_client_signs(&ekm1, &server_nonce, &client_nonce, &server_spki),
            transcript_client_signs(&ekm2, &server_nonce, &client_nonce, &server_spki),
            "different EKM must produce different client transcript"
        );
        let client_spki = vec![5u8; 44];
        assert_ne!(
            transcript_server_signs(&ekm1, &server_nonce, &client_nonce, &client_spki),
            transcript_server_signs(&ekm2, &server_nonce, &client_nonce, &client_spki),
            "different EKM must produce different server transcript"
        );
    }
}
