//! Phase 14 render tests — confirmed screen matches server TerminalState (PREDICT-01).
//!
//! Covers two success criteria from PREDICT-01:
//! - A `ClientScreen` confirmed grid matches a `TerminalState` visible-cell grid
//!   for the same byte stream, exercised via a real encode/decode wire round-trip
//!   (`confirmed_grid_matches_terminal_state_after_diff`).
//! - An end-to-end integration test connects a real client to a real server, types
//!   input, receives StateDiff datagrams, applies them, and asserts the typed text
//!   appears in the confirmed grid (`render_integration_client_screen_matches_server_output`).
//! - An idempotency test confirms duplicate datagrams emit no new cell content
//!   (`duplicate_datagram_is_idempotent`).

use std::sync::Arc;
use std::time::Duration;

use nosh_client::client;
use nosh_client::screen::ClientScreen;
use nosh_proto::datagram::{
    decode_datagram, encode_datagram, CellStyle, CursorPos, DiffRun, StateDiff,
};
use nosh_server::registry::SessionRegistry;
use nosh_server::server::AuthLimits;
use nosh_server::terminal::TerminalState;

mod common;
use common::{spawn_server_with_registry, TestKey, HOST};

const SH: &str = "/bin/sh";

fn have_sh() -> bool {
    std::path::Path::new(SH).exists()
}

/// Spawn a server authorising a single key.
async fn server_with_key(
    registry: Arc<SessionRegistry>,
    client_key: &TestKey,
) -> common::TestServer {
    let host_key = TestKey::generate();
    spawn_server_with_registry(
        &host_key,
        &[&client_key.public],
        AuthLimits::default(),
        Some(SH.to_string()),
        registry,
    )
    .await
}

/// Build a client endpoint for the given test key with a fresh temp known_hosts.
fn client_endpoint_for(key: &TestKey) -> (quinn::Endpoint, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let kh = dir.path().join("known_hosts");
    let ep = common::client_endpoint(key.client_identity(), kh).unwrap();
    (ep, dir)
}

// ── Test 1: Pure grid-comparison — confirmed grid matches TerminalState ──────

/// PREDICT-01 success criterion #2 (pure, no QUIC):
///
/// Drive a `TerminalState` with the same bytes that will be diffed. Build a
/// full-repaint `StateDiff` from the viewport, round-trip it through the real
/// wire encode/decode path (`encode_datagram` → `decode_datagram`), apply to a
/// `ClientScreen`, and assert cell-by-cell equality over the full 80×24 grid.
///
/// The full-grid loop (`for row in 0..24 { for col in 0..80 { assert_eq!(...) } }`)
/// is the load-bearing PREDICT-01 check — not a spot-check.
#[test]
fn confirmed_grid_matches_terminal_state_after_diff() {
    let cols: u16 = 80;
    let rows: u16 = 24;

    // ── 1. Drive a TerminalState with two lines of text ────────────────────────
    let mut server_ts = TerminalState::new(cols, rows);
    server_ts.advance(b"hello world\r\nline two\r\n");

    // ── 2. Build a full-repaint StateDiff from the viewport ───────────────────
    //
    // For each row, coalesce consecutive non-default cells sharing the same style
    // into DiffRun entries.  A cell is "non-default" if ch != ' ' OR any attribute
    // is set — because blank cells need not be transmitted (the client's confirmed
    // grid is already initialised to blanks).
    let mut runs: Vec<DiffRun> = Vec::new();

    for (row_idx, row_cells) in server_ts.viewport_rows() {
        // Walk the row and coalesce runs of non-blank cells.
        let mut col_start: Option<u16> = None;
        let mut current_style = CellStyle(CellStyle::NONE);
        let mut current_fg: Option<u8> = None;
        let mut current_bg: Option<u8> = None;
        let mut current_chars = String::new();

        let flush_run = |runs: &mut Vec<DiffRun>,
                         row: u16,
                         start: u16,
                         style: CellStyle,
                         fg: Option<u8>,
                         bg: Option<u8>,
                         chars: String| {
            if !chars.is_empty() {
                runs.push(DiffRun {
                    row,
                    start_col: start,
                    style,
                    fg,
                    bg,
                    chars,
                });
            }
        };

        for (col_idx, cell) in row_cells.iter().enumerate() {
            let col = col_idx as u16;
            let is_default = cell.ch == ' '
                && cell.style.0 == CellStyle::NONE
                && cell.fg.is_none()
                && cell.bg.is_none();

            if is_default {
                // Flush the in-progress run if any.
                if let Some(start) = col_start.take() {
                    flush_run(
                        &mut runs,
                        row_idx,
                        start,
                        current_style,
                        current_fg,
                        current_bg,
                        std::mem::take(&mut current_chars),
                    );
                }
                continue;
            }

            // Non-default cell.
            let same_style = cell.style == current_style
                && cell.fg == current_fg
                && cell.bg == current_bg;

            if col_start.is_some() && same_style {
                // Extend the current run.
                current_chars.push(cell.ch);
            } else {
                // Flush the previous run (if any) and start a new one.
                if let Some(start) = col_start.take() {
                    flush_run(
                        &mut runs,
                        row_idx,
                        start,
                        current_style,
                        current_fg,
                        current_bg,
                        std::mem::take(&mut current_chars),
                    );
                }
                col_start = Some(col);
                current_style = cell.style;
                current_fg = cell.fg;
                current_bg = cell.bg;
                current_chars.push(cell.ch);
            }
        }

        // Flush any trailing run.
        if let Some(start) = col_start.take() {
            flush_run(
                &mut runs,
                row_idx,
                start,
                current_style,
                current_fg,
                current_bg,
                std::mem::take(&mut current_chars),
            );
        }
    }

    let server_cursor = server_ts.cursor();
    let diff = StateDiff {
        epoch: 1,
        cols,
        rows,
        cursor: CursorPos {
            row: server_cursor.row,
            col: server_cursor.col,
        },
        runs,
    };

    // ── 3. Round-trip through the real wire encode/decode path ─────────────────
    //
    // Use a large cap (4096) so no runs are deferred — this is a unit test of the
    // full-repaint path, not the cap-enforcement path.  The cap is well within the
    // range of `encode_datagram`'s MIN_CAP guarantee.
    let cap = 4096;
    let (encoded_bytes, deferred) = encode_datagram(&diff, cap)
        .expect("encode_datagram must succeed for a full-repaint diff with cap=4096");

    // For this test we expect all runs to fit (they're short ASCII lines).
    assert!(
        deferred.is_empty(),
        "expected no deferred runs for a 2-line text diff at cap=4096 (got {})",
        deferred.len()
    );

    let decoded = decode_datagram(&encoded_bytes)
        .expect("decode_datagram must succeed for a valid encode_datagram output");

    // ── 4. Apply the decoded diff to a ClientScreen ────────────────────────────
    let mut screen = ClientScreen::new(cols, rows);
    screen.apply(&decoded);

    // ── 5. Assert cell-by-cell equality over the FULL 80×24 grid ───────────────
    //
    // This is the load-bearing PREDICT-01 assertion: every cell in the confirmed
    // grid must match the corresponding cell in the server's TerminalState.
    for row in 0..rows {
        for col in 0..cols {
            let server_cell = server_ts.cell(row, col);
            let client_cell = screen.confirmed_cell(row, col);

            assert_eq!(
                client_cell.ch, server_cell.ch,
                "ch mismatch at ({row}, {col}): client={:?}, server={:?}",
                client_cell.ch, server_cell.ch
            );
            assert_eq!(
                client_cell.fg, server_cell.fg,
                "fg mismatch at ({row}, {col}): client={:?}, server={:?}",
                client_cell.fg, server_cell.fg
            );
            assert_eq!(
                client_cell.bg, server_cell.bg,
                "bg mismatch at ({row}, {col}): client={:?}, server={:?}",
                client_cell.bg, server_cell.bg
            );
            assert_eq!(
                client_cell.style.0, server_cell.style.0,
                "style mismatch at ({row}, {col}): client=0x{:02x}, server=0x{:02x}",
                client_cell.style.0, server_cell.style.0
            );
        }
    }
}

// ── Test 2: Duplicate datagram is idempotent ──────────────────────────────────

/// Idempotency: applying the same datagram twice must not produce new cell output.
///
/// After the first apply, `render_to_stdout` returns non-empty ANSI (some cells
/// changed).  After the second apply of the *same epoch* (discarded by the
/// monotonic guard), `render_to_stdout` returns bytes containing only the final
/// MoveTo cursor-position escape — no cell-content characters.
#[test]
fn duplicate_datagram_is_idempotent() {
    let mut screen = ClientScreen::new(80, 24);

    // Build a diff with a single content run.
    let diff = StateDiff {
        epoch: 1,
        cols: 80,
        rows: 24,
        cursor: CursorPos { row: 0, col: 5 },
        runs: vec![DiffRun {
            row: 0,
            start_col: 0,
            style: CellStyle(CellStyle::NONE),
            fg: None,
            bg: None,
            chars: "hello".to_string(),
        }],
    };

    // First apply + render: must produce non-empty output.
    screen.apply(&diff);
    let mut buf1: Vec<u8> = Vec::new();
    screen
        .render_to_stdout(&mut buf1)
        .expect("render_to_stdout must succeed");
    assert!(
        !buf1.is_empty(),
        "first render after apply must produce non-empty ANSI output"
    );

    // Second apply of the same epoch: D-14-05 monotonic guard discards it.
    screen.apply(&diff); // epoch=1 <= last_applied=1 → no-op

    // Render again: physical already matches desired — only the final MoveTo is emitted.
    let mut buf2: Vec<u8> = Vec::new();
    screen
        .render_to_stdout(&mut buf2)
        .expect("render_to_stdout must succeed on second call");

    // buf2 must be strictly shorter than buf1 (no cell-content ANSI emitted).
    assert!(
        buf2.len() < buf1.len(),
        "second render after duplicate datagram must emit fewer bytes than first: \
         buf1.len()={}, buf2.len()={}",
        buf1.len(),
        buf2.len()
    );

    // buf2 must not contain any of the 'h','e','l','o' characters from the run —
    // only cursor-move escapes (digits, ';', 'H', '\x1b', '[') are expected.
    for ch in ['h', 'e', 'l', 'o'] {
        assert!(
            !buf2.contains(&(ch as u8)),
            "second render must not emit cell-content char {:?} (idempotency guard)",
            ch
        );
    }
}

// ── Test 3: Integration — typed text appears in confirmed grid from live datagrams

/// PREDICT-01 end-to-end: a real client connects to a real server, types
/// `echo hello`, receives StateDiff datagrams, applies them to a `ClientScreen`,
/// and asserts the substring "hello" appears contiguously in at least one
/// confirmed-grid row.
///
/// This closes the PREDICT-01 loop: datagram-rendered display reproduces the
/// server's visible terminal output for user-typed input.
#[tokio::test]
async fn render_integration_client_screen_matches_server_output() {
    if !have_sh() {
        eprintln!(
            "skipping render_integration_client_screen_matches_server_output: {SH} not available"
        );
        return;
    }

    let registry = SessionRegistry::new(5, Duration::from_secs(30));
    let client_key = TestKey::generate();
    let server = server_with_key(registry.clone(), &client_key).await;

    let (ep, _dir) = client_endpoint_for(&client_key);
    let conn = client::connect(&ep, server.addr, HOST, Duration::from_secs(30))
        .await
        .expect("connect");

    // Open a PTY session; keep streams alive while datagrams flow.
    let (mut send, mut recv) = client::open_session(&conn, "xterm".into(), 80, 24, vec![])
        .await
        .expect("open_session");

    // Discard the SessionOpened frame (contains the reattach token).
    match nosh_proto::read_message(&mut recv).await {
        Ok(_) => {} // SessionOpened — expected; discard token
        Err(e) => panic!("expected SessionOpened, got error: {e}"),
    }

    // Type input that will produce "hello" in the terminal output.
    client::send_input(&mut send, b"echo hello\n")
        .await
        .expect("send_input");

    // Build a ClientScreen to receive incoming datagrams.
    let mut screen = ClientScreen::new(80, 24);

    // Loop: receive datagrams, apply to screen, scan for "hello" in any row.
    // The server emits datagrams ~16 ms after PTY output (diff-interval tick).
    // We allow 5 s total before declaring a timeout failure.
    let deadline = Duration::from_secs(5);

    loop {
        match tokio::time::timeout(deadline, conn.read_datagram()).await {
            Ok(Ok(bytes)) => {
                // Decode and apply.  Non-StateDiff datagrams (e.g. wrong tag) are
                // silently skipped — the server only emits TAG_STATE_DIFF this phase,
                // so a decode error would indicate a framing bug.
                if let Ok(diff) = decode_datagram(&bytes) {
                    screen.apply(&diff);
                }

                // Scan every confirmed-grid row for the contiguous substring "hello".
                let (cols, rows) = screen.size();
                for r in 0..rows {
                    let row_str: String = (0..cols)
                        .map(|c| screen.confirmed_cell(r, c).ch)
                        .collect();
                    if row_str.contains("hello") {
                        // Found "hello" in the confirmed grid — PREDICT-01 satisfied.
                        drop(send);
                        drop(recv);
                        conn.close(0u32.into(), b"done");
                        ep.close(0u32.into(), b"done");
                        return;
                    }
                }
            }
            Ok(Err(e)) => panic!("connection error while waiting for datagram: {e}"),
            Err(_) => panic!(
                "timed out after 5s waiting for 'hello' to appear in the confirmed grid — \
                 PREDICT-01 end-to-end check failed"
            ),
        }
    }
}
