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

/// Maximum bytes retained for an OSC 52 clipboard-write payload (D-16-01c / CR-03).
///
/// Applied in `osc_dispatch` BEFORE storing into `osc52_pending`. Any data field
/// exceeding this cap is silently truncated. This re-mitigates the CR-03 DoS risk
/// (previously handled by `default-features = false` which limited all OSC to 1024
/// bytes via ArrayVec). Now that vte `std` is re-enabled, the transient vte buffer
/// can grow large; this cap bounds the STORED value.
pub const OSC_52_MAX_BYTES: usize = 65_536;

/// Maximum bytes for an OSC 0/2 window title (D-16-01c / CR-03).
///
/// Applied in `osc_dispatch` BEFORE storing into `title`. Titles exceeding this
/// cap are silently discarded. Bounding titles protects the server from DoS via
/// an application emitting an unbounded OSC 2 title sequence.
pub const MAX_TITLE_BYTES: usize = 1_024;

/// Maximum bytes accumulated for a single OSC sequence before the pre-filter
/// truncates it and resyncs the parser to ground state (D-19-01 / SEC-03).
///
/// This bounds what vte ALLOCATES while parsing, not what is stored.
/// It is distinct from and larger than `OSC_52_MAX_BYTES` (64 KiB) and
/// `MAX_TITLE_BYTES` (1 KiB) — those storage caps run in `osc_dispatch`
/// AFTER vte has already buffered the full sequence; this cap intercepts
/// BEFORE `parser.advance()` so vte never allocates past 1 MiB per OSC.
///
/// On overflow the pre-filter: feeds only the safe prefix to the parser,
/// replaces the mid-OSC parser with `vte::Parser::default()` (ground state),
/// and resets `osc_byte_count` to 0 so the next OSC starts fresh.
pub const OSC_ACCUMULATION_MAX: usize = 1_048_576; // 1 MiB

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
    /// Wide-character continuation marker (D-19-06).
    ///
    /// `false` for normal cells and for the primary cell of a width-2 glyph.
    /// `true` for the phantom spacer at `col+1` that belongs to the width-2 glyph
    /// at `col`. Both the diff encoder and the client renderer skip `wide:true` cells
    /// so the glyph is never double-written. The `ch` value of a continuation cell is
    /// always `' '` and carries no semantic meaning.
    pub wide: bool,
}

impl Default for Cell {
    fn default() -> Self {
        Cell {
            ch: ' ',
            style: CellStyle(CellStyle::NONE),
            fg: None,
            bg: None,
            wide: false,
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
    /// Saved primary screen state when the alternate screen is active.
    ///
    /// `None` when the primary screen is the active screen.
    /// `Some((grid, cursor, sgr))` when `?1049h` has been received — holds the
    /// full primary grid, cursor position, and SGR pen so that `?1049l` can
    /// restore them exactly (TUI-01, D-19-01..D-19-06, Pitfalls A-1/A-6).
    saved_primary: Option<(Vec<Vec<Cell>>, CursorPos, SgrState)>,
    /// Running count of bytes accumulated into the current in-flight OSC sequence
    /// across `advance()` calls (D-19-01 / SEC-03).
    ///
    /// Reset to 0 at OSC start (ESC ] / 0x9D) and at OSC end (BEL / ST) during
    /// normal scanning, and on overflow truncation. Also reset by RIS (ESC c).
    /// Used by the pre-filter in `advance()` to enforce `OSC_ACCUMULATION_MAX`.
    osc_byte_count: usize,
    /// Shadow flag tracking whether the parser is currently inside an OSC string.
    ///
    /// vte's internal `State` is private; this mirrors it for the pre-filter.
    /// Set true on ESC ] or 0x9D; set false on BEL (0x07) or ST (ESC \ = 0x1B 0x5C).
    in_osc: bool,
    /// Pending lone-ESC flag for split-ST detection (CR-01 / D-19-01 fix).
    ///
    /// Set true when `osc_prefilter` sees a lone `0x1B` at the very end of a
    /// slice while `in_osc` is true (the ESC may be the first byte of an
    /// `ESC \` String Terminator split across two `advance()` calls).
    ///
    /// On the next `osc_prefilter` call this flag is checked before the main
    /// scan: if the first byte is `0x5C` (`\`), the ST is completed and `in_osc`
    /// is cleared; if not, the pending ESC is counted as a regular payload byte.
    ///
    /// Reset to `false` on every path that also resets `in_osc`.
    pending_esc: bool,
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
            saved_primary: None,
            osc_byte_count: 0,
            in_osc: false,
            pending_esc: false,
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
    ///
    /// # OSC accumulation pre-bound (D-19-01 / SEC-03)
    ///
    /// Before feeding bytes to vte, a byte scanner tracks the running count of
    /// bytes accumulated in the current in-flight OSC sequence (`self.osc_byte_count`
    /// / `self.in_osc`). If the count would exceed `OSC_ACCUMULATION_MAX` (1 MiB),
    /// only the safe prefix (up to the offending byte) is fed to the parser;
    /// afterwards the parser is replaced with `vte::Parser::default()` (ground state)
    /// and the counter is reset. This ensures vte's internal `osc_raw` buffer never
    /// grows beyond 1 MiB per OSC sequence across multiple `advance()` calls.
    ///
    /// The truncation and resync happen AFTER feeding the safe prefix so that any
    /// OSC bytes below the cap are still dispatched normally (Pitfall 2).
    pub fn advance(&mut self, bytes: &[u8]) {
        // OSC accumulation pre-bound (D-19-01 / SEC-03 / T-19-06).
        //
        // Scan `bytes` byte-by-byte, tracking whether the parser is currently inside
        // an OSC string (mirroring vte's private State::OscString). We accumulate
        // `self.osc_byte_count` while inside an OSC; at `OSC_ACCUMULATION_MAX` we
        // truncate and resync the parser.
        //
        // OSC framing (mirroring vte's osc_start / osc_end transitions):
        //   Start:  single byte 0x9D  OR  two-byte ESC ] (0x1B 0x5D)
        //   End:    BEL (0x07)  OR  ST = ESC \ (0x1B 0x5C)
        //
        // The scanner does not need to track whether the preceding byte was ESC
        // for ST detection: when `in_osc` is true and we see 0x1B, the next byte
        // determines whether it is ST (0x5C → close) or starts a new control
        // sequence (anything else; vte transitions to Escape state and will end
        // the OSC implicitly). We conservatively treat ESC-as-ST-candidate as the
        // end of the OSC (resetting in_osc) if the next byte is 0x5C.
        //
        // Performance: O(n) per call where n = bytes.len() ≤ OS pipe-buffer size
        // (typically 4–8 KiB). The overhead is negligible relative to vte itself.

        let bytes_to_feed = self.osc_prefilter(bytes);

        let mut parser = std::mem::take(&mut self.parser);
        parser.advance(self, bytes_to_feed);

        // After feeding the (possibly truncated) bytes, check if we overflowed.
        // `osc_prefilter` sets `self.in_osc = false` and returns a truncated slice
        // AND signals overflow by returning a slice shorter than `bytes`. On overflow,
        // we must resync the parser to ground state (Pitfall 2: reset AFTER advance).
        if bytes_to_feed.len() < bytes.len() {
            // Overflow: the pre-filter truncated `bytes`. Replace the mid-OSC parser
            // with a fresh ground-state parser so vte does not stay in OscString.
            self.parser = vte::Parser::default();
            // osc_byte_count and in_osc are already reset by osc_prefilter on overflow.
        } else {
            self.parser = parser;
        }
    }

    /// OSC accumulation pre-filter: scan `bytes` and return the safe prefix to
    /// feed to vte. Updates `self.in_osc` and `self.osc_byte_count` across calls.
    ///
    /// Returns `bytes` unchanged when no overflow occurs. Returns a strict prefix
    /// (possibly empty) when feeding any more bytes would exceed `OSC_ACCUMULATION_MAX`.
    /// On overflow, `self.osc_byte_count` and `self.in_osc` are reset to 0/false so
    /// the next `advance()` call starts from a clean state.
    fn osc_prefilter<'a>(&mut self, bytes: &'a [u8]) -> &'a [u8] {
        // Use a local state copy to scan; commit back after each iteration.
        let mut in_osc = self.in_osc;
        let mut osc_byte_count = self.osc_byte_count;

        let mut i = 0;

        // CR-01 fix: if the previous call ended with a lone ESC while in_osc (a
        // potential ST split across calls), check whether this call begins with
        // 0x5C (\) to complete the ST terminator.
        if self.pending_esc && in_osc {
            if !bytes.is_empty() && bytes[0] == 0x5C {
                // The ESC from the previous call + this 0x5C = confirmed ST terminator.
                in_osc = false;
                osc_byte_count = 0;
                i = 1; // consume the 0x5C byte
            } else {
                // Not an ST: the lone ESC was a real payload byte — count it now.
                // (We deferred the count in the previous call to handle this case.)
                osc_byte_count += 1;
                if osc_byte_count > OSC_ACCUMULATION_MAX {
                    // Overflow from the deferred ESC count: truncate at byte 0.
                    self.in_osc = false;
                    self.osc_byte_count = 0;
                    self.pending_esc = false;
                    return &bytes[..0];
                }
                // The first byte of this call is NOT 0x5C, so it will be handled
                // normally in the main loop below (i remains 0).
            }
        }
        // Clear the pending_esc flag — we've handled it above.
        self.pending_esc = false;

        while i < bytes.len() {
            let b = bytes[i];

            if !in_osc {
                // Not in OSC: look for OSC start.
                if b == 0x9D {
                    // Single-byte OSC introducer (C1 control).
                    in_osc = true;
                    osc_byte_count = 0;
                    i += 1;
                    continue;
                }
                if b == 0x1B && i + 1 < bytes.len() && bytes[i + 1] == 0x5D {
                    // ESC ] (two-byte OSC introducer).
                    in_osc = true;
                    osc_byte_count = 0;
                    i += 2; // skip both ESC and ]
                    continue;
                }
                // Non-OSC byte: pass through.
                i += 1;
                continue;
            }

            // Inside OSC: accumulate bytes and look for terminator.
            if b == 0x07 {
                // BEL: OSC end.
                in_osc = false;
                osc_byte_count = 0;
                i += 1;
                continue;
            }
            if b == 0x1B {
                // Potential ST: ESC followed by \ (0x5C).
                if i + 1 < bytes.len() {
                    if bytes[i + 1] == 0x5C {
                        // Confirmed ST terminator.
                        in_osc = false;
                        osc_byte_count = 0;
                        i += 2; // skip ESC and \
                        continue;
                    }
                    // ESC followed by something else: vte will leave OscString on the
                    // next advance anyway; treat as end-of-OSC conservatively.
                    in_osc = false;
                    osc_byte_count = 0;
                    i += 1; // leave the next byte for the main vte advance
                    continue;
                }
                // CR-01 fix: ESC at the very end of the slice — we can't tell if ST
                // follows in the next call.  Do NOT count it as a payload byte yet;
                // set pending_esc so the next osc_prefilter call can resolve it.
                // Commit state and return the full slice (no overflow, no truncation).
                self.in_osc = in_osc;
                self.osc_byte_count = osc_byte_count;
                self.pending_esc = true;
                i += 1; // advance past the ESC so it is included in bytes_to_feed
                // Return the full slice including the ESC — vte will handle it.
                return bytes;
            }

            // Regular OSC payload byte: count against the cap.
            osc_byte_count += 1;
            if osc_byte_count > OSC_ACCUMULATION_MAX {
                // Overflow: we have fed too many bytes into the OSC. Truncate here —
                // return only [0..i] (the slice up to but NOT including this byte).
                // The caller will replace the parser with vte::Parser::default() after
                // feeding this safe prefix (Pitfall 2: reset after, not before, advance).
                self.in_osc = false;
                self.osc_byte_count = 0;
                self.pending_esc = false;
                return &bytes[..i];
            }
            i += 1;
        }

        // No overflow: commit updated state and return the full slice.
        self.in_osc = in_osc;
        self.osc_byte_count = osc_byte_count;
        bytes
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

        // TUI-02: also resize the saved primary grid if the alternate screen is active.
        // Applies the same column truncate/resize and row remove-top-to-scrollback /
        // push-blank logic as the active grid above. Excess rows on shrink go to
        // `self.scrollback` (not lost, obeying SCROLLBACK_LINE_CAP). Cursor in
        // saved_primary is clamped to the new bounds.
        if let Some((ref mut prim_grid, ref mut prim_cursor, _)) = self.saved_primary {
            // Resize columns.
            for row in prim_grid.iter_mut() {
                let current_len = row.len();
                let new_len = cols as usize;
                if current_len > new_len {
                    row.truncate(new_len);
                } else if current_len < new_len {
                    row.resize(new_len, Cell::default());
                }
            }
            // Grow or shrink rows.
            let current_prim_rows = prim_grid.len();
            let new_rows_usize = rows as usize;
            if current_prim_rows > new_rows_usize {
                // Shrink: top rows go into primary scrollback (not lost, T-19-02).
                let excess = current_prim_rows - new_rows_usize;
                for _ in 0..excess {
                    let top = prim_grid.remove(0);
                    self.scrollback.push_back(top);
                    if self.scrollback.len() > SCROLLBACK_LINE_CAP {
                        self.scrollback.pop_front();
                    }
                }
            } else if current_prim_rows < new_rows_usize {
                // Grow: add blank rows at the bottom.
                for _ in current_prim_rows..new_rows_usize {
                    prim_grid.push(vec![Cell::default(); cols as usize]);
                }
            }
            // Clamp saved cursor to new bounds (T-19-02 saturating clamp).
            prim_cursor.row = prim_cursor.row.min(rows.saturating_sub(1));
            prim_cursor.col = prim_cursor.col.min(cols.saturating_sub(1));
        }
    }

    // ── Alt-screen two-grid model (TUI-01, D-19-01..D-19-06) ─────────────────

    /// Enter the alternate screen buffer (?1049h / DECSET 1049).
    ///
    /// Atomically saves the primary grid, cursor, and SGR pen into `saved_primary`,
    /// then replaces `self.grid` with a fresh blank grid and resets cursor/SGR.
    /// Uses `std::mem::replace` to avoid cloning the grid into a temporary — the
    /// primary grid is moved into `saved_primary.0` and the blank alt grid is
    /// installed in one operation (Pitfall A-1: all three enter operations land
    /// together — save, blank-swap, cursor/SGR reset).
    ///
    /// Nested enter (second `?1049h` while already in alt-screen): safe — the current
    /// (alt) grid is saved, overwriting any prior `saved_primary`. Memory is bounded
    /// to one saved grid. This is xterm-divergent but avoids panic (T-19-03).
    fn enter_alt_screen(&mut self) {
        // Atomically swap the primary grid out, install a blank alt grid.
        let alt_grid = Self::make_grid(self.cols, self.rows);
        let prim_grid = std::mem::replace(&mut self.grid, alt_grid);
        // Save primary state (grid already moved via replace).
        self.saved_primary = Some((prim_grid, self.cursor, self.sgr.clone()));
        // Reset cursor and SGR pen for the alt screen.
        self.cursor = CursorPos { row: 0, col: 0 };
        self.sgr.reset();
        self.echo_state.alt_screen = true;
    }

    /// Exit the alternate screen buffer (?1049l / DECRST 1049).
    ///
    /// Restores the primary grid, cursor, and SGR pen from `saved_primary` if
    /// present. If `saved_primary` is `None` (bare exit with no prior enter —
    /// T-19-01), leaves the grid unchanged and just clears the flag (graceful no-op).
    fn exit_alt_screen(&mut self) {
        if let Some((prim_grid, prim_cursor, prim_sgr)) = self.saved_primary.take() {
            self.grid = prim_grid;
            self.cursor = prim_cursor;
            self.sgr = prim_sgr;
        }
        // Always clear the flag, even on a bare exit (saved_primary was None).
        self.echo_state.alt_screen = false;
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
    ///
    /// **D-19-09 gate**: while the alternate screen is active, the removed top row
    /// is discarded (not pushed to scrollback). Alt-screen content must never
    /// contaminate primary scrollback history. The blank-row push at the bottom
    /// always runs regardless. Phase 22 scrollback sync depends on this gate.
    fn scroll_up(&mut self) {
        if self.rows == 0 {
            return;
        }
        let top_row = self.grid.remove(0);
        if !self.echo_state.alt_screen {
            // Primary screen: push to scrollback with cap enforcement.
            self.scrollback.push_back(top_row);
            if self.scrollback.len() > SCROLLBACK_LINE_CAP {
                self.scrollback.pop_front();
            }
        }
        // Alt screen: top_row is dropped here (no scrollback for alt grid).
        self.grid.push(vec![Cell::default(); self.cols as usize]);
    }

    /// Write a character at the current cursor position, advance the cursor right,
    /// wrapping and scrolling as needed.
    ///
    /// Width is determined via `UnicodeWidthChar::width()` (D-19-04: default policy,
    /// East Asian Ambiguous → 1, matching the client predictor):
    ///
    /// - `Some(0)` (combining/ZWJ/zero-width) → return immediately; no cell write, no advance.
    /// - `Some(2)` (CJK wide) → write primary cell with `wide: false` at `col`, write a
    ///   continuation cell `Cell { ch: ' ', wide: true, .. }` at `col+1` if in bounds
    ///   (T-19-04: suppressed at right edge to avoid OOB), advance cursor by 2.
    /// - `Some(1)` or `None` (narrow / control-ish printable) → unchanged single-column
    ///   behaviour; `wide: false`.
    fn print_char(&mut self, c: char) {
        use unicode_width::UnicodeWidthChar;

        // Determine column width of this codepoint (D-19-04: use default width()).
        let col_width: u16 = match UnicodeWidthChar::width(c) {
            Some(0) => {
                // Zero-width combining mark or ZWJ: do not advance cursor, do not write.
                // D-19-05: cursor position unchanged.
                return;
            }
            Some(2) => 2,
            // Some(1) or None (control-ish printables that reach print_char): treat as 1.
            _ => 1,
        };

        // Clamp cursor to grid bounds (adversarial-safety).
        let row = (self.cursor.row as usize).min(self.rows.saturating_sub(1) as usize);
        let col = (self.cursor.col as usize).min(self.cols.saturating_sub(1) as usize);

        if !self.grid.is_empty() && row < self.grid.len() && col < self.grid[row].len() {
            // Write primary cell with wide: false.
            self.grid[row][col] = Cell {
                ch: c,
                style: self.sgr.style,
                fg: self.sgr.fg,
                bg: self.sgr.bg,
                wide: false,
            };

            // For width-2 glyphs, write a continuation marker at col+1 if in bounds.
            // T-19-04: right-edge guard — col+1 may be out of bounds; suppress silently.
            if col_width == 2 {
                let cont_col = col + 1;
                if cont_col < self.grid[row].len() {
                    self.grid[row][cont_col] = Cell {
                        ch: ' ',
                        style: self.sgr.style,
                        fg: self.sgr.fg,
                        bg: self.sgr.bg,
                        wide: true,
                    };
                }
            }
        }

        // Advance cursor by col_width (saturating to avoid u16 overflow).
        self.cursor.col = self.cursor.col.saturating_add(col_width);
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
    /// Returns a reference to the cell at `(row, col)` if the coordinates are
    /// in-bounds, or a **shared `&'static Cell`** (a global default cell, value
    /// `Cell::default()`) if they are out of bounds.
    ///
    /// **Phase 13 callers: do NOT store the returned reference across grid mutations
    /// (resize/advance/scroll) or compare pointer identity.** The out-of-bounds path
    /// returns a `'static` reference to a constant global sentinel, NOT a reference
    /// into the grid. If `cell()` returns the static default, it will still read as
    /// `' '` / no attributes / no color on subsequent dereferences — but it does NOT
    /// reflect any future in-bounds write to that coordinate. Copy the fields you
    /// need (`cell.ch`, `cell.fg`, etc.) rather than holding the reference.
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

    /// Drain the pending OSC 52 clipboard-write payload, returning it (if any) and
    /// clearing the field (Option::take semantics — prevents double-forwarding).
    ///
    /// Used by Phase 16 forwarding: after `push_output_and_parse`, the session loop
    /// calls this to collect any pending OSC 52 write for forwarding to the client
    /// over the reliable stream as `Message::TerminalControl(Clipboard{..})`.
    ///
    /// Returns `None` if no OSC 52 write was pending, or if the pending value was
    /// already drained by a prior call.
    pub fn take_osc52(&mut self) -> Option<(Vec<u8>, Vec<u8>)> {
        self.osc52_pending.take()
    }

    /// Drain the pending window title, returning it (if any) and clearing the field
    /// (Option::take semantics — prevents double-forwarding).
    ///
    /// Used by Phase 16 forwarding: after `push_output_and_parse`, the session loop
    /// calls this to collect any pending OSC 0/2 title for forwarding to the client
    /// over the reliable stream as `Message::TerminalControl(Title{..})`.
    ///
    /// Returns `None` if no title was pending, or if the pending value was already
    /// drained by a prior call.
    pub fn take_title(&mut self) -> Option<String> {
        self.title.take()
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
                        1049 => {
                            if enable {
                                self.enter_alt_screen();
                            } else {
                                self.exit_alt_screen();
                            }
                        }
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
                // SECURITY: use saturating_add to prevent u16 overflow before the
                // .min() clamp. `n` is untrusted (vte caps CSI params at u16::MAX =
                // 65535); if the cursor is at a nonzero row, plain `+` overflows →
                // debug panic (DoS) or silent wraparound (release, wrong position).
                // saturating_add(n) always returns a value ≥ cursor.row, so the
                // subsequent .min() clamp produces the correct in-bounds result.
                self.cursor.row = self.cursor.row
                    .saturating_add(n)
                    .min(self.rows.saturating_sub(1));
            }
            // Cursor right — count defaults to 1
            'C' => {
                let n = Self::cursor_count(params);
                // SECURITY: same saturating_add defence as CSI B above.
                self.cursor.col = self.cursor.col
                    .saturating_add(n)
                    .min(self.cols.saturating_sub(1));
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
    /// - `0` / `2` — set terminal title (icon + window / window only), capped at
    ///   [`MAX_TITLE_BYTES`] (1024 bytes). Titles exceeding the cap are silently discarded.
    /// - `52` — clipboard write (Phase 16 forwarding). The read/query form (`?`) is
    ///   **silently dropped** here (D-16-01a / T-16-01 security gate). Write payloads are
    ///   truncated to [`OSC_52_MAX_BYTES`] (65536) before storing (D-16-01c / CR-03).
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
                // Set terminal title (OSC 0: icon + window; OSC 2: window only).
                // D-16-01c / CR-03: only store titles within MAX_TITLE_BYTES to bound memory.
                if let Some(title_bytes) = params.get(1) {
                    if title_bytes.len() <= MAX_TITLE_BYTES {
                        if let Ok(title) = std::str::from_utf8(title_bytes) {
                            self.title = Some(title.to_owned());
                        }
                    }
                    // Titles exceeding MAX_TITLE_BYTES are silently discarded (anti-DoS).
                }
            }
            b"52" => {
                // OSC 52 clipboard passthrough (Phase 16, D-16-01).
                // Scope fence: only parse into osc52_pending; no clipboard read/write/exec.
                let data = params.get(2).copied().unwrap_or(b"");

                // D-16-01a / T-16-01 SECURITY GATE: silently drop the OSC 52 read/query form.
                // The read form `OSC 52;c;?` must NEVER be stored or forwarded to the client —
                // doing so would leak clipboard contents from the server. Drop unconditionally.
                if data == b"?" {
                    return;
                }

                let selection = params.get(1).copied().unwrap_or(b"c");
                // WR-01: Reject selection containing ESC (\x1b) or BEL (\x07) bytes.
                // A malicious server process could embed a premature BEL terminator in
                // `selection` to inject arbitrary OSC sequences into the client's local
                // terminal. Drop the entire OSC 52 frame if selection is malformed.
                if selection.iter().any(|&b| b == b'\x1b' || b == b'\x07') {
                    return; // drop malformed OSC 52 — injection attempt
                }
                // D-16-01c / CR-03: truncate data to OSC_52_MAX_BYTES before storing.
                // This re-mitigates the CR-03 DoS risk now that vte std is re-enabled.
                let capped_data = &data[..data.len().min(OSC_52_MAX_BYTES)];
                self.osc52_pending = Some((selection.to_vec(), capped_data.to_vec()));
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
                // Clear saved_primary so a subsequent ?1049l cannot restore
                // stale pre-reset content (Pitfall 5 / RIS invariant, D-19-01).
                self.saved_primary = None;
                // Reset OSC accumulation pre-filter state (D-19-01 / SEC-03).
                self.osc_byte_count = 0;
                self.in_osc = false;
                self.pending_esc = false;
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
                    // Grab the next param — should be `5` (256-color) or `2` (24-bit)
                    if let Some(next) = iter.next() {
                        if next[0] == 5 {
                            // 256-color: grab the color index.
                            // WR-02: validate range before cast. color_param[0] is u16;
                            // `as u8` would silently truncate values > 255 (e.g.
                            // CSI 38;5;300m → 300 mod 256 = 44, wrong palette index).
                            // Only accept valid 256-color indices (0..=255).
                            if let Some(color_param) = iter.next() {
                                if color_param[0] <= 255 {
                                    self.sgr.fg = Some(color_param[0] as u8);
                                }
                                // Out-of-range index: ignore the SGR (no color set).
                                // This matches the "ignore invalid parameter" behavior
                                // recommended by the VT specification.
                            }
                        } else if next[0] == 2 {
                            // 24-bit / truecolor (r;g;b): scope-fenced — not applied,
                            // but MUST drain the three r/g/b params so they are not
                            // misinterpreted as independent SGR codes (e.g., r=0 would
                            // trigger SGR 0 / reset if not consumed).
                            let _ = iter.next(); // r
                            let _ = iter.next(); // g
                            let _ = iter.next(); // b
                        }
                        // Other subtypes are scope-fenced; no drain needed.
                    }
                }
                // 256-color / 24-bit background: `48 ; 5 ; n` (Pitfall 6)
                48 => {
                    if let Some(next) = iter.next() {
                        if next[0] == 5 {
                            // WR-02: same range validation as fg (48;5;n).
                            if let Some(color_param) = iter.next() {
                                if color_param[0] <= 255 {
                                    self.sgr.bg = Some(color_param[0] as u8);
                                }
                                // Out-of-range: ignore (no color set).
                            }
                        } else if next[0] == 2 {
                            // 24-bit background: drain r;g;b to prevent misinterpretation.
                            let _ = iter.next(); // r
                            let _ = iter.next(); // g
                            let _ = iter.next(); // b
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

    // ── CR-01 regression: CSI B/C overflow from nonzero cursor ──────────────

    /// CR-01 regression: `CSI 65535 B` from a nonzero row must clamp, not panic.
    ///
    /// Before the fix, `self.cursor.row + n` with `cursor.row = 23` and `n = 65535`
    /// overflowed u16 → debug panic ("attempt to add with overflow") — a DoS on
    /// adversarial PTY output. In release it wrapped silently to a wrong position.
    /// After the fix, `saturating_add` always yields a value ≥ cursor.row, and the
    /// subsequent `.min(rows - 1)` clamps it to the last valid row.
    #[test]
    fn adversarial_csi_b_max_count_from_nonzero_row_clamps_no_panic() {
        let mut state = ts(80, 24);
        // Move to bottom-right so cursor.row is nonzero (row = 23, col = 79).
        state.advance(b"\x1b[24;80H"); // CSI 24;80H (1-based → row=23, col=79)
        assert_eq!(state.cursor(), CursorPos { row: 23, col: 79 });
        // CSI 65535 B: max-count cursor-down from nonzero row.
        // Before fix: 23 + 65535 = 65558 overflows u16 → panic in debug.
        // After fix: saturating_add → 65535, then .min(23) → 23 (clamped).
        state.advance(b"\x1b[65535B");
        assert_eq!(
            state.cursor().row, 23,
            "CSI 65535 B from row 23 must clamp to last row (23), not panic"
        );
        assert!(state.cursor().row < 24, "row must remain in bounds");
    }

    /// CR-01 regression: `CSI 65535 C` from a nonzero col must clamp, not panic.
    #[test]
    fn adversarial_csi_c_max_count_from_nonzero_col_clamps_no_panic() {
        let mut state = ts(80, 24);
        // Move to bottom-right so cursor.col is nonzero (row = 23, col = 79).
        state.advance(b"\x1b[24;80H"); // CSI 24;80H (1-based → row=23, col=79)
        assert_eq!(state.cursor(), CursorPos { row: 23, col: 79 });
        // CSI 65535 C: max-count cursor-right from nonzero col.
        // Before fix: 79 + 65535 = 65614 overflows u16 → panic in debug.
        // After fix: saturating_add → 65535, then .min(79) → 79 (clamped).
        state.advance(b"\x1b[65535C");
        assert_eq!(
            state.cursor().col, 79,
            "CSI 65535 C from col 79 must clamp to last col (79), not panic"
        );
        assert!(state.cursor().col < 80, "col must remain in bounds");
    }

    /// CR-01 regression: combined — move to nonzero position then apply huge B+C.
    ///
    /// This is the exact probe sequence from the adversarial verifier that exposed
    /// the original overflow bug: `CSI 24;80H` (move to bottom-right) followed by
    /// `CSI 65535B` and `CSI 65535C`. Both must clamp without panic.
    #[test]
    fn adversarial_huge_repeat_from_nonzero_position_clamps() {
        let mut state = ts(80, 24);
        // Move to a nonzero position first.
        state.advance(b"\x1b[24;80H");
        // Both operations must be panic-free and clamp correctly.
        state.advance(b"\x1b[65535B");
        state.advance(b"\x1b[65535C");
        assert!(state.cursor().row < 24, "row must be in bounds after huge CSI B");
        assert!(state.cursor().col < 80, "col must be in bounds after huge CSI C");
        // Verify the cursor is at the grid boundary (not wrapped to a wrong position).
        assert_eq!(state.cursor().row, 23, "row must clamp to max row");
        assert_eq!(state.cursor().col, 79, "col must clamp to max col");
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

    // ── Truecolor SGR drain (code-review fix IN-02) ───────────────────────────

    #[test]
    fn sgr_truecolor_38_2_rgb_does_not_misinterpret_rgb_as_sgr_codes() {
        // SGR 38;2;0;0;0 (truecolor black fg) must NOT trigger SGR 0 (reset) for
        // each of the r/g/b components. Without the drain fix, r=0 would fire
        // SGR 0 (reset all), then g=0 (reset again), then b=0 (reset again).
        // After the fix, the r/g/b params are drained and the preceding attributes
        // (e.g. BOLD) survive.
        let mut state = ts(80, 24);
        // Set BOLD, then emit truecolor fg — BOLD must survive the truecolor sequence.
        state.advance(b"\x1b[1m"); // SGR 1: BOLD
        state.advance(b"\x1b[38;2;0;0;0m"); // SGR 38;2;0;0;0 (truecolor black fg)
        state.advance(b"A");
        let cell = state.cell(0, 0);
        assert_eq!(
            cell.style.0 & CellStyle::BOLD,
            CellStyle::BOLD,
            "BOLD must survive after SGR 38;2;0;0;0 (truecolor fg drain must not fire SGR 0)"
        );
    }

    #[test]
    fn sgr_truecolor_48_2_rgb_does_not_misinterpret_rgb_as_sgr_codes() {
        // Same test for bg truecolor: SGR 48;2;0;0;0 must not fire SGR 0 via r/g/b.
        let mut state = ts(80, 24);
        state.advance(b"\x1b[1m"); // BOLD
        state.advance(b"\x1b[48;2;0;0;0m"); // truecolor bg
        state.advance(b"B");
        let cell = state.cell(0, 0);
        assert_eq!(
            cell.style.0 & CellStyle::BOLD,
            CellStyle::BOLD,
            "BOLD must survive after SGR 48;2;0;0;0 (truecolor bg drain must not fire SGR 0)"
        );
    }

    // ── WR-02 regression: 256-color index range validation ───────────────────

    /// WR-02 regression: `CSI 38;5;255m` (max valid index) must set fg=Some(255).
    #[test]
    fn sgr_256_color_fg_boundary_255_accepted() {
        let mut state = ts(80, 24);
        state.advance(b"\x1b[38;5;255mZ");
        assert_eq!(
            state.cell(0, 0).fg,
            Some(255),
            "index 255 (max valid) must be accepted as fg=Some(255)"
        );
    }

    /// WR-02 regression: `CSI 38;5;256m` (first out-of-range index) must not set fg.
    ///
    /// Before the fix, `256u16 as u8` truncated to 0, silently setting fg=Some(0)
    /// (explicit black) instead of leaving the color unchanged. After the fix, the
    /// out-of-range index is rejected and fg remains None (default).
    #[test]
    fn sgr_256_color_fg_out_of_range_256_rejected() {
        let mut state = ts(80, 24);
        // fg starts as None (default); send an out-of-range index.
        state.advance(b"\x1b[38;5;256mZ");
        assert_eq!(
            state.cell(0, 0).fg,
            None,
            "index 256 (out of 256-color range) must be rejected; fg must remain None"
        );
    }

    /// WR-02 regression: `CSI 38;5;300m` (truncation victim) must not set fg.
    ///
    /// 300u16 as u8 = 44 (300 mod 256). Before the fix, fg was silently set to
    /// Some(44). After the fix, the out-of-range index is rejected.
    #[test]
    fn sgr_256_color_fg_out_of_range_300_rejected() {
        let mut state = ts(80, 24);
        state.advance(b"\x1b[38;5;300mZ");
        assert_eq!(
            state.cell(0, 0).fg,
            None,
            "index 300 (would truncate to 44) must be rejected; fg must remain None"
        );
    }

    /// WR-02 regression: `CSI 48;5;256m` (bg, first out-of-range) must not set bg.
    #[test]
    fn sgr_256_color_bg_out_of_range_256_rejected() {
        let mut state = ts(80, 24);
        state.advance(b"\x1b[48;5;256mZ");
        assert_eq!(
            state.cell(0, 0).bg,
            None,
            "bg index 256 (out of 256-color range) must be rejected; bg must remain None"
        );
    }

    // ── CR-03 regression: bounded OSC via explicit caps in osc_dispatch (Phase 16) ──

    /// CR-03 regression: feeding a large OSC sequence must not panic or OOM.
    ///
    /// Phase 16 re-enables vte "std" (for large OSC 52 clipboard support) and
    /// re-mitigates CR-03 via explicit caps applied in `osc_dispatch` BEFORE storing:
    /// - MAX_TITLE_BYTES (1024): titles exceeding the cap are silently discarded.
    /// - OSC_52_MAX_BYTES (65536): OSC 52 data is truncated to this cap before storing.
    ///
    /// This test verifies:
    /// 1. Feeding a multi-MB OSC 2 title sequence does not panic or OOM.
    /// 2. The title is either not set (discarded as too large) or bounded by MAX_TITLE_BYTES.
    /// 3. Normal-sized OSC sequences still work after a large one.
    #[test]
    fn adversarial_large_osc_title_is_bounded_no_panic() {
        let mut state = ts(80, 24);

        // Build a large OSC 2 title sequence: "\x1b]2;" + 64 KiB of 'A' + "\x07"
        // With vte std + osc_dispatch cap: title exceeds MAX_TITLE_BYTES (1024) so it
        // is silently discarded. No OOM risk since osc_dispatch gates before storing.
        let large_payload = vec![b'A'; 64 * 1024]; // 64 KiB is enough to test cap
        let mut seq = Vec::new();
        seq.extend_from_slice(b"\x1b]2;");
        seq.extend_from_slice(&large_payload);
        seq.extend_from_slice(b"\x07"); // BEL terminator

        // Must not panic or OOM.
        state.advance(&seq);

        // The title must either be None (discarded as too large per MAX_TITLE_BYTES cap)
        // or a string bounded by MAX_TITLE_BYTES. It must NOT hold the full 64 KiB payload.
        if let Some(title) = state.title() {
            assert!(
                title.len() <= MAX_TITLE_BYTES,
                "title must be bounded by MAX_TITLE_BYTES ({}), got {} bytes",
                MAX_TITLE_BYTES,
                title.len()
            );
        }
        // No assertion on whether title is Some or None — title exceeds MAX_TITLE_BYTES
        // so it should be discarded (None), but we accept Some with bounded len.

        // Normal-sized OSC sequences must still work after the large one.
        state.advance(b"\x1b]2;Normal Title\x07");
        assert_eq!(
            state.title(),
            Some("Normal Title"),
            "normal OSC title must work after a large discarded one"
        );
    }

    /// CR-03 regression: feeding a large OSC 52 sequence must not OOM.
    ///
    /// Phase 16 re-mitigates via OSC_52_MAX_BYTES (65536) cap in osc_dispatch.
    /// With vte std re-enabled, osc_dispatch now receives the full (large) data,
    /// but truncates it to OSC_52_MAX_BYTES before storing in osc52_pending.
    #[test]
    fn adversarial_large_osc52_is_bounded_no_panic() {
        let mut state = ts(80, 24);

        // Build a large OSC 52 sequence: "\x1b]52;c;" + 128 KiB of base64 data + "\x07"
        // (128 KiB > OSC_52_MAX_BYTES = 64 KiB, so truncation occurs)
        let large_b64 = vec![b'A'; 128 * 1024]; // simulated base64 payload, larger than cap
        let mut seq = Vec::new();
        seq.extend_from_slice(b"\x1b]52;c;");
        seq.extend_from_slice(&large_b64);
        seq.extend_from_slice(b"\x07");

        // Must not panic or OOM.
        state.advance(&seq);

        // osc52_pending must either be None or hold a payload bounded by OSC_52_MAX_BYTES.
        if let Some((sel, data)) = state.osc52_pending() {
            assert!(
                data.len() <= OSC_52_MAX_BYTES,
                "osc52_pending data must be bounded by OSC_52_MAX_BYTES ({}), got {} bytes",
                OSC_52_MAX_BYTES,
                data.len()
            );
            let _ = sel; // selection bytes are small
        }

        // Normal OSC 52 must still work after a large one.
        state.advance(b"\x1b]52;c;SGVsbG8=\x07"); // "Hello" in base64
        let pending = state.osc52_pending();
        assert!(pending.is_some(), "normal OSC 52 must still be detected after large one");
        let (_, data) = pending.unwrap();
        assert_eq!(data, b"SGVsbG8=");
    }

    // ── Phase 16: OSC 52 security gate + drain method tests ──────────────────

    /// D-16-01a / T-16-01: OSC 52 read/query form must be SILENTLY DROPPED.
    ///
    /// The read form `OSC 52;c;?` is a terminal clipboard query: the terminal
    /// responds with its clipboard contents. The server must NEVER store or forward
    /// this form — doing so would leak server clipboard contents to the client.
    ///
    /// Security gate: `if data == b"?" { return; }` in `osc_dispatch` BEFORE storing.
    #[test]
    fn osc52_read_form_is_silently_dropped() {
        let mut state = ts(80, 24);
        // Feed the OSC 52 read/query form (data field is "?").
        state.advance(b"\x1b]52;c;?\x07");
        // The read form must NEVER be stored — osc52_pending must be None.
        assert!(
            state.osc52_pending().is_none(),
            "OSC 52 read/query form ('?') must be silently dropped — never stored (D-16-01a)"
        );
    }

    /// OSC 52 write form is stored correctly.
    #[test]
    fn osc52_write_form_is_stored() {
        let mut state = ts(80, 24);
        state.advance(b"\x1b]52;c;SGVsbG8=\x07");
        let pending = state.osc52_pending();
        assert!(pending.is_some(), "OSC 52 write form must be stored in osc52_pending");
        let (sel, data) = pending.unwrap();
        assert_eq!(sel, b"c", "selection must be 'c'");
        assert_eq!(data, b"SGVsbG8=", "data must be the base64 payload");
    }

    /// take_osc52() drains osc52_pending and clears it (Option::take semantics).
    #[test]
    fn take_osc52_drains_and_clears() {
        let mut state = ts(80, 24);
        state.advance(b"\x1b]52;c;SGVsbG8=\x07");
        // First drain returns the payload.
        let taken = state.take_osc52();
        assert!(taken.is_some(), "take_osc52 must return Some after a write-form advance");
        let (sel, data) = taken.unwrap();
        assert_eq!(sel, b"c");
        assert_eq!(data, b"SGVsbG8=");
        // Second drain must return None (field was cleared).
        assert!(
            state.take_osc52().is_none(),
            "take_osc52 must return None on second call (drain-once semantics)"
        );
        // osc52_pending() must also be None now.
        assert!(state.osc52_pending().is_none(), "osc52_pending must be None after take_osc52");
    }

    /// take_title() drains the title field and clears it (Option::take semantics).
    #[test]
    fn take_title_drains_and_clears() {
        let mut state = ts(80, 24);
        state.advance(b"\x1b]2;My Title\x07");
        // title() still shows the value before take.
        assert_eq!(state.title(), Some("My Title"), "title must be set before take");
        // take_title returns and clears.
        let taken = state.take_title();
        assert_eq!(taken.as_deref(), Some("My Title"), "take_title must return the title");
        // After take, title() must be None.
        assert!(state.title().is_none(), "title must be None after take_title");
        // Second take must be None.
        assert!(
            state.take_title().is_none(),
            "take_title must return None on second call (drain-once semantics)"
        );
    }

    // ── Task 2: Resize both grids and scroll_up gate (TUI-02, D-19-09) ───────

    /// TUI-02: resize while alt-screen is active resizes both the active alt grid
    /// and the saved primary grid. On ?1049l the restored primary has the new
    /// dimensions, not the old ones from before the resize.
    #[test]
    fn resize_while_alt_screen_active_resizes_both_grids() {
        let mut state = ts(80, 24);
        // Write distinctive primary content at known positions (avoiding bottom-right
        // which would trigger wrap+scroll and lose (0,0) content to scrollback).
        state.advance(b"P"); // 'P' at (0,0), cursor moves to (0,1)
        state.advance(b"\x1b[10;20HR"); // cursor to row=9, col=19 (1-based 10,20); write 'R'
        // Verify primary content is present.
        assert_eq!(state.cell(0, 0).ch, 'P', "sanity: P at (0,0) before alt-screen");
        assert_eq!(state.cell(9, 19).ch, 'R', "sanity: R at (9,19) before alt-screen");

        // Enter alt-screen at 80x24.
        state.advance(b"\x1b[?1049h");
        assert!(state.echo_state().alt_screen);
        assert_eq!(state.size(), (80, 24));

        // Resize to 100x30 while alt-screen is active.
        state.resize(100, 30);
        assert_eq!(state.size(), (100, 30), "active alt grid must have new size after resize");

        // Exit alt-screen — primary grid must be restored at new dimensions.
        state.advance(b"\x1b[?1049l");
        assert!(!state.echo_state().alt_screen);
        let (cols, rows) = state.size();
        assert_eq!(cols, 100, "restored primary grid must have new cols after resize");
        assert_eq!(rows, 30, "restored primary grid must have new rows after resize");
        // Primary content at both positions must survive.
        assert_eq!(state.cell(0, 0).ch, 'P', "primary content at (0,0) must survive resize+exit");
        assert_eq!(state.cell(9, 19).ch, 'R', "primary content at (9,19) must survive resize+exit");
        // Each row must have the new column count — accessing col 99 must not panic.
        for r in 0..30u16 {
            let _ = state.cell(r, 99);
        }
    }

    /// TUI-02: shrinking rows while alt-screen active pushes excess saved primary
    /// rows into self.scrollback (not lost), bounded by SCROLLBACK_LINE_CAP.
    #[test]
    fn resize_shrink_while_alt_screen_pushes_saved_primary_rows_to_scrollback() {
        let mut state = ts(80, 10);
        // Write content on all 10 primary rows before entering alt-screen.
        for _ in 0..10 {
            state.advance(b"DATA\n");
        }
        let scroll_before = state.scrollback.len();

        // Enter alt-screen.
        state.advance(b"\x1b[?1049h");

        // Shrink from 10 rows to 5 rows — the top 5 primary rows should go to scrollback.
        state.resize(80, 5);

        // Exit — primary restored at 5 rows.
        state.advance(b"\x1b[?1049l");
        assert_eq!(state.size(), (80, 5), "primary must be 5 rows after shrink");

        // Scrollback must have grown (the 5 excess saved primary rows were pushed).
        assert!(
            state.scrollback.len() > scroll_before,
            "scrollback must grow when saved primary shrinks: {} > {}",
            state.scrollback.len(),
            scroll_before
        );
    }

    /// D-19-09: scroll_up while alt-screen active must NOT push to scrollback.
    ///
    /// This is the gate Phase 22 (scrollback sync) depends on — alt-screen content
    /// must never contaminate primary scrollback history.
    #[test]
    fn scroll_up_in_alt_screen_does_not_push_to_scrollback() {
        let mut state = ts(80, 3); // small terminal to force scrolling quickly

        // Write to primary and scroll — scrollback must grow.
        state.advance(b"line1\nline2\nline3\nline4\n"); // forces scrollback on primary
        let scroll_after_primary = state.scrollback.len();
        assert!(scroll_after_primary > 0, "primary scrollback must be non-empty");

        // Enter alt-screen.
        state.advance(b"\x1b[?1049h");
        let scroll_on_enter = state.scrollback.len();

        // Force scrolling on the alt grid by filling it with newlines.
        state.advance(b"alt1\nalt2\nalt3\nalt4\nalt5\n");

        // Scrollback length must be UNCHANGED — alt content is discarded (D-19-09).
        assert_eq!(
            state.scrollback.len(), scroll_on_enter,
            "scrollback must not grow while alt-screen is active (D-19-09 gate)"
        );

        // Exit alt-screen.
        state.advance(b"\x1b[?1049l");

        // Primary scrollback must still be intact (unchanged by alt-screen activity).
        assert_eq!(
            state.scrollback.len(), scroll_after_primary,
            "primary scrollback must be intact after exiting alt-screen"
        );
    }

    // ── Task 1: Two-grid alt-screen model (TUI-01) ───────────────────────────

    /// TUI-01: entering alt-screen presents a blank grid at cursor (0,0).
    ///
    /// ?1049h must: clear grid to blank, move cursor to (0,0), reset SGR, set
    /// echo_state.alt_screen = true. Primary content written before enter must not
    /// be visible on the alt grid.
    #[test]
    fn alt_screen_enter_presents_blank_grid_at_origin() {
        let mut state = ts(80, 24);
        // Write distinctive primary content and move cursor.
        state.advance(b"\x1b[5;10H"); // cursor at row=4, col=9 (0-based)
        state.advance(b"\x1b[31mX");  // write 'X' with red fg at (4,9)
        assert_eq!(state.cell(4, 9).ch, 'X');

        // Enter alternate screen.
        state.advance(b"\x1b[?1049h");

        // After enter: alt_screen flag must be set.
        assert!(state.echo_state().alt_screen, "alt_screen flag must be true after ?1049h");
        // Cursor must be at (0,0).
        assert_eq!(state.cursor(), CursorPos { row: 0, col: 0 }, "cursor must be at (0,0) after ?1049h");
        // The grid must be all-blank (primary content must not bleed through).
        for row in 0..24u16 {
            for col in 0..80u16 {
                assert_eq!(
                    state.cell(row, col).ch, ' ',
                    "alt grid cell ({row},{col}) must be blank after ?1049h"
                );
                assert_eq!(
                    state.cell(row, col).fg, None,
                    "alt grid cell ({row},{col}) fg must be None after ?1049h"
                );
            }
        }
    }

    /// TUI-01: exiting alt-screen restores primary grid, cursor, and SGR pen exactly.
    ///
    /// The round-trip ?1049h/?1049l must restore: the full primary grid content,
    /// the exact cursor position at the time of enter, and the SGR pen state.
    #[test]
    fn alt_screen_exit_restores_primary_grid_cursor_sgr() {
        let mut state = ts(80, 24);

        // Write distinctive primary content: 'P' at (3,5) with bold fg=Some(1).
        state.advance(b"\x1b[1;31m"); // SGR bold + fg=1 (red)
        state.advance(b"\x1b[4;6HP"); // cursor to row=3, col=5 (1-based 4,6), write 'P'
        // Verify primary content is written.
        assert_eq!(state.cell(3, 5).ch, 'P');
        // Cursor should now be at (3,6) after writing 'P'.
        assert_eq!(state.cursor(), CursorPos { row: 3, col: 6 });

        // Enter alternate screen — save cursor at (3,6).
        state.advance(b"\x1b[?1049h");
        assert!(state.echo_state().alt_screen);
        // Scribble on alt grid — must not affect primary on exit.
        state.advance(b"ALTCONTENT");
        assert_eq!(state.cell(0, 0).ch, 'A', "alt content must be written");

        // Exit alternate screen — restore primary.
        state.advance(b"\x1b[?1049l");

        // Flag cleared.
        assert!(!state.echo_state().alt_screen, "alt_screen flag must be false after ?1049l");
        // Primary content at (3,5) must be restored.
        assert_eq!(
            state.cell(3, 5).ch, 'P',
            "primary grid content must be restored after ?1049l"
        );
        // Alt content must not appear on the primary grid.
        assert_ne!(
            state.cell(0, 0).ch, 'A',
            "alt grid content must not bleed into primary after ?1049l"
        );
        // Cursor must be restored to (3,6) — position at the time of ?1049h.
        assert_eq!(
            state.cursor(), CursorPos { row: 3, col: 6 },
            "cursor must be restored to its ?1049h position after ?1049l"
        );
        // SGR pen must be restored: fg=Some(1), bold. Write a cell and check its attrs.
        state.advance(b"Q");
        assert_eq!(
            state.cell(3, 6).fg, Some(1),
            "SGR fg must be restored after ?1049l"
        );
        assert_ne!(
            state.cell(3, 6).style.0 & CellStyle::BOLD, 0,
            "SGR BOLD must be restored after ?1049l"
        );
    }

    /// TUI-01: nested ?1049h (second enter while already in alt-screen) must not panic.
    ///
    /// xterm-divergent safe behaviour: overwrite saved_primary with current (alt) grid.
    /// Memory is bounded (one saved grid max). Thread T-19-03 (DoS).
    #[test]
    fn alt_screen_nested_enter_no_panic() {
        let mut state = ts(80, 24);
        // First enter.
        state.advance(b"\x1b[?1049h");
        assert!(state.echo_state().alt_screen);
        state.advance(b"FIRST_ALT");
        // Second enter while already in alt-screen — must not panic.
        state.advance(b"\x1b[?1049h");
        // Still in alt-screen.
        assert!(state.echo_state().alt_screen, "still in alt-screen after nested enter");
        // Exit — must not panic (saved_primary now holds the first alt grid).
        state.advance(b"\x1b[?1049l");
        assert!(!state.echo_state().alt_screen, "alt_screen cleared after exit");
    }

    /// TUI-01: bare ?1049l with no prior ?1049h is a graceful no-op (T-19-01).
    ///
    /// Grid must be unchanged; only the flag is cleared.
    #[test]
    fn alt_screen_bare_exit_no_prior_enter_is_noop() {
        let mut state = ts(80, 24);
        state.advance(b"hello");
        assert_eq!(state.cell(0, 0).ch, 'h');
        // Exit without a prior enter — must not panic or corrupt state.
        state.advance(b"\x1b[?1049l");
        // Grid unchanged.
        assert_eq!(
            state.cell(0, 0).ch, 'h',
            "grid must be unchanged after bare ?1049l"
        );
        // Flag cleared (it was false to begin with, still false).
        assert!(!state.echo_state().alt_screen, "alt_screen must be false after bare exit");
    }

    // ── Task 1 (19-02): Wide-character and zero-width tests (TDD RED gate) ──────

    /// TUI-03: A CJK width-2 char advances cursor.col by 2 and writes a wide
    /// continuation marker at col+1 (D-19-05, D-19-06).
    #[test]
    fn wide_char_cjk_advances_cursor_by_two_and_writes_continuation() {
        let mut state = ts(80, 24);
        // U+4E2D '中' is CJK, width 2.
        state.advance("中".as_bytes());
        // Cursor must have advanced 2 columns.
        assert_eq!(
            state.cursor(),
            CursorPos { row: 0, col: 2 },
            "CJK width-2 char must advance cursor by 2"
        );
        // Primary cell at col 0: the glyph itself.
        let primary = state.cell(0, 0);
        assert_eq!(primary.ch, '中', "col 0 must hold the CJK glyph");
        assert!(!primary.wide, "primary cell must not be wide:true");
        // Continuation cell at col 1: wide:true.
        let cont = state.cell(0, 1);
        assert!(cont.wide, "col 1 must be the wide continuation marker (wide:true)");
    }

    /// TUI-03: A zero-width combining mark (U+0301 combining acute) must not
    /// advance the cursor (D-19-05).
    #[test]
    fn zero_width_combining_mark_does_not_advance_cursor() {
        let mut state = ts(80, 24);
        // Write 'a' first so cursor is at col 1.
        state.advance(b"a");
        assert_eq!(state.cursor().col, 1, "after 'a' cursor must be at col 1");
        // U+0301 COMBINING ACUTE ACCENT — width 0.
        state.advance("\u{0301}".as_bytes());
        // Cursor must NOT advance.
        assert_eq!(
            state.cursor().col, 1,
            "zero-width combining mark must not advance cursor"
        );
    }

    /// TUI-03: A width-2 char at the right edge (col == cols-1) must not panic
    /// and must not write the continuation cell out of bounds (T-19-04).
    #[test]
    fn wide_char_at_right_edge_does_not_panic() {
        let mut state = ts(4, 1);
        // Move cursor to col 3 (last column in a 4-wide terminal).
        state.advance(b"   "); // 3 ASCII chars → cursor at col 3
        assert_eq!(state.cursor().col, 3, "cursor should be at col 3");
        // Writing '中' (width 2) at col 3: col+1 = 4 which is out of bounds.
        // Must not panic; continuation cell write is suppressed.
        state.advance("中".as_bytes());
        // Cursor must have advanced without panicking (wraps or clamps — exact
        // wrap behaviour is implementation-defined; the key invariant is no panic
        // and no out-of-bounds grid write).
        let _ = state.cursor(); // just verifying no panic
    }

    /// TUI-01: RIS (ESC c) while in alt-screen clears saved_primary so a subsequent
    /// ?1049l does not restore stale pre-reset content (Pitfall 5 / RIS invariant).
    #[test]
    fn alt_screen_ris_clears_saved_primary() {
        let mut state = ts(80, 24);
        // Write primary content, enter alt-screen.
        state.advance(b"PRIMARY");
        state.advance(b"\x1b[?1049h");
        assert!(state.echo_state().alt_screen);
        // RIS while in alt-screen.
        state.advance(b"\x1bc");
        // After RIS: alt_screen flag should be cleared (EchoState reset).
        assert!(!state.echo_state().alt_screen, "alt_screen must be false after RIS");
        // A bare ?1049l after RIS must not restore pre-reset primary content.
        state.advance(b"\x1b[?1049l");
        // Grid must not contain 'P' from "PRIMARY" — RIS cleared saved_primary.
        assert_eq!(
            state.cell(0, 0).ch, ' ',
            "?1049l after RIS must not restore pre-reset primary content"
        );
    }

    // ── Task 1 (19-03): SEC-03 OSC accumulation pre-bound (TDD RED → GREEN) ──────

    /// SEC-03 / T-19-06 / T-19-07: A multi-chunk OSC payload exceeding 1 MiB must
    /// be intercepted BEFORE vte allocates the full buffer.
    ///
    /// RED gate: before the `OSC_ACCUMULATION_MAX` pre-filter exists in `advance()`,
    /// feeding a ~10 MiB OSC 2 title across thousands of `advance()` calls would grow
    /// vte's internal `osc_raw` Vec to the full payload size, exhausting server memory.
    /// The test also asserts parser resync: after the truncated oversized OSC, a normal
    /// OSC 0/2 title and an OSC 52 sequence must still dispatch and store correctly.
    ///
    /// GREEN gate (post-fix): the pre-filter intercepts at `OSC_ACCUMULATION_MAX` bytes,
    /// resets the parser to ground state via `vte::Parser::default()`, and subsequent
    /// legitimate OSC sequences still parse and dispatch correctly.
    #[test]
    fn oversized_multi_chunk_osc_is_bounded_then_resyncs() {
        let mut state = ts(80, 24);

        // Build a 10 MiB OSC 2 title sequence delivered in 4096-byte chunks.
        // ESC ] 2 ; <10 * OSC_ACCUMULATION_MAX bytes of 'A'> BEL
        // After the fix: accumulation is capped at OSC_ACCUMULATION_MAX (1 MiB),
        // the oversized sequence is discarded, and the parser resyncs to ground.
        const CHUNK: usize = 4096;
        const TOTAL: usize = OSC_ACCUMULATION_MAX * 10; // 10 MiB
        let chunks = TOTAL / CHUNK;

        // Feed OSC 2 start sequence.
        state.advance(b"\x1b]2;");

        // Feed payload in chunks — after the fix, accumulation is bounded.
        // Without the fix, this would grow vte's osc_raw to ~10 MiB (OOM risk).
        let chunk_data = vec![b'A'; CHUNK];
        for _ in 0..chunks {
            state.advance(&chunk_data);
        }

        // Feed BEL terminator — triggers osc_dispatch (or is discarded after resync).
        state.advance(b"\x07");

        // After the oversized OSC, the title must NOT be a 10 MiB string.
        // Either it is None (discarded) or very small (bounded by MAX_TITLE_BYTES).
        if let Some(title) = state.title() {
            assert!(
                title.len() <= MAX_TITLE_BYTES,
                "title after 10 MiB OSC must be bounded by MAX_TITLE_BYTES ({}), got {} bytes",
                MAX_TITLE_BYTES,
                title.len()
            );
        }

        // D-19-02 / T-19-07: After the oversized OSC, the parser must have resynced
        // to ground state so subsequent legitimate OSC sequences still parse correctly.

        // A normal OSC 0/2 title must now be accepted and stored.
        state.advance(b"\x1b]2;OK\x07");
        assert_eq!(
            state.title(),
            Some("OK"),
            "after oversized-OSC resync, a normal OSC 2 title must be accepted (D-19-02)"
        );

        // A legitimate OSC 52 clipboard sequence must also still dispatch correctly.
        state.advance(b"\x1b]52;c;SGVsbG8=\x07");
        let pending = state.osc52_pending();
        assert!(
            pending.is_some(),
            "after oversized-OSC resync, OSC 52 clipboard must still dispatch (D-19-03)"
        );
        let (_, data) = pending.unwrap();
        assert_eq!(
            data, b"SGVsbG8=",
            "OSC 52 payload must be intact after resync"
        );

        // Existing storage caps must still hold after the oversized-OSC path.
        if let Some((_sel, payload)) = state.osc52_pending() {
            assert!(
                payload.len() <= OSC_52_MAX_BYTES,
                "OSC 52 cap (OSC_52_MAX_BYTES) must still hold after oversized-OSC path"
            );
        }
    }

    // ── CR-01 regression: split ST terminator across advance() calls ─────────────

    /// CR-01 regression: `ESC \` (String Terminator) split across two `advance()` calls
    /// must close the OSC, not leave `in_osc` permanently true.
    ///
    /// Before the fix: when `osc_prefilter` saw a lone `0x1B` at the end of a slice
    /// (while `in_osc` was true) it fell through to `osc_byte_count += 1` — counting
    /// the ESC as payload. On the next call, `0x5C` was also counted as payload rather
    /// than recognised as the ST second byte. The `in_osc` shadow flag stayed true
    /// indefinitely, causing all subsequent non-OSC output to be counted against the
    /// 1 MiB budget and eventually triggering a spurious parser reset (SEC-03 / D-19-01).
    ///
    /// After the fix (`pending_esc` field): the lone trailing ESC is flagged rather than
    /// counted; the next call checks for `0x5C` before the main scan and correctly
    /// closes the OSC.
    #[test]
    fn split_st_terminator_across_advance_calls_closes_osc() {
        let mut state = ts(80, 24);

        // Feed OSC 2 title start + payload + lone ESC (first byte of ESC \ ST).
        // The ESC lands at the very end of the first advance() call.
        state.advance(b"\x1b]2;Hello\x1b"); // ESC at end — potential ST first byte

        // Now feed the second byte of the ST terminator (0x5C = '\').
        // Before the fix: this would be counted as an OSC payload byte.
        // After the fix: the pending_esc + 0x5C → confirmed ST → in_osc = false.
        state.advance(b"\x5c"); // ST second byte: '\'

        // The OSC should now be closed. Feeding plain text must advance the cursor
        // normally — it must NOT be consumed by the OSC accumulator.
        state.advance(b"hello");
        assert_eq!(
            state.cursor().col, 5,
            "plain text after split-ST must advance cursor (col must be 5, not 0); \
             if in_osc stayed true the text was silently swallowed by the OSC accumulator"
        );
    }

    /// CR-01 regression: after a split-ST close, a new OSC sequence is still parsed.
    ///
    /// Verifies the parser resyncs cleanly: the in_osc shadow tracker is false after
    /// the split-ST, and the next OSC 2 title sequence is accepted normally.
    #[test]
    fn split_st_followed_by_normal_osc_parses_correctly() {
        let mut state = ts(80, 24);

        // First OSC 2 title, terminated with split ST.
        state.advance(b"\x1b]2;Title1\x1b");
        state.advance(b"\x5c");

        // A subsequent normal OSC 2 title (BEL-terminated) must parse.
        state.advance(b"\x1b]2;Title2\x07");
        assert_eq!(
            state.title(),
            Some("Title2"),
            "OSC 2 title after split-ST must be accepted and stored"
        );
    }

    // ── TUI-04 grid-assertion regression suite (19-05 D-19-07) ─────────────────
    //
    // Deterministic synthetic VT grid-assertion tests covering cursor-addressing paths
    // representative of real TUI rendering. These complement the per-feature tests from
    // plans 01–03 (alt-screen toggle, wide chars, OSC) — they do NOT duplicate those;
    // they exercise the cursor-addressing integration path that full-screen TUIs rely on.
    //
    // No sleeps, no network, no live-app invocation (D-19-07).

    /// TUI-04 (a): CUP absolute positioning followed by printing places glyphs at the
    /// exact addressed cells, independent of the cursor's prior position.
    ///
    /// This exercises the core rendering path that htop uses to paint its CPU bars:
    /// repeated CUP + text sequences where every glyph must land at a specific cell.
    #[test]
    fn cup_addressed_write_lands_glyphs_at_exact_cells() {
        let mut state = ts(80, 24);

        // CUP to (row=5, col=10) (1-based), write "HTOP".
        // Expected: 'H' at (4,9), 'T' at (4,10), 'O' at (4,11), 'P' at (4,12).
        state.advance(b"\x1b[6;11H"); // 1-based row=6 col=11 → 0-based row=5 col=10
        state.advance(b"HTOP");
        assert_eq!(state.cell(5, 10).ch, 'H', "CUP-addressed 'H' must land at (5,10)");
        assert_eq!(state.cell(5, 11).ch, 'T', "CUP-addressed 'T' must land at (5,11)");
        assert_eq!(state.cell(5, 12).ch, 'O', "CUP-addressed 'O' must land at (5,12)");
        assert_eq!(state.cell(5, 13).ch, 'P', "CUP-addressed 'P' must land at (5,13)");
        // Cursor must be positioned immediately after the last written character.
        assert_eq!(state.cursor(), CursorPos { row: 5, col: 14 },
            "cursor must be at (5,14) after 'HTOP'");

        // Second CUP to a different row — prior content must be undisturbed.
        state.advance(b"\x1b[3;1H"); // row=3, col=1 (1-based) → (2,0)
        state.advance(b"VIM");
        assert_eq!(state.cell(2, 0).ch, 'V', "second CUP 'V' must land at (2,0)");
        assert_eq!(state.cell(2, 1).ch, 'I');
        assert_eq!(state.cell(2, 2).ch, 'M');
        // Original row 5 content must be undisturbed.
        assert_eq!(state.cell(5, 10).ch, 'H', "row 5 must be undisturbed after second CUP");

        // CUP to (1,1) (home position) is a special case — must also work.
        state.advance(b"\x1b[1;1H");
        state.advance(b"Z");
        assert_eq!(state.cell(0, 0).ch, 'Z', "CUP home position must write to (0,0)");
        assert_eq!(state.cursor(), CursorPos { row: 0, col: 1 });
    }

    /// TUI-04 (b): Alt-screen enter → CUP-addressed full-screen paint → exit round trip.
    ///
    /// This is the integration-level atomicity test for the two-grid model.  A real vim
    /// session does exactly this: fill the alt-screen via CUP sequences, then exit and
    /// expect the primary shell scrollback to be exactly as it was. The test verifies
    /// that: the alt-screen paint is isolated to the alt grid, the primary content at
    /// specific CUP-addressed positions is preserved intact, and the cursor is restored.
    ///
    /// This is distinct from the unit-level toggle tests in plan 01 (`alt_screen_enter_
    /// presents_blank_grid_at_origin`, `alt_screen_exit_restores_primary_grid_cursor_sgr`)
    /// — those verify the raw save/restore mechanism; this verifies the full vim-like
    /// workflow including CUP-addressed painting on the alt grid.
    #[test]
    fn alt_screen_cup_fullscreen_paint_exit_restores_primary_exactly() {
        let mut state = ts(80, 24);

        // ── Phase 1: establish primary content at known CUP-addressed positions ──
        // Use CUP to write at three distinct positions so we can verify each survives.
        state.advance(b"\x1b[1;1H");  // home
        state.advance(b"shell prompt $");
        state.advance(b"\x1b[10;5H"); // (9,4)
        state.advance(b"primary-line-9");
        state.advance(b"\x1b[20;40H"); // (19,39)
        state.advance(b"anchor");
        // Save primary cursor position (after last write).
        let primary_cursor_after_writes = state.cursor();

        // ── Phase 2: enter alt-screen — primary must be saved atomically ──
        state.advance(b"\x1b[?1049h");
        assert!(state.echo_state().alt_screen,
            "alt_screen flag must be true after ?1049h");
        // Alt grid must be blank — no primary content bleeds through.
        assert_eq!(state.cell(0, 0).ch, ' ',
            "alt grid (0,0) must be blank after enter");
        assert_eq!(state.cell(9, 4).ch, ' ',
            "alt grid (9,4) must be blank — primary-line-9 must not bleed through");

        // ── Phase 3: paint the alt screen with CUP-addressed content ──
        // Simulate what vim does: CUP to every "important" position and write.
        state.advance(b"\x1b[1;1H");
        state.advance(b"-- INSERT MODE --");
        state.advance(b"\x1b[12;30H");
        state.advance(b"vim content here");
        state.advance(b"\x1b[24;1H"); // bottom status bar (row 24 = 0-based 23)
        state.advance(b"\"file.txt\" 1L");
        // Verify alt content is visible.
        assert_eq!(state.cell(0, 0).ch, '-',
            "alt screen (0,0) must show vim INSERT MODE '-'");
        assert_eq!(state.cell(11, 29).ch, 'v',
            "alt screen (11,29) must show vim content 'v'");
        assert_eq!(state.cell(23, 0).ch, '"',
            "alt screen status bar (23,0) must show '\"'");

        // ── Phase 4: exit alt-screen — primary must be restored exactly ──
        state.advance(b"\x1b[?1049l");
        assert!(!state.echo_state().alt_screen,
            "alt_screen flag must be false after ?1049l");

        // Primary content at all three original positions must be restored.
        assert_eq!(state.cell(0, 0).ch, 's',
            "primary (0,0) 's' from 'shell prompt $' must be restored after alt exit");
        assert_eq!(state.cell(9, 4).ch, 'p',
            "primary (9,4) 'p' from 'primary-line-9' must be restored");
        assert_eq!(state.cell(19, 39).ch, 'a',
            "primary (19,39) 'a' from 'anchor' must be restored");

        // Alt content must NOT appear on the primary grid.
        // Row 0, col 0 has 's' (from "shell prompt"), not '-' (from vim INSERT MODE).
        assert_ne!(state.cell(0, 0).ch, '-',
            "vim INSERT MODE '-' must not bleed into primary grid after exit");

        // Cursor must be restored to the position it was at when ?1049h was issued.
        assert_eq!(state.cursor(), primary_cursor_after_writes,
            "cursor must be restored to its pre-?1049h position after exit");
    }

    /// TUI-04 (c): Wide char written at a CUP-addressed position advances cursor by 2
    /// and the continuation cell at col+1 is present as a wide marker.
    ///
    /// This tests the combination of CUP positioning and wide-char rendering — the path
    /// used by apps that display CJK text at specific column offsets (e.g. status bars).
    /// Complements the unit-level tests in plan 02 (`wide_char_cjk_advances_cursor_by_
    /// two_and_writes_continuation`) — this adds CUP addressing as the antecedent.
    #[test]
    fn cup_addressed_wide_char_advances_correctly_and_has_continuation() {
        let mut state = ts(80, 24);

        // CUP to (row=3, col=10) (1-based 4,11 → 0-based 3,10), write U+4E2D '中' (width 2).
        state.advance(b"\x1b[4;11H"); // 1-based row=4 col=11 → 0-based row=3 col=10
        state.advance("中".as_bytes());

        // Primary cell at (3,10): the CJK glyph.
        let glyph = state.cell(3, 10);
        assert_eq!(glyph.ch, '中', "CUP-addressed '中' must be at (3,10)");
        assert!(!glyph.wide, "primary CJK cell must not have wide:true");

        // Continuation cell at (3,11): wide marker.
        let cont = state.cell(3, 11);
        assert!(cont.wide, "continuation cell (3,11) must have wide:true after CUP-write of '中'");

        // Cursor must have advanced by 2 columns from the addressed position.
        assert_eq!(state.cursor(), CursorPos { row: 3, col: 12 },
            "cursor must be at col 12 after CUP(3,10) + wide '中'");

        // Writing ASCII after the wide char must land at the correct column (12, not 11).
        state.advance(b"X");
        assert_eq!(state.cell(3, 12).ch, 'X',
            "ASCII after CUP-wide-char must land at col 12 (cursor was at 12)");
        assert_eq!(state.cursor().col, 13,
            "cursor must advance to col 13 after ASCII at col 12");

        // Row 2 must be untouched (CUP addressed row 3 only).
        assert_eq!(state.cell(2, 10).ch, ' ',
            "row 2 col 10 must be blank — CUP targeted row 3");
    }

    /// TUI-04 (d): Erase-in-display and erase-in-line after CUP positioning clears the
    /// expected region precisely, leaving cells outside the region undisturbed.
    ///
    /// This exercises the combined CUP + ED/EL path that full-screen TUIs use to clear
    /// regions before redrawing — the correctness of region boundaries is critical for
    /// apps like htop that rely on ED/EL to clear only the portion they intend to repaint.
    #[test]
    fn cup_then_ed_el_clears_correct_region_only() {
        let mut state = ts(20, 5); // small terminal to make assertions manageable

        // Paint every cell with a distinctive character so we can verify clears precisely.
        // Fill row by row using CUP + text.
        for r in 0u16..5 {
            state.advance(format!("\x1b[{};1H", r + 1).as_bytes()); // CUP to row r, col 0
            state.advance(b"XXXXXXXXXXXXXXXXXXXX"); // 20 X's
        }
        // Verify a cell in row 2 is 'X' before any erase.
        assert_eq!(state.cell(2, 0).ch, 'X', "sanity: row 2 col 0 must be 'X' before erase");

        // ── EL 0 (erase to end of line) after CUP ──
        // CUP to (row=2, col=10) then EL 0: cols 9..19 on row 2 must clear; cols 0..8 intact.
        state.advance(b"\x1b[3;10H"); // 1-based row=3 col=10 → 0-based row=2 col=9
        state.advance(b"\x1b[K");     // EL 0 (default = 0): erase from cursor to end of line
        // Cursor is at (2,9). EL 0 clears from col 9 to end of row.
        for c in 0u16..9 {
            assert_eq!(state.cell(2, c).ch, 'X',
                "col {c} on row 2 must still be 'X' (before cursor pos 9)");
        }
        for c in 9u16..20 {
            assert_eq!(state.cell(2, c).ch, ' ',
                "col {c} on row 2 must be ' ' after EL 0 from col 9");
        }
        // Other rows must be undisturbed.
        assert_eq!(state.cell(1, 0).ch, 'X', "row 1 col 0 must be undisturbed after EL on row 2");
        assert_eq!(state.cell(3, 0).ch, 'X', "row 3 col 0 must be undisturbed after EL on row 2");

        // ── ED 0 (erase to end of screen) after CUP ──
        // CUP to (row=4, col=5) then ED 0: from (3,4) to end-of-screen must clear.
        // Row 2 already has partial 'X' content from above; row 1 is all 'X'.
        state.advance(b"\x1b[4;5H"); // 1-based row=4 col=5 → 0-based row=3 col=4
        state.advance(b"\x1b[J");    // ED 0: erase from cursor to end of screen
        // Row 3: cols 0..3 must remain 'X'; cols 4..19 must be cleared.
        for c in 0u16..4 {
            assert_eq!(state.cell(3, c).ch, 'X',
                "row 3 col {c} must be 'X' (before cursor col 4)");
        }
        for c in 4u16..20 {
            assert_eq!(state.cell(3, c).ch, ' ',
                "row 3 col {c} must be ' ' after ED 0 from col 4");
        }
        // Row 4 (index 4) must be fully cleared (it is below the cursor row).
        for c in 0u16..20 {
            assert_eq!(state.cell(4, c).ch, ' ',
                "row 4 col {c} must be ' ' after ED 0 (entirely below cursor)");
        }
        // Rows 0 and 1 must be undisturbed (they are above the cursor row).
        assert_eq!(state.cell(0, 0).ch, 'X', "row 0 must be undisturbed after ED 0");
        assert_eq!(state.cell(1, 0).ch, 'X', "row 1 must be undisturbed after ED 0");
        // Row 2 mixed content (from EL above) must be undisturbed by ED 0.
        assert_eq!(state.cell(2, 0).ch, 'X', "row 2 col 0 must still be 'X' after ED 0");
        assert_eq!(state.cell(2, 9).ch, ' ', "row 2 col 9 must still be ' ' after ED 0");
    }
}
