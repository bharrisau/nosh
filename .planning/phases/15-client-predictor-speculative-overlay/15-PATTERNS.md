# Phase 15: Client Predictor — Speculative Overlay - Pattern Map

**Mapped:** 2026-06-02
**Files analyzed:** 5 new/modified files
**Analogs found:** 5 / 5

## File Classification

| New/Modified File | Role | Data Flow | Closest Analog | Match Quality |
|-------------------|------|-----------|----------------|---------------|
| `crates/nosh-client/src/predictor.rs` | service | event-driven | `crates/nosh-client/src/screen.rs` (`ConnectionLossOverlay` + `Overlay` trait) | role-match (same overlay contract; different internal logic) |
| `crates/nosh-client/src/screen.rs` | service | CRUD | self (small additions to existing file) | exact |
| `crates/nosh-client/src/main.rs` | controller | request-response | self (hook into existing `run_pump` select arms) | exact |
| `crates/nosh-client/src/lib.rs` | config | — | self (add `pub mod predictor`) | exact |
| `crates/nosh-client/Cargo.toml` | config | — | self (add `unicode-width = "0.2"`) | exact |
| `crates/nosh-client/tests/predict.rs` | test | event-driven | `crates/nosh-client/tests/render.rs` | role-match |

---

## Pattern Assignments

### `crates/nosh-client/src/predictor.rs` (new service, event-driven)

**Analog:** `crates/nosh-client/src/screen.rs`

**Imports pattern** (screen.rs lines 26-31):
```rust
use std::io::Write;

use crossterm::cursor::MoveTo;
use crossterm::QueueableCommand;
use nosh_proto::datagram::{CellStyle, CursorPos, StateDiff};
```

For `predictor.rs`, the import pattern adapts to:
```rust
use std::collections::VecDeque;
use nosh_proto::datagram::{CellStyle, CursorPos};
use unicode_width::UnicodeWidthChar;
use crate::screen::{Cell, ClientScreen, Overlay};
```

**Core Overlay implementation pattern** (screen.rs lines 78-94):
```rust
// Analog: ConnectionLossOverlay — the no-op overlay that PredictionOverlay replaces.
pub trait Overlay {
    fn cell_at(&self, row: u16, col: u16) -> Option<Cell>;
}

pub struct ConnectionLossOverlay;

impl Overlay for ConnectionLossOverlay {
    fn cell_at(&self, _row: u16, _col: u16) -> Option<Cell> {
        None // no-op this phase
    }
}
```

`PredictionOverlay::cell_at` returns `Some(Cell)` for active non-tentative predictions (with `style.0 | CellStyle::UNDERLINE` when `self.flagging`), or `None` to pass through.

**Module-level doc comment pattern** (screen.rs lines 1-24):
```rust
//! `ClientScreen` — confirmed framebuffer compositor (Mosh Display model).
//!
//! Implements D-14-01, D-14-01a, D-14-04, D-14-05.
//!
//! # Architecture
//!
//! [explanation of the model, then load-bearing decisions]
//!
//! # Security
//!
//! - T-xx-01: [invariant]
```

Follow the same doc pattern for `predictor.rs`:
```rust
//! `PredictionOverlay` — speculative echo overlay (Mosh PredictionEngine model).
//!
//! Implements PREDICT-02, PREDICT-03, PREDICT-04, PREDICT-05, PREDICT-06.
//!
//! # Architecture
//!
//! [overview of epoch/Validity state machine, noecho suppression mechanism]
//!
//! # Security
//!
//! - No-echo suppression is structural (confirmed_epoch never advances when
//!   server suppresses echo) — not an explicit flag.
//! - Bulk/paste input suppression prevents prediction during paste.
```

**OnceLock default pattern for OOB access** (screen.rs lines 383-390):
```rust
// Pattern for returning a safe default for out-of-bounds coordinates.
pub fn confirmed_cell(&self, row: u16, col: u16) -> &Cell {
    static DEFAULT_CELL: std::sync::OnceLock<Cell> = std::sync::OnceLock::new();
    let default = DEFAULT_CELL.get_or_init(Cell::default);
    self.confirmed
        .get(row as usize)
        .and_then(|r| r.get(col as usize))
        .unwrap_or(default)
}
```

Use the same `OnceLock` pattern if `PredictionOverlay` needs a default return.

**tracing::warn! error pattern** (main.rs lines 689-692):
```rust
screen.render_to_stdout(&mut buf).unwrap_or_else(|e| {
    tracing::warn!("render_to_stdout error: {e}");
});
```

Use the same `unwrap_or_else(|e| tracing::warn!(...))` form for non-fatal errors in `predictor.rs` methods.

**Security constants pattern** (screen.rs lines 37-39):
```rust
const MAX_TERMINAL_COLS: u16 = 512;
const MAX_TERMINAL_ROWS: u16 = 256;
```

Use the same top-of-file const pattern for RTT threshold constants in `predictor.rs`:
```rust
const SRTT_TRIGGER_HIGH_MS: u64 = 30;
const SRTT_TRIGGER_LOW_MS:  u64 = 20;
const FLAG_TRIGGER_HIGH_MS: u64 = 80;
const FLAG_TRIGGER_LOW_MS:  u64 = 50;
const BULK_SUPPRESS_THRESHOLD: usize = 4;
```

**Inline test module pattern** (screen.rs lines 440-834):
```rust
#[cfg(test)]
mod tests {
    use nosh_proto::datagram::{CellStyle, CursorPos, DiffRun, StateDiff};
    use super::*;

    fn make_diff(epoch: u64, chars: &str) -> StateDiff { ... }

    #[test]
    fn test_name_describes_behaviour() {
        // arrange
        // act
        // assert with descriptive message
    }
}
```

Copy this pattern for the unit tests inside `predictor.rs` (classification tests, validity tests, tentative-check tests).

---

### `crates/nosh-client/src/screen.rs` (modify existing service, CRUD)

**Analog:** self (existing file, lines 117-139 for overlay stack construction)

**Overlay stack construction** (screen.rs lines 117, 137-138):
```rust
// Existing overlay vec field:
overlays: Vec<Box<dyn Overlay>>,

// Existing constructor pre-loading:
overlays: vec![Box::new(ConnectionLossOverlay)],
```

Phase 15 adds `PredictionOverlay` at index 1. Copy the same `Box::new(...)` push pattern. The overlay is either added in `new()` or via an `add_overlay(overlay: Box<dyn Overlay>)` method following the existing field pattern.

**compose_desired loop** (screen.rs lines 271-283) — unchanged; the loop already handles multiple overlays:
```rust
fn compose_desired(&self) -> Vec<Vec<Cell>> {
    let mut desired = self.confirmed.clone();
    for overlay in &self.overlays {
        for (r, row_cells) in desired.iter_mut().enumerate() {
            for (c, cell_slot) in row_cells.iter_mut().enumerate() {
                if let Some(cell) = overlay.cell_at(r as u16, c as u16) {
                    *cell_slot = cell;
                }
            }
        }
    }
    desired
}
```

**render_to_stdout final MoveTo** (screen.rs lines 350-352):
```rust
// Current: always positions at confirmed_cursor.
out.queue(MoveTo(desired_cursor.col, desired_cursor.row))?;
```

Phase 15 must substitute `predicted_cursor` when non-tentative cursor predictions exist. The research recommends a `predicted_cursor()` method on `PredictionOverlay` passed in by `run_pump` rather than modifying the `Overlay` trait. The modification reads the override from `PredictionOverlay` before calling `render_to_stdout`, or extends `render_to_stdout` to accept `Option<CursorPos>`.

**confirmed_cell accessor** (screen.rs lines 382-390) — used by `predictor.cull()` to compare predicted vs. actual cell content. No change needed; `predictor.rs` receives `&ClientScreen` and calls `screen.confirmed_cell(row, col)`.

---

### `crates/nosh-client/src/main.rs` (modify controller, request-response)

**Analog:** self (existing `run_pump` at lines 608-768)

**Stdin arm pattern to copy** (main.rs lines 721-737):
```rust
n = stdin.read(&mut stdin_buf) => {
    match n {
        Ok(0) => return Ok(PumpOutcome::UserQuit),
        Ok(n) => {
            let result = escape.process(&stdin_buf[..n]);
            if result.quit {
                return Ok(PumpOutcome::UserQuit);
            }
            if !result.bytes_to_forward.is_empty()
                && client::send_input(send, &result.bytes_to_forward).await.is_err()
            {
                return Ok(PumpOutcome::TransportDrop);
            }
        }
        Err(_) => return Ok(PumpOutcome::UserQuit),
    }
}
```

Phase 15 inserts between `escape.process(...)` and `client::send_input(...)`:
```rust
// Hook predictor AFTER escape machine, BEFORE send_input.
predictor.on_input(&result.bytes_to_forward, &screen);
// Re-render with new overlay state.
let mut buf: Vec<u8> = Vec::new();
screen.render_to_stdout_with_cursor(&mut buf, predictor.predicted_cursor())
    .unwrap_or_else(|e| tracing::warn!("render: {e}"));
if !buf.is_empty() {
    // same stdout.write_all pattern as datagram arm (lines 692-699)
}
```

**Datagram arm render pattern to copy** (main.rs lines 678-716):
```rust
datagram = conn.read_datagram() => {
    match datagram {
        Ok(bytes) => {
            if let Ok(diff) = nosh_proto::datagram::decode_datagram(&bytes) {
                if diff.epoch > screen.last_applied_epoch() {
                    screen.apply(&diff);
                    let mut buf: Vec<u8> = Vec::new();
                    screen.render_to_stdout(&mut buf).unwrap_or_else(|e| {
                        tracing::warn!("render_to_stdout error: {e}");
                    });
                    if !buf.is_empty() {
                        if let Err(e) = stdout.write_all(&buf).await {
                            tracing::warn!("stdout write_all failed: {e} — forcing full repaint");
                            screen.reset_physical();
                        } else if let Err(e) = stdout.flush().await {
                            tracing::warn!("stdout flush failed: {e} — forcing full repaint");
                            screen.reset_physical();
                        }
                    }
                    let ack_payload = nosh_proto::datagram::encode_epoch_ack(diff.epoch);
                    let _ = conn.send_datagram(ack_payload);
                }
            }
        }
        Err(e) => {
            tracing::warn!("datagram channel error, triggering reconnect: {e}");
            return Ok(PumpOutcome::TransportDrop);
        }
    }
}
```

Phase 15 adds `predictor.cull(&screen, diff.epoch, conn.rtt().as_millis() as u64)` immediately after `screen.apply(&diff)`, before the render.

**Args struct clap pattern** (main.rs lines 282-317):
```rust
#[derive(Parser, Debug)]
#[command(name = "nosh-client", about = "...", long_about = "...")]
struct Args {
    #[arg(long, default_value = "127.0.0.1")]
    addr: IpAddr,

    #[arg(long, default_value_t = 4433)]
    port: u16,

    // ...

    #[arg(long)]
    identity_file: Option<PathBuf>,
}
```

Add `--predict` following the same `#[arg(long, default_value = "adaptive")]` pattern, with a `PredictDisplayMode` enum (`Always`, `Adaptive`, `Never`) that implements `clap::ValueEnum`.

**run_pump signature pattern** (main.rs lines 607-617):
```rust
#[allow(clippy::too_many_arguments)] // 8 args are load-bearing: conn + streams + state + watcher + baseline
async fn run_pump(
    conn: &quinn::Connection,
    cols: u16,
    rows: u16,
    send: &mut quinn::SendStream,
    recv: &mut quinn::RecvStream,
    highest_applied: &mut u64,
    resize: &mut platform::ResizeWatcher,
    _seq_baseline: u64,
) -> anyhow::Result<PumpOutcome> {
```

Phase 15 adds a `predict_mode: PredictDisplayMode` parameter (or constructs `PredictionOverlay` inside `run_pump` from the mode). The `#[allow(clippy::too_many_arguments)]` pattern already present covers the new arg.

---

### `crates/nosh-client/src/lib.rs` (modify config)

**Analog:** self (existing lib.rs)

**Module declaration pattern** (lib.rs lines 4-6):
```rust
pub mod client;
pub mod platform;
pub mod screen; // NEW: ClientScreen, Overlay, ConnectionLossOverlay
```

Phase 15 adds:
```rust
pub mod predictor; // NEW: PredictionOverlay, PendingPrediction, Validity, InputAction
```

---

### `crates/nosh-client/Cargo.toml` (modify config)

**Analog:** self (existing Cargo.toml lines 14-33)

**Dependency addition pattern** (Cargo.toml lines 14-33):
```toml
[dependencies]
nosh-proto = { path = "../nosh-proto" }
nosh-auth = { path = "../nosh-auth" }
quinn = { workspace = true }
rustls = { workspace = true }
tokio = { workspace = true, features = ["signal", "io-std"] }
clap = { workspace = true }
anyhow = { workspace = true }
tracing = { workspace = true }
tracing-subscriber = { workspace = true }
bytes = { workspace = true }
ssh-key = { version = "0.6", default-features = false, features = ["ed25519", "std", "alloc"] }
crossterm = { version = "0.29", features = ["events"] }
dirs = "5"
```

Phase 15 adds immediately after `dirs`:
```toml
unicode-width = "0.2"
```

No workspace entry is needed (first use of this crate in the workspace). Place in `[dependencies]`, not `[dev-dependencies]` — `unicode-width` is used in the production `predictor.rs`, not only in tests.

---

### `crates/nosh-client/tests/predict.rs` (new test, event-driven)

**Analog:** `crates/nosh-client/tests/render.rs`

**Test file header pattern** (render.rs lines 1-28):
```rust
//! Phase 14 render tests — confirmed screen matches server TerminalState (PREDICT-01).
//!
//! Covers two success criteria from PREDICT-01:
//! - [criterion 1]
//! - [criterion 2]

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
```

For `predict.rs`, adapt to:
```rust
//! Phase 15 prediction tests — speculative overlay (PREDICT-02 through PREDICT-06).
//!
//! D-15-04 validation matrix: all adversarial cases must pass before phase is done.

use nosh_client::screen::{Cell, ClientScreen};
use nosh_client::predictor::{PredictionOverlay, PredictDisplayMode};
use nosh_proto::datagram::{CellStyle, CursorPos, DiffRun, StateDiff, encode_datagram, decode_datagram};
```

**Pure unit test helper pattern** (render.rs lines 70-245 — `confirmed_grid_matches_terminal_state_after_diff`):
```rust
// Pattern: construct structs directly (no QUIC), feed them data, assert grid state.
#[test]
fn test_case_name() {
    // Build structs under test directly (no spawned server).
    let mut screen = ClientScreen::new(80, 24);
    let mut predictor = PredictionOverlay::new(PredictDisplayMode::Always);

    // Feed fake keystrokes.
    predictor.on_input(b"hello", &screen);

    // Feed fake StateDiff to confirm/cull.
    let diff = make_diff(1, "hello", CursorPos { row: 0, col: 5 });
    screen.apply(&diff);
    predictor.cull(&screen, diff.epoch, 50); // rtt_ms = 50 (above FLAG_TRIGGER_HIGH)

    // Assert overlay cell state.
    assert!(predictor.cell_at(0, 0).is_some(), "predicted cell must be visible");
    assert_eq!(predictor.confirmed_epoch(), 1);
}
```

**Integration test async pattern** (render.rs lines 324-399):
```rust
#[tokio::test]
async fn render_integration_client_screen_matches_server_output() {
    if !have_sh() {
        eprintln!("skipping ...: {SH} not available");
        return;
    }

    let registry = SessionRegistry::new(5, Duration::from_secs(30));
    let client_key = TestKey::generate();
    let server = server_with_key(registry.clone(), &client_key).await;
    let (ep, _dir) = client_endpoint_for(&client_key);
    let conn = client::connect(&ep, server.addr, HOST, Duration::from_secs(30))
        .await.expect("connect");

    // ... drive session, collect datagrams, assert ...

    loop {
        match tokio::time::timeout(deadline, conn.read_datagram()).await {
            Ok(Ok(bytes)) => {
                if let Ok(diff) = decode_datagram(&bytes) {
                    screen.apply(&diff);
                }
                // assert condition ...
            }
            Ok(Err(e)) => panic!("connection error: {e}"),
            Err(_) => panic!("timed out after 5s — ..."),
        }
    }
}
```

**Server spawn helper pattern** (render.rs lines 35-48):
```rust
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
```

Copy exactly; `predict.rs` needs the same server spawn helper for the end-to-end `read -s` and simulated-loss tests (D-15-04 cases that require a live server).

**Client endpoint helper pattern** (render.rs lines 50-56):
```rust
fn client_endpoint_for(key: &TestKey) -> (quinn::Endpoint, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let kh = dir.path().join("known_hosts");
    let ep = common::client_endpoint(key.client_identity(), kh).unwrap();
    (ep, dir)
}
```

Copy exactly for `predict.rs`.

---

## Shared Patterns

### Overlay trait contract
**Source:** `crates/nosh-client/src/screen.rs` lines 78-82
**Apply to:** `predictor.rs` (`PredictionOverlay` implements `Overlay`)
```rust
pub trait Overlay {
    fn cell_at(&self, row: u16, col: u16) -> Option<Cell>;
}
```
`PredictionOverlay::cell_at` returns `Some(Cell)` with optional `UNDERLINE` style for non-tentative active predictions, `None` otherwise.

### render_to_stdout stdout-flush pattern
**Source:** `crates/nosh-client/src/main.rs` lines 687-699
**Apply to:** Both stdin arm and datagram arm additions in `main.rs`
```rust
let mut buf: Vec<u8> = Vec::new();
screen.render_to_stdout(&mut buf).unwrap_or_else(|e| {
    tracing::warn!("render_to_stdout error: {e}");
});
if !buf.is_empty() {
    if let Err(e) = stdout.write_all(&buf).await {
        tracing::warn!("stdout write_all failed: {e} — forcing full repaint");
        screen.reset_physical();
    } else if let Err(e) = stdout.flush().await {
        tracing::warn!("stdout flush failed: {e} — forcing full repaint");
        screen.reset_physical();
    }
}
```

### CellStyle bitflag usage
**Source:** `crates/nosh-client/src/screen.rs` lines 415-436, `crates/nosh-proto/src/datagram.rs` lines 128-140
**Apply to:** `predictor.rs` when building a `Cell` for an unconfirmed prediction
```rust
// Underline when flagging (RTT > FLAG_TRIGGER_HIGH_MS), plain otherwise.
let style_bits = if self.flagging {
    CellStyle(CellStyle::UNDERLINE)
} else {
    CellStyle(CellStyle::NONE)
};
Cell { ch: pred.predicted_ch, style: style_bits, fg: None, bg: None }
```

### TransportDrop / UserQuit return pattern
**Source:** `crates/nosh-client/src/main.rs` lines 729-735
**Apply to:** `main.rs` additions in stdin arm (send_input failure after predictor hook)
```rust
if client::send_input(send, &result.bytes_to_forward).await.is_err() {
    return Ok(PumpOutcome::TransportDrop);
}
```

### Monotonic epoch guard
**Source:** `crates/nosh-client/src/screen.rs` lines 166-168
**Apply to:** `predictor.cull()` — process only diffs with `new_epoch > last_applied_epoch` (already enforced by `screen.apply()` before `cull()` is called; `cull()` receives the already-applied epoch)
```rust
if diff.epoch <= self.last_applied_epoch {
    return;
}
```

### tracing-instrument pattern for async context
**Source:** `crates/nosh-client/src/main.rs` — consistent use of `tracing::warn!` with structured fields
**Apply to:** `predictor.rs` public methods
```rust
tracing::warn!("render_to_stdout error: {e}");
tracing::warn!(epoch = diff.epoch, "datagram channel error: {e}");
```

---

## No Analog Found

All files have close analogs. No files require falling back to RESEARCH.md-only patterns.

However, the following sub-patterns within `predictor.rs` are novel to this codebase (no existing analog — use RESEARCH.md patterns directly):

| Sub-pattern | Role | Reason |
|-------------|------|--------|
| `Validity` enum state machine | service | No existing epoch-tracking state machine in codebase |
| `InputAction` classifier (`classify_input`) | utility | No existing byte-level input classifier (escape machine is higher-level) |
| RTT hysteresis thresholds (`update_rtt_thresholds`) | utility | First use of `conn.rtt()` for adaptive display logic |
| `become_tentative()` / `kill_epoch()` | service | No existing prediction-epoch bookkeeping |

For these sub-patterns, use the RESEARCH.md code examples directly (they are traceable to Mosh primary source).

---

## Metadata

**Analog search scope:** `crates/nosh-client/src/`, `crates/nosh-client/tests/`, `crates/nosh-proto/src/`
**Files scanned:** 10 source files read in full
**Pattern extraction date:** 2026-06-02
