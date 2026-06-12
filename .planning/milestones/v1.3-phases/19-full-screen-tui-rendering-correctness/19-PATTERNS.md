# Phase 19: Full-Screen TUI Rendering Correctness - Pattern Map

**Mapped:** 2026-06-07
**Files analysed:** 8 new/modified files
**Analogs found:** 8 / 8

---

## File Classification

| New/Modified File | Role | Data Flow | Closest Analog | Match Quality |
|-------------------|------|-----------|----------------|---------------|
| `crates/nosh-server/src/terminal.rs` | model | transform | itself (existing) | self-modification |
| `crates/nosh-server/Cargo.toml` | config | — | `crates/nosh-client/Cargo.toml` | role-match |
| `crates/nosh-proto/src/datagram.rs` | model | request-response | itself (existing) | self-modification |
| `crates/nosh-server/src/server.rs` | service | CRUD | itself (existing) | self-modification |
| `crates/nosh-client/src/screen.rs` | component | transform | itself (existing) | self-modification |
| `crates/nosh-client/src/predictor.rs` | component | event-driven | itself (existing) | self-modification |
| `crates/nosh-client/src/main.rs` | controller | request-response | itself (existing) | self-modification |
| `fuzz/fuzz_targets/osc_accumulation.rs` | test | batch | itself (existing) | self-modification |

---

## Pattern Assignments

### `crates/nosh-server/src/terminal.rs` — two-grid alt-screen, wide-char, OSC pre-bound, scroll_up gate

**Primary analog:** itself — all changes are extensions to existing patterns.

**Existing struct-field addition pattern** (`terminal.rs` lines 161–185):
```rust
pub struct TerminalState {
    cols: u16,
    rows: u16,
    grid: Vec<Vec<Cell>>,
    scrollback: VecDeque<Vec<Cell>>,
    cursor: CursorPos,
    echo_state: EchoState,
    title: Option<String>,
    osc52_pending: Option<(Vec<u8>, Vec<u8>)>,
    parser: vte::Parser,
    sgr: SgrState,
    // NEW FIELDS follow the same private-field pattern:
    // saved_primary: Option<(Vec<Vec<Cell>>, CursorPos, SgrState)>,
    // osc_byte_count: usize,
}
```
Fields are private (no `pub`). New fields initialised in `TerminalState::new` (lines 192–206) and reset in `esc_dispatch` RIS (lines 743–751).

**`new()` constructor initialisation pattern** (`terminal.rs` lines 192–206):
```rust
pub fn new(cols: u16, rows: u16) -> Self {
    let grid = Self::make_grid(cols, rows);
    TerminalState {
        cols,
        rows,
        grid,
        scrollback: VecDeque::new(),
        cursor: CursorPos { row: 0, col: 0 },
        echo_state: EchoState::default(),
        title: None,
        osc52_pending: None,
        parser: vte::Parser::default(),
        sgr: SgrState::default(),
        // New fields added here:
        // saved_primary: None,
        // osc_byte_count: 0,
    }
}
```

**`make_grid` helper** (`terminal.rs` lines 209–212) — copy for `enter_alt_screen`:
```rust
fn make_grid(cols: u16, rows: u16) -> Vec<Vec<Cell>> {
    (0..rows as usize)
        .map(|_| vec![Cell::default(); cols as usize])
        .collect()
}
```

**`advance` borrow-split pattern** (`terminal.rs` lines 222–226) — the OSC pre-filter must wrap this, not replace it:
```rust
pub fn advance(&mut self, bytes: &[u8]) {
    let mut parser = std::mem::take(&mut self.parser);
    parser.advance(self, bytes);
    self.parser = parser;
}
```
The new pre-filter computes a `bytes_to_feed` slice (possibly truncated), then calls the same `std::mem::take` pattern. After overflow truncation, replace parser with `vte::Parser::default()` and reset `osc_byte_count = 0`.

**`resize` row/column extend-or-truncate pattern** (`terminal.rs` lines 236–275) — copy verbatim for `saved_primary` resize:
```rust
pub fn resize(&mut self, cols: u16, rows: u16) {
    for row in &mut self.grid {
        let current_len = row.len();
        let new_len = cols as usize;
        if current_len > new_len {
            row.truncate(new_len);
        } else if current_len < new_len {
            row.resize(new_len, Cell::default());
        }
    }
    let current_rows = self.grid.len();
    let new_rows = rows as usize;
    if current_rows > new_rows {
        let excess = current_rows - new_rows;
        for _ in 0..excess {
            let top_row = self.grid.remove(0);
            self.scrollback.push_back(top_row);
            if self.scrollback.len() > SCROLLBACK_LINE_CAP {
                self.scrollback.pop_front();
            }
        }
    } else if current_rows < new_rows {
        let extra = new_rows - current_rows;
        for _ in 0..extra {
            self.grid.push(vec![Cell::default(); cols as usize]);
        }
    }
    self.cols = cols;
    self.rows = rows;
    self.cursor.row = self.cursor.row.min(rows.saturating_sub(1));
    self.cursor.col = self.cursor.col.min(cols.saturating_sub(1));
}
```
Apply the same row/column logic to `saved_primary.0` when `saved_primary` is `Some`. Shrinking the saved primary grid pushes top rows to `self.scrollback` (NOT a separate saved scrollback).

**`scroll_up` — existing pattern** (`terminal.rs` lines 292–302):
```rust
fn scroll_up(&mut self) {
    if self.rows == 0 {
        return;
    }
    let top_row = self.grid.remove(0);
    self.scrollback.push_back(top_row);
    if self.scrollback.len() > SCROLLBACK_LINE_CAP {
        self.scrollback.pop_front();
    }
    self.grid.push(vec![Cell::default(); self.cols as usize]);
}
```
Add `if !self.echo_state.alt_screen {` guard around the `push_back` and cap check (D-19-09). When alt-screen is active, `top_row` is discarded (no scrollback for the alt grid). The `grid.push` blank-line line always runs.

**`print_char` — existing pattern** (`terminal.rs` lines 306–327):
```rust
fn print_char(&mut self, c: char) {
    let row = (self.cursor.row as usize).min(self.rows.saturating_sub(1) as usize);
    let col = (self.cursor.col as usize).min(self.cols.saturating_sub(1) as usize);

    if !self.grid.is_empty() && row < self.grid.len() && col < self.grid[row].len() {
        self.grid[row][col] = Cell {
            ch: c,
            style: self.sgr.style,
            fg: self.sgr.fg,
            bg: self.sgr.bg,
        };
    }

    self.cursor.col += 1;
    if self.cursor.col >= self.cols {
        self.cursor.col = 0;
        self.lf();
    }
}
```
Extend with `UnicodeWidthChar::width(c)` dispatch before the bounds check. Use `saturating_add` on cursor advancement (pattern established in `csi_dispatch` CSI B/C, lines 530–540). The continuation cell write at `col+1` mirrors the primary cell write but with `wide: true`.

**`csi_dispatch` DEC private mode pattern** (`terminal.rs` lines 496–511):
```rust
if intermediates == b"?" {
    if action == 'h' || action == 'l' {
        let enable = action == 'h';
        for param in params.iter() {
            let mode = param[0];
            match mode {
                25 => self.echo_state.cursor_visible = enable,
                1049 => self.echo_state.alt_screen = enable,  // ← REPLACE THIS LINE
                2004 => self.echo_state.bracketed_paste = enable,
                1 => self.echo_state.app_cursor_keys = enable,
                _ => { /* scope fence */ }
            }
        }
    }
    return;
}
```
Replace `1049 => self.echo_state.alt_screen = enable` with:
```rust
1049 => {
    if enable { self.enter_alt_screen(); } else { self.exit_alt_screen(); }
}
```

**`esc_dispatch` RIS reset pattern** (`terminal.rs` lines 741–757):
```rust
fn esc_dispatch(&mut self, _intermediates: &[u8], _ignore: bool, byte: u8) {
    match byte {
        b'c' => {
            self.grid = Self::make_grid(self.cols, self.rows);
            self.scrollback.clear();
            self.cursor = CursorPos { row: 0, col: 0 };
            self.sgr.reset();
            self.echo_state = EchoState::default();
            self.title = None;
            self.osc52_pending = None;
            // NEW: self.saved_primary = None;
            // NEW: self.osc_byte_count = 0;
        }
        _ => {}
    }
}
```

**`Cell` struct** (`terminal.rs` lines 74–97) — add `wide: bool` field following the same pattern as existing fields:
```rust
#[derive(Clone, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub style: CellStyle,
    pub fg: Option<u8>,
    pub bg: Option<u8>,
    // NEW:
    pub wide: bool,  // true = this cell is a wide-char continuation marker
}

impl Default for Cell {
    fn default() -> Self {
        Cell {
            ch: ' ',
            style: CellStyle(CellStyle::NONE),
            fg: None,
            bg: None,
            wide: false,  // NEW
        }
    }
}
```
Every `Cell { ch: ..., style: ..., fg: ..., bg: ... }` literal in the file (in `print_char`, erase loops, `make_grid`) must have `wide: false` added. Use the compile-error list to find them all — the struct is not `#[non_exhaustive]`.

**`SgrState::reset` / `SgrState::clone`** (`terminal.rs` lines 135–151) — already `Clone`-derived. Use `self.sgr.clone()` in `enter_alt_screen` and `self.sgr.reset()` to reset the alt-screen pen:
```rust
impl SgrState {
    fn reset(&mut self) {
        self.style = CellStyle(CellStyle::NONE);
        self.fg = None;
        self.bg = None;
    }
}
```

**Constant definition pattern** (`terminal.rs` lines 47–63) — add `OSC_ACCUMULATION_MAX` following same doc + `pub const` pattern:
```rust
/// Maximum bytes allowed for one OSC sequence before truncation (D-19-01 / SEC-03).
/// Distinct from OSC_52_MAX_BYTES (storage cap) and MAX_TITLE_BYTES (storage cap).
/// This bounds what vte ALLOCATES while parsing, not what is stored.
pub const OSC_ACCUMULATION_MAX: usize = 1_048_576; // 1 MiB
```

**Test helper pattern** (`terminal.rs` lines 882–884):
```rust
fn ts(cols: u16, rows: u16) -> TerminalState {
    TerminalState::new(cols, rows)
}
```
All new tests in `terminal.rs` use `ts(80, 24)` as their starting point. Grid-assertion tests follow the pattern at lines 914–922: `state.advance(bytes)` then `state.cell(row, col).field` assertions. The existing `decset_alt_screen_toggled_by_1049` test (lines 1100–1107) must be extended to assert grid content, not just the flag.

**Existing adversarial OSC test pattern** (`terminal.rs` lines 1502–1537) — the model for the new multi-chunk SEC-03 test:
```rust
#[test]
fn adversarial_large_osc_title_is_bounded_no_panic() {
    let mut state = ts(80, 24);
    let large_payload = vec![b'A'; 64 * 1024];
    let mut seq = Vec::new();
    seq.extend_from_slice(b"\x1b]2;");
    seq.extend_from_slice(&large_payload);
    seq.extend_from_slice(b"\x07");
    state.advance(&seq);
    // assertions...
    state.advance(b"\x1b]2;Normal Title\x07");
    assert_eq!(state.title(), Some("Normal Title"));
}
```
The new SEC-03 test must call `state.advance()` in a loop (multi-chunk), not in one call. The fuzz target extension mirrors the same multi-chunk pattern.

---

### `crates/nosh-server/Cargo.toml` — add `unicode-width`

**Analog:** `crates/nosh-client/Cargo.toml` line 34:
```toml
unicode-width = "0.2"
```
Add the same line to `crates/nosh-server/Cargo.toml` in the `[dependencies]` section, after the `vte` entry (keep related deps grouped). No features needed — the default crate API surface includes `UnicodeWidthChar`.

---

### `crates/nosh-proto/src/datagram.rs` — add `alt_screen: bool` to `StateDiff`

**Existing `StateDiff` struct** (`datagram.rs` lines 50–76):
```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateDiff {
    pub epoch: u64,
    pub cols: u16,
    pub rows: u16,
    pub cursor: CursorPos,
    pub runs: Vec<DiffRun>,
}
```
Add `pub alt_screen: bool` following the same field ordering pattern (after `cursor`, before `runs` — group it with the non-content fields):
```rust
pub struct StateDiff {
    pub epoch: u64,
    pub cols: u16,
    pub rows: u16,
    pub cursor: CursorPos,
    pub alt_screen: bool,  // NEW: true when server ?1049 alt screen is active (TUI-05)
    pub runs: Vec<DiffRun>,
}
```
This is a **postcard wire-format breaking change** — old clients will misparse new datagrams. Acceptable per REQUIREMENTS.md ("nosh is built from source; client and server are the same version"). Document as a breaking change in the phase summary.

Every `StateDiff { epoch, cols, rows, cursor, runs }` literal in the codebase (`server.rs` lines 280–286, 294–350, test helpers in `datagram.rs` lines 538–546) must add `alt_screen: false` (or the actual value from `echo_state`). The compile-error list will find all sites.

**Test helper pattern for struct literals** (`datagram.rs` lines 538–546):
```rust
fn make_diff(epoch: u64, runs: Vec<DiffRun>) -> StateDiff {
    StateDiff {
        epoch,
        cols: 80,
        rows: 24,
        cursor: CursorPos { row: 12, col: 40 },
        runs,
        // ADD: alt_screen: false,
    }
}
```

---

### `crates/nosh-server/src/server.rs` — diff encoder (`build_state_diff`) and wide-continuation skip

**`build_state_diff` closure pattern** (`server.rs` lines 309–317):
```rust
let (cols, rows, cursor, cells) = slot.with_terminal_state(|ts| {
    let (cols, rows) = ts.size();
    let cursor = ts.cursor();
    let cells: Vec<Vec<Cell>> = ts
        .viewport_rows()
        .map(|(_, row)| row.to_vec())
        .collect();
    (cols, rows, cursor, cells)
});
```
Add `alt_screen` extraction using the same pattern:
```rust
let (cols, rows, cursor, alt_screen, cells) = slot.with_terminal_state(|ts| {
    let (cols, rows) = ts.size();
    let cursor = ts.cursor();
    let alt_screen = ts.echo_state().alt_screen;  // NEW
    let cells: Vec<Vec<Cell>> = ts
        .viewport_rows()
        .map(|(_, row)| row.to_vec())
        .collect();
    (cols, rows, cursor, alt_screen, cells)
});
```

**`StateDiff` construction** (`server.rs` line 350):
```rust
let diff = StateDiff { epoch: sent_epoch, cols, rows, cursor, runs: all_runs };
// BECOMES:
let diff = StateDiff { epoch: sent_epoch, cols, rows, cursor, alt_screen, runs: all_runs };
```

**`compute_diff_runs` — continuation cell skip** (`server.rs` lines 212–264). The `chars.push(c2.ch)` call at line 255 must skip cells where `c2.wide == true`:
```rust
// Existing pattern (lines 244–257):
while (col as usize) < current_row.len() {
    let cc = col as usize;
    let c2 = &current_row[cc];
    if c2.style != style || c2.fg != fg || c2.bg != bg {
        break;
    }
    let base2 = baseline_row.get(cc);
    if base2.map(|b| b == c2).unwrap_or(false) {
        break;
    }
    chars.push(c2.ch);
    col += 1;
}
```
Extend to skip wide-continuation cells:
```rust
    if c2.wide {
        // Wide-char continuation marker: advance column counter but do NOT
        // push ch to chars (the wide glyph at col-1 already occupies two columns).
        col += 1;
        continue;
    }
    chars.push(c2.ch);
    col += 1;
```
This keeps `DiffRun.chars` as a sequence of logical glyphs (one per wide character, not two). The `start_col` adjustment for continuation cells follows from the column counter advancing normally.

---

### `crates/nosh-client/src/screen.rs` — skip wide-continuation cells in `emit_diff`

**`emit_diff` character write pattern** (`screen.rs` lines 428–491):
```rust
fn emit_diff<W: Write>(
    &mut self,
    out: &mut W,
    desired: &[Vec<Cell>],
    desired_cursor: CursorPos,
) -> std::io::Result<()> {
    // ...
    for (r, (des_row, phys_row)) in desired.iter().zip(self.physical.iter()).enumerate().take(rows) {
        let row = r as u16;
        for (c, (want, have)) in des_row.iter().zip(phys_row.iter()).enumerate().take(cols) {
            let col = c as u16;
            if want == have {
                continue;
            }
            // MoveTo + SGR emit ...
            let mut buf = [0u8; 4];
            let s = want.ch.encode_utf8(&mut buf);
            out.write_all(s.as_bytes())?;
            last_col = Some(last_col.unwrap_or(col) + 1);
        }
    }
    // ...
}
```
Add a `want.wide` check immediately after `if want == have { continue; }`:
```rust
if want.wide {
    // Wide-char continuation cell: the physical terminal already advanced
    // two columns when the wide glyph was written at col-1. Do not write
    // anything here — writing a space or sentinel would corrupt the display.
    // Update physical to match desired so the next render sees no diff.
    *have_cell = want.clone();  // NOTE: need mutable access; see below
    continue;
}
```
Because `emit_diff` uses `zip(self.physical.iter())` (immutable), the physical commit at lines 483–488 already handles this via `*phys_cell = des_cell.clone()`. The skip only needs the `continue` — physical commit happens unconditionally at the end of the loop. No structural change required.

The `Cell` struct in `screen.rs` (lines 50–70) must also gain `pub wide: bool` with `wide: false` in `Default`. It mirrors the server-side `Cell` (field-for-field pattern per the module doc at line 49).

**`apply` pattern** (`screen.rs` lines 242–263) — when the server skips continuation cells in `DiffRun.chars`, the client `apply` doesn't need changes: `run.chars.chars()` is already the logical sequence and `(start..).zip(run.chars.chars())` writes char at the matching column. However, if continuation cells are included in the diff (Option B wire format), the `apply` loop must skip `wide: true` cells — but under Option A (server skips them in `chars`), `apply` is unchanged.

---

### `crates/nosh-client/src/predictor.rs` — alt-screen suppression

**Bracketed-paste suppression pattern** (`predictor.rs` lines 505–512) — the direct analog for alt-screen suppression:
```rust
InputAction::BracketedPasteStart => {
    self.in_bracketed_paste = true;
    self.reset();  // clears pending + increments prediction_epoch
}
InputAction::BracketedPasteEnd => {
    self.in_bracketed_paste = false;
}
```

**`reset()` implementation** (`predictor.rs` lines 665–676):
```rust
pub fn reset(&mut self) {
    self.pending.clear();
    self.become_tentative();
    self.cursor_motion_pending = false;
    self.needs_epoch_start_sync = true;
}
```
This is the exact call to use when entering alt-screen. After `reset()`, `pending.len() == 0` (success criterion 5: "predictor.pending empty after ?1049h is processed").

No change to `predictor.rs` itself is needed. The suppression hook lives in `main.rs` (the datagram arm). The predictor's existing `reset()` mechanism satisfies TUI-05 without modification.

---

### `crates/nosh-client/src/main.rs` — datagram arm alt-screen hook

**Datagram arm pattern** (`main.rs` lines 999–1097) — the full datagram processing sequence:
```rust
datagram = conn.read_datagram() => {
    match datagram {
        Ok(bytes) => {
            if let Ok(diff) = nosh_proto::datagram::decode_datagram(&bytes) {
                if diff.epoch > screen.last_applied_epoch() {
                    last_datagram_time = tokio::time::Instant::now();
                    if loss_overlay.active { loss_overlay.active = false; }

                    let (cols_before, rows_before) = screen.size();
                    screen.apply(&diff);
                    let rtt_ms = conn.rtt().as_millis() as u64;
                    let epoch_before_cull = predictor.confirmed_epoch();
                    predictor.cull(&screen, diff.epoch, rtt_ms);
                    let (cols_after, rows_after) = screen.size();
                    if cols_after != cols_before || rows_after != rows_before {
                        predictor.set_size(cols_after, rows_after);
                        predictor.reset();
                    }
                    predictor.sync_cursor_from_confirmed(screen.confirmed_cursor());
                    // ... epoch ack, render ...
                }
            }
        }
        Err(e) => { /* transport drop */ }
    }
}
```

The alt-screen suppression hook inserts after `screen.apply(&diff)` and before `predictor.cull(...)`. Use a local `was_alt_screen` variable (same pattern as `cols_before`/`rows_before` for the resize hook):
```rust
// Capture alt_screen state BEFORE apply (to detect transitions).
// Analogy: cols_before/rows_before pattern at lines 1016–1017.
let was_alt_screen = /* last known alt_screen state — track in run_pump local var */;
screen.apply(&diff);

// Alt-screen entry suppression (TUI-05).
if diff.alt_screen && !was_alt_screen {
    predictor.reset();
}
// was_alt_screen = diff.alt_screen;  // update for next iteration
```

The `was_alt_screen` local variable must be declared before the `select!` loop (same scope as `predictor`, `screen`, `loss_overlay` — around line 819). Initialise to `false`.

The resize reset hook (`predictor.reset()` when dimensions change, lines 1026–1029) is the direct structural analog — alt-screen reset is the same idea applied to a different trigger condition.

---

### `fuzz/fuzz_targets/osc_accumulation.rs` — extend for multi-chunk accumulation bound

**Existing fuzz target** (`osc_accumulation.rs` lines 1–31):
```rust
#![no_main]
use libfuzzer_sys::fuzz_target;
use nosh_server::terminal::{TerminalState, OSC_52_MAX_BYTES, MAX_TITLE_BYTES};

fuzz_target!(|data: &[u8]| {
    let mut state = TerminalState::new(80, 24);
    state.advance(data);

    if let Some((_sel, payload)) = state.osc52_pending() {
        assert!(payload.len() <= OSC_52_MAX_BYTES, "...");
    }
    if let Some(title) = state.title() {
        assert!(title.len() <= MAX_TITLE_BYTES, "...");
    }
});
```

Extension pattern — add a deterministic regression test (not a libFuzzer closure) in `terminal.rs` for multi-chunk delivery, following the existing `adversarial_large_osc_title_is_bounded_no_panic` test shape (lines 1502–1538). The fuzz target itself is extended to also import `OSC_ACCUMULATION_MAX` and assert that `state` does not OOM on multi-chunk OSC input:
```rust
use nosh_server::terminal::{TerminalState, OSC_52_MAX_BYTES, MAX_TITLE_BYTES, OSC_ACCUMULATION_MAX};

fuzz_target!(|data: &[u8]| {
    let mut state = TerminalState::new(80, 24);

    // Original single-chunk test (unchanged):
    state.advance(data);

    // NEW: multi-chunk accumulation test using deterministic 10 MB OSC.
    // Construct a multi-megabyte OSC 2 title sequence and feed it in chunks
    // to exercise the accumulation pre-bound (SEC-03 / D-19-01).
    let mut state2 = TerminalState::new(80, 24);
    // ESC ] 2 ; <10 MB of 'A'> BEL
    let osc_start = b"\x1b]2;";
    let osc_end = b"\x07";
    const CHUNK: usize = 4096;
    const TOTAL: usize = OSC_ACCUMULATION_MAX * 10; // 10 MiB
    state2.advance(osc_start);
    let chunk = vec![b'A'; CHUNK];
    let chunks = TOTAL / CHUNK;
    for _ in 0..chunks {
        state2.advance(&chunk); // must not OOM
    }
    state2.advance(osc_end);
    // After truncation + resync, a normal OSC must still parse:
    state2.advance(b"\x1b]2;OK\x07");
    if let Some(title) = state2.title() {
        assert!(title.len() <= MAX_TITLE_BYTES, "title cap after multi-chunk OSC");
    }

    // Original assertions (unchanged):
    if let Some((_sel, payload)) = state.osc52_pending() {
        assert!(payload.len() <= OSC_52_MAX_BYTES, "OSC 52 payload exceeded cap");
    }
    if let Some(title) = state.title() {
        assert!(title.len() <= MAX_TITLE_BYTES, "title exceeded cap");
    }
});
```
Note: the `max_len` default of 4096 is too small to reach `OSC_ACCUMULATION_MAX` via `data` alone — the deterministic construction above is the correct approach. The fuzz input `data` continues to drive the single-chunk path.

---

## Shared Patterns

### `std::mem::replace` for atomic swap (enter_alt_screen)

**Source:** `terminal.rs` `resize` + research code examples in `19-RESEARCH.md`.

The atomicity requirement (PITFALL A-1) is met by `std::mem::replace`, which moves the old grid into `saved_primary` and installs the new blank grid in `self.grid` in a single operation:
```rust
fn enter_alt_screen(&mut self) {
    let alt_grid = Self::make_grid(self.cols, self.rows);
    let prim_grid = std::mem::replace(&mut self.grid, alt_grid);
    self.saved_primary = Some((prim_grid, self.cursor, self.sgr.clone()));
    self.cursor = CursorPos { row: 0, col: 0 };
    self.sgr.reset();
    self.echo_state.alt_screen = true;
}

fn exit_alt_screen(&mut self) {
    if let Some((prim_grid, prim_cursor, prim_sgr)) = self.saved_primary.take() {
        self.grid = prim_grid;
        self.cursor = prim_cursor;
        self.sgr = prim_sgr;
    }
    self.echo_state.alt_screen = false;
}
```
Apply to: `crates/nosh-server/src/terminal.rs` only.

### Saturating arithmetic for cursor bounds

**Source:** `terminal.rs` `csi_dispatch` CSI B/C handlers (lines 530–540).

All cursor arithmetic uses `saturating_add` / `saturating_sub` / `.min(bound)`. The new `print_char` wide-char cursor advance must follow the same pattern:
```rust
self.cursor.col = self.cursor.col.saturating_add(col_width);
if self.cursor.col >= self.cols {
    self.cursor.col = 0;
    self.lf();
}
```
Apply to: `crates/nosh-server/src/terminal.rs` (`print_char`).

### `OnceLock` static default sentinel

**Source:** `terminal.rs` `cell()` method (lines 361–367), `screen.rs` `confirmed_cell()` (lines 542–548).

Both the server and client use the same `OnceLock<Cell>` pattern for out-of-bounds reads. New code must not introduce mutable statics or direct `unwrap()` on grid accesses.

### `with_terminal_state` closure — no async inside

**Source:** `server.rs` `build_state_diff` (lines 309–317).

All terminal state reads in `server.rs` go through `slot.with_terminal_state(|ts| { ... })`. The closure must be synchronous — no `.await`, no blocking I/O. The new `alt_screen` extraction follows this constraint by reading from `ts.echo_state().alt_screen` (a `bool` field copy).

### Test isolation invariant

**Source:** `terminal.rs` module-level doc (lines 33–35):

> this module has NO imports from `quinn`, `tokio`, `crate::session`, `crate::registry`, or `crate::server`. It is a pure in-memory data structure testable without any network or async runtime.

All new tests in `terminal.rs` must maintain this invariant. Tests use only `ts()`, `state.advance()`, `state.cell()`, and `state.echo_state()`.

---

## No Analog Found

All files in scope have direct analogs in the existing codebase. No file requires pure-research patterns.

| File | Note |
|------|------|
| — | All 8 files are extensions of existing code |

---

## Metadata

**Analog search scope:** `crates/nosh-server/src/`, `crates/nosh-client/src/`, `crates/nosh-proto/src/`, `fuzz/fuzz_targets/`
**Files scanned:** 15 source files + 2 Cargo.toml files
**Pattern extraction date:** 2026-06-07
