//! `ClientScreen` — confirmed framebuffer compositor (Mosh Display model).
//!
//! Implements D-14-01, D-14-01a, D-14-04, D-14-05.
//!
//! # Architecture
//!
//! The screen maintains two grids:
//! - **confirmed**: cells last applied from datagram `StateDiff` messages (the
//!   server-authoritative state).
//! - **physical**: what ANSI escape sequences have actually been emitted to the
//!   terminal (the last-rendered state).
//!
//! `render_to_stdout` composes `desired = confirmed ⊕ overlays`, diffs against
//! `physical`, and emits only the minimal ANSI needed to advance `physical` to
//! `desired`. This is the ONLY function that may write display output — per
//! CLAUDE.md "single screen-composition path, never direct stdout once predictor
//! exists".
//!
//! # Security
//!
//! - T-14-01: OOB row/col guards in `apply` (continue/break on range checks).
//! - T-14-03: Monotonic epoch guard — stale/replayed diffs are silently discarded.
//! - Only `Cell.ch` (a single Unicode scalar validated by the server's TerminalState)
//!   reaches stdout. No raw server byte stream passes through.

use std::io::Write;

use crossterm::cursor::MoveTo;
use crossterm::QueueableCommand;
use nosh_proto::datagram::{CellStyle, CursorPos, StateDiff};

use crate::predictor::PredictionOverlay;

/// Maximum allowed terminal width — any larger value in a `StateDiff` is
/// rejected before `resize()` is called. Keeps the two-grid allocation at
/// `≤ 512 × 256 × 12 × 2 ≈ 3 MB` and closes the T-14-02 OOM-crash vector
/// (a compromised server sending `cols=65535, rows=65535` would otherwise
/// attempt a ~103 GB allocation).
const MAX_TERMINAL_COLS: u16 = 512;
/// Maximum allowed terminal height (see `MAX_TERMINAL_COLS`).
const MAX_TERMINAL_ROWS: u16 = 256;

// ── Cell ──────────────────────────────────────────────────────────────────────

/// A single terminal cell in the client's framebuffer.
///
/// Field-for-field mirror of `nosh_server::terminal::Cell` (D-14-04). Declared
/// locally so production code does NOT import `nosh_server` (which is a
/// `[dev-dependency]` only).
#[derive(Clone, PartialEq, Eq)]
pub struct Cell {
    /// Unicode scalar value. `' '` = blank/empty.
    pub ch: char,
    /// SGR attributes packed as bitflags (same type as `DiffRun.style`).
    pub style: CellStyle,
    /// ANSI 256-color foreground. `None` = terminal default.
    pub fg: Option<u8>,
    /// ANSI 256-color background. `None` = terminal default.
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

// ── Overlay trait ─────────────────────────────────────────────────────────────

/// A screen overlay layer applied on top of the confirmed grid in `compose_desired`.
///
/// Phase 14: only `ConnectionLossOverlay` exists and it is a no-op.
/// Phase 15 will slot the speculative-echo overlay here.
/// Phase 16 will activate the loss-banner overlay here.
pub trait Overlay {
    /// Return `Some(cell)` to override the confirmed cell at `(row, col)`, or
    /// `None` to pass the confirmed cell through unchanged.
    fn cell_at(&self, row: u16, col: u16) -> Option<Cell>;
}

/// Connection-loss overlay (D-14-01a, Phase 16).
///
/// When `active` is true and no datagram has arrived for >5 s, overlays a
/// row-0 reverse-video banner with a live elapsed-seconds counter and the
/// `~.` disconnect hint (Mosh convention). Activated / cleared by `run_pump`.
pub struct ConnectionLossOverlay {
    /// True when the overlay banner should be shown.
    pub active: bool,
    /// `Instant` of the last received datagram — used to compute elapsed secs.
    pub last_contact: std::time::Instant,
    /// Terminal width (columns) — used to pad the banner to full width.
    pub cols: u16,
}

impl ConnectionLossOverlay {
    /// Create a new, inactive overlay.  `last_contact` is set to `Instant::now()`
    /// (safe initial value — the timer in `run_pump` will overwrite it on activation).
    pub fn new(cols: u16) -> Self {
        ConnectionLossOverlay {
            active: false,
            last_contact: std::time::Instant::now(),
            cols,
        }
    }
}

impl Overlay for ConnectionLossOverlay {
    /// Return `None` unless `active && row == 0`.
    ///
    /// When active, builds a banner like:
    /// `nosh: reconnecting — last contact 7s ago. Press ~. to disconnect.`
    /// padded with spaces to `cols` width, rendered in reverse-video (SGR 7).
    fn cell_at(&self, row: u16, col: u16) -> Option<Cell> {
        if !self.active || row != 0 {
            return None;
        }
        let elapsed = self.last_contact.elapsed().as_secs();
        let banner = format!(
            "nosh: reconnecting \u{2014} last contact {elapsed}s ago. Press ~. to disconnect."
        );
        // Space-pad to terminal width.
        let padded: Vec<char> = banner
            .chars()
            .chain(std::iter::repeat(' '))
            .take(self.cols as usize)
            .collect();
        let ch = padded.get(col as usize).copied().unwrap_or(' ');
        Some(Cell {
            ch,
            style: CellStyle(CellStyle::REVERSE),
            fg: None,
            bg: None,
        })
    }
}

// ── ClientScreen ──────────────────────────────────────────────────────────────

/// Confirmed framebuffer compositor for the `nosh` client.
///
/// Holds two grids (`confirmed` and `physical`) and emits minimal ANSI diffs
/// via `render_to_stdout`. See module-level doc for the full model.
pub struct ClientScreen {
    cols: u16,
    rows: u16,
    /// Cells last applied from datagram `StateDiff` messages (server-authoritative).
    confirmed: Vec<Vec<Cell>>,
    /// Cursor position from the last applied `StateDiff`.
    confirmed_cursor: CursorPos,
    /// Cells that have been emitted to the terminal (last-rendered state).
    physical: Vec<Vec<Cell>>,
    /// Tracked physical cursor position (last `MoveTo` emitted).
    physical_cursor: CursorPos,
    /// Monotonic epoch for staleness/replay detection (D-14-05).
    /// Initialised to 0; server epochs start at 1, so the first diff always applies.
    last_applied_epoch: u64,
    /// Overlay stack (D-14-01a). Phase 14: one `ConnectionLossOverlay` no-op.
    overlays: Vec<Box<dyn Overlay>>,
}

impl ClientScreen {
    // ── Construction ──────────────────────────────────────────────────────────

    /// Create a new `ClientScreen` with the given terminal dimensions.
    ///
    /// Both `confirmed` and `physical` grids are initialised to default cells.
    /// `ConnectionLossOverlay` is no longer in the overlay Vec — it is hoisted
    /// to `run_pump` (mutably owned, mirroring `PredictionOverlay`) so Phase 16
    /// can activate it without requiring interior mutability (Pitfall 3).
    pub fn new(cols: u16, rows: u16) -> Self {
        ClientScreen {
            cols,
            rows,
            confirmed: Self::make_grid(cols, rows),
            confirmed_cursor: CursorPos { row: 0, col: 0 },
            physical: Self::make_grid(cols, rows),
            physical_cursor: CursorPos { row: 0, col: 0 },
            last_applied_epoch: 0, // server starts at 1 → first diff always applies (Pitfall 6)
            overlays: vec![],
        }
    }

    /// Build a blank grid of `rows` × `cols` default cells.
    fn make_grid(cols: u16, rows: u16) -> Vec<Vec<Cell>> {
        (0..rows as usize)
            .map(|_| vec![Cell::default(); cols as usize])
            .collect()
    }

    // ── Apply ─────────────────────────────────────────────────────────────────

    /// Apply a `StateDiff` to the confirmed grid.
    ///
    /// D-14-05 monotonic guard: diffs with an epoch ≤ `last_applied_epoch` are
    /// silently discarded (stale / duplicate / replayed — T-14-03).
    ///
    /// If the diff's `cols`/`rows` differ from the current dimensions,
    /// `resize` is called first (physical resets to blank → forces full repaint).
    ///
    /// # Security (T-14-01 — V5 input validation)
    ///
    /// - `run.row >= rows`: the entire run is skipped (`continue`).
    /// - `col >= row_width`: writing stops at the row boundary (`break`).
    ///
    /// No panic, no out-of-bounds write.
    pub fn apply(&mut self, diff: &StateDiff) {
        // D-14-05: monotonic staleness check — discard stale or duplicate diffs.
        if diff.epoch <= self.last_applied_epoch {
            return;
        }

        // T-14-02: reject implausible terminal dimensions BEFORE resize() allocates
        // two grids proportional to cols × rows. A compromised server sending
        // cols=65535, rows=65535 would otherwise attempt a ~103 GB allocation.
        // Zero dimensions are also rejected (make_grid with rows=0 or cols=0
        // produces a degenerate grid that could cause subtle render issues).
        if diff.cols == 0
            || diff.rows == 0
            || diff.cols > MAX_TERMINAL_COLS
            || diff.rows > MAX_TERMINAL_ROWS
        {
            tracing::warn!(
                epoch = diff.epoch,
                cols = diff.cols,
                rows = diff.rows,
                "StateDiff dimensions out of range — discarding (T-14-02)"
            );
            return;
        }

        // Resize if terminal dimensions changed.
        if diff.cols != self.cols || diff.rows != self.rows {
            self.resize(diff.cols, diff.rows);
        }

        // Apply each DiffRun to the confirmed grid.
        for run in &diff.runs {
            let row = run.row as usize;
            if row >= self.confirmed.len() {
                continue; // OOB row guard (T-14-01 / SECURITY V5)
            }
            let row_cells = &mut self.confirmed[row];
            let start = run.start_col as usize;
            for (col, ch) in (start..).zip(run.chars.chars()) {
                if col >= row_cells.len() {
                    break; // OOB col guard (T-14-01 / SECURITY V5)
                }
                row_cells[col] = Cell {
                    ch,
                    style: run.style,
                    fg: run.fg,
                    bg: run.bg,
                };
            }
        }

        self.confirmed_cursor = diff.cursor;
        self.last_applied_epoch = diff.epoch;
    }

    // ── Resize ────────────────────────────────────────────────────────────────

    /// Resize both grids to the new dimensions.
    ///
    /// - `confirmed`: rows and columns are truncated or extended with default cells.
    /// - `physical`: **reset to blank** (NOT copied from confirmed) — Pitfall 2.
    ///   The next `render_to_stdout` will perform a full repaint.
    /// - Cursors are clamped to the new bounds.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        // Resize confirmed: adjust each row's column count.
        let new_cols = cols as usize;
        for row in &mut self.confirmed {
            if row.len() > new_cols {
                row.truncate(new_cols);
            } else {
                row.resize(new_cols, Cell::default());
            }
        }

        // Grow or shrink confirmed rows.
        let cur_rows = self.confirmed.len();
        let new_rows = rows as usize;
        if cur_rows > new_rows {
            self.confirmed.truncate(new_rows);
        } else {
            for _ in cur_rows..new_rows {
                self.confirmed.push(vec![Cell::default(); new_cols]);
            }
        }

        // Physical MUST reset to blank (NOT copied) — Pitfall 2.
        // After a resize the terminal is blank; resetting physical forces a full
        // repaint on the next render_to_stdout call.
        self.physical = Self::make_grid(cols, rows);

        self.cols = cols;
        self.rows = rows;

        // Clamp cursors to new bounds.
        self.confirmed_cursor.row = self.confirmed_cursor.row.min(rows.saturating_sub(1));
        self.confirmed_cursor.col = self.confirmed_cursor.col.min(cols.saturating_sub(1));
        self.physical_cursor = CursorPos { row: 0, col: 0 };
    }

    // ── Render ────────────────────────────────────────────────────────────────

    /// Compose the desired grid: `confirmed ⊕ overlays`.
    ///
    /// This is the **single composition seam** (D-14-01a). Phase 14 overlays are
    /// all no-ops so `desired == confirmed`, but the loop MUST remain so Phase 15
    /// (speculative overlay) and Phase 16 (loss banner) can slot in without
    /// restructuring.
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

    /// Diff `desired` against `physical` and emit minimal ANSI to `out`.
    ///
    /// This is the **only** function in `nosh-client` that writes display output
    /// (CLAUDE.md single-path invariant). After writing, `physical` is updated to
    /// `desired` so the next call emits nothing if nothing changed (idempotency).
    ///
    /// # I/O
    ///
    /// `out` is `&mut impl std::io::Write` — NOT `tokio::io::AsyncWrite` (Pitfall 1).
    /// The caller buffers into `Vec<u8>` and flushes to tokio stdout with
    /// `write_all` after this function returns.
    ///
    /// # Cursor
    ///
    /// `MoveTo(col, row)` — crossterm uses **(col, row)** order with 0-based
    /// coordinates (Pitfall 7). A final `MoveTo` is always emitted to position the
    /// cursor at `confirmed_cursor`.
    pub fn render_to_stdout<W: Write>(&mut self, out: &mut W) -> std::io::Result<()> {
        self.render_to_stdout_with_cursor(out, None)
    }

    /// Variant of `render_to_stdout` that overrides the final cursor `MoveTo`.
    ///
    /// When `cursor_override` is `Some(pos)`, the final cursor `MoveTo` is emitted
    /// at `pos` instead of `confirmed_cursor`. Used by the speculative-echo overlay
    /// to position the cursor at the predicted position (Phase 15).
    ///
    /// When `cursor_override` is `None`, behaviour is identical to `render_to_stdout`.
    pub fn render_to_stdout_with_cursor<W: Write>(
        &mut self,
        out: &mut W,
        cursor_override: Option<CursorPos>,
    ) -> std::io::Result<()> {
        let desired = self.compose_desired();
        let desired_cursor = cursor_override.unwrap_or(self.confirmed_cursor);

        self.emit_diff(out, &desired, desired_cursor)?;
        Ok(())
    }

    /// Render the confirmed grid composed with the loss overlay and the prediction overlay.
    ///
    /// This method is the **single display path** for speculative-echo rendering
    /// (Phase 15/16, CLAUDE.md single-path invariant). It:
    ///
    /// 1. Composes `desired = confirmed ⊕ overlays` (existing `compose_desired`).
    /// 2. Applies `loss.cell_at(r, c)` as the next overlay layer (row-0 banner when active).
    /// 3. Applies `predictor.cell_at(r, c)` on top of the loss overlay.
    /// 4. Emits the minimal ANSI diff against `physical`.
    /// 5. Uses `predictor.predicted_cursor().unwrap_or(confirmed_cursor)` as the
    ///    final `MoveTo` target.
    ///
    /// Both `loss` and `predictor` are NOT in the `overlays` Vec — they are mutably
    /// owned by `run_pump` (for activation / `on_input` / `cull` calls). The predictor
    /// renders on top of the loss banner so speculative echo overrides the banner chars
    /// when the user types (edge-case tolerance — banner is row 0, echo is row 0+ cursor).
    pub fn render_with_predictor<W: Write>(
        &mut self,
        out: &mut W,
        predictor: &PredictionOverlay,
        loss: &ConnectionLossOverlay,
    ) -> std::io::Result<()> {
        // Step 1: compose confirmed ⊕ existing overlays (none in Phase 16 overlays Vec).
        let mut desired = self.compose_desired();

        // Step 2: apply the connection-loss banner overlay (row 0, reverse-video).
        for (r, row_cells) in desired.iter_mut().enumerate() {
            for (c, cell_slot) in row_cells.iter_mut().enumerate() {
                if let Some(cell) = loss.cell_at(r as u16, c as u16) {
                    *cell_slot = cell;
                }
            }
        }

        // Step 3: apply predictor overlay cells on top.
        for (r, row_cells) in desired.iter_mut().enumerate() {
            for (c, cell_slot) in row_cells.iter_mut().enumerate() {
                if let Some(cell) = predictor.cell_at(r as u16, c as u16) {
                    *cell_slot = cell;
                }
            }
        }

        // Step 4 + 5: emit diff with predicted cursor override.
        let desired_cursor = predictor.predicted_cursor().unwrap_or(self.confirmed_cursor);
        self.emit_diff(out, &desired, desired_cursor)?;
        Ok(())
    }

    /// Shared ANSI-diff emitter: diff `desired` against `physical` and emit
    /// minimal ANSI escape sequences to `out`, then commit `physical = desired`.
    ///
    /// This is the single place that writes cell content and cursor moves (the
    /// "single ANSI-diff loop" acceptance criterion for Task 1).
    ///
    /// `desired_cursor`: the final cursor position to emit with `MoveTo`.
    fn emit_diff<W: Write>(
        &mut self,
        out: &mut W,
        desired: &[Vec<Cell>],
        desired_cursor: CursorPos,
    ) -> std::io::Result<()> {
        let rows = desired.len().min(self.physical.len());
        let cols = if rows > 0 {
            desired[0].len().min(self.physical[0].len())
        } else {
            0
        };

        let mut last_row: Option<u16> = None;
        let mut last_col: Option<u16> = None;
        let mut last_sgr: Option<(CellStyle, Option<u8>, Option<u8>)> = None;

        for (r, (des_row, phys_row)) in desired.iter().zip(self.physical.iter()).enumerate().take(rows) {
            let row = r as u16;
            for (c, (want, have)) in des_row.iter().zip(phys_row.iter()).enumerate().take(cols) {
                let col = c as u16;
                if want == have {
                    continue; // idempotent: skip unchanged cells (Pitfall 5)
                }

                // Move cursor only when not already positioned at (row, col).
                if last_row != Some(row) || last_col != Some(col) {
                    // Pitfall 7: MoveTo(col, row) — first arg is COLUMN, second is ROW.
                    out.queue(MoveTo(col, row))?;
                    last_row = Some(row);
                    last_col = Some(col);
                }

                // Emit SGR only when attributes differ from the previous cell.
                let want_sgr = (want.style, want.fg, want.bg);
                if last_sgr != Some(want_sgr) {
                    emit_sgr(out, want.style, want.fg, want.bg)?;
                    last_sgr = Some(want_sgr);
                }

                // Write the character (single Unicode scalar).
                let mut buf = [0u8; 4];
                let s = want.ch.encode_utf8(&mut buf);
                out.write_all(s.as_bytes())?;

                // Advance tracked column position.
                last_col = Some(last_col.unwrap_or(col) + 1);
            }
        }

        // Always emit a final cursor-position MoveTo.
        out.queue(MoveTo(desired_cursor.col, desired_cursor.row))?;
        out.flush()?;

        // Commit: set physical = desired.
        for (des_row, phys_row) in desired.iter().zip(self.physical.iter_mut()) {
            for (des_cell, phys_cell) in des_row.iter().zip(phys_row.iter_mut()) {
                *phys_cell = des_cell.clone();
            }
        }
        self.physical_cursor = desired_cursor;

        Ok(())
    }

    /// Reset `physical` to all-default cells so the next `render_to_stdout` is a
    /// full repaint.
    ///
    /// Called by the reattach path (Plan 02) after reconnecting, to compensate for
    /// terminal state that may have changed during the connection drop (scrolling,
    /// resize, etc.) — Open Question 3 in RESEARCH.md.
    pub fn reset_physical(&mut self) {
        self.physical = Self::make_grid(self.cols, self.rows);
        self.physical_cursor = CursorPos { row: 0, col: 0 };
    }

    /// Emit a physical clear to the terminal and reset the physical grid.
    ///
    /// Writes `\x1b[2J\x1b[H` (ED2 = Erase Entire Display, then cursor-home) to `out`,
    /// then calls `reset_physical()` so the subsequent `render_to_stdout` forces a full
    /// repaint from a known-clean terminal state.
    ///
    /// # Invariant exception (D-03 / CONTEXT.md)
    ///
    /// This is the **ONE sanctioned exception** to the "all output through
    /// `render_to_stdout`" invariant. It fires **once**, at connect time, before the
    /// first datagram arrives. Because it calls `reset_physical()` after writing the
    /// escape sequence, all subsequent output still flows through `render_to_stdout`
    /// and the physical-model / terminal-state invariant is preserved for every
    /// subsequent render.
    ///
    /// The ED2+home escape is the minimal choice: it clears the terminal without
    /// disturbing the confirmed or physical grids (which are reset separately via
    /// `reset_physical`).
    ///
    /// # Call site
    ///
    /// Called once from `run_pump` immediately after constructing `ClientScreen`,
    /// before entering the datagram receive loop (plan 999.3-04). Buffer into
    /// `Vec<u8>` and flush to tokio stdout with `write_all` + `flush` (the
    /// established pattern from the render path).
    pub fn emit_connect_clear<W: Write>(&mut self, out: &mut W) -> std::io::Result<()> {
        out.write_all(b"\x1b[2J\x1b[H")?;
        self.reset_physical();
        Ok(())
    }

    // ── Read API ──────────────────────────────────────────────────────────────

    /// Read a cell from the confirmed grid.
    ///
    /// Returns a shared default `Cell` for out-of-bounds coordinates (never panics).
    /// Uses `OnceLock` per the IN-02 precedent (`terminal.rs` `cell()` method).
    pub fn confirmed_cell(&self, row: u16, col: u16) -> &Cell {
        static DEFAULT_CELL: std::sync::OnceLock<Cell> = std::sync::OnceLock::new();
        let default = DEFAULT_CELL.get_or_init(Cell::default);
        self.confirmed
            .get(row as usize)
            .and_then(|r| r.get(col as usize))
            .unwrap_or(default)
    }

    /// Return the epoch of the last successfully applied `StateDiff`.
    pub fn last_applied_epoch(&self) -> u64 {
        self.last_applied_epoch
    }

    /// Return the cursor position from the last applied `StateDiff`.
    ///
    /// Used by the prediction overlay to seed `predicted_cursor` from the
    /// confirmed position so predictions land on the correct row (CR-01 fix).
    pub fn confirmed_cursor(&self) -> CursorPos {
        self.confirmed_cursor
    }

    /// Return the current terminal dimensions as `(cols, rows)`.
    pub fn size(&self) -> (u16, u16) {
        (self.cols, self.rows)
    }
}

// ── SGR emission ──────────────────────────────────────────────────────────────

/// Emit an ANSI SGR escape sequence resetting all attributes then re-applying
/// the given style, fg, and bg.
///
/// Format: `\x1b[0[;1][;3][;4][;7][;38;5;N][;48;5;N]m`
///
/// Always starts with SGR 0 (reset) so that attributes from a previous cell
/// that are not present in `style` are cleared.
fn emit_sgr<W: Write>(
    out: &mut W,
    style: CellStyle,
    fg: Option<u8>,
    bg: Option<u8>,
) -> std::io::Result<()> {
    let mut params = String::from("0");
    if style.0 & CellStyle::BOLD != 0 {
        params.push_str(";1");
    }
    if style.0 & CellStyle::ITALIC != 0 {
        params.push_str(";3");
    }
    if style.0 & CellStyle::UNDERLINE != 0 {
        params.push_str(";4");
    }
    if style.0 & CellStyle::REVERSE != 0 {
        params.push_str(";7");
    }
    if let Some(n) = fg {
        params.push_str(&format!(";38;5;{n}"));
    }
    if let Some(n) = bg {
        params.push_str(&format!(";48;5;{n}"));
    }
    write!(out, "\x1b[{params}m")
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use nosh_proto::datagram::{CellStyle, CursorPos, DiffRun, StateDiff};

    use super::*;
    use crate::predictor::{PredictDisplayMode, PredictionOverlay};

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn make_diff(epoch: u64, chars: &str) -> StateDiff {
        StateDiff {
            epoch,
            cols: 80,
            rows: 24,
            cursor: CursorPos { row: 0, col: 0 },
            runs: vec![DiffRun {
                row: 0,
                start_col: 0,
                style: CellStyle(CellStyle::NONE),
                fg: None,
                bg: None,
                chars: chars.to_string(),
            }],
        }
    }

    // ── Task 1: model / apply / resize / bounds / overlay tests ──────────────

    #[test]
    fn apply_fresh_writes_chars_to_confirmed_grid() {
        let mut screen = ClientScreen::new(80, 24);
        let diff = make_diff(1, "hello");

        screen.apply(&diff);

        assert_eq!(screen.confirmed_cell(0, 0).ch, 'h');
        assert_eq!(screen.confirmed_cell(0, 1).ch, 'e');
        assert_eq!(screen.confirmed_cell(0, 2).ch, 'l');
        assert_eq!(screen.confirmed_cell(0, 3).ch, 'l');
        assert_eq!(screen.confirmed_cell(0, 4).ch, 'o');
        assert_eq!(screen.last_applied_epoch(), 1);
    }

    #[test]
    fn apply_monotonic_same_epoch_is_noop() {
        let mut screen = ClientScreen::new(80, 24);
        let diff1 = make_diff(1, "hello");
        screen.apply(&diff1);

        // Apply a different diff with the SAME epoch — must be ignored.
        let diff_same = StateDiff {
            epoch: 1, // same epoch
            cols: 80,
            rows: 24,
            cursor: CursorPos { row: 0, col: 5 },
            runs: vec![DiffRun {
                row: 0,
                start_col: 0,
                style: CellStyle(CellStyle::NONE),
                fg: None,
                bg: None,
                chars: "XXXXX".to_string(),
            }],
        };
        screen.apply(&diff_same);

        // Confirmed grid must still have the first apply's content.
        assert_eq!(screen.confirmed_cell(0, 0).ch, 'h');
        assert_eq!(screen.last_applied_epoch(), 1);
    }

    #[test]
    fn apply_monotonic_lower_epoch_is_noop() {
        let mut screen = ClientScreen::new(80, 24);
        let diff2 = make_diff(2, "world");
        screen.apply(&diff2);

        let diff1 = make_diff(1, "hello"); // older epoch
        screen.apply(&diff1);

        // Confirmed grid must still have epoch=2's content.
        assert_eq!(screen.confirmed_cell(0, 0).ch, 'w');
        assert_eq!(screen.last_applied_epoch(), 2);
    }

    #[test]
    fn apply_resize_changes_dims_and_resets_physical() {
        let mut screen = ClientScreen::new(80, 24);

        // First apply some content.
        screen.apply(&make_diff(1, "hello"));
        assert_eq!(screen.size(), (80, 24));

        // Now apply a diff with different dimensions.
        let diff_resized = StateDiff {
            epoch: 2,
            cols: 40,
            rows: 10,
            cursor: CursorPos { row: 0, col: 0 },
            runs: vec![],
        };
        screen.apply(&diff_resized);

        assert_eq!(screen.size(), (40, 10));
        // Physical should be reset to blank (all default cells).
        // Direct check: physical was rebuilt to 10 rows × 40 cols.
        assert_eq!(screen.confirmed.len(), 10);
        assert_eq!(screen.confirmed[0].len(), 40);
        assert_eq!(screen.physical.len(), 10);
        assert_eq!(screen.physical[0].len(), 40);
    }

    #[test]
    fn apply_oob_row_is_skipped_no_panic() {
        let mut screen = ClientScreen::new(80, 24);
        let diff = StateDiff {
            epoch: 1,
            cols: 80,
            rows: 24,
            cursor: CursorPos { row: 0, col: 0 },
            runs: vec![
                DiffRun {
                    row: 999, // WAY out of bounds
                    start_col: 0,
                    style: CellStyle(CellStyle::NONE),
                    fg: None,
                    bg: None,
                    chars: "boom".to_string(),
                },
                DiffRun {
                    row: 0, // valid run after the OOB one
                    start_col: 0,
                    style: CellStyle(CellStyle::NONE),
                    fg: None,
                    bg: None,
                    chars: "ok".to_string(),
                },
            ],
        };
        // Must not panic.
        screen.apply(&diff);
        // The valid run must still have been applied.
        assert_eq!(screen.confirmed_cell(0, 0).ch, 'o');
        assert_eq!(screen.confirmed_cell(0, 1).ch, 'k');
    }

    #[test]
    fn apply_oob_col_chars_clamped_no_panic() {
        let mut screen = ClientScreen::new(5, 3); // tiny grid: 5 cols, 3 rows
        let diff = StateDiff {
            epoch: 1,
            cols: 5,
            rows: 3,
            cursor: CursorPos { row: 0, col: 0 },
            runs: vec![DiffRun {
                row: 0,
                start_col: 3, // starts at col 3 (valid), but 8 chars → would overflow
                style: CellStyle(CellStyle::NONE),
                fg: None,
                bg: None,
                chars: "ABCDEFGH".to_string(), // only A,B can fit (cols 3,4)
            }],
        };
        // Must not panic.
        screen.apply(&diff);
        // Cols 3 and 4 should be written; col 5+ clamped.
        assert_eq!(screen.confirmed_cell(0, 3).ch, 'A');
        assert_eq!(screen.confirmed_cell(0, 4).ch, 'B');
        // Out-of-bounds access returns default (space).
        assert_eq!(screen.confirmed_cell(0, 5).ch, ' ');
    }

    #[test]
    fn confirmed_cell_oob_returns_default_no_panic() {
        let screen = ClientScreen::new(80, 24);
        // Row out of bounds.
        assert_eq!(screen.confirmed_cell(100, 0).ch, ' ');
        // Col out of bounds.
        assert_eq!(screen.confirmed_cell(0, 200).ch, ' ');
        // Both out of bounds.
        assert_eq!(screen.confirmed_cell(999, 999).ch, ' ');
    }

    // ── CR-01: dimension bounds guard (T-14-02 OOM guard) ────────────────────

    #[test]
    fn apply_oversized_cols_is_rejected_grid_unchanged() {
        let mut screen = ClientScreen::new(80, 24);
        // Apply a legitimate diff first to set confirmed state.
        screen.apply(&make_diff(1, "hello"));
        let epoch_before = screen.last_applied_epoch();
        let size_before = screen.size();

        // An oversized diff (cols exceeds MAX_TERMINAL_COLS).
        let oversized = StateDiff {
            epoch: 2,
            cols: MAX_TERMINAL_COLS + 1,
            rows: 24,
            cursor: CursorPos { row: 0, col: 0 },
            runs: vec![],
        };
        // Must not panic; must be silently discarded.
        screen.apply(&oversized);

        // Grid and epoch must be unchanged (diff was discarded).
        assert_eq!(screen.last_applied_epoch(), epoch_before, "epoch must not advance");
        assert_eq!(screen.size(), size_before, "dimensions must not change");
        assert_eq!(screen.confirmed_cell(0, 0).ch, 'h', "confirmed cell must be unchanged");
    }

    #[test]
    fn apply_oversized_rows_is_rejected_grid_unchanged() {
        let mut screen = ClientScreen::new(80, 24);
        screen.apply(&make_diff(1, "world"));
        let epoch_before = screen.last_applied_epoch();

        let oversized = StateDiff {
            epoch: 2,
            cols: 80,
            rows: MAX_TERMINAL_ROWS + 1,
            cursor: CursorPos { row: 0, col: 0 },
            runs: vec![],
        };
        screen.apply(&oversized);

        assert_eq!(screen.last_applied_epoch(), epoch_before, "epoch must not advance");
        assert_eq!(screen.size(), (80, 24), "dimensions must not change");
    }

    #[test]
    fn apply_zero_cols_is_rejected_no_panic() {
        let mut screen = ClientScreen::new(80, 24);
        let zero_cols = StateDiff {
            epoch: 1,
            cols: 0,
            rows: 24,
            cursor: CursorPos { row: 0, col: 0 },
            runs: vec![],
        };
        screen.apply(&zero_cols);
        assert_eq!(screen.last_applied_epoch(), 0, "epoch must not advance on zero cols");
    }

    #[test]
    fn apply_zero_rows_is_rejected_no_panic() {
        let mut screen = ClientScreen::new(80, 24);
        let zero_rows = StateDiff {
            epoch: 1,
            cols: 80,
            rows: 0,
            cursor: CursorPos { row: 0, col: 0 },
            runs: vec![],
        };
        screen.apply(&zero_rows);
        assert_eq!(screen.last_applied_epoch(), 0, "epoch must not advance on zero rows");
    }

    #[test]
    fn apply_max_allowed_dimensions_is_accepted() {
        // Exactly at the cap: must succeed without panic.
        let mut screen = ClientScreen::new(80, 24);
        let at_cap = StateDiff {
            epoch: 1,
            cols: MAX_TERMINAL_COLS,
            rows: MAX_TERMINAL_ROWS,
            cursor: CursorPos { row: 0, col: 0 },
            runs: vec![],
        };
        screen.apply(&at_cap);
        assert_eq!(screen.last_applied_epoch(), 1, "diff at cap must be accepted");
        assert_eq!(screen.size(), (MAX_TERMINAL_COLS, MAX_TERMINAL_ROWS));
    }

    #[test]
    fn connection_loss_overlay_inactive_returns_none() {
        // Inactive overlay must return None for all coordinates (no-op).
        let overlay = ConnectionLossOverlay::new(80);
        assert!(!overlay.active, "overlay must start inactive");
        assert!(overlay.cell_at(0, 0).is_none(), "inactive: row 0 col 0 must be None");
        assert!(overlay.cell_at(0, 79).is_none(), "inactive: row 0 col 79 must be None");
        assert!(overlay.cell_at(23, 79).is_none(), "inactive: row 23 col 79 must be None");
        assert!(overlay.cell_at(999, 999).is_none(), "inactive: OOB must be None");
    }

    #[test]
    fn connection_loss_overlay_active_renders_row0() {
        // Active overlay must render row 0 in REVERSE style; other rows return None.
        let mut overlay = ConnectionLossOverlay::new(80);
        overlay.active = true;
        // Row 0, col 0 must be Some with REVERSE style.
        let cell = overlay.cell_at(0, 0).expect("active overlay must return Some at row 0, col 0");
        assert_eq!(cell.style.0 & CellStyle::REVERSE, CellStyle::REVERSE, "banner must use REVERSE style");
        assert!(cell.fg.is_none(), "banner fg must be None (terminal default)");
        assert!(cell.bg.is_none(), "banner bg must be None (terminal default)");
        // Row 1 must return None (banner is row-0 only).
        assert!(overlay.cell_at(1, 0).is_none(), "active overlay must return None for rows != 0");
        assert!(overlay.cell_at(23, 79).is_none(), "active overlay must return None for row 23");
    }

    #[test]
    fn connection_loss_overlay_banner_contains_tilde_dot() {
        // The banner text MUST advertise ~. (D-16-03a).
        let mut overlay = ConnectionLossOverlay::new(80);
        overlay.active = true;
        // Collect row-0 characters.
        let banner: String = (0..80u16).filter_map(|c| overlay.cell_at(0, c).map(|cell| cell.ch)).collect();
        assert!(
            banner.contains("~."),
            "banner must advertise ~. disconnect hint (D-16-03a); banner: {:?}",
            banner
        );
    }

    // ── BUG-H: blank-cell emission + connect-clear (D-03a / D-03b) ──────────

    /// BUG-H regression: prior terminal content bleeds through after Ctrl-L / ED2 clear.
    ///
    /// Before the fix: a cell that is blank in `want` but non-blank in `have` (physical
    /// shows a visible char) would be skipped if the skip condition was too broad — leaving
    /// old content visible on the terminal instead of overwriting with a space.
    ///
    /// After the fix: `emit_diff` emits a space for any cell where `want != have`, including
    /// the case where `want.ch == ' '` (blank) and `have.ch != ' '` (physical shows a char).
    /// A second render with no change must be minimal (idempotent — no double-write).
    #[test]
    fn bug_h_blank_cell_after_clear_is_emitted() {
        // FAIL BEFORE FIX: if the skip condition were `want.ch == have.ch` (char-only),
        //                  a styled blank vs default-blank would be incorrectly skipped.
        //                  More critically: if the skip were overly broad, 'X' in physical
        //                  with ' ' in confirmed would not produce a space write.
        // PASS AFTER FIX:  `want != have` → space IS emitted; physical is updated to blank,
        //                  so the next render is a no-op for that cell (idempotent).
        let mut screen = ClientScreen::new(80, 24);

        // Step 1: Write 'X' to confirmed and render — physical now has 'X' at (0,0).
        screen.apply(&make_diff(1, "X"));
        let mut buf1 = Vec::<u8>::new();
        screen.render_to_stdout(&mut buf1).unwrap();
        assert!(
            String::from_utf8_lossy(&buf1).contains('X'),
            "first render must contain 'X'"
        );

        // Step 2: Server sends an ED-style diff that blanks (0,0) back to space.
        // physical[0][0] is still Cell { ch: 'X', ... }.
        // confirmed[0][0] becomes Cell::default() = { ch: ' ', style: NONE, fg: None, bg: None }.
        let blank_diff = StateDiff {
            epoch: 2,
            cols: 80,
            rows: 24,
            cursor: CursorPos { row: 0, col: 0 },
            runs: vec![DiffRun {
                row: 0,
                start_col: 0,
                chars: " ".to_string(),
                style: CellStyle(CellStyle::NONE),
                fg: None,
                bg: None,
            }],
        };
        screen.apply(&blank_diff);

        // Step 3: Render — want.ch==' ', have.ch=='X' → want != have → MUST emit a space
        // (MoveTo + SGR + ' ') so the terminal's 'X' is overwritten.
        let mut buf2 = Vec::<u8>::new();
        screen.render_to_stdout(&mut buf2).unwrap();

        // The output MUST contain more than just the final cursor MoveTo.
        // A cell write at (0,0) produces: MoveTo(0,0) + SGR(\x1b[0m) + ' '.
        // We assert the SGR reset sequence is present — it only appears when a cell is
        // written, never as part of the bare cursor-position final move.
        let output = String::from_utf8_lossy(&buf2);
        assert!(
            output.contains("\x1b[0m"),
            "BUG-H: blanking a previously non-blank cell (have.ch='X', want.ch=' ') \
             must emit a space write (including SGR reset), not just skip the cell; \
             output was {:?}",
            output
        );

        // Step 4: Idempotency — a second render with no confirmed change must NOT
        // re-emit the space (physical was updated to blank, so want == have now).
        let mut buf3 = Vec::<u8>::new();
        screen.render_to_stdout(&mut buf3).unwrap();
        assert!(
            !output.contains("\x1b[0m") || buf3.len() < buf2.len(),
            "BUG-H idempotency: second render after blank must not re-emit the space; \
             buf2.len()={}, buf3.len()={}",
            buf2.len(),
            buf3.len()
        );
    }

    /// BUG-H regression: on connect, prior terminal content from a previous process
    /// bleeds through because nosh never clears the physical terminal before the first
    /// render. The `emit_connect_clear` method is the single sanctioned exception to the
    /// render-only invariant — it fires once at connect.
    ///
    /// Before the fix: no clear is issued on connect; old terminal content remains visible
    /// under nosh's rendering because `emit_diff` skips blank-vs-blank cells (physical model
    /// says blank, terminal also appears blank to nosh, but actually shows old content).
    ///
    /// After the fix: `emit_connect_clear` writes `\x1b[2J\x1b[H` (ED2 + cursor-home) then
    /// calls `reset_physical()` so the subsequent `render_to_stdout` forces a full repaint
    /// from a known-clean terminal state.
    #[test]
    fn bug_h_connect_clear_emits_ed2_home_and_resets_physical() {
        // FAIL BEFORE FIX: emit_connect_clear does not exist — compile error.
        // PASS AFTER FIX:  method exists, writes \x1b[2J\x1b[H, and resets physical so
        //                  the next render is a full repaint (mirrors reset_physical_forces_full_repaint).
        let mut screen = ClientScreen::new(80, 24);

        // Dirty the physical grid by rendering some content.
        screen.apply(&make_diff(1, "hello"));
        let mut buf_dirty = Vec::<u8>::new();
        screen.render_to_stdout(&mut buf_dirty).unwrap();
        assert!(
            String::from_utf8_lossy(&buf_dirty).contains('h'),
            "setup: first render must contain 'h'"
        );

        // Verify a second render is minimal (physical is in sync with confirmed).
        let mut buf_minimal = Vec::<u8>::new();
        screen.render_to_stdout(&mut buf_minimal).unwrap();
        assert!(
            buf_minimal.len() < buf_dirty.len(),
            "setup: second render must be minimal; \
             buf_dirty.len()={}, buf_minimal.len()={}",
            buf_dirty.len(),
            buf_minimal.len()
        );

        // Call emit_connect_clear — the ONE sanctioned pre-render clear (D-03 / CONTEXT.md).
        let mut clear_buf = Vec::<u8>::new();
        screen.emit_connect_clear(&mut clear_buf).unwrap();

        // Assert 1: the clear buffer contains both \x1b[2J (ED2) and \x1b[H (cursor-home).
        let clear_str = String::from_utf8_lossy(&clear_buf);
        assert!(
            clear_str.contains("\x1b[2J"),
            "BUG-H: emit_connect_clear must write \\x1b[2J (ED2); clear_buf was {:?}",
            clear_str
        );
        assert!(
            clear_str.contains("\x1b[H"),
            "BUG-H: emit_connect_clear must write \\x1b[H (cursor-home); clear_buf was {:?}",
            clear_str
        );

        // Assert 2: physical was reset — the next render is a full repaint (confirmed
        // still has 'hello', physical is now blank defaults from reset_physical()).
        let mut buf_after_clear = Vec::<u8>::new();
        screen.render_to_stdout(&mut buf_after_clear).unwrap();
        assert!(
            buf_after_clear.len() >= buf_dirty.len(),
            "BUG-H: after emit_connect_clear, render must be a full repaint (reset_physical \
             was called); buf_dirty.len()={}, buf_after_clear.len()={}",
            buf_dirty.len(),
            buf_after_clear.len()
        );
        assert!(
            String::from_utf8_lossy(&buf_after_clear).contains('h'),
            "BUG-H: full repaint after emit_connect_clear must include confirmed content 'h'"
        );
    }

    // ── Task 2: render / idempotency / SGR / reset_physical tests ────────────

    #[test]
    fn render_after_apply_emits_nonempty_ansi_with_chars() {
        let mut screen = ClientScreen::new(80, 24);
        let diff = make_diff(1, "hello");
        screen.apply(&diff);

        let mut buf = Vec::<u8>::new();
        screen.render_to_stdout(&mut buf).unwrap();

        let output = String::from_utf8_lossy(&buf);
        assert!(!buf.is_empty(), "first render must emit ANSI");
        // Must contain the literal chars.
        assert!(output.contains('h'), "output must contain 'h'");
        assert!(output.contains('e'), "output must contain 'e'");
        assert!(output.contains('l'), "output must contain 'l'");
        assert!(output.contains('o'), "output must contain 'o'");
        // Must contain at least one MoveTo (CSI sequence starting with \x1b[).
        assert!(buf.contains(&0x1b), "output must contain ESC");
    }

    #[test]
    fn duplicate_datagram_produces_minimal_ansi() {
        let mut screen = ClientScreen::new(80, 24);
        let diff = make_diff(1, "hello");

        // First apply + render.
        screen.apply(&diff);
        let mut buf1 = Vec::<u8>::new();
        screen.render_to_stdout(&mut buf1).unwrap();
        assert!(!buf1.is_empty(), "first render must emit ANSI");

        // Second apply with same epoch → stale, discarded.
        screen.apply(&diff); // epoch 1 <= 1 → no-op
        let mut buf2 = Vec::<u8>::new();
        screen.render_to_stdout(&mut buf2).unwrap();

        // Second render must be strictly shorter (only final cursor MoveTo).
        assert!(
            buf2.len() < buf1.len(),
            "duplicate datagram must produce minimal ANSI (only cursor position, no cell writes); \
             buf1.len()={}, buf2.len()={}",
            buf1.len(),
            buf2.len()
        );
    }

    #[test]
    fn emit_sgr_bold_fg_produces_correct_sequence() {
        let mut buf = Vec::<u8>::new();
        emit_sgr(
            &mut buf,
            CellStyle(CellStyle::BOLD),
            Some(1), // red
            None,
        )
        .unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert_eq!(s, "\x1b[0;1;38;5;1m");
    }

    #[test]
    fn emit_sgr_all_attributes() {
        let mut buf = Vec::<u8>::new();
        emit_sgr(
            &mut buf,
            CellStyle(CellStyle::BOLD | CellStyle::ITALIC | CellStyle::UNDERLINE | CellStyle::REVERSE),
            Some(5),
            Some(10),
        )
        .unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert_eq!(s, "\x1b[0;1;3;4;7;38;5;5;48;5;10m");
    }

    #[test]
    fn emit_sgr_none_attrs_produces_reset_only() {
        let mut buf = Vec::<u8>::new();
        emit_sgr(&mut buf, CellStyle(CellStyle::NONE), None, None).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert_eq!(s, "\x1b[0m");
    }

    #[test]
    fn reset_physical_forces_full_repaint() {
        let mut screen = ClientScreen::new(80, 24);
        let diff = make_diff(1, "hello");
        screen.apply(&diff);

        // First render: updates physical.
        let mut buf1 = Vec::<u8>::new();
        screen.render_to_stdout(&mut buf1).unwrap();

        // Second render with no changes: should be minimal (just cursor MoveTo).
        let mut buf2 = Vec::<u8>::new();
        screen.render_to_stdout(&mut buf2).unwrap();
        assert!(buf2.len() < buf1.len(), "second render should be minimal");

        // reset_physical: next render must be a full repaint (same as first render).
        screen.reset_physical();
        let mut buf3 = Vec::<u8>::new();
        screen.render_to_stdout(&mut buf3).unwrap();
        // After reset, render should again emit cell content.
        assert!(
            buf3.len() >= buf1.len(),
            "after reset_physical, render must be a full repaint; \
             buf1.len()={}, buf3.len()={}",
            buf1.len(),
            buf3.len()
        );
    }

    // ── Task 1 (Phase 15): cursor override + render_with_predictor tests ─────

    /// (a) render_to_stdout_with_cursor with Some(override) emits MoveTo at the
    /// override position, not at confirmed_cursor.
    #[test]
    fn render_to_stdout_with_cursor_override_positions_at_override() {
        let mut screen = ClientScreen::new(80, 24);
        // Apply a diff so confirmed_cursor is at (0, 0) (the diff's cursor field).
        screen.apply(&make_diff(1, "hello"));
        // The diff's cursor is at row=0, col=0.

        let override_pos = CursorPos { row: 3, col: 7 };
        let mut buf = Vec::<u8>::new();
        screen
            .render_to_stdout_with_cursor(&mut buf, Some(override_pos))
            .unwrap();

        // The output must contain a MoveTo for the override position.
        // crossterm MoveTo(col, row) emits ESC [ <row+1> ; <col+1> H
        // (1-based in CSI sequences). Encode the expected sequence.
        let expected_move = format!("\x1b[{};{}H", override_pos.row + 1, override_pos.col + 1);
        let output = String::from_utf8_lossy(&buf);
        assert!(
            output.contains(&expected_move),
            "render_to_stdout_with_cursor must emit MoveTo at override position ({}, {}); \
             expected sequence {:?} not found in output {:?}",
            override_pos.row,
            override_pos.col,
            expected_move,
            output
        );

        // Must NOT contain a MoveTo at the confirmed cursor (row=0, col=0 is the
        // origin, and MoveTo(0,0) = ESC [ 1 ; 1 H — only check it's not the FINAL
        // move by verifying the override appears last among cursor moves).
        // Simpler: assert the override MoveTo is present (sufficient for acceptance).
    }

    /// (b) render_to_stdout() wrapper still positions at confirmed_cursor (regression).
    #[test]
    fn render_to_stdout_wrapper_uses_confirmed_cursor() {
        let mut screen = ClientScreen::new(80, 24);
        // Construct a diff with a non-trivial confirmed_cursor.
        let diff = StateDiff {
            epoch: 1,
            cols: 80,
            rows: 24,
            cursor: CursorPos { row: 5, col: 12 },
            runs: vec![DiffRun {
                row: 0,
                start_col: 0,
                style: CellStyle(CellStyle::NONE),
                fg: None,
                bg: None,
                chars: "ab".to_string(),
            }],
        };
        screen.apply(&diff);

        let mut buf = Vec::<u8>::new();
        screen.render_to_stdout(&mut buf).unwrap();

        // Must contain a MoveTo at confirmed_cursor (row=5, col=12).
        // CSI sequence is 1-based: ESC [ 6 ; 13 H
        let expected = format!("\x1b[{};{}H", 5 + 1, 12 + 1);
        let output = String::from_utf8_lossy(&buf);
        assert!(
            output.contains(&expected),
            "render_to_stdout wrapper must emit MoveTo at confirmed_cursor (5, 12); \
             expected {:?} in output {:?}",
            expected,
            output
        );
    }

    /// (c) render_with_predictor with an Always-mode predictor overlays the
    /// predicted cell into the emitted diff.
    #[test]
    fn render_with_predictor_overlays_predicted_cell() {
        let mut screen = ClientScreen::new(80, 24);
        // Start with a blank screen (no confirmed content at col 0, row 0).
        let mut predictor = PredictionOverlay::new(PredictDisplayMode::Always, 80, 24);
        let loss = ConnectionLossOverlay::new(80); // inactive

        // Simulate a keystroke 'x' — should produce a PredictChar at (0,0).
        // on_input on an empty screen (last_applied_epoch = 0).
        predictor.on_input(b"x", &screen);

        // CR-03 fix: awaiting_first_cull is true after the first on_input of a fresh
        // epoch. Simulate the datagram arm: cull() with epoch 0 (below epoch_required=1)
        // clears awaiting_first_cull without removing the prediction (epoch_required=1 > 0).
        // The prediction remains non-tentative (tentative_until_epoch=0 == confirmed_epoch=0).
        predictor.cull(&screen, 0, 5); // clears awaiting_first_cull

        let mut buf = Vec::<u8>::new();
        screen.render_with_predictor(&mut buf, &predictor, &loss).unwrap();

        // The output must contain 'x' (the predicted character at col 0, row 0).
        let output = String::from_utf8_lossy(&buf);
        assert!(
            output.contains('x'),
            "render_with_predictor must include the predicted character 'x' in the diff; \
             output: {:?}",
            output
        );
    }
}
