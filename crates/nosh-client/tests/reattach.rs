//! Phase 6 cold-reattach integration tests — Roadmap success criteria SC#1–4.
//!
//! Each test drives an in-process server + client and validates a distinct
//! aspect of the cold-reattach protocol:
//!
//! - SC#1 (`reattach_replays_unacked_output_byte_exact`): replay continuity —
//!   no duplicated or dropped bytes relative to the full server output.
//! - SC#2/#3 (`reattach_wrong_key_rejected_like_bad_token`): two-factor auth
//!   and no-oracle: a valid token with the wrong key and a bad token with the
//!   right key both yield the same opaque Err.
//! - SC#4 (`reattach_rejected_while_session_active`): mutual exclusion — a
//!   reattach for an Active session is rejected (D-12, Pitfall #10).

use std::sync::Arc;
use std::time::Duration;

use nosh_client::client::{self, ReattachOutcome};
use nosh_server::registry::SessionRegistry;

mod common;
use common::{spawn_server_with_registry, TestKey, HOST};

const SH: &str = "/bin/sh";

fn have_sh() -> bool {
    std::path::Path::new(SH).exists()
}

/// Spawn a server authorizing a single key.
async fn server_with_key(
    registry: Arc<SessionRegistry>,
    client_key: &TestKey,
) -> common::TestServer {
    let host_key = TestKey::generate();
    spawn_server_with_registry(
        &host_key,
        &[&client_key.public],
        nosh_server::server::AuthLimits::default(),
        Some(SH.to_string()),
        registry,
    )
    .await
}

/// Build a client endpoint for the given test key and a fresh temp known_hosts.
fn client_endpoint_for(key: &TestKey) -> (quinn::Endpoint, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let kh = dir.path().join("known_hosts");
    let ep = common::client_endpoint(key.client_identity(), kh).unwrap();
    (ep, dir)
}

// ── SC#1: replay continuity (byte-exact, multi-cycle) ─────────────────────────

/// Mirror of the production `run_pump` applied-seq accounting in
/// `nosh-client/src/main.rs`, so this test exercises the REAL client counter
/// contract rather than a hardcoded `last_acked_seq`.
///
/// Invariant (next-expected-seq convention, see `Message::Reattach`):
/// - fresh session: counter starts at 0.
/// - each applied `PtyData` chunk: counter += 1 (so the counter equals the
///   count of chunks applied == the seq of the next chunk expected).
/// - on reattach: the client rebases the counter to the server's
///   `replaying_from_seq` (NOT `replaying_from_seq - 1`) so the first replayed
///   chunk lands at the right offset.
///
/// The value sent in `Reattach.last_acked_seq` / `Ack.seq` is exactly this
/// counter. If the counter logic or the server's replay/trim boundary is off
/// by one, this test drops or duplicates a chunk and fails.
struct ClientCounter {
    highest_applied: u64,
}

impl ClientCounter {
    fn fresh() -> Self {
        Self { highest_applied: 0 }
    }
    /// Apply one received chunk exactly as `run_pump` does.
    fn apply_chunk(&mut self) {
        self.highest_applied = self.highest_applied.saturating_add(1);
    }
    /// Rebase on reattach exactly as `reattach_session` does.
    fn rebase_on_reattach(&mut self, replaying_from_seq: u64) {
        self.highest_applied = replaying_from_seq;
    }
    /// The value the client reports to the server.
    fn last_acked_seq(&self) -> u64 {
        self.highest_applied
    }
}

/// SC#1 / ROAM-02 (byte-exact, MULTI-CYCLE): drive the ACTUAL client counter
/// through several disconnect→reattach cycles and assert the client observes
/// EVERY emitted output marker EXACTLY ONCE, in order — no duplicate, no drop.
///
/// This is the crux test for the fence-post BLOCKER: the previous version
/// hardcoded `last_acked_seq = 0` and only did a fuzzy "≥3 markers" substring
/// check, so it could not detect the drop-one-chunk-per-reconnect bug. This
/// version:
///  - has the server emit N distinct, totally-ordered markers (`MARK000000`…),
///  - reads them through the real client counter,
///  - disconnects mid-stream on EACH cycle (without ever Acking, so the server
///    must replay the un-applied tail),
///  - reattaches using the counter value (next-expected-seq) and rebases,
///  - repeats for several cycles to surface the COMPOUNDING off-by-one,
///  - finally asserts the concatenated applied output contains every marker
///    exactly once and in strictly increasing order.
#[tokio::test]
async fn reattach_replays_unacked_output_byte_exact() {
    if !have_sh() {
        eprintln!("skipping reattach_replays_unacked_output_byte_exact: /bin/sh unavailable");
        return;
    }

    const TOTAL_MARKERS: u32 = 40;
    const CYCLES: u32 = 4;

    let registry = SessionRegistry::new(5, Duration::ZERO);
    let client_key = TestKey::generate();
    let server = server_with_key(registry.clone(), &client_key).await;

    // ── Fresh session ────────────────────────────────────────────────────────
    let (ep, _dir) = client_endpoint_for(&client_key);
    let conn = client::connect(&ep, server.addr, HOST).await.expect("connect");
    let (mut send, mut recv, mut token) =
        client::open_session_with_token(&conn, "xterm".to_string(), 80, 24, vec![])
            .await
            .expect("open_session_with_token");

    // Emit ALL TOTAL_MARKERS distinct, ordered markers up front (one tight
    // loop, no per-marker sleep), THEN keep the shell alive with a long sleep.
    // `printf` with a zero-padded counter gives strictly-ordered, unique,
    // easily-greppable tokens (MARK000000, MARK000001, …).
    //
    // Producing all output BEFORE the first disconnect — and sleeping (no new
    // PTY output) across every orphan→reattach gap — isolates the sequence /
    // replay / trim contract (FIX 1's scope) from the orthogonal PTY-reader
    // handoff timing. Every byte the client misses is buffered server-side and
    // must be replayed via `seq >= last_acked_seq`, so a single off-by-one at
    // the replay/trim boundary drops or duplicates a marker and fails the test.
    // After emitting the markers, the shell blocks on `read` (waiting for
    // stdin). This keeps it alive across the orphan→reattach cycles WITHOUT a
    // fixed-duration `sleep` that would stall teardown: sending any line at the
    // end unblocks `read` and the shell exits immediately.
    let script = format!(
        "i=0; while [ $i -lt {TOTAL_MARKERS} ]; do printf 'MARK%06d\\n' $i; i=$((i+1)); done; echo DONEMARKER; read _x\n"
    );
    client::send_input(&mut send, script.as_bytes())
        .await
        .expect("send script");
    // Let the shell finish producing every marker before we disconnect.
    tokio::time::sleep(Duration::from_millis(400)).await;

    // Accumulate everything the client APPLIES across all cycles.
    let mut applied = Vec::<u8>::new();
    let mut counter = ClientCounter::fresh();

    // Helper: drain `recv` into `applied`, advancing the counter per chunk,
    // until we've seen `stop_after_chunks` newly-applied chunks, OR output goes
    // idle (no PtyData for ~1.2s, meaning everything buffered has arrived), OR
    // the stream errors. Returns true if the stream is still alive.
    //
    // The idle cutoff matters: there are far fewer PtyData frames than markers
    // (the PTY batches many lines per read), so a fixed chunk target would
    // block until a deadline. Stopping on idle keeps the test fast while still
    // applying every byte the server sends in this window.
    async fn drain_n(
        recv: &mut quinn::RecvStream,
        applied: &mut Vec<u8>,
        counter: &mut ClientCounter,
        stop_after_chunks: u32,
    ) -> bool {
        let mut got = 0u32;
        let mut idle_strikes = 0u32;
        while got < stop_after_chunks {
            match tokio::time::timeout(Duration::from_millis(400), nosh_proto::read_message(recv)).await {
                Ok(Ok(nosh_proto::Message::PtyData { data })) => {
                    applied.extend_from_slice(&data);
                    counter.apply_chunk();
                    got += 1;
                    idle_strikes = 0;
                }
                Ok(Ok(_)) => { /* ignore non-PtyData control frames */ }
                Ok(Err(_)) => return false, // stream closed
                Err(_) => {
                    // No data this window. After 3 idle windows (~1.2s) assume
                    // the buffered output has all arrived and return.
                    idle_strikes += 1;
                    if idle_strikes >= 3 {
                        return true;
                    }
                }
            }
        }
        true
    }

    // Apply a handful of chunks on the fresh session before the first drop.
    drain_n(&mut recv, &mut applied, &mut counter, 3).await;

    // ── Disconnect → reattach cycles ───────────────────────────────────────────
    let mut cur_conn = conn;
    let mut cur_ep = ep;
    let mut _cur_dir = _dir;

    for cycle in 0..CYCLES {
        // Abrupt drop (no SessionClose, NO Ack) → orphan with un-applied tail.
        cur_conn.close(1u32.into(), b"test transport loss");
        drop(send);
        drop(recv);
        drop(cur_conn);
        cur_ep.close(0u32.into(), b"done");
        drop(cur_ep);

        // Wait for the server to orphan the session.
        let orphan_deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if registry.total_orphans() >= 1 {
                break;
            }
            if std::time::Instant::now() > orphan_deadline {
                panic!("server did not orphan within 5s (cycle {cycle})");
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }

        // Reconnect with the SAME key; reattach using the REAL counter value.
        let (ep2, dir2) = client_endpoint_for(&client_key);
        let conn2 = client::connect(&ep2, server.addr, HOST)
            .await
            .expect("reconnect");
        let (mut send2, mut recv2) = conn2.open_bi().await.expect("open bi");
        client::send_reattach(&mut send2, token, counter.last_acked_seq())
            .await
            .expect("send reattach");

        let outcome = client::await_reattach_reply(&mut recv2)
            .await
            .expect("await_reattach_reply");
        let replaying_from_seq = match outcome {
            ReattachOutcome::Ok { new_token, replaying_from_seq, truncated } => {
                assert!(!truncated, "64 KiB buffer must not truncate {TOTAL_MARKERS} tiny markers (cycle {cycle})");
                token = new_token;
                replaying_from_seq
            }
            ReattachOutcome::Err => panic!("reattach must succeed (cycle {cycle})"),
        };
        // Rebase exactly as the production client does.
        counter.rebase_on_reattach(replaying_from_seq);

        // Apply a couple more chunks this cycle (drain everything on the final
        // cycle via the large target + idle cutoff).
        let take = if cycle == CYCLES - 1 { u32::MAX } else { 2 };
        drain_n(&mut recv2, &mut applied, &mut counter, take).await;

        send = send2;
        recv = recv2;
        cur_conn = conn2;
        cur_ep = ep2;
        _cur_dir = dir2;
    }

    // Final top-up: the last cycle already drained to idle, but make sure the
    // terminal marker landed (bounded by idle cutoff inside drain_n).
    drain_n(&mut recv, &mut applied, &mut counter, u32::MAX).await;

    // Clean up: unblock the shell's `read` so it exits immediately.
    let _ = client::send_input(&mut send, b"\n").await;
    drop(send);
    drop(recv);
    cur_conn.close(0u32.into(), b"done");
    cur_ep.close(0u32.into(), b"done");

    // ── BYTE-EXACT ASSERTION: every marker exactly once, in order ──────────────
    let text = String::from_utf8_lossy(&applied).into_owned();
    let mut last_idx: Option<usize> = None;
    for i in 0..TOTAL_MARKERS {
        let marker = format!("MARK{i:06}");
        let occurrences = text.matches(&marker).count();
        assert_eq!(
            occurrences, 1,
            "marker {marker} must appear EXACTLY once (no drop, no dup); appeared {occurrences} times.\n\
             Counter ended at {}.\nApplied transcript:\n{}",
            counter.last_acked_seq(),
            &text[..text.len().min(2000)]
        );
        // Strictly-increasing order: each marker appears after the previous one.
        let idx = text.find(&marker).unwrap();
        if let Some(prev) = last_idx {
            assert!(
                idx > prev,
                "marker {marker} must appear AFTER the previous marker (ordering); got idx {idx} <= {prev}"
            );
        }
        last_idx = Some(idx);
    }
}

// ── SC#2/#3: two-factor auth / no-oracle ─────────────────────────────────────

/// SC#2/#3 / IDENT-02: valid token with wrong identity → Err; bad token with
/// right identity → Err. The two rejections must be indistinguishable
/// (no-oracle property, D-07).
#[tokio::test]
async fn reattach_wrong_key_rejected_like_bad_token() {
    if !have_sh() {
        eprintln!("skipping reattach_wrong_key_rejected_like_bad_token: /bin/sh unavailable");
        return;
    }

    let registry = SessionRegistry::new(5, Duration::ZERO);
    let key_a = TestKey::generate();
    let key_b = TestKey::generate();
    let host_key = TestKey::generate();

    // Server authorizes BOTH key A and key B.
    let server = spawn_server_with_registry(
        &host_key,
        &[&key_a.public, &key_b.public],
        nosh_server::server::AuthLimits::default(),
        Some(SH.to_string()),
        registry.clone(),
    )
    .await;

    // Connect with key A, open session, capture token, then drop → orphan.
    let (ep_a1, _dir_a1) = client_endpoint_for(&key_a);
    let conn_a1 = client::connect(&ep_a1, server.addr, HOST).await.expect("connect A");
    let (_send, _recv, token_a) =
        client::open_session_with_token(&conn_a1, "xterm".to_string(), 80, 24, vec![])
            .await
            .expect("open session A");
    conn_a1.close(1u32.into(), b"test drop");
    drop(conn_a1);
    ep_a1.close(0u32.into(), b"done");
    drop(ep_a1);

    let orphan_deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if registry.total_orphans() >= 1 {
            break;
        }
        if std::time::Instant::now() > orphan_deadline {
            panic!("orphan not observed within 5s");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Case 1: valid token_a, but reconnect with key B (wrong identity) → Err.
    let (ep_b, _dir_b) = client_endpoint_for(&key_b);
    let conn_b = client::connect(&ep_b, server.addr, HOST).await.expect("connect B");
    let (outcome_b, _, _) = client::reattach_collect(&conn_b, token_a, 0)
        .await
        .expect("reattach with wrong key");
    conn_b.close(0u32.into(), b"done");
    ep_b.close(0u32.into(), b"done");

    assert_eq!(
        outcome_b,
        ReattachOutcome::Err,
        "valid token + wrong identity must yield ReattachOutcome::Err"
    );

    // Case 2: bogus token, right identity A → Err.
    let bogus_token = [0xDEu8; 16];
    let (ep_a2, _dir_a2) = client_endpoint_for(&key_a);
    let conn_a2 = client::connect(&ep_a2, server.addr, HOST).await.expect("connect A2");
    let (outcome_a2, _, _) = client::reattach_collect(&conn_a2, bogus_token, 0)
        .await
        .expect("reattach with bogus token");
    conn_a2.close(0u32.into(), b"done");
    ep_a2.close(0u32.into(), b"done");

    assert_eq!(
        outcome_a2,
        ReattachOutcome::Err,
        "bad token + correct identity must yield ReattachOutcome::Err"
    );

    // NO-ORACLE ASSERTION: both rejections are ReattachOutcome::Err — structurally
    // identical (ReattachErr is fieldless, so there is no distinguishing data).
    assert_eq!(
        outcome_b, outcome_a2,
        "wrong-key rejection and bad-token rejection must be indistinguishable (no oracle, D-07)"
    );
}

// ── SC#4: mutual exclusion ────────────────────────────────────────────────────

/// SC#4 / D-12: a Reattach for a session that is still Active (client still
/// attached) must be rejected — prevents two-clients-one-session race.
#[tokio::test]
async fn reattach_rejected_while_session_active() {
    if !have_sh() {
        eprintln!("skipping reattach_rejected_while_session_active: /bin/sh unavailable");
        return;
    }

    let registry = SessionRegistry::new(5, Duration::ZERO);
    let client_key = TestKey::generate();
    let server = server_with_key(registry.clone(), &client_key).await;

    // Connect with key A and KEEP the connection active (do NOT drop).
    let (ep1, _dir1) = client_endpoint_for(&client_key);
    let conn1 = client::connect(&ep1, server.addr, HOST).await.expect("connect 1");
    let (_send1, _recv1, token) =
        client::open_session_with_token(&conn1, "xterm".to_string(), 80, 24, vec![])
            .await
            .expect("open session 1");

    // Give the server a moment to register the slot as Active.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // From a second endpoint with the SAME key, attempt to reattach the Active session.
    let (ep2, _dir2) = client_endpoint_for(&client_key);
    let conn2 = client::connect(&ep2, server.addr, HOST).await.expect("connect 2");
    let (outcome2, _, _) = client::reattach_collect(&conn2, token, 0)
        .await
        .expect("reattach_collect for active slot");
    conn2.close(0u32.into(), b"done");
    ep2.close(0u32.into(), b"done");

    assert_eq!(
        outcome2,
        ReattachOutcome::Err,
        "Reattach for Active session must be rejected (D-12 mutual exclusion)"
    );

    // Cleanup: drop the original session.
    conn1.close(0u32.into(), b"done");
    ep1.close(0u32.into(), b"done");
}
