//! Phase 24 WebTransport end-to-end integration tests (WT-03, WT-05).
//!
//! Proves the Phase 24 win condition with three tests:
//!
//! - SC#1 / WT-03: a `--webtransport` client dials an in-process
//!   `--mode webtransport` server and completes a live interactive shell
//!   session (IS_TTY, hello-nosh, stty size round-trip).
//!
//! - SC#2 / WT-03: datagram state-sync flows over WebTransport —
//!   `transport.max_datagram_size()` returns `Some(_)` (D-03 capsule-adjusted)
//!   and a non-empty `StateDiff` datagram is received over the WT session.
//!
//! - SC#3 / WT-05: a raw-QUIC (native quinn) client pointed at the
//!   WebTransport-mode server FAILS to establish a session (downgrade
//!   rejection). The server never surfaces a raw-QUIC connection as a
//!   session; the native client errors or times out.
//!
//! All tests bind the server on an ephemeral port (127.0.0.1:0 — no privilege
//! needed) and use `tokio::time::timeout` so a hang fails loudly rather than
//! blocking CI.
//!
//! Build and run with:
//!   cargo test -p nosh-client --features webtransport --test webtransport

#![cfg(feature = "webtransport")]

use std::time::Duration;

use nosh_client::client;
use nosh_proto::datagram::decode_datagram;

mod common;

const SH: &str = "/bin/sh";

fn have_sh() -> bool {
    std::path::Path::new(SH).exists()
}

// ── SC#1 / WT-03: live interactive shell over WebTransport ────────────────────

/// SC#1 / WT-03: a WebTransport client completes a live PTY shell session.
///
/// Dials the in-process WT server with `connect_wt` (using the test-only
/// `with_server_certificate_hashes` path so the client trusts the self-signed
/// outer cert), opens a session, and runs a shell script that proves:
/// - stdin is a real TTY (`test -t 0 && echo IS_TTY`)
/// - shell output round-trips (`echo hello-nosh`)
/// - the PTY size was applied (`stty size` → "40 132")
#[tokio::test]
async fn wt01_live_shell_over_webtransport() {
    if !have_sh() {
        eprintln!("skipping wt01_live_shell_over_webtransport: {SH} not available");
        return;
    }

    let server = common::spawn_wt_server(Some(SH.to_string()))
        .await
        .expect("spawn_wt_server returned None — /bin/sh missing");

    // Build a WT client config that trusts the server's self-signed cert by
    // its SHA-256 hash (WebTransport certificate-hashes W3C API equivalent).
    // This is the test-only path; production clients use with_native_certs.
    let config = wtransport::ClientConfig::builder()
        .with_bind_default()
        .with_server_certificate_hashes([server.cert_hash.clone()])
        .build();

    let url = format!("https://127.0.0.1:{}/nosh", server.addr.port());
    let transport = tokio::time::timeout(
        Duration::from_secs(10),
        nosh_client::wt_transport::connect_wt(config, &url),
    )
    .await
    .expect("connect_wt timed out after 10s")
    .expect("connect_wt failed");

    // Run the shell script through the generic transport-agnostic pump.
    let script = b"test -t 0 && echo IS_TTY; echo hello-nosh; stty size; exit 0\n";
    let (out, code) = tokio::time::timeout(
        Duration::from_secs(20),
        client::run_session_collect(&*transport, "xterm-256color", 132, 40, vec![], script),
    )
    .await
    .expect("session timed out after 20s — run_session_collect hung")
    .expect("run_session_collect returned an error");

    let text = String::from_utf8_lossy(&out);

    assert!(
        text.contains("IS_TTY"),
        "stdin must be a real TTY (SC#1 / WT-03): output={text:?}"
    );
    assert!(
        text.contains("hello-nosh"),
        "shell output must round-trip (SC#1 / WT-03): output={text:?}"
    );
    assert!(
        text.contains("40 132"),
        "PTY size must be 40 rows x 132 cols (SC#1 / WT-03): output={text:?}"
    );
    assert_eq!(code, 0, "shell must exit 0 (SC#1 / WT-03)");
}

// ── SC#2 / WT-03: datagram state-sync over WebTransport ───────────────────────

/// SC#2 / WT-03: datagram transport works identically over WebTransport.
///
/// Opens a session, sends input to drive PTY output, then loops
/// `transport.read_datagram()` until a non-empty `StateDiff` datagram arrives.
/// Also asserts `max_datagram_size().is_some()` (D-03 capsule-adjusted value).
#[tokio::test]
async fn wt02_datagram_sync_over_webtransport() {
    if !have_sh() {
        eprintln!("skipping wt02_datagram_sync_over_webtransport: {SH} not available");
        return;
    }

    let server = common::spawn_wt_server(Some(SH.to_string()))
        .await
        .expect("spawn_wt_server returned None — /bin/sh missing");

    let config = wtransport::ClientConfig::builder()
        .with_bind_default()
        .with_server_certificate_hashes([server.cert_hash.clone()])
        .build();

    let url = format!("https://127.0.0.1:{}/nosh", server.addr.port());
    let transport = tokio::time::timeout(
        Duration::from_secs(10),
        nosh_client::wt_transport::connect_wt(config, &url),
    )
    .await
    .expect("connect_wt timed out")
    .expect("connect_wt failed");

    // D-03: max_datagram_size() on a WT transport returns the capsule-adjusted
    // payload size — NOT the raw quinn value. Must be Some(_).
    assert!(
        transport.max_datagram_size().is_some(),
        "max_datagram_size() must return Some(_) over WebTransport (D-03 / SC#2)"
    );

    // Open a session and send input to drive PTY output → trigger StateDiff datagrams.
    let (mut send, mut recv) =
        client::open_session(&*transport, "xterm".to_string(), 80, 24, vec![])
            .await
            .expect("open_session over WT");

    // Discard the SessionOpened frame (contains the reattach token).
    let _ = nosh_proto::read_message_ns(&mut *recv).await;

    // Send input to generate PTY output and trigger the diff-interval tick.
    client::send_input(&mut *send, b"echo wt-datagram-probe\n")
        .await
        .expect("send_input");

    // Loop: read datagrams until a StateDiff with non-empty runs arrives.
    let deadline = Duration::from_secs(8);
    let diff = loop {
        match tokio::time::timeout(deadline, transport.read_datagram()).await {
            Ok(Ok(bytes)) => match decode_datagram(&bytes) {
                Ok(d) if !d.runs.is_empty() => break d,
                Ok(_) => continue, // empty diff (no visible changes yet)
                Err(_) => continue, // unexpected non-StateDiff datagram; skip
            },
            Ok(Err(e)) => panic!("connection error waiting for datagram: {e}"),
            Err(_) => panic!("timed out after 8s waiting for a non-empty StateDiff over WebTransport (SC#2 / WT-03)"),
        }
    };

    assert!(
        diff.epoch >= 1,
        "server must have incremented epoch at least once (got epoch={}) (SC#2)",
        diff.epoch
    );

    drop(send);
    drop(recv);
}

// ── SC#3 / WT-05: raw-QUIC downgrade rejection ────────────────────────────────

/// SC#3 / WT-05: a raw-QUIC (native quinn) client CANNOT reach a session on a
/// `--mode webtransport` server.
///
/// The wtransport endpoint only surfaces `IncomingSession` after a successful
/// HTTP/3 CONNECT upgrade. A raw-QUIC client gets a protocol error at the
/// upgrade stage and never becomes a session.
///
/// The assertion is that the native `client::connect` call either returns an
/// error or that no session is reachable within a short timeout (≤ 5s).
#[tokio::test]
async fn wt03_raw_quic_downgrade_rejected() {
    let server = common::spawn_wt_server(None)
        .await
        .expect("spawn_wt_server must succeed (endpoint bind or identity generation failed)");

    // Build a native QUIC client endpoint.
    // We need a throwaway host key pair to satisfy make_endpoint's signature.
    // Use a fresh known_hosts file; the WT server does NOT perform QUIC-level SSH
    // auth, so this will fail at the transport layer before auth is relevant.
    let client_key = common::TestKey::generate();
    let dir = tempfile::tempdir().unwrap();
    let kh = dir.path().join("known_hosts");
    let endpoint = common::client_endpoint(client_key.client_identity(), kh)
        .expect("native QUIC client endpoint");

    // Attempt a native QUIC connect to the WT server's address. The WT endpoint
    // uses the WebTransport ALPN ("h3"), not "nosh/0", so the connection will fail
    // at ALPN negotiation or TLS handshake level (not a session).
    //
    // We assert: the connect either returns Err, or if it somehow completes the
    // TLS level (unlikely — the WT server presents a self-signed cert the QUIC
    // client can't verify), no usable shell session is reachable.
    //
    // Use a short timeout (4s) to make a hang fail loudly.
    let result = tokio::time::timeout(
        Duration::from_secs(4),
        client::connect(&endpoint, server.addr, common::HOST, Duration::from_secs(3)),
    )
    .await;

    match result {
        // Timeout from our outer deadline: the connection hung (never resolved).
        // This counts as "no session" — the raw-QUIC client did not get a shell.
        Err(_outer_timeout) => {
            // Expected: the WT server doesn't respond to raw-QUIC CONNECT.
        }
        // connect() returned within our deadline — but it should have errored:
        // the WT server presents the WebTransport ALPN and a self-signed cert.
        Ok(Err(_)) => {
            // Expected: connect() returned an error (ALPN mismatch, TLS failure, etc.)
        }
        // If connect() somehow succeeded, assert no shell session is reachable.
        Ok(Ok(conn)) => {
            use nosh_client::quinn_transport::QuinnTransport;
            let qt = QuinnTransport(conn.clone());
            let session_result = tokio::time::timeout(
                Duration::from_secs(3),
                client::run_session_collect(&qt, "xterm", 80, 24, vec![], b"exit 0\n"),
            )
            .await;
            match session_result {
                Err(_) | Ok(Err(_)) => {
                    // No session reached — downgrade protection holds.
                }
                Ok(Ok(_)) => {
                    panic!(
                        "WT-05 VIOLATED: a raw-QUIC client obtained a session on a \
                        --mode webtransport server (SC#3 / WT-05)"
                    );
                }
            }
            conn.close(0u32.into(), b"downgrade-test-done");
        }
    }

    endpoint.close(0u32.into(), b"done");
}
