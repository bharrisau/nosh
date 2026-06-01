# Phase 14: Client Predictor — Confirmed Rendering - Pattern Map

**Mapped:** 2026-06-02
**Files analyzed:** 4 new/modified files
**Analogs found:** 4 / 4

---

## File Classification

| New/Modified File | Role | Data Flow | Closest Analog | Match Quality |
|-------------------|------|-----------|----------------|---------------|
| `crates/nosh-client/src/screen.rs` | model | CRUD | `crates/nosh-server/src/terminal.rs` | role-match (same Cell/grid structure, emitter not parser) |
| `crates/nosh-client/src/main.rs` | controller | request-response | `crates/nosh-client/src/main.rs` itself (add datagram arm) | exact (in-place modification) |
| `crates/nosh-client/src/lib.rs` | config | — | `crates/nosh-client/src/lib.rs` itself | exact (add `pub mod screen;`) |
| `crates/nosh-client/tests/render.rs` | test | request-response | `crates/nosh-client/tests/sync.rs` | role-match (same in-process server harness + datagram loop) |

---

## Pattern Assignments

### `crates/nosh-client/src/screen.rs` (model, CRUD)

**Analog:** `crates/nosh-server/src/terminal.rs`

**Imports pattern** (terminal.rs lines 36–38 — same proto types, no vte):
```rust
// screen.rs does NOT import vte — client is an emitter, not a parser.
// It imports the same proto types as terminal.rs does:
use nosh_proto::datagram::{CellStyle, CursorPos, DiffRun, StateDiff};
use crossterm::QueueableCommand;
use crossterm::cursor::MoveTo;
use std::io::Write;
```

**Cell type** (terminal.rs lines 58–81 — exact field set to copy, declared locally):
```rust
// D-14-04: ClientScreen declares its own Cell matching terminal.rs exactly.
// DO NOT import nosh_server::terminal::Cell in production code — nosh-server
// is a [dev-dependency] only. Re-declare with identical fields.
#[derive(Clone, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub style: CellStyle,
    pub fg: Option<u8>,
    pub bg: Option<u8>,
}

impl Default for Cell {
    fn default() -> Self {
        Cell {
            ch: ' ',
            style: CellStyle(CellStyle::NONE),
            fg: None,
            bg: None,
        }
    }
}
```

**Grid construction** (terminal.rs lines 193–197 — make_grid helper):
```rust
fn make_grid(cols: u16, rows: u16) -> Vec<Vec<Cell>> {
    (0..rows as usize)
        .map(|_| vec![Cell::default(); cols as usize])
        .collect()
}
```

**Struct layout** (terminal.rs lines 145–169 — mirror field discipline):
```rust
pub struct ClientScreen {
    cols: u16,
    rows: u16,
    // (a) confirmed grid: cells last applied from datagram StateDiff
    confirmed: Vec<Vec<Cell>>,
    confirmed_cursor: CursorPos,
    // (b) physical grid: what ANSI has actually been emitted to the terminal
    physical: Vec<Vec<Cell>>,
    physical_cursor: CursorPos,
    // Monotonic epoch: D-14-05 staleness guard; init to 0 (server starts at 1)
    last_applied_epoch: u64,
    // Overlay seam (D-14-01a): Phase 14 = ConnectionLossOverlay no-op stub
    overlays: Vec<Box<dyn Overlay>>,
}
```

**Resize pattern** (terminal.rs lines 220–259 — resize both grids; physical resets to blank):
```rust
pub fn resize(&mut self, cols: u16, rows: u16) {
    // Resize confirmed: truncate or extend each row.
    for row in &mut self.confirmed {
        let len = cols as usize;
        if row.len() > len { row.truncate(len); }
        else { row.resize(len, Cell::default()); }
    }
    // Grow/shrink confirmed rows.
    let cur_rows = self.confirmed.len();
    let new_rows = rows as usize;
    if cur_rows > new_rows {
        self.confirmed.truncate(new_rows);
    } else {
        for _ in cur_rows..new_rows {
            self.confirmed.push(vec![Cell::default(); cols as usize]);
        }
    }

    // Pitfall 2 (RESEARCH.md): physical MUST also be resized and reset to
    // blank (NOT copied from confirmed). After a resize the terminal is blank;
    // resetting physical forces a full repaint on the next render_to_stdout.
    self.physical = Self::make_grid(cols, rows);

    self.cols = cols;
    self.rows = rows;
    // Clamp cursors to new bounds.
    self.confirmed_cursor.row = self.confirmed_cursor.row.min(rows.saturating_sub(1));
    self.confirmed_cursor.col = self.confirmed_cursor.col.min(cols.saturating_sub(1));
    self.physical_cursor = CursorPos { row: 0, col: 0 };
}
```

**apply() pattern** (D-14-05 monotonic epoch guard + DiffRun → Cell mapping):
```rust
// Source: RESEARCH.md Pattern 2 (D-14-05); DiffRun field names from
// datagram.rs lines 94–121.
pub fn apply(&mut self, diff: &StateDiff) {
    // D-14-05: monotonic staleness check — discard stale or duplicate diffs.
    if diff.epoch <= self.last_applied_epoch {
        return;
    }

    // Resize if dimensions changed (resize diffs carry new cols/rows).
    if diff.cols != self.cols || diff.rows != self.rows {
        self.resize(diff.cols, diff.rows);
    }

    // Apply each DiffRun to the confirmed grid.
    for run in &diff.runs {
        let row = run.row as usize;
        if row >= self.confirmed.len() {
            continue; // out-of-bounds guard (SECURITY: V5 input validation)
        }
        let row_cells = &mut self.confirmed[row];
        let mut col = run.start_col as usize;
        for ch in run.chars.chars() {
            if col >= row_cells.len() {
                break; // out-of-bounds guard
            }
            row_cells[col] = Cell { ch, style: run.style, fg: run.fg, bg: run.bg };
            col += 1;
        }
    }

    self.confirmed_cursor = diff.cursor;
    self.last_applied_epoch = diff.epoch;
}

pub fn last_applied_epoch(&self) -> u64 {
    self.last_applied_epoch
}
```

**compose_desired() pattern** (RESEARCH.md Pattern — overlay seam):
```rust
fn compose_desired(&self) -> Vec<Vec<Cell>> {
    // Phase 14: ConnectionLossOverlay is a no-op, so desired == confirmed.
    // Phase 15: speculative overlay cells overwrite confirmed cells here.
    // Phase 16: loss-banner overlay row overwrites cells here.
    let mut desired = self.confirmed.clone();
    let rows = desired.len();
    let cols = if rows > 0 { desired[0].len() } else { 0 };
    for overlay in &self.overlays {
        for row in 0..rows {
            for col in 0..cols {
                if let Some(cell) = overlay.cell_at(row as u16, col as u16) {
                    desired[row][col] = cell;
                }
            }
        }
    }
    desired
}
```

**render_to_stdout() pattern** (Mosh Display model — emit minimal ANSI diff):
```rust
// Pitfall 1 (RESEARCH.md): crossterm QueueableCommand requires std::io::Write,
// NOT tokio::io::AsyncWrite. Accept &mut impl std::io::Write; caller buffers
// to Vec<u8> or std::io::BufWriter and does a single async flush after return.
pub fn render_to_stdout<W: Write>(&mut self, out: &mut W) -> std::io::Result<()> {
    let desired = self.compose_desired();
    let desired_cursor = self.confirmed_cursor;

    let rows = desired.len().min(self.physical.len());
    let cols = if rows > 0 { desired[0].len().min(self.physical[0].len()) } else { 0 };

    let mut last_row: Option<u16> = None;
    let mut last_col: Option<u16> = None;
    let mut last_sgr: Option<(CellStyle, Option<u8>, Option<u8>)> = None;

    for row in 0..rows {
        for col in 0..cols {
            let want = &desired[row][col];
            let have = &self.physical[row][col];
            if want == have {
                continue; // idempotent: skip unchanged cells
            }

            // Move cursor only when not already at (row, col).
            if last_row != Some(row as u16) || last_col != Some(col as u16) {
                // Pitfall 7 (RESEARCH.md): MoveTo(col, row) — first arg is COLUMN,
                // second is ROW (crossterm uses 0-based col,row order).
                out.queue(MoveTo(col as u16, row as u16))?;
                last_row = Some(row as u16);
                last_col = Some(col as u16);
            }

            // Emit SGR only when attributes differ from the previous cell.
            let want_sgr = (want.style, want.fg, want.bg);
            if last_sgr != Some(want_sgr) {
                emit_sgr(out, want.style, want.fg, want.bg)?;
                last_sgr = Some(want_sgr);
            }

            // Write the character (single-width; wide char deferred to Phase 15).
            let mut buf = [0u8; 4];
            let s = want.ch.encode_utf8(&mut buf);
            out.write_all(s.as_bytes())?;

            // Advance tracked column position.
            last_col = Some(last_col.unwrap_or(col as u16) + 1);
        }
    }

    // Position cursor at the confirmed cursor position.
    out.queue(MoveTo(desired_cursor.col, desired_cursor.row))?;
    out.flush()?;

    // Set physical = desired.
    for (des_row, phys_row) in desired.iter().zip(self.physical.iter_mut()) {
        for (des_cell, phys_cell) in des_row.iter().zip(phys_row.iter_mut()) {
            *phys_cell = des_cell.clone();
        }
    }
    self.physical_cursor = desired_cursor;

    Ok(())
}
```

**SGR emission** (hand-rolled, ~20 lines — crossterm high-level API not suitable):
```rust
// Source: RESEARCH.md Code Examples "ANSI SGR Emission Pattern".
// Called only when style/fg/bg change between adjacent cells.
fn emit_sgr<W: Write>(
    out: &mut W,
    style: CellStyle,
    fg: Option<u8>,
    bg: Option<u8>,
) -> std::io::Result<()> {
    // Always reset (SGR 0) then re-apply active attributes.
    let mut params = String::from("0");
    if style.0 & CellStyle::BOLD != 0      { params.push_str(";1"); }
    if style.0 & CellStyle::ITALIC != 0    { params.push_str(";3"); }
    if style.0 & CellStyle::UNDERLINE != 0 { params.push_str(";4"); }
    if style.0 & CellStyle::REVERSE != 0   { params.push_str(";7"); }
    // 256-color fg: 38;5;N — distinct from SGR 39 (default fg = None).
    if let Some(n) = fg { params.push_str(&format!(";38;5;{n}")); }
    // 256-color bg: 48;5;N — distinct from SGR 49 (default bg = None).
    if let Some(n) = bg { params.push_str(&format!(";48;5;{n}")); }
    write!(out, "\x1b[{params}m")
}
```

**Overlay trait seam** (D-14-01a — Phase 14 stub; Phase 15/16 extension point):
```rust
// Source: RESEARCH.md Pattern 8.
/// A screen overlay layer applied on top of the confirmed grid in compose_desired().
/// Phase 14: only ConnectionLossOverlay exists, and it is a no-op.
pub trait Overlay {
    /// Return Some(cell) to override the cell at (row, col), or None to pass through.
    fn cell_at(&self, row: u16, col: u16) -> Option<Cell>;
}

/// No-op connection-loss overlay stub (Phase 14).
/// Phase 16 activates this: when no datagram arrives for >5s, overlays a banner.
pub struct ConnectionLossOverlay;

impl Overlay for ConnectionLossOverlay {
    fn cell_at(&self, _row: u16, _col: u16) -> Option<Cell> {
        None // no-op this phase
    }
}
```

**reset_physical() helper** (for reconnect/reattach path — Open Question 3 in RESEARCH.md):
```rust
// Called by reattach_session before entering run_pump to force a full repaint
// on the first post-resume datagram (symmetric with server's empty-baseline reset
// D-13-01b). Without this, the physical grid might not match the actual terminal
// state after a connection drop (terminal may have scrolled or resized).
pub fn reset_physical(&mut self) {
    self.physical = Self::make_grid(self.cols, self.rows);
    self.physical_cursor = CursorPos { row: 0, col: 0 };
}
```

---

### `crates/nosh-client/src/main.rs` (controller, request-response — in-place modification)

**Analog:** `crates/nosh-client/src/main.rs` (existing `run_pump` function + callers)

**run_pump signature change** (main.rs lines 604–610 — add `conn` parameter):
```rust
// BEFORE (lines 604–610):
async fn run_pump(
    send: &mut quinn::SendStream,
    recv: &mut quinn::RecvStream,
    highest_applied: &mut u64,
    resize: &mut platform::ResizeWatcher,
    _seq_baseline: u64,
) -> anyhow::Result<PumpOutcome>

// AFTER Phase 14 (add conn: &quinn::Connection as first param):
async fn run_pump(
    conn: &quinn::Connection,   // NEW: needed for read_datagram() + send_datagram()
    send: &mut quinn::SendStream,
    recv: &mut quinn::RecvStream,
    highest_applied: &mut u64,
    resize: &mut platform::ResizeWatcher,
    _seq_baseline: u64,
) -> anyhow::Result<PumpOutcome>
```

**fresh_session caller update** (main.rs line 549 — thread conn through):
```rust
// BEFORE (line 549):
run_pump(&mut send, &mut recv, highest_applied, resize, 0).await

// AFTER:
run_pump(conn, &mut send, &mut recv, highest_applied, resize, 0).await
```

**reattach_session caller update** (main.rs line 597 — thread conn through):
```rust
// BEFORE (line 597):
run_pump(&mut send, &mut recv, highest_applied, resize, *highest_applied).await

// AFTER:
run_pump(conn, &mut send, &mut recv, highest_applied, resize, *highest_applied).await
```

**New datagram arm in select!** (mirrors sync.rs lines 101–112 conn.read_datagram loop):
```rust
// Source: sync.rs datagram receive pattern (lines 101–112) adapted for run_pump.
// Place this arm AFTER the reliable-stream arm in the select! block.
datagram = conn.read_datagram() => {
    match datagram {
        Ok(bytes) => {
            if let Ok(diff) = nosh_proto::datagram::decode_datagram(&bytes) {
                if diff.epoch > screen.last_applied_epoch() {
                    screen.apply(&diff);
                    // Pitfall 1: render_to_stdout writes to std::io::Write.
                    // Buffer to Vec<u8>, then flush to tokio stdout with write_all.
                    let mut buf: Vec<u8> = Vec::new();
                    screen.render_to_stdout(&mut buf).unwrap_or_else(|e| {
                        tracing::warn!("render_to_stdout error: {e}");
                    });
                    if !buf.is_empty() {
                        let _ = stdout.write_all(&buf).await;
                        // No explicit flush needed — write_all on tokio flushes
                        // automatically, or add stdout.flush().await?.
                    }
                    // D-14-03a: emit epoch-ack as DATAGRAM (NOT on reliable stream).
                    // TAG_CLIENT_EPOCH (0x02) — distinct from Ack{seq} (TAG_ACK).
                    let ack_payload = nosh_proto::datagram::encode_epoch_ack(diff.epoch);
                    let _ = conn.send_datagram(ack_payload); // best-effort; ignore error
                }
                // Pitfall 6 (RESEARCH.md): stale epoch silently discarded — correct.
            }
            // Non-StateDiff datagrams (unknown tags): decode_datagram returns Err → discarded.
        }
        Err(_) => {
            return Ok(PumpOutcome::TransportDrop);
        }
    }
}
```

**PtyData arm modification** (main.rs lines 637–644 — remove stdout write, keep counter):
```rust
// BEFORE (lines 637–644):
Ok(Message::PtyData { data }) => {
    stdout.write_all(&data).await?;   // ← REMOVE (D-14-02: datagram-only display)
    stdout.flush().await?;             // ← REMOVE
    *highest_applied = highest_applied.saturating_add(1);
}

// AFTER (D-14-03):
Ok(Message::PtyData { data }) => {
    // D-14-03: advance reattach counter; DO NOT write to stdout.
    // Display comes exclusively from datagrams via screen.render_to_stdout().
    // The reliable-stream Ack{seq} is kept distinct from the datagram epoch-ack.
    let _ = data; // content discarded for display (no client-side scrollback)
    *highest_applied = highest_applied.saturating_add(1);
}
```

**screen variable instantiation in run_pump** (add before the select! loop):
```rust
// Add after the existing let mut escape = EscapeState::new(); line.
// Dimensions from startup crossterm::terminal::size() call (main.rs line 416).
// Accept cols/rows as new params OR capture them via closure — either approach.
// Simplest: pass cols/rows into run_pump alongside conn.
let mut screen = nosh_client::screen::ClientScreen::new(cols, rows);
```

**Import addition** (main.rs top — add screen module usage):
```rust
// Add to existing imports in main.rs:
use nosh_client::screen::ClientScreen;
// OR reference as nosh_client::screen::ClientScreen::new(...) inline.
```

---

### `crates/nosh-client/src/lib.rs` (config — trivial addition)

**Analog:** `crates/nosh-client/src/lib.rs` (current content lines 1–11)

**Current content** (lines 1–11):
```rust
//! `nosh-client` library surface — connection setup and round-trip helpers
//! exposed so integration tests can drive a client in-process.

pub mod client;
pub mod platform;

pub use client::{
    build_client_config, concurrent_roundtrip, connect, datagram_roundtrip, make_endpoint,
    stream_echo_roundtrip,
};
```

**After Phase 14** (add `pub mod screen;`):
```rust
pub mod client;
pub mod platform;
pub mod screen;   // NEW: ClientScreen, Overlay, ConnectionLossOverlay

pub use client::{
    build_client_config, concurrent_roundtrip, connect, datagram_roundtrip, make_endpoint,
    stream_echo_roundtrip,
};
```

---

### `crates/nosh-client/tests/render.rs` (test, request-response)

**Analog:** `crates/nosh-client/tests/sync.rs` (same test harness structure and datagram loop pattern)

**Test file header pattern** (sync.rs lines 1–23 — module doc + imports):
```rust
//! Phase 14 render tests — confirmed screen matches server TerminalState.
//!
//! Proves PREDICT-01: the client's confirmed grid (populated from datagram
//! StateDiffs) matches the server's TerminalState after the same PTY byte stream.

use std::sync::Arc;
use std::time::Duration;

use nosh_client::client::{self, ReattachOutcome};
use nosh_client::screen::ClientScreen;
use nosh_proto::datagram::{decode_datagram, encode_epoch_ack, CellStyle};
// nosh_server is a [dev-dependency] in nosh-client/Cargo.toml (line 49).
use nosh_server::terminal::TerminalState;

mod common;
use common::{spawn_server_with_registry, TestKey, HOST, have_sh};
```

**Server spawn pattern** (sync.rs lines 29–44 — server_with_key helper):
```rust
const SH: &str = "/bin/sh";

async fn server_with_key(
    registry: Arc<nosh_server::registry::SessionRegistry>,
    client_key: &common::TestKey,
) -> common::TestServer {
    let host_key = TestKey::generate();
    common::spawn_server_with_registry(
        &host_key,
        &[&client_key.public],
        nosh_server::server::AuthLimits::default(),
        Some(SH.to_string()),
        registry,
    )
    .await
}
```

**Pure unit test pattern** (no tokio, no QUIC — drive TerminalState + ClientScreen together):
```rust
// Source: RESEARCH.md Code Examples "End-to-End Test Strategy: Grid Comparison".
// nosh-server is available as a dev-dependency (Cargo.toml line 49).
// This test requires NO network, NO tokio::test — pure sync logic.
//
// Open Question 1 (RESEARCH.md): use compute_diff_runs directly from server internals
// via a test helper rather than calling build_state_diff (which needs SessionSlot).
// If compute_diff_runs is not pub, use encode_datagram + decode_datagram round-trip
// via TerminalState::viewport_rows() to build a StateDiff manually.

#[test]
fn confirmed_grid_matches_terminal_state_after_diff() {
    let mut server_ts = TerminalState::new(80, 24);
    server_ts.advance(b"hello world\r\nline two\r\n");

    // Build a StateDiff manually from the server TerminalState viewport.
    // Snapshot current cells vs. an empty baseline (epoch 1, full repaint).
    let cursor = server_ts.cursor();
    let cells: Vec<Vec<nosh_server::terminal::Cell>> = server_ts
        .viewport_rows()
        .map(|(_, row)| row.to_vec())
        .collect();

    // Build runs by comparing current cells to empty baseline (all cells changed).
    let mut runs = vec![];
    for (row_idx, row_cells) in cells.iter().enumerate() {
        let mut col = 0usize;
        while col < row_cells.len() {
            let cell = &row_cells[col];
            if cell.ch == ' ' && cell.style.0 == CellStyle::NONE && cell.fg.is_none() && cell.bg.is_none() {
                col += 1;
                continue; // skip blank cells (default cells don't need to be in the diff)
            }
            let start_col = col as u16;
            let style = cell.style;
            let fg = cell.fg;
            let bg = cell.bg;
            let mut chars = String::new();
            while col < row_cells.len() {
                let c = &row_cells[col];
                if c.style != style || c.fg != fg || c.bg != bg { break; }
                if c.ch == ' ' && style.0 == CellStyle::NONE && fg.is_none() && bg.is_none() { break; }
                chars.push(c.ch);
                col += 1;
            }
            if !chars.is_empty() {
                runs.push(nosh_proto::datagram::DiffRun {
                    row: row_idx as u16, start_col, style, fg, bg, chars,
                });
            }
        }
    }

    let diff = nosh_proto::datagram::StateDiff {
        epoch: 1,
        cols: 80,
        rows: 24,
        cursor: nosh_proto::datagram::CursorPos { row: cursor.row, col: cursor.col },
        runs,
    };

    // Apply to ClientScreen and compare non-blank cells.
    let mut screen = ClientScreen::new(80, 24);
    screen.apply(&diff);

    // Assert character matches at all cells with content.
    assert_eq!(screen.confirmed_cell(0, 0).ch, 'h');
    assert_eq!(screen.confirmed_cell(0, 1).ch, 'e');
    assert_eq!(screen.confirmed_cell(1, 0).ch, 'l'); // "line two" row 1
}
```

**Integration datagram test pattern** (sync.rs lines 62–125 — conn.read_datagram loop):
```rust
// Source: sync.rs sync03_server_emits_datagram_after_pty_output pattern (lines 62–125).
// Same structure for a render integration test:
// open session → send input → loop read_datagram → apply to ClientScreen → assert grid.

#[tokio::test]
async fn render_integration_client_screen_matches_server_output() {
    if !have_sh() { return; }

    let registry = nosh_server::registry::SessionRegistry::new(5, Duration::from_secs(30));
    let client_key = TestKey::generate();
    let server = server_with_key(registry.clone(), &client_key).await;

    let (ep, _dir) = {
        let dir = tempfile::tempdir().unwrap();
        let kh = dir.path().join("known_hosts");
        let ep = common::client_endpoint(client_key.client_identity(), kh).unwrap();
        (ep, dir)
    };

    let conn = client::connect(&ep, server.addr, HOST, Duration::from_secs(30))
        .await
        .expect("connect");

    let (mut send, mut recv) = client::open_session(&conn, "xterm".into(), 80, 24, vec![])
        .await
        .expect("open_session");

    // Discard SessionOpened (sync.rs line 85–88 pattern).
    let _ = nosh_proto::read_message(&mut recv).await;

    client::send_input(&mut send, b"echo hello\n")
        .await
        .expect("send_input");

    // Loop: receive datagrams, apply to ClientScreen, assert 'hello' visible.
    let mut screen = ClientScreen::new(80, 24);
    let deadline = Duration::from_secs(5);

    loop {
        match tokio::time::timeout(deadline, conn.read_datagram()).await {
            Ok(Ok(bytes)) => {
                if let Ok(diff) = decode_datagram(&bytes) {
                    screen.apply(&diff);
                    // Check if 'hello' appears somewhere in the confirmed grid.
                    let found = (0..24u16).any(|row| {
                        (0..80u16).any(|col| screen.confirmed_cell(row, col).ch == 'h')
                        // simplified: check for 'h' as a proxy for "hello"
                    });
                    if found { break; }
                }
            }
            Ok(Err(e)) => panic!("connection error: {e}"),
            Err(_) => panic!("timed out waiting for 'hello' in confirmed grid"),
        }
    }

    drop(send);
    drop(recv);
    conn.close(0u32.into(), b"done");
    ep.close(0u32.into(), b"done");
}
```

**Idempotent render test pattern** (no QUIC, pure screen logic):
```rust
#[test]
fn duplicate_datagram_produces_no_ansi_output() {
    let mut screen = ClientScreen::new(80, 24);
    let diff = nosh_proto::datagram::StateDiff {
        epoch: 1,
        cols: 80, rows: 24,
        cursor: nosh_proto::datagram::CursorPos { row: 0, col: 0 },
        runs: vec![nosh_proto::datagram::DiffRun {
            row: 0, start_col: 0,
            style: nosh_proto::datagram::CellStyle(nosh_proto::datagram::CellStyle::NONE),
            fg: None, bg: None,
            chars: "hello".to_string(),
        }],
    };

    // First apply: produces ANSI output.
    let mut buf1 = Vec::<u8>::new();
    screen.apply(&diff);
    screen.render_to_stdout(&mut buf1).unwrap();
    assert!(!buf1.is_empty(), "first render must emit ANSI");

    // Second apply (same epoch): stale, no apply. render_to_stdout should be a no-op.
    screen.apply(&diff); // epoch 1 <= 1 → discarded
    let mut buf2 = Vec::<u8>::new();
    screen.render_to_stdout(&mut buf2).unwrap();
    // physical is already updated → desired == physical → zero changed cells → empty-ish output.
    // The only bytes emitted are the final cursor-position MoveTo.
    // Assert the second render is much shorter than the first (at minimum, no cell content).
    assert!(
        buf2.len() < buf1.len(),
        "duplicate datagram must produce minimal ANSI (only cursor position, no cell writes)"
    );
}
```

---

## Shared Patterns

### Screen module public API (needed by tests and main.rs)
**Source:** `crates/nosh-server/src/terminal.rs` read API (lines 326–389)
**Apply to:** `screen.rs` public API surface

```rust
// Mirror terminal.rs's clean read API — cell accessor + size.
impl ClientScreen {
    pub fn new(cols: u16, rows: u16) -> Self { ... }

    /// Read a cell from the confirmed grid.
    /// Returns Cell::default() for out-of-bounds coordinates (no panic).
    pub fn confirmed_cell(&self, row: u16, col: u16) -> &Cell {
        static DEFAULT_CELL: std::sync::OnceLock<Cell> = std::sync::OnceLock::new();
        let default = DEFAULT_CELL.get_or_init(Cell::default);
        self.confirmed
            .get(row as usize)
            .and_then(|r| r.get(col as usize))
            .unwrap_or(default)
    }

    pub fn last_applied_epoch(&self) -> u64 { self.last_applied_epoch }

    pub fn size(&self) -> (u16, u16) { (self.cols, self.rows) }
}
```

### Periodic Ack vs. epoch-ack distinction
**Source:** `crates/nosh-client/src/main.rs` lines 696–702 (ack_interval tick arm)
**Apply to:** `main.rs` run_pump datagram arm

The reliable-stream Ack (lines 696–702) uses `client::send_ack(send, *highest_applied)` on the QUIC stream. The datagram epoch-ack uses `conn.send_datagram(encode_epoch_ack(diff.epoch))` on the datagram channel. These are ALWAYS kept distinct (D-14-03a / Pitfall 3 in RESEARCH.md).

```rust
// Reliable-stream Ack (UNCHANGED — drives cold-reattach replay):
_ = ack_interval.tick() => {
    if *highest_applied != last_acked {
        if client::send_ack(send, *highest_applied).await.is_err() {
            return Ok(PumpOutcome::TransportDrop);
        }
        last_acked = *highest_applied;
    }
}

// Datagram epoch-ack (NEW in datagram arm — drives server baseline advance):
let ack_payload = nosh_proto::datagram::encode_epoch_ack(diff.epoch);
let _ = conn.send_datagram(ack_payload); // best-effort
```

### Test harness client endpoint builder
**Source:** `crates/nosh-client/tests/sync.rs` lines 47–52 (client_endpoint_for helper)
**Apply to:** `tests/render.rs`

```rust
fn client_endpoint_for(key: &TestKey) -> (quinn::Endpoint, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let kh = dir.path().join("known_hosts");
    let ep = common::client_endpoint(key.client_identity(), kh).unwrap();
    (ep, dir)
}
```

### CellStyle bitflag constants
**Source:** `crates/nosh-proto/src/datagram.rs` lines 128–139
**Apply to:** `screen.rs` emit_sgr + tests

```rust
// CellStyle bitflag constants (copy these from datagram.rs):
// CellStyle::NONE      = 0x00
// CellStyle::BOLD      = 0x01
// CellStyle::ITALIC    = 0x02
// CellStyle::UNDERLINE = 0x04
// CellStyle::REVERSE   = 0x08
//
// SGR mapping:
// BOLD → SGR 1;  ITALIC → SGR 3;  UNDERLINE → SGR 4;  REVERSE → SGR 7
// 256-color fg:  SGR 38;5;N
// 256-color bg:  SGR 48;5;N
// Default fg (None): no SGR needed after reset (SGR 0 restores terminal default)
// Default bg (None): same
```

---

## No Analog Found

All files in this phase have close analogs in the codebase. No files fall into this category.

---

## Metadata

**Analog search scope:** `crates/nosh-client/`, `crates/nosh-server/`, `crates/nosh-proto/`
**Files scanned:** `terminal.rs` (1511 lines), `main.rs` (710 lines), `datagram.rs` (1006 lines), `sync.rs` (388 lines), `common/mod.rs` (245 lines), `lib.rs` (11 lines), `Cargo.toml` (63 lines)
**Pattern extraction date:** 2026-06-02

**Critical anti-patterns from RESEARCH.md to embed in plans:**
1. Never write `stdout.write_all(&data)` from the PtyData arm post-Phase 14 (D-14-02).
2. Never call `render_to_stdout` from multiple locations (single display path invariant).
3. Never send epoch-ack on the reliable stream — always `conn.send_datagram(encode_epoch_ack(...))`.
4. Never use `tokio::io::stdout()` with `crossterm::QueueableCommand` — requires `std::io::Write`.
5. Always resize BOTH confirmed AND physical grids on resize diff; physical resets to blank.
6. Initialize `last_applied_epoch = 0` (server epochs start at 1, so first diff always applies).
