# Phase 16: QoL Feature Pack + Windows CI Gate - Pattern Map

**Mapped:** 2026-06-02
**Files analyzed:** 8 (6 modified, 1 new source, 1 new CI)
**Analogs found:** 8 / 8

## File Classification

| New/Modified File | Role | Data Flow | Closest Analog | Match Quality |
|-------------------|------|-----------|----------------|---------------|
| `crates/nosh-server/src/terminal.rs` | model + utility | event-driven (vte Perform) | self — extend existing `osc_dispatch` + add drain methods | exact |
| `crates/nosh-proto/src/messages.rs` | model | request-response (postcard codec) | self — append new enum variant | exact |
| `crates/nosh-client/src/screen.rs` | component | event-driven (overlay render) | `crates/nosh-client/src/predictor.rs` `PredictionOverlay` | exact |
| `crates/nosh-client/src/main.rs` | controller | request-response + event-driven | self — extend `run_pump` select! arms + `Args` struct | exact |
| `crates/nosh-server/Cargo.toml` | config | — | self — change `vte` dep line | exact |
| `crates/nosh-client/Cargo.toml` | config | — | self — add `osc52` feature to `crossterm` | exact |
| `.github/workflows/ci.yml` | config (CI) | — | `.github/workflows/windows-cross.yml` | role-match |
| `crates/nosh-client/tests/` + `crates/nosh-server/src/terminal.rs` (tests) | test | — | `tests/render.rs`, `terminal.rs` `#[cfg(test)]` block | exact |

---

## Pattern Assignments

### `crates/nosh-server/src/terminal.rs` — osc_dispatch security gate + drain methods

**Analog:** self (`crates/nosh-server/src/terminal.rs`)

**Existing fields** (lines 159–163):
```rust
/// Window/icon title set by OSC 0 or OSC 2.
title: Option<String>,
/// Last parsed OSC 52 clipboard-write payload (D-12-04 — detection only;
/// forwarding is Phase 16). Replaced on each new OSC 52 sequence.
osc52_pending: Option<(Vec<u8>, Vec<u8>)>,
```
Both already use `Option<T>` — `.take()` drain semantics fit naturally.

**Existing accessor pattern** (lines 358–372):
```rust
pub fn title(&self) -> Option<&str> {
    self.title.as_deref()
}

pub fn osc52_pending(&self) -> Option<(&[u8], &[u8])> {
    self.osc52_pending
        .as_ref()
        .map(|(sel, data)| (sel.as_slice(), data.as_slice()))
}
```
Phase 16 adds `take_title() -> Option<String>` and `take_osc52() -> Option<(Vec<u8>, Vec<u8>)>` using the same `Option::take()` idiom.

**Existing osc_dispatch core** (lines 637–663):
```rust
fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
    if params.is_empty() {
        return;
    }
    match params[0] {
        b"0" | b"2" => {
            if let Some(title_bytes) = params.get(1) {
                if let Ok(title) = std::str::from_utf8(title_bytes) {
                    self.title = Some(title.to_owned());
                }
            }
        }
        b"52" => {
            let selection = params.get(1).copied().unwrap_or(b"c");
            let data = params.get(2).copied().unwrap_or(b"");
            self.osc52_pending = Some((selection.to_vec(), data.to_vec()));
        }
        _ => {}
    }
}
```

**Phase 16 changes to `b"52"` arm** — add security gate + cap before storing:
```rust
b"52" => {
    let selection = params.get(1).copied().unwrap_or(b"c");
    let data = params.get(2).copied().unwrap_or(b"");

    // SECURITY (D-16-01a): silently drop OSC 52 read/query form.
    // The read form sends '?' as the data parameter. Honoring it would
    // let a remote process exfiltrate the LOCAL clipboard.
    if data == b"?" {
        return;
    }

    // Cap retained payload (approach b, D-16-01c). Accompanies vte std
    // re-enable in Cargo.toml — without this cap, CR-03 unbounded-OSC DoS
    // is reintroduced.
    const OSC_52_MAX_BYTES: usize = 65_536; // 64 KiB
    let data = &data[..data.len().min(OSC_52_MAX_BYTES)];

    self.osc52_pending = Some((selection.to_vec(), data.to_vec()));
}
```

**Phase 16 change to `b"0" | b"2"` arm** — add title length cap:
```rust
b"0" | b"2" => {
    if let Some(title_bytes) = params.get(1) {
        if let Ok(title) = std::str::from_utf8(title_bytes) {
            const MAX_TITLE_BYTES: usize = 1024;
            if title.len() <= MAX_TITLE_BYTES {
                self.title = Some(title.to_owned());
            }
        }
    }
}
```

**New drain methods** — copy the `Option::take` idiom, no analog needed:
```rust
/// Take and clear the pending OSC 52 payload for forwarding over reliable stream.
pub fn take_osc52(&mut self) -> Option<(Vec<u8>, Vec<u8>)> {
    self.osc52_pending.take()
}

/// Take and clear the pending window title for forwarding over reliable stream.
pub fn take_title(&mut self) -> Option<String> {
    self.title.take()
}
```

**Existing adversarial test template** (lines 1436–1509) — the two `adversarial_large_osc_*` tests MUST be updated: their `<= 1024` assertions become `<= MAX_TITLE_BYTES` / `<= OSC_52_MAX_BYTES` respectively after vte `std` re-enable.

---

### `crates/nosh-proto/src/messages.rs` — new `TerminalControl` variant

**Analog:** self (`crates/nosh-proto/src/messages.rs`, lines 21–153)

**Discriminant ordering invariant** (lines 56–62, comment):
```
// These five variants are appended AFTER `SessionClose` to preserve the
// postcard discriminant order of all existing variants. Inserting or
// reordering is NOT backward-compatible.
```
The new `TerminalControl` variant MUST be appended after `Ack` (discriminant 8 → new discriminant 9).

**Existing variant pattern** (lines 148–153):
```rust
Ack {
    /// Next-expected-seq == count of output chunks the client has applied.
    seq: u64,
},
```

**Serde/postcard derive** (line 20):
```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Message {
```

**`variant_name` match arm pattern** (lines 160–172) — every new variant needs a branch here:
```rust
pub fn variant_name(&self) -> &'static str {
    match self {
        Message::SessionOpen { .. } => "SessionOpen",
        // ... existing arms ...
        Message::Ack { .. } => "Ack",
        // ADD:
        Message::TerminalControl(_) => "TerminalControl",
    }
}
```

**New variant and payload type to append**:
```rust
/// Server → client: out-of-band terminal control passthrough (D-16-01, D-16-02).
/// Carries OSC 52 clipboard writes and OSC 0/2 title updates over the reliable
/// stream (no MTU limit). Client re-emits directly to stdout — NOT through
/// the compositor (D-16-01).
TerminalControl(TerminalControlPayload),
```

```rust
/// Payload for `Message::TerminalControl`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TerminalControlPayload {
    /// OSC 52 clipboard write (WRITE-ONLY — read/query form is never forwarded, D-16-01a).
    Clipboard {
        selection: Vec<u8>,
        data: Vec<u8>,
    },
    /// Terminal title from OSC 0/2.
    Title { title: String },
}
```

---

### `crates/nosh-client/src/screen.rs` — ConnectionLossOverlay activation

**Analog:** `crates/nosh-client/src/predictor.rs` — `PredictionOverlay` as an external-mutation overlay passed to render, NOT stored in `overlays` Vec.

**Key insight from RESEARCH.md Pitfall 3 + screen.rs line 339–363:**
`PredictionOverlay` is NOT in the `overlays` Vec. It is mutably owned by `run_pump` and passed by shared ref to `render_with_predictor`. `ConnectionLossOverlay` must follow the same pattern.

**Existing no-op stub to replace** (lines 86–96):
```rust
pub struct ConnectionLossOverlay;

impl Overlay for ConnectionLossOverlay {
    fn cell_at(&self, _row: u16, _col: u16) -> Option<Cell> {
        None // no-op this phase
    }
}
```

**Overlay trait to implement** (lines 80–84):
```rust
pub trait Overlay {
    fn cell_at(&self, row: u16, col: u16) -> Option<Cell>;
}
```

**PredictionOverlay cell_at pattern** (predictor.rs lines 575–597) — return `Some(Cell { ... })` with specific style bits, `None` when inactive:
```rust
fn cell_at(&self, row: u16, col: u16) -> Option<Cell> {
    if !self.active || row != 0 {
        return None;
    }
    // ... build cell for banner row 0 ...
    Some(Cell {
        ch,
        style: CellStyle(CellStyle::REVERSE), // reverse-video banner
        fg: None,
        bg: None,
    })
}
```

**Cell and CellStyle** (screen.rs lines 51–71) — already imported; `CellStyle::REVERSE` bitmask at screen.rs line 507.

**render_with_predictor pattern** (screen.rs lines 343–364) — analog for adding a second explicit overlay parameter:
```rust
pub fn render_with_predictor<W: Write>(
    &mut self,
    out: &mut W,
    predictor: &PredictionOverlay,
) -> std::io::Result<()> {
    let mut desired = self.compose_desired();
    for (r, row_cells) in desired.iter_mut().enumerate() {
        for (c, cell_slot) in row_cells.iter_mut().enumerate() {
            if let Some(cell) = predictor.cell_at(r as u16, c as u16) {
                *cell_slot = cell;
            }
        }
    }
    let desired_cursor = predictor.predicted_cursor().unwrap_or(self.confirmed_cursor);
    self.emit_diff(out, &desired, desired_cursor)?;
    Ok(())
}
```
Phase 16 extends this (or adds a new method) to also apply `ConnectionLossOverlay` as an additional layer. One approach: extend `render_with_predictor` signature to accept `Option<&ConnectionLossOverlay>` and apply it BEFORE the predictor layer (so the predictor renders on top of the banner).

**`ClientScreen::new` — remove ConnectionLossOverlay from overlays Vec** (line 139):
```rust
// Current (Phase 14):
overlays: vec![Box::new(ConnectionLossOverlay)],
// Phase 16: ConnectionLossOverlay is hoisted to run_pump scope
overlays: vec![],
```

---

### `crates/nosh-client/src/main.rs` — datagram-silence timer + --status + OSC re-emit + WSAEMSGSIZE filter

**Analog:** self (`crates/nosh-client/src/main.rs`)

**Args struct to extend** (lines 283–326) — copy the `--predict` field pattern:
```rust
/// Speculative-echo prediction mode (PREDICT-05, D-15-02).
#[arg(long, default_value = "adaptive")]
predict: PredictDisplayMode,
```
Add `--status` using the same `#[arg(long)]` idiom:
```rust
/// Display measured RTT in the terminal title (QOL-04).
/// Updates the title on each datagram with "nosh: Nms".
/// When active, forwarded OSC 0/2 titles are suppressed.
#[arg(long)]
status: bool,
```

**tracing subscriber init to extend** (lines 406–411):
```rust
// Current:
tracing_subscriber::fmt()
    .with_env_filter(
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
    )
    .with_writer(std::io::stderr)
    .init();
```
Phase 16 wraps with `#[cfg(target_os = "windows")]`-gated directive — copy the `EnvFilter::try_from_default_env().unwrap_or_else` pattern:
```rust
// HARDEN-03: Suppress quinn_udp WSAEMSGSIZE WARN on Windows.
// The warning fires when Windows GRO receive path appends UDP_COALESCED_INFO
// to the control buffer and the buffer is too small (128 bytes). The datagram
// is NOT lost — only GRO metadata is missing.
// Upstream: quinn-rs/quinn#2041 (open as of 2026-06).
// Suppressed at WARN level: quinn_udp=error. Only quinn_udp WARN is suppressed,
// not quinn WARN (connection/auth errors remain visible).
#[cfg(target_os = "windows")]
let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
    .unwrap_or_else(|_| "info".into())
    .add_directive("quinn_udp=error".parse().unwrap());

#[cfg(not(target_os = "windows"))]
let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
    .unwrap_or_else(|_| "info".into());

tracing_subscriber::fmt()
    .with_env_filter(env_filter)
    .with_writer(std::io::stderr)
    .init();
```

**run_pump signature to extend** (lines 622–632):
```rust
async fn run_pump(
    conn: &quinn::Connection,
    cols: u16,
    rows: u16,
    send: &mut quinn::SendStream,
    recv: &mut quinn::RecvStream,
    highest_applied: &mut u64,
    resize: &mut platform::ResizeWatcher,
    _seq_baseline: u64,
    predict_mode: PredictDisplayMode,
) -> anyhow::Result<PumpOutcome>
```
Add `status: bool` parameter.

**Silence timer — copy the `resize_deadline` / `resize_sleep` async-block pattern** (lines 636, 664–669):
```rust
// Existing resize deadline pattern:
let mut resize_deadline: Option<tokio::time::Instant> = None;
// ...
let resize_sleep = async {
    match resize_deadline {
        Some(d) => tokio::time::sleep_until(d).await,
        None => std::future::pending::<()>().await,
    }
};
```
The silence timer uses the same `sleep_until`-or-`pending` idiom:
```rust
let mut last_datagram_time: tokio::time::Instant = tokio::time::Instant::now();
let mut loss_overlay_active = false;
// In the select! loop:
let silence_check = async {
    tokio::time::sleep_until(last_datagram_time + std::time::Duration::from_secs(5)).await
};
```

**Datagram arm — existing conn.rtt() call** (lines 703–766):
```rust
datagram = conn.read_datagram() => {
    match datagram {
        Ok(bytes) => {
            if let Ok(diff) = nosh_proto::datagram::decode_datagram(&bytes) {
                if diff.epoch > screen.last_applied_epoch() {
                    // ...
                    let rtt_ms = conn.rtt().as_millis() as u64;  // line 714
                    // ...
                    let mut buf: Vec<u8> = Vec::new();
                    screen.render_with_predictor(&mut buf, &predictor).unwrap_or_else(|e| {
                        tracing::warn!("render_with_predictor error: {e}");
                    });
                    if !buf.is_empty() {
                        if let Err(e) = stdout.write_all(&buf).await {
                            // ...
                        } else if let Err(e) = stdout.flush().await {
                            // ...
                        }
                    }
```
Phase 16 adds after `render_with_predictor`:
1. Reset `last_datagram_time = tokio::time::Instant::now()` + clear overlay if active
2. If `status` flag: emit `\x1b]0;nosh: {rtt_ms}ms\x07` directly to stdout

**Reliable-stream arm — existing pattern** (lines 673–698):
```rust
msg = nosh_proto::read_message(recv) => {
    match msg {
        Ok(Message::PtyData { data }) => { /* ... */ }
        Ok(Message::SessionClose { exit_code: code, .. }) => { /* ... */ }
        Ok(Message::SessionOpened { .. }) => { /* ignore */ }
        Ok(_) => {} // ignore other control frames
        Err(e) => {
            tracing::warn!("reliable stream error, triggering reconnect: {e}");
            return Ok(PumpOutcome::TransportDrop);
        }
    }
}
```
Phase 16 adds a match arm (replacing the `Ok(_) => {}` catch-all):
```rust
Ok(Message::TerminalControl(payload)) => {
    match payload {
        TerminalControlPayload::Clipboard { selection, data } => {
            // Out-of-band: write OSC 52 directly to stdout (NOT compositor).
            let sel = String::from_utf8_lossy(&selection);
            let b64 = String::from_utf8_lossy(&data);
            let _ = stdout.write_all(
                format!("\x1b]52;{sel};{b64}\x07").as_bytes()
            ).await;
            let _ = stdout.flush().await;
        }
        TerminalControlPayload::Title { title } => {
            // Only re-emit if --status is not active (status title takes precedence).
            if !status {
                let _ = stdout.write_all(
                    format!("\x1b]0;{title}\x07").as_bytes()
                ).await;
                let _ = stdout.flush().await;
            }
        }
    }
}
```

**ConnectionLossOverlay in run_pump** — mirror `predictor` ownership (line 654):
```rust
// Existing predictor pattern (analog):
let mut predictor = PredictionOverlay::new(predict_mode, cols, rows);

// Phase 16 — parallel pattern:
let mut loss_overlay = ConnectionLossOverlay::new(cols);
```
Passed to `render_with_predictor` (or extended render method) by shared ref, same as predictor.

**stdout write pattern** (lines 753–761) — the `write_all + flush` error handling pattern for OSC re-emission:
```rust
if let Err(e) = stdout.write_all(&buf).await {
    tracing::warn!("stdout write_all failed: {e} — forcing full repaint");
    screen.reset_physical();
} else if let Err(e) = stdout.flush().await {
    tracing::warn!("stdout flush failed: {e} — forcing full repaint");
    screen.reset_physical();
}
```
OSC re-emissions are best-effort (no `reset_physical` on error — they are control sequences, not display state).

---

### `crates/nosh-server/Cargo.toml` — vte dependency change

**Analog:** self (line 52)

**Current**:
```toml
# CR-03: Disable vte's "std" feature ...
vte = { version = "0.15", default-features = false }
```

**Phase 16 replacement** (approach b, D-16-01c):
```toml
# Phase 16 (D-16-01c approach b): re-enable vte "std" to support OSC 52
# clipboard payloads larger than the 1024-byte ArrayVec cap. The CR-03
# unbounded-OSC DoS is re-mitigated by explicit size caps in osc_dispatch:
#   - MAX_TITLE_BYTES = 1024 for OSC 0/2 titles
#   - OSC_52_MAX_BYTES = 65536 (64 KiB) for OSC 52 clipboard data
# The transient Vec<u8> during vte parsing is bounded by the OS pipe buffer
# (typically 64 KiB) and QUIC stream chunk size — not unbounded.
# See .planning/phases/16-qol-feature-pack-windows-ci-gate/16-RESEARCH.md.
vte = { version = "0.15" }
```

---

### `crates/nosh-client/Cargo.toml` — crossterm osc52 feature

**Analog:** self (line 32)

**Current**:
```toml
crossterm = { version = "0.29", features = ["events"] }
```

**Phase 16** (add `osc52` feature for `CopyToClipboard` module availability; raw OSC write is also acceptable without it):
```toml
crossterm = { version = "0.29", features = ["events", "osc52"] }
```
Note from RESEARCH.md: the raw `"\x1b]52;{sel};{b64}\x07"` write is equally correct and simpler. The `osc52` feature makes `crossterm::clipboard::CopyToClipboard` available but is optional. If the planner chooses raw write, the feature addition may be skipped.

---

### `.github/workflows/ci.yml` (NEW) — replaces `windows-cross.yml`

**Analog:** `.github/workflows/windows-cross.yml`

**Existing workflow structure** (windows-cross.yml lines 12–64):
```yaml
on:
  push:
    branches: [main]
  pull_request:
    branches: [main]

jobs:
  windows-cross-check:
    name: cargo check --target x86_64-pc-windows-gnu
    runs-on: ubuntu-latest
    steps:
      - name: Checkout
        uses: actions/checkout@v4
      - name: Install Rust stable
        uses: dtolnay/rust-toolchain@stable
        with:
          targets: x86_64-pc-windows-gnu
      # ...MinGW setup (NOT needed for windows-latest native)...
      - name: Cache cargo registry
        uses: actions/cache@v4
        # ...
      - name: cargo check nosh-client for Windows
        run: cargo check -p nosh-client --target x86_64-pc-windows-gnu
```

**Phase 16 replacement** — two jobs, `cargo build` not `cargo check`, `windows-latest` not `ubuntu-latest` for the Windows job:
```yaml
name: CI

on:
  push:
    branches: [main]
  pull_request:
    branches: [main]

jobs:
  linux:
    name: cargo test (Linux)
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - name: Build
        run: cargo build --locked
      - name: Test
        run: cargo test --locked

  build-windows:
    name: cargo build nosh-client (Windows MSVC, HARDEN-02)
    runs-on: windows-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          targets: x86_64-pc-windows-msvc
      - uses: Swatinem/rust-cache@v2
      - name: Build Windows client
        # Only nosh-client: nosh-server has portable-pty + nix (Unix-only).
        # nosh-auth is built transitively as a dep of nosh-client.
        # windows-latest (Windows Server 2022) has MSVC Build Tools pre-installed.
        # ring 0.17.x ships precompiled x86_64-windows asm objects; no NASM needed.
        run: cargo build --locked --target x86_64-pc-windows-msvc -p nosh-client
```

**Key differences from analog:**
- `runs-on: windows-latest` (not `ubuntu-latest`) — native MSVC, no MinGW `apt-get` step
- `cargo build` not `cargo check` — catches linker/ABI issues (D-16-04)
- `x86_64-pc-windows-msvc` not `x86_64-pc-windows-gnu` — MSVC ABI for real Windows
- `Swatinem/rust-cache@v2` not `actions/cache@v4` — standard Rust CI caching action

---

### `crates/nosh-client/tests/` + `crates/nosh-server/src/terminal.rs` tests

**Analog:** `crates/nosh-client/tests/render.rs` + `crates/nosh-server/src/terminal.rs` `#[cfg(test)]` block

**Test harness pattern** (tests/common/mod.rs lines 1–60):
```rust
// Shared test harness: TestKey::generate(), spawn_server_with_registry(),
// client_endpoint(), HOST constant.
mod common;
use common::{spawn_server_with_registry, TestKey, HOST};
```

**Unit test pattern in terminal.rs** (lines 1000–1029):
```rust
#[test]
fn osc52_detected_and_no_clipboard_action() {
    let mut state = ts(80, 24); // ts() is the local helper: TerminalState::new
    state.advance(b"\x1b]52;c;SGVsbG8=\x07");
    let pending = state.osc52_pending();
    assert!(pending.is_some(), "OSC 52 must be detected");
    let (sel, data) = pending.unwrap();
    assert_eq!(sel, b"c");
    assert_eq!(data, b"SGVsbG8=");
}
```

**Adversarial test pattern** (lines 1436–1509) — tests to UPDATE (cap values change after vte std re-enable):
```rust
// BEFORE (Phase 12 cap = vte's 1024-byte ArrayVec):
assert!(title.len() <= 1024, ...);
assert!(data.len() <= 1024, ...);

// AFTER (Phase 16 explicit caps in osc_dispatch):
assert!(title.len() <= MAX_TITLE_BYTES, ...);  // MAX_TITLE_BYTES = 1024
assert!(data.len() <= OSC_52_MAX_BYTES, ...);  // OSC_52_MAX_BYTES = 65536
```

**New unit tests to add in terminal.rs** — follow `osc52_detected_and_no_clipboard_action` pattern:
```rust
#[test]
fn osc52_read_form_is_silently_dropped() {
    let mut state = ts(80, 24);
    state.advance(b"\x1b]52;c;?\x07"); // read/query form
    assert!(state.osc52_pending().is_none(), "OSC 52 read form must not be stored");
}

#[test]
fn take_osc52_drains_and_clears() {
    let mut state = ts(80, 24);
    state.advance(b"\x1b]52;c;SGVsbG8=\x07");
    let taken = state.take_osc52();
    assert!(taken.is_some());
    assert!(state.osc52_pending().is_none(), "take_osc52 must clear the field");
}

#[test]
fn take_title_drains_and_clears() {
    let mut state = ts(80, 24);
    state.advance(b"\x1b]2;My Title\x07");
    let taken = state.take_title();
    assert_eq!(taken.as_deref(), Some("My Title"));
    assert!(state.title().is_none(), "take_title must clear the field");
}
```

**Overlay unit test pattern** (screen.rs lines 796–802) — template for ConnectionLossOverlay tests:
```rust
#[test]
fn connection_loss_overlay_is_noop() {
    let overlay = ConnectionLossOverlay;
    assert!(overlay.cell_at(0, 0).is_none());
    assert!(overlay.cell_at(23, 79).is_none());
    assert!(overlay.cell_at(999, 999).is_none());
}
```
Phase 16 adds an `active` variant test following the same structure.

**Integration test pattern** (tests/render.rs lines 30–56) — server + client connected via real QUIC, for OSC passthrough integration tests. Uses `common::spawn_server_with_registry` + `common::client_endpoint`.

---

## Shared Patterns

### Postcard discriminant ordering (append-only)
**Source:** `crates/nosh-proto/src/messages.rs` lines 56–62 (comment)
**Apply to:** `messages.rs` new `TerminalControl` variant
New variants MUST be appended after all existing variants. Never insert or reorder. `TerminalControl` gets discriminant 9 (after `Ack` at 8).

### tokio::select! arm structure
**Source:** `crates/nosh-client/src/main.rs` lines 663–779
**Apply to:** `main.rs` silence timer arm
Pattern: define an async block outside the `select!` for conditional sleep (`sleep_until` or `pending`), then reference it as a named arm inside `tokio::select!`.

### out-of-band stdout write (control sequences bypass compositor)
**Source:** `crates/nosh-client/src/main.rs` lines 753–761 (stdout.write_all + flush error handling)
**Apply to:** OSC 52 re-emit, OSC 0/2 title re-emit, `--status` RTT title emit
```rust
// Out-of-band OSC writes are best-effort (no reset_physical on error).
let _ = stdout.write_all(osc_bytes).await;
let _ = stdout.flush().await;
```
These are NOT routed through `render_with_predictor` — they are control sequences, not display cells.

### `#[cfg(target_os = "windows")]` platform gating
**Source:** `crates/nosh-client/src/main.rs` lines 344–349 (--identity warning), `crates/nosh-client/Cargo.toml` lines 36–48 (platform-conditional deps)
**Apply to:** WSAEMSGSIZE tracing filter in `main()`
Pattern: use `#[cfg]`-gated `let` bindings, not `if cfg!(...)` at runtime.

### Option::take drain semantics
**Source:** `crates/nosh-server/src/terminal.rs` lines 358–372 (existing accessors)
**Apply to:** `take_osc52()` and `take_title()` drain methods
`Option::take()` atomically reads and clears — prevents double-forwarding without a separate `clear()` call.

---

## No Analog Found

All files have close analogs in the codebase.

| File | Role | Data Flow | Note |
|------|------|-----------|------|
| — | — | — | All patterns have existing analogs |

---

## Metadata

**Analog search scope:** `crates/nosh-client/src/`, `crates/nosh-server/src/`, `crates/nosh-proto/src/`, `.github/workflows/`, `crates/nosh-client/tests/`
**Files scanned:** 14
**Pattern extraction date:** 2026-06-02
