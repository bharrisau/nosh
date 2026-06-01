//! Server-side terminal state model: authoritative grid, cursor, bounded scrollback,
//! and the four observable DEC private-mode echo flags.
//!
//! # Design decisions
//!
//! - **SYNC-02**: `TerminalState` maintains the server's authoritative terminal record.
//!   It is fed from the same callsite as `SequencedOutputBuffer` (via
//!   `SessionSlot::push_output_and_parse`) so both the replay buffer and the state
//!   model see the identical byte stream.
//!
//! - **Cell types**: `Cell.fg` and `Cell.bg` are `Option<u8>` — the SAME type as
//!   `DiffRun.fg`/`bg` from `nosh_proto::datagram`. `None` = terminal-default color;
//!   `Some(n)` = explicit palette index `n` (0..=255). `Some(0)` is explicit black
//!   and is DISTINCT from `None` (default). This enables Phase 13 diff extraction with
//!   zero type conversion.
//!
//! - **Borrow-split advance** (Pitfall 1): `TerminalState` owns a `vte::Parser` AND
//!   implements `vte::Perform`. Calling `self.parser.advance(self, bytes)` would require
//!   two mutable borrows of `self`. Solution: `std::mem::take` the parser before the
//!   call and restore it after.
//!
//! - **Scrollback cap** (D-12-02): bounded at `SCROLLBACK_LINE_CAP` lines (10,000),
//!   mirroring the spirit of `SequencedOutputBuffer`'s 64 KiB byte cap. Oldest lines
//!   are dropped first when the cap is exceeded (drop-oldest semantics).
//!
//! - **Scope fence** (D-12-02b): only the common VT subset is handled (text, cursor
//!   motion CSI A/B/C/D/H, erase J/K, SGR m, DEC private modes ?25/?1049/?2004/?1,
//!   OSC 0/2 title, OSC 52 clipboard detection). Exotic sequences (sixel, DCS via
//!   hook/put/unhook, mouse) are intentionally left as default no-ops. This fence is
//!   permanent until explicitly extended by a future phase decision.
//!
//! - **Isolation**: this module has NO imports from `quinn`, `tokio`, `crate::session`,
//!   `crate::registry`, or `crate::server`. It is a pure in-memory data structure
//!   testable without any network or async runtime.

use std::collections::VecDeque;

use nosh_proto::datagram::{CellStyle, CursorPos};

// ── Constants ─────────────────────────────────────────────────────────────────

/// Maximum number of scrollback lines retained by `TerminalState`.
///
/// This cap mirrors the spirit of `SequencedOutputBuffer`'s 64 KiB byte cap:
/// enough to hold a day's typical shell output while bounding memory use.
/// Oldest lines are dropped first (drop-oldest semantics, same as the byte buffer).
const SCROLLBACK_LINE_CAP: usize = 10_000;

// ── Cell ──────────────────────────────────────────────────────────────────────

/// A single terminal cell.
///
/// The field types are chosen to match `nosh_proto::datagram::DiffRun` exactly so
/// Phase 13 diff extraction can operate with zero type conversion:
/// - `style: CellStyle` — same as `DiffRun.style`
/// - `fg: Option<u8>` — same as `DiffRun.fg` (`None` = default, `Some(n)` = index)
/// - `bg: Option<u8>` — same as `DiffRun.bg`
#[derive(Clone, PartialEq, Eq)]
pub struct Cell {
    /// Unicode scalar value in this cell. `' '` means blank/empty.
    pub ch: char,
    /// SGR attributes packed as bitflags. Same type as `DiffRun.style`.
    pub style: CellStyle,
    /// ANSI 256-color foreground. `None` = terminal default; `Some(n)` = palette index `n`.
    /// `Some(0)` (explicit black) is DISTINCT from `None` (default).
    pub fg: Option<u8>,
    /// ANSI 256-color background. `None` = terminal default; `Some(n)` = palette index `n`.
    /// `Some(0)` (explicit black) is DISTINCT from `None` (default).
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

// ── EchoState ────────────────────────────────────────────────────────────────

/// Observable DEC private-mode echo flags (D-12-01).
///
/// These four modes are the only terminal state observable from the server-side
/// PTY master output stream that meaningfully affect the client's rendering and
/// input behavior. They are toggled by `CSI ? Pm h` (DECSET) / `CSI ? Pm l`
/// (DECRST) sequences detected in `csi_dispatch`.
///
/// Note: true termios `ECHO` (password input, `read -s`) is NOT observable from
/// the master output stream — do NOT add a termios slave-side probe (D-12-01a).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct EchoState {
    /// DECTCEM `?25`: cursor visible when true.
    pub cursor_visible: bool,
    /// Alternate screen `?1049`: alternate screen buffer active when true.
    pub alt_screen: bool,
    /// Bracketed paste `?2004`: bracketed paste mode active when true.
    pub bracketed_paste: bool,
    /// Application cursor keys `?1`: application cursor key mode when true.
    pub app_cursor_keys: bool,
}

// ── SGR running attributes ────────────────────────────────────────────────────

/// Ephemeral SGR attribute state carried between `print` calls.
///
/// These are the "current pen" attributes that get stamped onto each cell as it
/// is written. Reset by SGR 0 / bare CSI m; updated by subsequent SGR sequences.
#[derive(Clone)]
struct SgrState {
    style: CellStyle,
    fg: Option<u8>,
    bg: Option<u8>,
}

impl Default for SgrState {
    fn default() -> Self {
        SgrState {
            style: CellStyle(CellStyle::NONE),
            fg: None,
            bg: None,
        }
    }
}

impl SgrState {
    fn reset(&mut self) {
        self.style = CellStyle(CellStyle::NONE);
        self.fg = None;
        self.bg = None;
    }
}

// ── TerminalState ─────────────────────────────────────────────────────────────

/// Server-side authoritative terminal state model.
///
/// Implements `vte::Perform` and is driven by `advance(&mut self, bytes)`. The
/// advance method uses the `std::mem::take` borrow-split pattern to avoid the
/// two-mutable-borrow conflict between the owned `parser` field and the `Perform`
/// impl (Pitfall 1 / Pattern 6).
pub struct TerminalState {
    cols: u16,
    rows: u16,
    /// Viewport grid: `grid[row][col]`. Outer Vec is rows (0 = top), inner Vec is
    /// columns. Length is always `rows` × `cols`.
    grid: Vec<Vec<Cell>>,
    /// Scrollback history. Bounded at `SCROLLBACK_LINE_CAP` lines; oldest lines
    /// are dropped first when the cap is exceeded. Lines are pushed in from the
    /// top of the viewport when the cursor scrolls past the bottom.
    scrollback: VecDeque<Vec<Cell>>,
    /// Current cursor position (0-based row/col). Always clamped to grid bounds.
    cursor: CursorPos,
    /// Observable private-mode flags (D-12-01).
    echo_state: EchoState,
    /// Window/icon title set by OSC 0 or OSC 2.
    title: Option<String>,
    /// Last parsed OSC 52 clipboard-write payload (D-12-04 — detection only;
    /// forwarding is Phase 16). Replaced on each new OSC 52 sequence.
    osc52_pending: Option<(Vec<u8>, Vec<u8>)>,
    /// The vte parser (holds the Paul Williams state machine across `advance` calls).
    /// NEVER access directly — always use `advance` which implements the borrow-split.
    parser: vte::Parser,
    /// Current SGR running attributes (applied to each printed cell).
    sgr: SgrState,
}

impl TerminalState {
    /// Create a new terminal state with the given dimensions.
    ///
    /// The grid is initialized to `cols × rows` default cells (`' '`, no attributes).
    /// Cursor is at (row=0, col=0). All echo-state flags are false. Scrollback is empty.
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
        }
    }

    /// Build a blank grid of the given dimensions.
    fn make_grid(cols: u16, rows: u16) -> Vec<Vec<Cell>> {
        (0..rows as usize)
            .map(|_| vec![Cell::default(); cols as usize])
            .collect()
    }

    /// Feed raw PTY bytes into the terminal state model.
    ///
    /// Uses the `std::mem::take` borrow-split to avoid the two-mutable-borrow
    /// conflict between `self.parser` (which needs `&mut vte::Parser`) and `self`
    /// (which implements `vte::Perform` and needs `&mut TerminalState`). The taken
    /// parser is ground-state per `Parser::Default`, and since we restore it
    /// immediately after the advance call, no state is lost across calls.
    pub fn advance(&mut self, bytes: &[u8]) {
        let mut parser = std::mem::take(&mut self.parser);
        parser.advance(self, bytes);
        self.parser = parser;
    }

    /// Resize the terminal grid to the new dimensions (D-12-03: no reflow).
    ///
    /// - Each row is truncated or extended to `cols` with default cells.
    /// - On shrink (rows < current), the top rows that no longer fit scroll into
    ///   scrollback (respecting `SCROLLBACK_LINE_CAP`).
    /// - On grow (rows > current), blank rows are added at the bottom.
    /// - Scrollback lines are kept as-is (original column count preserved).
    /// - Cursor is clamped to new grid bounds.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        // Resize each existing row to the new column width.
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
            // Shrink: top rows scroll into scrollback.
            let excess = current_rows - new_rows;
            for _ in 0..excess {
                let top_row = self.grid.remove(0);
                self.scrollback.push_back(top_row);
                if self.scrollback.len() > SCROLLBACK_LINE_CAP {
                    self.scrollback.pop_front();
                }
            }
        } else if current_rows < new_rows {
            // Grow: add blank rows at the bottom.
            let extra = new_rows - current_rows;
            for _ in 0..extra {
                self.grid.push(vec![Cell::default(); cols as usize]);
            }
        }

        self.cols = cols;
        self.rows = rows;

        // Clamp cursor to new bounds.
        self.cursor.row = self.cursor.row.min(rows.saturating_sub(1));
        self.cursor.col = self.cursor.col.min(cols.saturating_sub(1));
    }

    // ── Internal helpers ─────────────────────────────────────────────────────

    /// Process a linefeed: advance cursor row, scrolling viewport into scrollback
    /// when the cursor is at the last row.
    fn lf(&mut self) {
        if self.cursor.row + 1 >= self.rows {
            self.scroll_up();
        } else {
            self.cursor.row += 1;
        }
    }

    /// Scroll the viewport up by one line: push the top row into scrollback (with
    /// cap enforcement) and append a blank row at the bottom. Cursor stays at the
    /// last row (unchanged by scroll_up — the viewport moved, not the cursor).
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

    /// Write a character at the current cursor position, advance the cursor right,
    /// wrapping and scrolling as needed.
    fn print_char(&mut self, c: char) {
        // Clamp cursor to grid bounds (adversarial-safety).
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

        // Advance cursor.
        self.cursor.col += 1;
        if self.cursor.col >= self.cols {
            // Wrap to next line.
            self.cursor.col = 0;
            self.lf();
        }
    }

    /// Get default param value for cursor motion: treat 0 as 1 (VT standard —
    /// an omitted parameter defaults to 1, vte delivers 0 for omitted params).
    fn cursor_count(params: &vte::Params) -> u16 {
        params
            .iter()
            .next()
            .and_then(|p| p.first().copied())
            .unwrap_or(0)
            .max(1)
    }

    // ── Public read API ──────────────────────────────────────────────────────

    /// Current cursor position (0-based).
    pub fn cursor(&self) -> CursorPos {
        self.cursor
    }

    /// Read a cell at the given (row, col) position.
    ///
    /// Returns a reference to the cell, or a reference to a default cell if
    /// the coordinates are out of bounds (bounds-safe, never panics).
    pub fn cell(&self, row: u16, col: u16) -> &Cell {
        static DEFAULT_CELL: std::sync::OnceLock<Cell> = std::sync::OnceLock::new();
        let default = DEFAULT_CELL.get_or_init(Cell::default);
        self.grid
            .get(row as usize)
            .and_then(|r| r.get(col as usize))
            .unwrap_or(default)
    }

    /// Current echo-state flags.
    pub fn echo_state(&self) -> &EchoState {
        &self.echo_state
    }

    /// Window/icon title (set by OSC 0/2), if any.
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    /// Last detected OSC 52 clipboard-write payload, if any.
    ///
    /// Returns `Some((selection, base64_data))` where `selection` is the clipboard
    /// selection designator (e.g. `b"c"`) and `base64_data` is the base64-encoded
    /// clipboard content. Detection only — no clipboard action is taken here (D-12-04).
    pub fn osc52_pending(&self) -> Option<(&[u8], &[u8])> {
        self.osc52_pending
            .as_ref()
            .map(|(sel, data)| (sel.as_slice(), data.as_slice()))
    }

    /// Current terminal dimensions as `(cols, rows)`.
    pub fn size(&self) -> (u16, u16) {
        (self.cols, self.rows)
    }

    /// Row iterator over the visible viewport grid (top to bottom).
    ///
    /// Yields `(row_index, &[Cell])` for each row. Used by Phase 13 diff extraction
    /// to iterate over the visible viewport without cloning the grid.
    pub fn viewport_rows(&self) -> impl Iterator<Item = (u16, &[Cell])> {
        self.grid
            .iter()
            .enumerate()
            .map(|(i, row)| (i as u16, row.as_slice()))
    }
}

// ── vte::Perform implementation ───────────────────────────────────────────────

impl vte::Perform for TerminalState {
    /// Print a Unicode scalar value at the current cursor position.
    fn print(&mut self, c: char) {
        self.print_char(c);
    }

    /// Execute a C0/C1 control byte.
    ///
    /// Handled: `\r` (carriage return), `\n`/`\x0B`/`\x0C` (linefeed), `\x08`
    /// (backspace), `\x07` (BEL — ignored in the state model).
    /// All other C0 bytes are scope-fenced (ignored).
    fn execute(&mut self, byte: u8) {
        match byte {
            b'\r' => {
                self.cursor.col = 0;
            }
            b'\n' | b'\x0B' | b'\x0C' => {
                self.lf();
            }
            b'\x08' => {
                // Backspace: move cursor left, clamped at 0.
                self.cursor.col = self.cursor.col.saturating_sub(1);
            }
            b'\x07' => {
                // BEL: no-op in the state model.
            }
            _ => {
                // Scope fence: other C0 control codes are intentionally ignored.
                // This includes TAB (\x09), SO (\x0E), SI (\x0F), etc.
            }
        }
    }

    /// Dispatch a CSI sequence.
    ///
    /// # DEC private modes (intermediates == b"?")
    ///
    /// When `intermediates == b"?"`, this is a DECSET (`h`) or DECRST (`l`)
    /// sequence. We handle the four observable modes (D-12-01) and return
    /// without falling through to the standard CSI handlers.
    ///
    /// Handled modes: ?25 (DECTCEM), ?1049 (alt screen), ?2004 (bracketed paste),
    /// ?1 (application cursor keys). Unknown modes are scope-fenced.
    ///
    /// # Standard CSI actions
    ///
    /// - `A`/`B`/`C`/`D` — cursor up/down/right/left (default count 1)
    /// - `H`/`f` — cursor position (1-based → 0-based, clamped)
    /// - `J` — erase in display (0=below, 1=above, 2=all, 3=all+scrollback)
    /// - `K` — erase in line (0=right, 1=left, 2=whole)
    /// - `m` — SGR attributes
    ///
    /// All other CSI actions are scope-fenced (ignored).
    fn csi_dispatch(
        &mut self,
        params: &vte::Params,
        intermediates: &[u8],
        _ignore: bool,
        action: char,
    ) {
        // ── DEC private modes ────────────────────────────────────────────────
        if intermediates == b"?" {
            // Only handle h (set) and l (reset); other actions on ? are scope-fenced.
            if action == 'h' || action == 'l' {
                let enable = action == 'h';
                for param in params.iter() {
                    let mode = param[0]; // u16; subparams are irrelevant for mode numbers
                    match mode {
                        25 => self.echo_state.cursor_visible = enable,
                        1049 => self.echo_state.alt_screen = enable,
                        2004 => self.echo_state.bracketed_paste = enable,
                        1 => self.echo_state.app_cursor_keys = enable,
                        _ => { /* scope fence: unknown private mode */ }
                    }
                }
            }
            return; // do NOT fall through to standard CSI
        }

        // ── Standard CSI actions ─────────────────────────────────────────────
        match action {
            // Cursor up — count defaults to 1 (Pitfall 3)
            'A' => {
                let n = Self::cursor_count(params);
                self.cursor.row = self.cursor.row.saturating_sub(n);
            }
            // Cursor down — count defaults to 1
            'B' => {
                let n = Self::cursor_count(params);
                self.cursor.row = (self.cursor.row + n).min(self.rows.saturating_sub(1));
            }
            // Cursor right — count defaults to 1
            'C' => {
                let n = Self::cursor_count(params);
                self.cursor.col = (self.cursor.col + n).min(self.cols.saturating_sub(1));
            }
            // Cursor left — count defaults to 1
            'D' => {
                let n = Self::cursor_count(params);
                self.cursor.col = self.cursor.col.saturating_sub(n);
            }
            // Cursor position (CUP) — 1-based, 0 treated as 1 (Pitfall 2)
            'H' | 'f' => {
                let mut iter = params.iter();
                let row_param = iter
                    .next()
                    .and_then(|p| p.first().copied())
                    .unwrap_or(0);
                let col_param = iter
                    .next()
                    .and_then(|p| p.first().copied())
                    .unwrap_or(0);
                // 1-based → 0-based; 0 treated as 1 per VT100 spec
                let row = row_param.max(1).saturating_sub(1);
                let col = col_param.max(1).saturating_sub(1);
                self.cursor.row = row.min(self.rows.saturating_sub(1));
                self.cursor.col = col.min(self.cols.saturating_sub(1));
            }
            // Erase in display
            'J' => {
                let n = params
                    .iter()
                    .next()
                    .and_then(|p| p.first().copied())
                    .unwrap_or(0);
                match n {
                    0 => {
                        // Erase from cursor to end of screen (inclusive of cursor position)
                        let row = self.cursor.row as usize;
                        let col = self.cursor.col as usize;
                        if row < self.grid.len() {
                            // Clear from cursor col to end of current row
                            for c in col..self.grid[row].len() {
                                self.grid[row][c] = Cell::default();
                            }
                            // Clear all rows below
                            for r in (row + 1)..self.grid.len() {
                                for c in 0..self.grid[r].len() {
                                    self.grid[r][c] = Cell::default();
                                }
                            }
                        }
                    }
                    1 => {
                        // Erase from start of screen to cursor (inclusive)
                        let row = self.cursor.row as usize;
                        let col = self.cursor.col as usize;
                        // Clear all rows above
                        for r in 0..row {
                            for c in 0..self.grid[r].len() {
                                self.grid[r][c] = Cell::default();
                            }
                        }
                        // Clear from start of current row to cursor col (inclusive)
                        if row < self.grid.len() {
                            for c in 0..=col.min(self.grid[row].len().saturating_sub(1)) {
                                self.grid[row][c] = Cell::default();
                            }
                        }
                    }
                    2 => {
                        // Erase entire display
                        for row in &mut self.grid {
                            for cell in row.iter_mut() {
                                *cell = Cell::default();
                            }
                        }
                    }
                    3 => {
                        // Erase entire display + clear scrollback (Pitfall 5)
                        for row in &mut self.grid {
                            for cell in row.iter_mut() {
                                *cell = Cell::default();
                            }
                        }
                        self.scrollback.clear();
                    }
                    _ => { /* scope fence: unknown ED variant */ }
                }
            }
            // Erase in line
            'K' => {
                let n = params
                    .iter()
                    .next()
                    .and_then(|p| p.first().copied())
                    .unwrap_or(0);
                let row = self.cursor.row as usize;
                let col = self.cursor.col as usize;
                if row < self.grid.len() {
                    match n {
                        0 => {
                            // Erase from cursor to end of line
                            for c in col..self.grid[row].len() {
                                self.grid[row][c] = Cell::default();
                            }
                        }
                        1 => {
                            // Erase from start of line to cursor (inclusive)
                            for c in 0..=col.min(self.grid[row].len().saturating_sub(1)) {
                                self.grid[row][c] = Cell::default();
                            }
                        }
                        2 => {
                            // Erase entire line
                            for cell in self.grid[row].iter_mut() {
                                *cell = Cell::default();
                            }
                        }
                        _ => { /* scope fence: unknown EL variant */ }
                    }
                }
            }
            // SGR — Select Graphic Rendition
            'm' => {
                self.handle_sgr(params);
            }
            _ => {
                // Scope fence: all other CSI actions (mouse, window ops, etc.) are
                // intentionally ignored. This is a permanent scope fence per D-12-02b.
            }
        }
    }

    /// Dispatch an OSC (Operating System Command) sequence.
    ///
    /// Handled:
    /// - `0` / `2` — set terminal title (icon + window / window only)
    /// - `52` — clipboard write (DETECTION ONLY, D-12-04; Phase 16 owns forwarding)
    ///
    /// All other OSC codes are scope-fenced (ignored). `params[0]` is compared as a
    /// byte slice (e.g. `b"52"` is two bytes `[0x35, 0x32]`) — NOT as an integer
    /// (Pitfall 7).
    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        if params.is_empty() {
            return;
        }
        match params[0] {
            b"0" | b"2" => {
                // Set terminal title (OSC 0: icon + window; OSC 2: window only)
                if let Some(title_bytes) = params.get(1) {
                    if let Ok(title) = std::str::from_utf8(title_bytes) {
                        self.title = Some(title.to_owned());
                    }
                }
            }
            b"52" => {
                // OSC 52 clipboard-write: detect and store pending; do NOT forward.
                // Actual clipboard passthrough is Phase 16 (D-12-04).
                // Scope fence: only parse into osc52_pending; no clipboard read/write/exec.
                let selection = params.get(1).copied().unwrap_or(b"c");
                let data = params.get(2).copied().unwrap_or(b"");
                self.osc52_pending = Some((selection.to_vec(), data.to_vec()));
            }
            _ => {
                // Scope fence: all other OSC codes (e.g. OSC 7 working dir, OSC 8 hyperlinks,
                // sixel OSC) are intentionally ignored per D-12-02b.
            }
        }
    }

    /// Dispatch an ESC sequence.
    ///
    /// Handled:
    /// - `c` (RIS — Reset to Initial State): clear grid, reset cursor, reset SGR
    ///   running attributes, reset echo state.
    ///
    /// All other ESC sequences are scope-fenced.
    ///
    /// `hook`/`put`/`unhook` (DCS sequences) are left as default no-ops — DCS is
    /// out of scope per D-12-02b.
    fn esc_dispatch(&mut self, _intermediates: &[u8], _ignore: bool, byte: u8) {
        match byte {
            b'c' => {
                // RIS: full reset
                self.grid = Self::make_grid(self.cols, self.rows);
                self.scrollback.clear();
                self.cursor = CursorPos { row: 0, col: 0 };
                self.sgr.reset();
                self.echo_state = EchoState::default();
                self.title = None;
                self.osc52_pending = None;
            }
            _ => {
                // Scope fence: other ESC sequences (SI/SO, SS2/SS3, etc.) are ignored.
            }
        }
    }

    // hook / put / unhook: default no-ops inherited from the trait.
    // DCS (device control strings, e.g. sixel) are intentionally out of scope per D-12-02b.
}

// ── Private helpers ───────────────────────────────────────────────────────────

impl TerminalState {
    /// Process SGR (Select Graphic Rendition) parameters and update the running
    /// SGR state. The updated state is applied to subsequently printed cells.
    ///
    /// # Option<u8> color model
    ///
    /// `fg`/`bg` in `SgrState` (and therefore `Cell.fg`/`bg`) use `Option<u8>`:
    /// - `None` = terminal default color (NOT the same as palette index 0 / black)
    /// - `Some(n)` = explicit palette index `n` (0..=255)
    /// - `Some(0)` is explicit black — DISTINCT from `None` (default)
    ///
    /// SGR 39 (`fg = None`) and SGR 49 (`bg = None`) return to terminal default.
    /// This maps 1:1 onto `DiffRun.fg`/`bg` for zero-conversion Phase 13 extraction.
    ///
    /// # 256-color parsing (Pitfall 6)
    ///
    /// `CSI 38 ; 5 ; n m` arrives as three separate `Params` items (not subparams
    /// when semicolon-separated). The implementation walks `params.iter()` as a
    /// stateful sequence: on seeing `38`, it grabs the next two items for `5` and `n`.
    fn handle_sgr(&mut self, params: &vte::Params) {
        // No params == SGR 0 (reset all attributes).
        let mut iter = params.iter().peekable();
        if iter.peek().is_none() {
            self.sgr.reset();
            return;
        }

        while let Some(param) = iter.next() {
            let code = param[0]; // u16
            match code {
                0 => self.sgr.reset(),
                1 => self.sgr.style.0 |= CellStyle::BOLD,
                3 => self.sgr.style.0 |= CellStyle::ITALIC,
                4 => self.sgr.style.0 |= CellStyle::UNDERLINE,
                7 => self.sgr.style.0 |= CellStyle::REVERSE,
                22 => self.sgr.style.0 &= !CellStyle::BOLD,
                23 => self.sgr.style.0 &= !CellStyle::ITALIC,
                24 => self.sgr.style.0 &= !CellStyle::UNDERLINE,
                27 => self.sgr.style.0 &= !CellStyle::REVERSE,
                // Standard foreground colors (palette 0..=7)
                30..=37 => self.sgr.fg = Some((code - 30) as u8),
                // Default foreground (NOT Some(0) — terminal default is None)
                39 => self.sgr.fg = None,
                // Standard background colors (palette 0..=7)
                40..=47 => self.sgr.bg = Some((code - 40) as u8),
                // Default background (NOT Some(0) — terminal default is None)
                49 => self.sgr.bg = None,
                // Bright/high-intensity foreground colors (palette 8..=15)
                90..=97 => self.sgr.fg = Some((code - 90 + 8) as u8),
                // Bright/high-intensity background colors (palette 8..=15)
                100..=107 => self.sgr.bg = Some((code - 100 + 8) as u8),
                // 256-color / 24-bit foreground: `38 ; 5 ; n` (Pitfall 6)
                38 => {
                    // Grab the next param — should be `5` (256-color) or `2` (24-bit, not in scope)
                    if let Some(next) = iter.next() {
                        if next[0] == 5 {
                            // 256-color: grab the color index
                            if let Some(color_param) = iter.next() {
                                self.sgr.fg = Some(color_param[0] as u8);
                            }
                        }
                        // SGR 38;2;r;g;b (24-bit) is scope-fenced — consume but ignore
                    }
                }
                // 256-color / 24-bit background: `48 ; 5 ; n` (Pitfall 6)
                48 => {
                    if let Some(next) = iter.next() {
                        if next[0] == 5 {
                            if let Some(color_param) = iter.next() {
                                self.sgr.bg = Some(color_param[0] as u8);
                            }
                        }
                    }
                }
                _ => {
                    // Scope fence: other SGR codes (strikethrough, double-underline, etc.)
                    // are intentionally ignored per D-12-02b.
                }
            }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Test helper: create a fresh TerminalState with the given dimensions.
    /// NO network/session/quinn/tokio/registry/server imports — isolation enforced.
    fn ts(cols: u16, rows: u16) -> TerminalState {
        TerminalState::new(cols, rows)
    }

    // ── Task 1 skeleton verification ─────────────────────────────────────────

    #[test]
    fn new_produces_blank_grid_at_origin() {
        let state = ts(80, 24);
        assert_eq!(state.cursor(), CursorPos { row: 0, col: 0 });
        assert_eq!(state.size(), (80, 24));
        // All cells should be default (space, no attrs, no color)
        for row in 0..24 {
            for col in 0..80 {
                let cell = state.cell(row, col);
                assert_eq!(cell.ch, ' ', "cell ({row},{col}) should be space");
                assert_eq!(cell.fg, None, "cell ({row},{col}) fg should be None");
                assert_eq!(cell.bg, None, "cell ({row},{col}) bg should be None");
                assert_eq!(cell.style.0, CellStyle::NONE);
            }
        }
        assert!(!state.echo_state().cursor_visible);
        assert!(!state.echo_state().alt_screen);
        assert!(!state.echo_state().bracketed_paste);
        assert!(!state.echo_state().app_cursor_keys);
        assert_eq!(state.title(), None);
        assert!(state.osc52_pending().is_none());
        assert_eq!(state.scrollback.len(), 0);
    }

    // ── Plain text (acceptance criterion 1) ─────────────────────────────────

    #[test]
    fn plain_text_writes_cells_and_advances_cursor() {
        let mut state = ts(80, 24);
        state.advance(b"abc");
        assert_eq!(state.cell(0, 0).ch, 'a');
        assert_eq!(state.cell(0, 1).ch, 'b');
        assert_eq!(state.cell(0, 2).ch, 'c');
        assert_eq!(state.cursor(), CursorPos { row: 0, col: 3 });
    }

    #[test]
    fn print_wraps_at_right_edge() {
        let mut state = ts(4, 4); // narrow terminal
        state.advance(b"abcde");
        // 'a','b','c','d' on row 0 cols 0-3; 'e' wraps to row 1 col 0
        assert_eq!(state.cell(0, 0).ch, 'a');
        assert_eq!(state.cell(0, 3).ch, 'd');
        assert_eq!(state.cell(1, 0).ch, 'e');
        assert_eq!(state.cursor(), CursorPos { row: 1, col: 1 });
    }

    #[test]
    fn linefeed_scrolls_when_at_bottom() {
        let mut state = ts(80, 3); // 3-row terminal
        state.advance(b"line1\nline2\nline3\n"); // scroll after 3rd newline
        // After 3 newlines from row 0, scrollback should have the first line
        assert_eq!(state.scrollback.len(), 1);
        // First scrollback row should contain 'line1' cells
        let scroll_row = &state.scrollback[0];
        assert_eq!(scroll_row[0].ch, 'l');
        assert_eq!(scroll_row[4].ch, '1');
    }

    #[test]
    fn scrollback_bounded_by_cap() {
        let mut state = ts(1, 1); // 1x1 terminal to force rapid scrolling
        // Push more than SCROLLBACK_LINE_CAP newlines
        let many_newlines = b"\n".repeat(SCROLLBACK_LINE_CAP + 100);
        state.advance(&many_newlines);
        assert!(
            state.scrollback.len() <= SCROLLBACK_LINE_CAP,
            "scrollback.len() {} must not exceed SCROLLBACK_LINE_CAP {}",
            state.scrollback.len(),
            SCROLLBACK_LINE_CAP
        );
    }

    // ── Cursor motion CSI A/B/C/D (acceptance criterion) ────────────────────

    #[test]
    fn cursor_position_cup_1based_to_0based() {
        let mut state = ts(80, 24);
        state.advance(b"\x1b[5;10H"); // row=5, col=10 (1-based)
        assert_eq!(state.cursor(), CursorPos { row: 4, col: 9 }); // 0-based
    }

    #[test]
    fn cursor_up_csi_a_by_count() {
        let mut state = ts(80, 24);
        state.advance(b"\x1b[5;10H"); // row=5, col=10 (1-based) → row=4, col=9
        state.advance(b"\x1b[2A"); // cursor up 2
        assert_eq!(state.cursor(), CursorPos { row: 2, col: 9 });
    }

    #[test]
    fn cursor_up_bare_csi_a_moves_by_1() {
        // Pitfall 3: omitted param should be treated as 1, not 0
        let mut state = ts(80, 24);
        state.advance(b"\x1b[5;10H"); // row=5, col=10 (1-based) → row=4, col=9
        state.advance(b"\x1b[A"); // bare CSI A — should move up by 1
        assert_eq!(state.cursor(), CursorPos { row: 3, col: 9 });
    }

    #[test]
    fn cursor_down_csi_b() {
        let mut state = ts(80, 24);
        state.advance(b"\x1b[2B"); // down 2 from row 0
        assert_eq!(state.cursor().row, 2);
    }

    #[test]
    fn cursor_right_csi_c() {
        let mut state = ts(80, 24);
        state.advance(b"\x1b[5C"); // right 5 from col 0
        assert_eq!(state.cursor().col, 5);
    }

    #[test]
    fn cursor_left_csi_d() {
        let mut state = ts(80, 24);
        state.advance(b"\x1b[5;10H"); // col=9 (0-based)
        state.advance(b"\x1b[3D"); // left 3
        assert_eq!(state.cursor().col, 6);
    }

    #[test]
    fn cursor_motion_clamped_to_grid_bounds() {
        let mut state = ts(80, 24);
        // Move far beyond grid bounds — must clamp, not panic
        state.advance(b"\x1b[9999;9999H");
        assert!(state.cursor().row < 24, "row must be clamped");
        assert!(state.cursor().col < 80, "col must be clamped");
        // Also test CSI A at row 0 — must not underflow
        state.advance(b"\x1b[1;1H"); // top-left
        state.advance(b"\x1b[100A"); // up 100 from row 0
        assert_eq!(state.cursor().row, 0, "row must clamp to 0 on up overflow");
    }

    // ── Erase in display (acceptance criterion) ──────────────────────────────

    #[test]
    fn erase_in_display_0_clears_below_cursor() {
        let mut state = ts(80, 24);
        state.advance(b"abc\x1b[J"); // CSI 0 J (default = 0): erase from cursor to end
        // 'a' and 'b' at cols 0,1 remain; cursor is at col 3 after writing 3 chars
        assert_eq!(state.cell(0, 0).ch, 'a');
        assert_eq!(state.cell(0, 1).ch, 'b');
        assert_eq!(state.cell(0, 2).ch, 'c');
        // Cursor is at (0,3). ED 0 clears from (0,3) to end of screen.
        assert_eq!(state.cell(0, 3).ch, ' ');
        assert_eq!(state.cell(1, 0).ch, ' ');
        assert_eq!(state.cell(23, 79).ch, ' ');
    }

    #[test]
    fn erase_in_display_2_clears_all() {
        let mut state = ts(10, 5);
        state.advance(b"hello"); // write some chars
        state.advance(b"\x1b[2J"); // CSI 2 J: erase all
        for row in 0..5 {
            for col in 0..10 {
                assert_eq!(state.cell(row, col).ch, ' ');
            }
        }
    }

    #[test]
    fn erase_in_display_3_clears_scrollback() {
        let mut state = ts(80, 3);
        // Force some scrollback
        state.advance(b"line1\nline2\nline3\n");
        assert!(!state.scrollback.is_empty());
        // CSI 3 J: erase display + scrollback
        state.advance(b"\x1b[3J");
        assert!(
            state.scrollback.is_empty(),
            "ED 3 must clear scrollback (Pitfall 5)"
        );
    }

    // ── OSC 0/2 title (acceptance criterion) ────────────────────────────────

    #[test]
    fn osc2_sets_title() {
        let mut state = ts(80, 24);
        state.advance(b"\x1b]2;My Title\x07");
        assert_eq!(state.title(), Some("My Title"));
    }

    #[test]
    fn osc0_also_sets_title() {
        let mut state = ts(80, 24);
        state.advance(b"\x1b]0;Another Title\x07");
        assert_eq!(state.title(), Some("Another Title"));
    }

    // ── OSC 52 detection (acceptance criterion) ──────────────────────────────

    #[test]
    fn osc52_detected_and_no_clipboard_action() {
        let mut state = ts(80, 24);
        // OSC 52 ; c ; SGVsbG8= BEL  ("Hello" in base64)
        state.advance(b"\x1b]52;c;SGVsbG8=\x07");
        let pending = state.osc52_pending();
        assert!(pending.is_some(), "OSC 52 must be detected");
        let (sel, data) = pending.unwrap();
        assert_eq!(sel, b"c");
        assert_eq!(data, b"SGVsbG8=");
        // No clipboard action — just detection. (We can't directly assert no
        // side-effect here, but the test verifies the parsing path without any
        // observable clipboard mutation, which is the contract per D-12-04.)
    }

    // ── DEC private modes / echo state ───────────────────────────────────────

    #[test]
    fn decset_alt_screen_toggled_by_1049() {
        let mut state = ts(80, 24);
        assert!(!state.echo_state().alt_screen);
        state.advance(b"\x1b[?1049h");
        assert!(state.echo_state().alt_screen);
        state.advance(b"\x1b[?1049l");
        assert!(!state.echo_state().alt_screen);
    }

    #[test]
    fn decset_cursor_visible_toggled_by_25() {
        let mut state = ts(80, 24);
        state.advance(b"\x1b[?25h");
        assert!(state.echo_state().cursor_visible);
        state.advance(b"\x1b[?25l");
        assert!(!state.echo_state().cursor_visible);
    }

    #[test]
    fn decset_bracketed_paste_toggled_by_2004() {
        let mut state = ts(80, 24);
        state.advance(b"\x1b[?2004h");
        assert!(state.echo_state().bracketed_paste);
        state.advance(b"\x1b[?2004l");
        assert!(!state.echo_state().bracketed_paste);
    }

    #[test]
    fn decset_app_cursor_keys_toggled_by_1() {
        let mut state = ts(80, 24);
        state.advance(b"\x1b[?1h");
        assert!(state.echo_state().app_cursor_keys);
        state.advance(b"\x1b[?1l");
        assert!(!state.echo_state().app_cursor_keys);
    }

    #[test]
    fn decset_combined_multiple_modes_in_one_sequence() {
        let mut state = ts(80, 24);
        // Combined: \x1b[?25;1049h sets both cursor_visible and alt_screen
        state.advance(b"\x1b[?25;1049h");
        assert!(state.echo_state().cursor_visible);
        assert!(state.echo_state().alt_screen);
    }

    // ── SGR attributes ────────────────────────────────────────────────────────

    #[test]
    fn sgr_bold_and_reset() {
        let mut state = ts(80, 24);
        state.advance(b"\x1b[1mA\x1b[0mB");
        let a = state.cell(0, 0);
        let b_cell = state.cell(0, 1);
        assert_eq!(
            a.style.0 & CellStyle::BOLD,
            CellStyle::BOLD,
            "cell 0 must have BOLD set"
        );
        assert_eq!(b_cell.style.0, CellStyle::NONE, "cell 1 must have no style after SGR 0");
    }

    #[test]
    fn sgr_bare_m_resets_all() {
        // Pitfall 4: bare CSI m (no params) = SGR 0 (reset)
        let mut state = ts(80, 24);
        state.advance(b"\x1b[1;3;4mA"); // BOLD|ITALIC|UNDERLINE
        state.advance(b"\x1b[m"); // bare CSI m = reset
        state.advance(b"B");
        assert_eq!(state.cell(0, 1).style.0, CellStyle::NONE);
        assert_eq!(state.cell(0, 1).fg, None);
        assert_eq!(state.cell(0, 1).bg, None);
    }

    #[test]
    fn sgr_256_color_fg() {
        let mut state = ts(80, 24);
        state.advance(b"\x1b[38;5;201mZ");
        assert_eq!(state.cell(0, 0).fg, Some(201));
    }

    #[test]
    fn sgr_256_color_bg() {
        let mut state = ts(80, 24);
        state.advance(b"\x1b[48;5;100mZ");
        assert_eq!(state.cell(0, 0).bg, Some(100));
    }

    // ── Option<u8> color model: None vs Some(0) ─────────────────────────────

    #[test]
    fn default_color_is_none_not_some_zero() {
        let mut state = ts(80, 24);
        state.advance(b"a"); // default-color write
        assert_eq!(state.cell(0, 0).fg, None, "default fg must be None, not Some(0)");
        assert_eq!(state.cell(0, 0).bg, None, "default bg must be None, not Some(0)");
    }

    #[test]
    fn explicit_black_is_some_zero_distinct_from_default() {
        let mut state = ts(80, 24);
        state.advance(b"\x1b[30mB"); // SGR 30 → explicit black fg (palette index 0)
        assert_eq!(
            state.cell(0, 0).fg,
            Some(0),
            "explicit black fg must be Some(0)"
        );
        // Reset fg to default via SGR 39
        state.advance(b"\x1b[39mC"); // SGR 39 → default fg
        assert_eq!(
            state.cell(0, 1).fg,
            None,
            "SGR 39 must restore fg to None (default), not Some(0)"
        );
        // Confirm Some(0) and None are not equal
        assert_ne!(Some(0u8), None, "Some(0) must be distinct from None");
    }

    #[test]
    fn sgr_39_49_reset_fg_bg_to_none() {
        let mut state = ts(80, 24);
        // Set explicit fg/bg colors
        state.advance(b"\x1b[31;42mA"); // fg=Some(1) (red), bg=Some(2) (green)
        assert_eq!(state.cell(0, 0).fg, Some(1));
        assert_eq!(state.cell(0, 0).bg, Some(2));
        // Reset fg with SGR 39, bg with SGR 49
        state.advance(b"\x1b[39;49mB");
        assert_eq!(state.cell(0, 1).fg, None, "SGR 39 must reset fg to None");
        assert_eq!(state.cell(0, 1).bg, None, "SGR 49 must reset bg to None");
    }

    // ── Adversarial robustness ───────────────────────────────────────────────

    #[test]
    fn adversarial_huge_cursor_position_clamped() {
        let mut state = ts(80, 24);
        // Must not panic; must clamp to grid bounds
        state.advance(b"\x1b[9999;9999H");
        assert!(state.cursor().row < 24, "row must be < rows");
        assert!(state.cursor().col < 80, "col must be < cols");
    }

    #[test]
    fn adversarial_long_newline_burst_bounded_scrollback() {
        let mut state = ts(80, 10);
        // Push many more newlines than SCROLLBACK_LINE_CAP
        let burst = b"\n".repeat(SCROLLBACK_LINE_CAP + 500);
        state.advance(&burst);
        assert!(
            state.scrollback.len() <= SCROLLBACK_LINE_CAP,
            "scrollback must be bounded: {} > {}",
            state.scrollback.len(),
            SCROLLBACK_LINE_CAP
        );
    }

    #[test]
    fn adversarial_out_of_bounds_cell_access_returns_default() {
        let state = ts(80, 24);
        // Access beyond grid bounds must not panic
        let cell = state.cell(100, 100);
        assert_eq!(cell.ch, ' ');
        assert_eq!(cell.fg, None);
    }

    // ── Erase in line ────────────────────────────────────────────────────────

    #[test]
    fn erase_in_line_0_clears_right() {
        let mut state = ts(10, 5);
        state.advance(b"hello"); // cols 0-4
        state.advance(b"\x1b[2D"); // back 2: cursor at col 3
        state.advance(b"\x1b[K"); // CSI 0 K: erase to right (incl cursor)
        assert_eq!(state.cell(0, 0).ch, 'h');
        assert_eq!(state.cell(0, 2).ch, 'l');
        assert_eq!(state.cell(0, 3).ch, ' ');
        assert_eq!(state.cell(0, 4).ch, ' ');
    }

    #[test]
    fn erase_in_line_2_clears_whole_line() {
        let mut state = ts(10, 5);
        state.advance(b"hello");
        state.advance(b"\x1b[2K"); // CSI 2 K: erase whole line
        for col in 0..10 {
            assert_eq!(state.cell(0, col).ch, ' ');
        }
    }

    // ── Carriage return / backspace ──────────────────────────────────────────

    #[test]
    fn carriage_return_resets_col() {
        let mut state = ts(80, 24);
        state.advance(b"hello\r");
        assert_eq!(state.cursor().col, 0);
    }

    #[test]
    fn backspace_decrements_col() {
        let mut state = ts(80, 24);
        state.advance(b"abc\x08");
        assert_eq!(state.cursor().col, 2);
    }

    // ── ESC c (RIS) reset ────────────────────────────────────────────────────

    #[test]
    fn esc_c_ris_resets_state() {
        let mut state = ts(80, 24);
        state.advance(b"\x1b[?1049h"); // set alt screen
        state.advance(b"\x1b]2;Title\x07"); // set title
        state.advance(b"hello");
        // ESC c is the RIS (Reset to Initial State) sequence: 0x1B 0x63.
        // Note: \x1b[c (with '[') is CSI 'c' (Device Attributes), NOT RIS.
        // RIS is ESC c without any intermediate '['.
        state.advance(b"\x1bc"); // ESC 'c' = RIS (0x1B, 0x63)
        assert_eq!(state.cursor(), CursorPos { row: 0, col: 0 });
        assert!(!state.echo_state().alt_screen);
        assert_eq!(state.title(), None);
        assert_eq!(state.cell(0, 0).ch, ' ');
    }

    // ── mem::take borrow-split: SCROLLBACK_LINE_CAP const present ───────────

    #[test]
    fn scrollback_line_cap_constant_is_present_and_correct() {
        // Just verifies the constant is accessible and has the expected value.
        assert_eq!(SCROLLBACK_LINE_CAP, 10_000);
    }
}
