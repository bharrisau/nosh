#![no_main]
//! Raw QUIC-packet fuzzer (D-02).
//!
//! Feeds arbitrary/malformed/oversized/truncated UDP bytes into quinn-proto's
//! sans-IO [`Endpoint::handle`] in-process.  No network socket, no subprocess —
//! deterministic and reproducible.
//!
//! # What we are testing
//!
//! Our `EndpointConfig::default()` + `migration(true)` wiring must degrade
//! gracefully on hostile input.  quinn-proto internals are treated as a
//! maintained black box: a panic *inside* quinn-proto is an upstream issue to
//! report; a panic in OUR config-construction or dispatch path is ours to fix.
//!
//! # Invariant
//!
//! `Endpoint::handle` on arbitrary bytes must never panic or OOM.  Returning
//! `None` (silently dropped) or `Some(DatagramEvent::Response { .. })` (a
//! Version-Negotiation or Stateless-Reset reply) are both fine.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use bytes::BytesMut;
use libfuzzer_sys::fuzz_target;
use quinn_proto::{Endpoint, EndpointConfig, ServerConfig as QpServerConfig};

// ────────────────────────────────────────────────────────────────────────────
// Minimal server config (no key files, no authorized_keys)
// ────────────────────────────────────────────────────────────────────────────

/// Build an ephemeral self-signed TLS server config that mirrors the production
/// rustls→quinn wiring without requiring any on-disk key material.
///
/// Deliberately does NOT use `nosh_server::build_server_config` — that function
/// requires a host-key file and an authorized-keys file (RESEARCH Open
/// Question 3 resolution).  Our goal here is to validate that
/// `EndpointConfig::default()` + `migration(true)` do not misuse the API in
/// a way that causes panics on hostile input, NOT to fuzz the TLS auth chain.
fn make_minimal_server_config() -> Arc<QpServerConfig> {
    // Install the ring crypto provider (idempotent — ignore the already-installed error).
    let _ = rustls::crypto::ring::default_provider().install_default();

    // Ephemeral self-signed cert — no real keys or disk files needed.
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()])
        .expect("rcgen self-signed cert generation must not fail");
    let cert_der =
        rustls::pki_types::CertificateDer::from(cert.cert.der().to_vec());
    let key_der = rustls::pki_types::PrivateKeyDer::try_from(
        cert.signing_key.serialize_der(),
    )
    .expect("DER private key conversion must not fail");

    let mut rustls_cfg = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key_der)
        .expect("rustls ServerConfig construction must not fail");

    // Mirror production ALPN (server.rs line 86).
    rustls_cfg.alpn_protocols = vec![nosh_proto::ALPN.to_vec()];

    let quic_crypto = quinn::crypto::rustls::QuicServerConfig::try_from(rustls_cfg)
        .expect("QuicServerConfig conversion must not fail");

    let mut server_config = QpServerConfig::with_crypto(Arc::new(quic_crypto));

    // Mirror production transport config (server.rs line 91): datagram buffers,
    // idle timeout.  quinn::TransportConfig is a re-export of
    // quinn_proto::TransportConfig, so this composes cleanly.
    server_config.transport_config(Arc::new(nosh_proto::transport_config(false)));

    // D-02: MUST mirror production migration(true) (server.rs line 98).
    // This is the config misuse we are specifically validating — connection
    // migration must not affect the pre-auth anti-amplification posture.
    server_config.migration(true);

    Arc::new(server_config)
}

// ────────────────────────────────────────────────────────────────────────────
// Endpoint construction
// ────────────────────────────────────────────────────────────────────────────

/// Construct the sans-IO [`Endpoint`] once per fuzz process.
///
/// - `allow_mtud = false` — deterministic, no MTU probing.
/// - `rng_seed = Some([0u8; 32])` — fixed PRNG seed for reproducible crashes
///   (RESEARCH assumption A8; confirmed by quinn-proto docs).
fn make_server_endpoint() -> Endpoint {
    let endpoint_cfg = Arc::new(EndpointConfig::default());
    let server_cfg = make_minimal_server_config();
    Endpoint::new(
        endpoint_cfg,
        Some(server_cfg),
        false,           // allow_mtud: false for determinism
        Some([0u8; 32]), // rng_seed: deterministic for reproducible crashes
    )
}

// ────────────────────────────────────────────────────────────────────────────
// Fuzz target
// ────────────────────────────────────────────────────────────────────────────

fuzz_target!(|data: &[u8]| {
    // Build the endpoint once per fuzz process (construction is expensive;
    // per-iteration construction would collapse coverage throughput).
    // RESEARCH Pitfall 5: state accumulation across iterations is acceptable —
    // our goal is config-misuse detection, not state-machine coverage.
    static ENDPOINT: OnceLock<Mutex<Endpoint>> = OnceLock::new();
    let endpoint = ENDPOINT.get_or_init(|| Mutex::new(make_server_endpoint()));

    let remote: SocketAddr = "127.0.0.1:12345".parse().unwrap();
    let now = Instant::now();
    let mut buf = Vec::new();

    let mut ep = endpoint.lock().unwrap();

    // Feed arbitrary bytes as a raw UDP packet.
    // Invariant: no panic, no OOM.
    // - None → silently dropped (expected for malformed/short packets)
    // - Some(DatagramEvent::Response { .. }) → Version-Negotiation or
    //   Stateless-Reset; we discard it without further processing
    // - Some(DatagramEvent::NewConnection(incoming)) → release via ep.ignore() so
    //   the endpoint's Incoming slab / CID index does not grow across iterations
    //   (WR-01: dropping the Incoming leaks half-open state; quinn warns it "may
    //   cause memory leak and eventual inability to accept new connections").
    // - Some(DatagramEvent::ConnectionEvent { .. }) → routed to an existing
    //   connection handle; none are open, so this is unreachable in practice
    //
    // NEVER .unwrap() the result — only a panic or OOM fails the fuzzer.
    let pkt = BytesMut::from(data);
    if let Some(quinn_proto::DatagramEvent::NewConnection(incoming)) =
        ep.handle(now, remote, None, None, pkt, &mut buf)
    {
        let _ = ep.ignore(incoming);
    }
    // `buf` may contain a response (Version-Negotiation, Stateless-Reset).
    // We drain it to avoid unbounded growth across iterations.
    buf.clear();
});
