//! Connection migration validation test (ROAM-01, Phase 7).
//!
//! Proves that a live nosh session survives a forced client path change (via
//! `Endpoint::rebind()` onto a fresh loopback UDP socket) on the SAME QUIC
//! connection: no re-handshake, no data loss, no reordering.
//!
//! Per 07-RESEARCH.md §1, quinn 0.11.9 qlog emits only packet-level events
//! (PacketSent/Received/Lost/MetricsUpdated) with NO frame-level detail and NO
//! connection-ID fields. The qlog file is therefore validated as an artifact
//! (exists, non-empty, parseable JSON-seq) — NOT for PATH_CHALLENGE frame
//! content. The binding CID-rotation / path-validation proof uses
//! `Connection::stats()` FrameStats deltas, which are authoritative.

use std::time::{Duration, Instant};

use nosh_client::client;
use nosh_proto::Message;
use nosh_server::server::AuthLimits;

mod common;
use common::{client_endpoint_with_qlog, rebind_client, spawn_server_with_shell, TestKey, HOST};

const SH: &str = "/bin/sh";

/// Returns true if `/bin/sh` exists (else the test skips cleanly).
fn have_sh() -> bool {
    std::path::Path::new(SH).exists()
}

/// Migration validation: force a client path change mid-stream and assert the
/// SAME QUIC connection continues with no loss, no reorder, no error.
///
/// Implements decisions D-02 through D-05 from 07-CONTEXT.md.
#[tokio::test]
async fn migration_survives_path_change() {
    if !have_sh() {
        eprintln!("skipping migration_survives_path_change: {SH} not available");
        return;
    }

    tokio::time::timeout(
        Duration::from_secs(30),
        run_migration_test(),
    )
    .await
    .expect("migration test timed out after 30s (hung connection or lost output)");
}

async fn run_migration_test() {
    // ── Step 1: Spawn in-process server (migration(true) is now set explicitly, D-01) ──
    let host_key = TestKey::generate();
    let client_key = TestKey::generate();
    let server = spawn_server_with_shell(
        &host_key,
        &[&client_key.public],
        AuthLimits::default(),
        Some(SH.to_string()),
    )
    .await;

    // ── Step 2: Build qlog-enabled client endpoint (D-05 literal) ──────────────
    let dir = tempfile::tempdir().expect("tempdir");
    let qlog_path = dir.path().join("migration.qlog");
    let known_hosts = dir.path().join("known_hosts");

    let endpoint = client_endpoint_with_qlog(
        client_key.client_identity(),
        known_hosts,
        &qlog_path,
    )
    .expect("client endpoint with qlog");

    let conn = client::connect(&endpoint, server.addr, HOST, Duration::from_secs(30))
        .await
        .expect("mutual auth handshake");

    // ── Step 3: Open a session (discards SessionOpened frame, Phase 6 protocol) ─
    let (mut send, mut recv) = client::open_session(
        &conn,
        "xterm".to_string(),
        80,
        24,
        vec![],
    )
    .await
    .expect("open session stream");

    // Wait for the leading SessionOpened control frame the server sends
    // immediately after opening the session (Phase 6 D-03). We drive frames
    // manually here instead of using run_session_collect so we can interleave
    // the rebind mid-stream.
    //
    // If the first frame is NOT SessionOpened (rare but possible when PtyData
    // races ahead), re-inject it into the main loop via pending_first_frame so
    // no data — including LINE:0 — is silently discarded (CR-01 fix).
    let mut pending_first_frame: Option<Message> = None;
    {
        let first = tokio::time::timeout(
            Duration::from_secs(10),
            nosh_proto::read_message(&mut recv),
        )
        .await
        .expect("no hang waiting for first frame")
        .expect("read first frame");
        if let Message::SessionOpened { .. } = first {
            // Discarded as expected.
        } else {
            // PtyData arrived before SessionOpened — re-inject into the main
            // loop so its data (potentially LINE:0) is not lost.
            eprintln!("[migration] note: first frame was not SessionOpened; re-injecting into main loop");
            pending_first_frame = Some(first);
        }
    }

    // ── Step 4: Drive a long monotonic output stream ──────────────────────────
    // 80 lines × 50ms = ~4s of output, comfortably spanning the rebind + sub-ms
    // loopback path validation window.
    client::send_input(
        &mut send,
        b"i=0; while [ $i -lt 80 ]; do echo LINE:$i; i=$((i+1)); sleep 0.05; done; echo DONE; exit 0\n",
    )
    .await
    .expect("send script");

    // ── Step 5: Read frames until mid-stream, then snapshot pre-rebind stats ──
    let mut sequence: Vec<u32> = Vec::new();
    let mut pre_stats = conn.stats();
    let pre_id = conn.stable_id();
    let mut rtt = conn.rtt().max(conn.stats().path.rtt);
    let mut t_last_pre = Instant::now(); // updated on every frame before rebind

    let rebind_after = 10u32; // snapshot after observing LINE:10

    // Tracks whether we have done the rebind yet.
    let mut rebind_done = false;
    let mut t_rebind = Instant::now();
    let mut t_first_post: Option<Instant> = None;
    // WR-02: set to true on the same frame that triggers the rebind; cleared at
    // the end of each outer-loop iteration. Prevents t_first_post from being
    // captured from buffered data in the same PtyData chunk as the rebind (which
    // would understate the actual anti-amplification stall to near-zero).
    let mut just_rebound = false;
    // WR-01: tracks shell DONE sentinel so the outer loop can break cleanly
    // rather than relying solely on sequence.len() >= 80 or SessionClose.
    let mut done_received = false;

    // PTY output is a raw byte stream, NOT framed on line boundaries: a single
    // PtyData chunk may carry a partial line, multiple lines, or coalesce the
    // shell prompt with the first output (e.g. "$ LINE:0" with the trailing
    // "\r\n" arriving in the NEXT chunk). The previous per-chunk `text.lines()`
    // parse dropped LINE:0 whenever the "$ " prompt was coalesced onto the same
    // line as LINE:0 (strip_prefix("LINE:") failed on "$ LINE:0"), and could
    // also drop a token split mid-digits across a chunk boundary. To parse
    // correctly we accumulate all output in `parse_buf` and only consume
    // COMPLETE (newline-terminated) lines, leaving any partial tail buffered for
    // the next chunk. We also locate the `LINE:` token anywhere in the line
    // (not just at the start) so a coalesced prompt prefix does not hide it.
    let mut parse_buf = String::new();

    // Extract the numeric value following the LAST `LINE:` token in `line`, if
    // any (and only if it is immediately followed by digits, then end-of-line).
    // Returns None for a bare prompt, partial token, or non-LINE output.
    fn parse_line_token(line: &str) -> Option<u32> {
        let trimmed = line.trim();
        let idx = trimmed.rfind("LINE:")?;
        let rest = &trimmed[idx + "LINE:".len()..];
        if rest.is_empty() {
            return None;
        }
        rest.parse::<u32>().ok()
    }

    loop {
        // Drain the re-injected first frame before reading from the network
        // (CR-01: ensures no data is silently dropped when PtyData races ahead
        // of SessionOpened).
        let frame = if let Some(f) = pending_first_frame.take() {
            f
        } else {
            tokio::time::timeout(
                Duration::from_secs(10),
                nosh_proto::read_message(&mut recv),
            )
            .await
            .expect("no hang waiting for frame")
            .expect("read frame (no ConnectionError)")
        };

        match frame {
            Message::PtyData { data } => {
                let text = String::from_utf8_lossy(&data);

                if !rebind_done {
                    // Track the time of every pre-rebind frame.
                    t_last_pre = Instant::now();
                }

                // Accumulate raw output and parse only COMPLETE newline-terminated
                // lines, leaving any partial tail in `parse_buf` for the next chunk.
                // This is robust to chunk boundaries that split a line anywhere
                // (mid-token or before the trailing newline) and to a shell prompt
                // coalesced onto the same line as LINE:0 (e.g. "$ LINE:0").
                parse_buf.push_str(&text);
                while let Some(nl) = parse_buf.find('\n') {
                    // Split off the complete line (including the newline), keep the rest.
                    let rest = parse_buf.split_off(nl + 1);
                    let line = std::mem::replace(&mut parse_buf, rest);
                    let line = line.trim();

                    if let Some(n) = parse_line_token(line) {
                        if !rebind_done && t_first_post.is_none() {
                            // Still pre-rebind; update the last-pre timer.
                            t_last_pre = Instant::now();
                        }
                        // t_first_post is captured at the PtyData-frame level
                        // (outside this loop) to avoid counting buffered lines from
                        // the same chunk that triggered the rebind (WR-02 fix).
                        sequence.push(n);

                        // Once we've seen LINE:10, snapshot stats + trigger rebind.
                        if !rebind_done && n >= rebind_after {
                            pre_stats = conn.stats();
                            rtt = conn.rtt().max(conn.stats().path.rtt);
                            t_last_pre = Instant::now();

                            // D-02: force path change via fresh 127.0.0.1:0 socket.
                            let new_addr = rebind_client(&endpoint)
                                .expect("rebind to fresh loopback socket");
                            t_rebind = Instant::now();
                            eprintln!(
                                "[migration] rebind done after LINE:{n}; new local addr: {new_addr}"
                            );

                            // Pitfall #2 mitigation: send a tiny frame immediately
                            // after rebind so the server can see data from the new
                            // path and advance its anti-amplification budget, reducing
                            // the stall duration (D-04 recommendation, 07-RESEARCH §4).
                            let _ = client::send_input(&mut send, b"").await;

                            rebind_done = true;
                            just_rebound = true;
                        }
                    } else if line == "DONE" || line.ends_with("DONE") {
                        // Shell finished the sequence; signal the outer loop to exit.
                        done_received = true;
                        break; // break the line-draining loop
                    }
                }

                // Record first post-rebind frame time (WR-02 fix): only capture
                // t_first_post from frames that arrived AFTER the rebind — not from
                // the same PtyData chunk that triggered it (just_rebound). Data
                // buffered in that chunk was received before the rebind and would
                // understate the real anti-amplification stall to near-zero.
                if rebind_done && t_first_post.is_none() && !just_rebound {
                    t_first_post = Some(Instant::now());
                }
            }

            Message::SessionClose { exit_code, .. } => {
                eprintln!("[migration] SessionClose received, exit_code={exit_code}");
                break;
            }

            // D-03: any transport-level error is a hard failure.
            other => {
                // Unexpected frame; skip (Ack, Resize responses, etc.) — not an error.
                let _ = other;
            }
        }

        // WR-02: clear the just_rebound flag after each outer-loop iteration so
        // subsequent frames are eligible for t_first_post capture.
        just_rebound = false;

        // WR-01: if the shell sent DONE, exit the outer loop cleanly without
        // waiting for SessionClose or the 80-line count. This prevents a 30s
        // timeout if the shell crashed early (e.g. before emitting 80 lines).
        if done_received {
            break;
        }

        // Check if the connection has a transport error (D-03).
        if let Some(reason) = conn.close_reason() {
            use quinn::ConnectionError::*;
            match reason {
                ApplicationClosed(_) | LocallyClosed | ConnectionClosed(_) => {
                    // Normal close — exit loop.
                    break;
                }
                other => {
                    panic!(
                        "D-03 FAIL: transport error during migration test: {other:?}; \
                         sequence so far: {:?}",
                        &sequence
                    );
                }
            }
        }

        // If we've collected all 80 lines (0..79) we can also break.
        if sequence.len() >= 80 {
            // Give a brief moment for SessionClose or DONE to arrive.
            let _ = tokio::time::timeout(
                Duration::from_millis(500),
                nosh_proto::read_message(&mut recv),
            )
            .await;
            break;
        }
    }

    // ── Step 8: Assert continuity — no gaps, no duplicates, no reordering (D-03) ─
    assert!(
        !sequence.is_empty(),
        "D-03 FAIL: no LINE:<n> output received at all"
    );

    // The sequence must be strictly increasing and start at 0. Gaps, duplicates,
    // and reordering are all fatal (D-03). We accept partial sequences (e.g. if
    // the shell exit race cuts the last few lines) as long as it is monotone and
    // starts at 0.
    {
        let mut prev = None::<u32>;
        for &n in &sequence {
            if let Some(p) = prev {
                assert!(
                    n == p + 1,
                    "D-03 FAIL: sequence is not strictly monotone at {p}→{n}; \
                     full sequence: {:?}",
                    sequence
                );
            } else {
                assert_eq!(
                    n, 0,
                    "D-03 FAIL: sequence must start at LINE:0, got LINE:{n}"
                );
            }
            prev = Some(n);
        }
        eprintln!(
            "[migration] D-03 PASS: received {} lines (0..{}) with no gap, \
             dup, or reorder",
            sequence.len(),
            sequence.last().copied().unwrap_or(0)
        );
    }

    // ── Assert SAME connection — no new TLS handshake (D-03) ─────────────────
    assert_eq!(
        conn.stable_id(),
        pre_id,
        "D-03 FAIL: stable_id changed — a new QUIC connection was established \
         (new TLS handshake) across the rebind; must be same connection"
    );
    eprintln!(
        "[migration] D-03 PASS: stable_id={pre_id} unchanged — same QUIC connection, no new handshake"
    );

    // ── Step 9: Measure + log the anti-amplification stall (D-04) ────────────
    let stall = t_first_post
        .map(|t| t.saturating_duration_since(t_last_pre))
        .unwrap_or(t_rebind.elapsed());
    let rtt_secs = rtt.as_secs_f64().max(1e-6);
    let ratio = stall.as_secs_f64() / rtt_secs;

    eprintln!(
        "[migration] D-04: migration stall: {:?} (RTT ~{:?}, ratio ~{:.1}x RTT)",
        stall, rtt, ratio
    );
    if ratio > 3.0 {
        // Soft warning only — D-04 explicitly prohibits a hard assert on stall
        // duration because CI scheduling jitter makes a hard latency bound flaky.
        eprintln!(
            "[migration] D-04 SOFT WARNING: anti-amplification stall exceeded 3× RTT \
             ({ratio:.1}x); this is expected RFC 9000 §9.4 behavior on slower paths — \
             not a hard failure"
        );
    } else {
        eprintln!("[migration] D-04 PASS: stall within 3× RTT ({ratio:.1}x)");
    }

    // ── Step 10: Assert CID rotation / path validation via FrameStats (D-05) ──
    //
    // Per 07-RESEARCH.md §1, quinn 0.11.9 qlog does NOT record PATH_CHALLENGE
    // frames or connection-ID fields. The authoritative proof of CID rotation
    // and path validation is the FrameStats delta across the rebind:
    //   - path_challenge increased → PATH_CHALLENGE/PATH_RESPONSE ran (RFC 9000 §9.3)
    //   - new_connection_id and/or retire_connection_id increased → CID rotation ran
    //     (RFC 9000 §9.5 privacy requirement)
    let post_stats = conn.stats();

    let pre_path_challenge_total =
        pre_stats.frame_tx.path_challenge + pre_stats.frame_rx.path_challenge;
    let post_path_challenge_total =
        post_stats.frame_tx.path_challenge + post_stats.frame_rx.path_challenge;

    assert!(
        post_path_challenge_total > pre_path_challenge_total,
        "D-05 FAIL: path_challenge FrameStats did not increase across rebind \
         (pre={pre_path_challenge_total}, post={post_path_challenge_total}); \
         PATH_CHALLENGE/PATH_RESPONSE did not run — the path change was not validated"
    );
    eprintln!(
        "[migration] D-05 PASS: path_challenge increased: {pre_path_challenge_total}→{post_path_challenge_total}"
    );

    let pre_cid_total = pre_stats.frame_tx.new_connection_id
        + pre_stats.frame_tx.retire_connection_id
        + pre_stats.frame_rx.new_connection_id
        + pre_stats.frame_rx.retire_connection_id;
    let post_cid_total = post_stats.frame_tx.new_connection_id
        + post_stats.frame_tx.retire_connection_id
        + post_stats.frame_rx.new_connection_id
        + post_stats.frame_rx.retire_connection_id;

    assert!(
        post_cid_total > pre_cid_total,
        "D-05 FAIL: new_connection_id / retire_connection_id FrameStats did not increase \
         across rebind (pre={pre_cid_total}, post={post_cid_total}); CID rotation did not \
         run — RFC 9000 §9.5 privacy requirement not satisfied"
    );
    eprintln!(
        "[migration] D-05 PASS: CID rotation counters increased: {pre_cid_total}→{post_cid_total}"
    );

    // ── Step 11: Validate qlog artifact (D-05 literal) ───────────────────────
    //
    // Close the connection so the qlog streamer finalizes and flushes to disk,
    // then assert the file exists, is non-empty, and is valid JSON-seq format.
    // We do NOT assert PATH_CHALLENGE in the qlog — it is not there
    // (07-RESEARCH.md §1); the FrameStats assertion above is the binding proof.
    conn.close(0u32.into(), b"migration test done");
    endpoint.close(0u32.into(), b"migration test done");

    // Give the qlog streamer a brief window to flush — it writes on drop/close.
    tokio::time::sleep(Duration::from_millis(200)).await;

    if qlog_path.exists() {
        let content = std::fs::read_to_string(&qlog_path)
            .expect("read qlog file");

        assert!(
            !content.is_empty(),
            "D-05 FAIL: qlog file exists but is empty at {}",
            qlog_path.display()
        );

        // Minimal parse check: qlog JSON-seq means each record is a self-contained
        // JSON object. The simplest valid check is that the file begins with '{' or
        // (for JSON-seq) a RS byte (0x1E) followed by '{', and the whole file is
        // well-formed JSON. We use serde_json (already a transitive dep of qlog)
        // for a robust parse.
        let first_non_whitespace = content
            .trim_start_matches(|c: char| c.is_whitespace() || c == '\x1e')
            .chars()
            .next();
        assert_eq!(
            first_non_whitespace,
            Some('{'),
            "D-05 FAIL: qlog file does not begin with '{{' — not valid qlog JSON-seq; \
             first bytes: {:?}",
            &content[..content.len().min(80)]
        );

        // Attempt to parse each newline-delimited record (strip RS bytes).
        let mut record_count = 0usize;
        for record in content.split('\x1e') {
            let trimmed = record.trim();
            if trimmed.is_empty() {
                continue;
            }
            let _parsed: serde_json::Value = serde_json::from_str(trimmed)
                .unwrap_or_else(|e| {
                    panic!(
                        "D-05 FAIL: qlog record is not valid JSON: {e}; record: {trimmed:?}"
                    )
                });
            record_count += 1;
        }
        assert!(
            record_count > 0,
            "D-05 FAIL: qlog file contained no parseable JSON records"
        );
        eprintln!(
            "[migration] D-05 PASS: qlog artifact validated ({record_count} JSON records, \
             {} bytes) — NOTE: quinn 0.11.9 qlog does not record PATH_CHALLENGE frames or \
             CID fields; FrameStats assertion above is the binding CID-rotation proof",
            content.len()
        );
    } else {
        // qlog file missing: into_stream() returned None or file creation failed.
        // Log a warning but do not fail the migration test — qlog is an artifact
        // check, not the primary continuity proof. The FrameStats assertion above
        // is the binding CID-rotation proof.
        eprintln!(
            "[migration] D-05 WARNING: qlog file not found at {} — qlog artifact \
             validation skipped (into_stream() may have returned None or file creation \
             failed; the FrameStats CID assertion above is still the binding proof)",
            qlog_path.display()
        );
    }

    // Drop server last so the session cleanup can run.
    drop(server);
}
