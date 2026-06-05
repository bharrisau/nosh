//! `PredictionOverlay` — speculative echo overlay (Mosh PredictionEngine model).
//!
//! Implements PREDICT-02, PREDICT-03, PREDICT-04, PREDICT-05, PREDICT-06.
//!
//! # Architecture
//!
//! Translates Mosh's `PredictionEngine` / `ConditionalOverlayCell` design to Rust.
//! The epoch/Validity state machine tracks speculative predictions at (row, col)
//! positions in the confirmed grid. Each `PendingPrediction` carries:
//!
//! - `epoch_required`: the StateDiff epoch at or above which this prediction is
//!   due for confirmation (>= check tolerates dropped datagrams — Pitfall 4).
//! - `tentative_until_epoch`: predictions are hidden (cell_at returns None) until
//!   `confirmed_epoch >= tentative_until_epoch`. This is the **noecho suppression
//!   mechanism** — structural, not an explicit flag.
//!
//! Noecho suppression (T-15-01 / PREDICT-04) falls out structurally: if the server
//! never echoes a typed character (stty -echo / read -s), cull() always finds a
//! mismatch, resets, and `confirmed_epoch` never advances past the initial value.
//! All new predictions therefore remain tentative (hidden) indefinitely.
//!
//! # Security
//!
//! - **Noecho suppression is structural** — `confirmed_epoch` never advances when
//!   the server suppresses echo (stty -echo / read -s). This is not an explicit flag;
//!   the invariant is: if `confirmed_epoch == initial` throughout a typing session,
//!   `cell_at` returns `None` for all positions. Proven by unit test `noecho_suppression`.
//! - **Bulk/paste input suppression prevents prediction during paste** — inputs
//!   larger than `BULK_SUPPRESS_THRESHOLD` bytes, and bracketed paste sequences
//!   (`\x1b[200~` / `\x1b[201~`), suppress all cell predictions and reset the epoch.
//!   The `pending` VecDeque cannot grow without bound (cull removes entries; reset
//!   clears all; mismatch resets all — T-15-03).
//! - **Prediction is display-only** — `on_input` mutates only local overlay state.
//!   Keystrokes are forwarded by the unchanged `send_input` path in the integration
//!   plan. This module has no `SendStream` or network handle — structurally cannot
//!   write to the wire (T-15-02).
//! - **CJK width miscount guarded** — `unicode-width::UnicodeWidthChar::width()`
//!   (not `width_cjk`) is used; ambiguous/combining/ZWJ → epoch reset (T-15-04).
//!   Wide char at terminal right edge → `become_tentative` (Pitfall 6).

use std::collections::VecDeque;

use nosh_proto::datagram::{CellStyle, CursorPos};
use unicode_width::UnicodeWidthChar;

use crate::screen::{Cell, ClientScreen, Overlay};

// ── RTT / bulk constants ──────────────────────────────────────────────────────

/// Show predictions when smoothed RTT exceeds this threshold (ms).
const SRTT_TRIGGER_HIGH_MS: u64 = 30;
/// Stop showing predictions when RTT drops below this (ms); hysteresis gate.
const SRTT_TRIGGER_LOW_MS: u64 = 20;
/// Start underlining unconfirmed predictions above this RTT (ms).
const FLAG_TRIGGER_HIGH_MS: u64 = 80;
/// Stop underlining below this RTT (ms).
const FLAG_TRIGGER_LOW_MS: u64 = 50;
/// Bulk input: >4 bytes in a single read batch → suppress prediction.
const BULK_SUPPRESS_THRESHOLD: usize = 4;

// ── PredictDisplayMode ────────────────────────────────────────────────────────

/// Controls when speculative echo predictions are displayed.
///
/// Mapped to the `--predict` CLI flag (Phase 15 integration plan).
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
#[value(rename_all = "lower")]
pub enum PredictDisplayMode {
    /// Always show predictions regardless of RTT (useful for testing).
    Always,
    /// Show predictions only when RTT is above the activation threshold (~30 ms).
    /// Default: invisible on loopback connections (D-15-02).
    Adaptive,
    /// Never show predictions.
    Never,
}

// ── Validity ──────────────────────────────────────────────────────────────────

/// Validity state of a pending prediction cell.
///
/// Direct translation of Mosh `terminaloverlay.h:56` `enum Validity`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Validity {
    /// Still waiting for server epoch confirmation.
    Pending,
    /// Server confirmed this exact cell content (non-trivial). Advances
    /// `confirmed_epoch` so future predictions in the epoch become visible.
    Correct,
    /// Server confirmed but trivially (blank → blank). Removed without
    /// advancing `confirmed_epoch`.
    CorrectNoCredit,
    /// Server state differs from prediction, or prediction has expired.
    IncorrectOrExpired,
    /// Prediction is inactive (not in use).
    Inactive,
}

// ── InputAction ───────────────────────────────────────────────────────────────

/// Classifier output for a single keystroke batch.
///
/// Each variant drives a different predictor action in `on_input`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputAction {
    /// Predict this printable char at cursor; advance cursor by `col_width`.
    PredictChar {
        ch: char,
        /// Column width: 1 (narrow) or 2 (CJK wide).
        col_width: u16,
    },
    /// Predict backspace: move predicted cursor left 1 (cursor-only, per Open Question 2).
    PredictBackspace,
    /// Predict cursor left 1 column (← arrow).
    PredictCursorLeft,
    /// Predict cursor right 1 column (→ arrow).
    PredictCursorRight,
    /// Predict cursor to start of line (Home / Ctrl-A → col=0).
    PredictLineStart,
    /// Predict cursor to end of confirmed row content (End / Ctrl-E).
    PredictLineEnd,
    /// Reset the prediction epoch; display nothing until server confirms.
    EpochReset,
    /// Begin bracketed paste — suppress all prediction.
    BracketedPasteStart,
    /// End bracketed paste — re-enable prediction (still tentative until server confirms).
    BracketedPasteEnd,
    /// Input suppressed: bulk batch > `BULK_SUPPRESS_THRESHOLD` bytes.
    BulkSuppressed,
}

// ── PendingPrediction ─────────────────────────────────────────────────────────

/// A single speculative prediction at a (row, col) position.
///
/// Translated from Mosh `ConditionalOverlayCell` in `terminaloverlay.h`.
#[derive(Debug, Clone)]
pub struct PendingPrediction {
    /// Screen row.
    pub row: u16,
    /// Screen column.
    pub col: u16,
    /// Predicted character.
    pub predicted_ch: char,
    /// Column width (1 or 2).
    pub col_width: u16,
    /// Server epoch at or above which this prediction is confirmed.
    /// Mapped from Mosh's `expiration_frame`.
    pub epoch_required: u64,
    /// Minimum confirmed_epoch for this prediction to be DISPLAYED.
    /// Predictions are tentative (hidden) when `tentative_until_epoch > confirmed_epoch`.
    /// Mapped from Mosh's `tentative_until_epoch`.
    pub tentative_until_epoch: u64,
}

// ── PredictionOverlay ─────────────────────────────────────────────────────────

/// Client-side speculative echo overlay.
///
/// Implements the Mosh `PredictionEngine` epoch/Validity state machine.
/// Added to the `ClientScreen` overlay stack so `compose_desired` picks up
/// speculative cells without any changes to the render path.
pub struct PredictionOverlay {
    /// Monotonic epoch: advances when a Correct (non-trivial) prediction is confirmed.
    /// Noecho suppression: if server never echoes, this stays at 0 and all predictions
    /// remain tentative (hidden) — structural, not an explicit flag.
    confirmed_epoch: u64,
    /// Current prediction epoch (increments on `become_tentative` / epoch reset).
    prediction_epoch: u64,
    /// All active predictions, in insertion order.
    pending: VecDeque<PendingPrediction>,
    /// Display mode from `--predict` flag.
    display_mode: PredictDisplayMode,
    /// Whether predictions are currently being displayed (RTT above show threshold).
    srtt_trigger: bool,
    /// Whether unconfirmed predictions should be underlined (RTT above flag threshold).
    flagging: bool,
    /// Whether bracketed paste mode is active (suppress all cell prediction).
    in_bracketed_paste: bool,
    /// Current predicted cursor position.
    predicted_cursor: CursorPos,
    /// True when the user has issued local cursor motion (←/→/Home/End/Ctrl-A/
    /// Ctrl-E) that diverges `predicted_cursor` from the confirmed cursor WITHOUT
    /// enqueuing a printable prediction (BUG-D symptom 3). When set, the predicted
    /// cursor is rendered (and is NOT snapped back to the confirmed cursor by
    /// `sync_cursor_from_confirmed`) so left/right motion is visibly speculated.
    /// Cleared on epoch reset and on confirming sync once pending is empty.
    cursor_motion_pending: bool,
    /// Terminal column count.
    term_cols: u16,
    /// Terminal row count.
    term_rows: u16,
    /// The confirmed cursor column at the start of the most recent fresh input epoch.
    ///
    /// Set by `sync_cursor_from_confirmed` once after each `reset()` call (i.e. after
    /// Enter / Ctrl-C / epoch-reset), under the CR-01 guard (pending empty AND no
    /// outstanding cursor motion). The `needs_epoch_start_sync` flag gates this so that
    /// mid-epoch confirmations (pending empties because typed chars were confirmed by the
    /// server) do NOT overwrite the floor — only the first sync after a reset captures
    /// the true "where typing begins" boundary. Used as a clamp floor in `PredictBackspace`
    /// and `PredictCursorLeft` so the predicted caret cannot retreat past the prompt start —
    /// fixing BUG-E (D-01). Initialized to 0.
    epoch_start_col: u16,
    /// Set to `true` by `reset()` (and `reset_with_cursor()`); cleared once
    /// `sync_cursor_from_confirmed` captures `epoch_start_col` for the new epoch.
    /// Prevents mid-epoch datagram confirmations from updating the clamp floor (D-01).
    needs_epoch_start_sync: bool,
}

impl PredictionOverlay {
    /// Create a new overlay with the given display mode and terminal dimensions.
    pub fn new(mode: PredictDisplayMode, cols: u16, rows: u16) -> Self {
        PredictionOverlay {
            confirmed_epoch: 0,
            prediction_epoch: 0,
            pending: VecDeque::new(),
            display_mode: mode,
            srtt_trigger: false,
            flagging: false,
            in_bracketed_paste: false,
            predicted_cursor: CursorPos { row: 0, col: 0 },
            cursor_motion_pending: false,
            term_cols: cols,
            term_rows: rows,
            epoch_start_col: 0,
            needs_epoch_start_sync: false,
        }
    }

    // ── Public accessors ──────────────────────────────────────────────────────

    /// Return the current confirmed epoch (test + assertion surface).
    pub fn confirmed_epoch(&self) -> u64 {
        self.confirmed_epoch
    }

    /// Return the current prediction epoch (test + assertion surface).
    pub fn prediction_epoch(&self) -> u64 {
        self.prediction_epoch
    }

    /// Return the number of pending predictions (test + assertion surface).
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// Sync `predicted_cursor` from the confirmed cursor position (CR-01 fix).
    ///
    /// Must be called in the datagram arm after `cull()` so that new predictions
    /// land on the correct row (the confirmed cursor row), not the hard-zeroed row 0.
    ///
    /// Only syncs when `pending` is empty AND there is no outstanding local cursor
    /// motion (`cursor_motion_pending`). Snapping to the confirmed cursor while the
    /// user has speculatively moved left/right (←/→/Home/End) but the server has not
    /// yet confirmed the motion would clobber the predicted caret on every datagram —
    /// the "arrow keys don't move / overwrite mode" symptom (BUG-D symptom 3). The
    /// confirming datagram for the motion arrives with the cursor already at the
    /// predicted column, so once that lands the next sync (with motion cleared) is a
    /// no-op anyway.
    pub fn sync_cursor_from_confirmed(&mut self, confirmed: CursorPos) {
        if self.pending.is_empty() && !self.cursor_motion_pending {
            self.predicted_cursor = confirmed;
            // D-01 (BUG-E): record the column where typing begins after a fresh epoch
            // reset (Enter / Ctrl-C / epoch-reset). The `needs_epoch_start_sync` flag
            // is set by `reset()` / `reset_with_cursor()` and cleared here after the
            // first sync so that mid-epoch confirmations (server echoing back typed
            // chars that empty `pending`) do NOT overwrite the floor — only the first
            // datagram after a reset captures the true prompt-boundary column.
            if self.needs_epoch_start_sync {
                self.epoch_start_col = confirmed.col;
                self.needs_epoch_start_sync = false;
            }
        }
    }

    /// Update the terminal dimensions used for right-edge and row-bounds checks (WR-01 fix).
    ///
    /// Must be called (together with `reset()`) whenever the terminal is resized so that
    /// the wide-char right-edge guard (`col + col_width > term_cols`) uses the current size.
    pub fn set_size(&mut self, cols: u16, rows: u16) {
        self.term_cols = cols;
        self.term_rows = rows;
    }

    /// Return the predicted cursor position, if predictions are currently displayed
    /// and at least one non-tentative active prediction exists.
    ///
    /// Returns `None` when predictions are not displayed or all pending predictions
    /// are tentative. Consumed by the integration plan's render path (Pitfall 3).
    pub fn predicted_cursor(&self) -> Option<CursorPos> {
        if !self.should_display() {
            return None;
        }
        // The predicted caret is emitted when EITHER:
        //   - at least one non-tentative pending char prediction exists (printable
        //     echo advanced the caret), OR
        //   - the user issued local cursor motion (←/→/Home/End/Ctrl-A/Ctrl-E) that
        //     has not yet been confirmed (BUG-D symptom 3 — pure motion enqueues no
        //     pending cell, so without this branch the caret never moved).
        // Both are gated by the tentative/epoch machinery elsewhere; motion is only
        // pending within a live (non-reset) epoch.
        let has_visible = self.pending.iter().any(|p| !self.is_tentative(p));
        if has_visible || self.cursor_motion_pending {
            Some(self.predicted_cursor)
        } else {
            None
        }
    }

    // ── Input handling ────────────────────────────────────────────────────────

    /// Process a keystroke batch and update the overlay state.
    ///
    /// Must be called AFTER the escape machine, BEFORE `send_input` (per PATTERNS.md).
    /// The keystroke is still forwarded to the server unchanged; this only updates
    /// local display state.
    pub fn on_input(&mut self, bytes: &[u8], screen: &ClientScreen) {
        let action = classify_input(bytes);
        match action {
            InputAction::PredictChar { ch, col_width } => {
                if self.in_bracketed_paste {
                    // Suppress all cell predictions during bracketed paste.
                    return;
                }
                let col = self.predicted_cursor.col;
                let row = self.predicted_cursor.row;
                // Pitfall 6: wide char at right edge → become_tentative instead of predicting.
                if col.saturating_add(col_width) > self.term_cols {
                    self.become_tentative();
                    return;
                }
                // BUG-D: noecho suppression is provided by the tentative-epoch machinery
                // (a prediction with tentative_until_epoch > confirmed_epoch is hidden by
                // is_tentative()/cell_at()). After the Enter that precedes any password
                // prompt (read -s / ssh / sudo), the epoch is reset → become_tentative →
                // predictions are tentative and hidden. The previous `awaiting_first_cull`
                // flag ADDITIONALLY hid the first non-tentative prediction of every fresh
                // epoch until a confirming datagram arrived — which suppressed the headline
                // predictive-echo for the common echoing-shell case (space/char not shown,
                // caret not advancing until the next keystroke). That over-suppression is
                // removed; the tentative mechanism remains the structural noecho guard.
                let epoch_required = screen.last_applied_epoch() + 1;
                let tentative_until_epoch = self.prediction_epoch;
                self.pending.push_back(PendingPrediction {
                    row,
                    col,
                    predicted_ch: ch,
                    col_width,
                    epoch_required,
                    tentative_until_epoch,
                });
                self.predicted_cursor.col = col + col_width;
                // A printable prediction is now the source of the visible caret; any
                // prior pure-motion divergence is subsumed by this pending cell.
                self.cursor_motion_pending = false;
            }
            InputAction::PredictBackspace => {
                // CR-02 fix: remove the prediction at the vacated column so it no
                // longer shows in the overlay. Without this, the deleted char remains
                // visible until the next cull() — worse than no prediction (D-15-01).
                // D-01 (BUG-E): clamp at epoch_start_col (the prompt boundary) instead
                // of col 0 so backspace cannot walk the predicted caret past the prompt.
                if self.predicted_cursor.col > self.epoch_start_col {
                    let vacated_col = self.predicted_cursor.col - 1;
                    let row = self.predicted_cursor.row;
                    self.pending.retain(|p| !(p.row == row && p.col == vacated_col));
                    self.predicted_cursor.col = vacated_col;
                    // Backspace is a leftward cursor motion: keep the predicted caret
                    // displayed even if pending is now empty (BUG-D symptom 3 sibling).
                    self.cursor_motion_pending = true;
                }
            }
            InputAction::PredictCursorLeft => {
                // D-01 (BUG-E): clamp at epoch_start_col (the prompt boundary) instead
                // of col 0 so cursor-left cannot walk the predicted caret past the prompt.
                if self.predicted_cursor.col > self.epoch_start_col {
                    self.predicted_cursor.col -= 1;
                }
                // Pure motion enqueues no pending cell; mark so predicted_cursor() emits
                // the moved caret and sync_cursor_from_confirmed does not snap it back.
                self.cursor_motion_pending = true;
            }
            InputAction::PredictCursorRight => {
                if self.predicted_cursor.col + 1 < self.term_cols {
                    self.predicted_cursor.col += 1;
                }
                self.cursor_motion_pending = true;
            }
            InputAction::PredictLineStart => {
                self.predicted_cursor.col = 0;
                self.cursor_motion_pending = true;
            }
            InputAction::PredictLineEnd => {
                // Scan confirmed row right-to-left for last non-blank cell (Open Question 3).
                let row = self.predicted_cursor.row;
                let end_col = self.find_line_end(row, screen);
                self.predicted_cursor.col = end_col;
                self.cursor_motion_pending = true;
            }
            InputAction::EpochReset | InputAction::BulkSuppressed => {
                // Reset clears all pending predictions AND increments prediction_epoch,
                // ensuring no stale speculative state remains visible after a control
                // sequence (Ctrl-C, ESC, Tab, Enter, cursor-addressing, bulk input).
                self.reset();
            }
            InputAction::BracketedPasteStart => {
                self.in_bracketed_paste = true;
                // Reset ensures no stale predictions from before paste start are visible.
                self.reset();
            }
            InputAction::BracketedPasteEnd => {
                self.in_bracketed_paste = false;
                // Still tentative until server confirms (become_tentative already called at start).
            }
        }
    }

    /// Confirm or cull predictions against the latest confirmed grid state.
    ///
    /// Call this after `screen.apply(diff)` in the datagram arm.
    ///
    /// # Arguments
    ///
    /// - `screen`: the updated confirmed grid.
    /// - `new_epoch`: the epoch of the diff just applied.
    /// - `rtt_ms`: current smoothed RTT from `conn.rtt().as_millis()`.
    pub fn cull(&mut self, screen: &ClientScreen, new_epoch: u64, rtt_ms: u64) {
        self.update_rtt_thresholds(rtt_ms);

        // Collect indices of predictions to remove (confirmed or no-credit).
        // On a non-tentative mismatch: full reset and early return (Pitfall 1).
        // WR-02 fix: epochs_to_kill collected inline (single pass) so the second loop
        // (with O(n²) to_remove.contains(&i)) is eliminated.
        let mut to_remove: Vec<usize> = Vec::new();
        let mut epochs_to_kill: std::collections::HashSet<u64> = std::collections::HashSet::new();

        for (i, pred) in self.pending.iter().enumerate() {
            // Pitfall 4: >= check, NOT ==. Tolerates dropped datagrams.
            if pred.epoch_required <= new_epoch {
                let confirmed_ch = screen.confirmed_cell(pred.row, pred.col).ch;
                let validity = Self::check_validity(confirmed_ch, pred.predicted_ch);
                match validity {
                    Validity::Correct => {
                        // Advance confirmed_epoch if this prediction's epoch is higher.
                        if pred.tentative_until_epoch > self.confirmed_epoch {
                            self.confirmed_epoch = pred.tentative_until_epoch;
                        }
                        to_remove.push(i);
                    }
                    Validity::CorrectNoCredit => {
                        // Trivially correct (blank→blank) — remove without advancing epoch.
                        to_remove.push(i);
                    }
                    Validity::IncorrectOrExpired => {
                        if self.is_tentative(pred) {
                            // Tentative mismatch: prune only this epoch's predictions.
                            // Collect epoch inline (WR-02 fix — no second pass needed).
                            epochs_to_kill.insert(pred.tentative_until_epoch);
                            to_remove.push(i);
                        } else {
                            // Non-tentative mismatch: full reset (Pitfall 1).
                            self.reset();
                            return;
                        }
                    }
                    Validity::Pending | Validity::Inactive => {
                        // Still pending — leave in place.
                    }
                }
            }
        }

        // Kill tentative-mismatch epochs (prunes related predictions).
        for epoch in epochs_to_kill {
            self.kill_epoch(epoch);
        }

        // Remove confirmed/credited predictions in reverse index order.
        to_remove.sort_unstable();
        to_remove.dedup();
        for &i in to_remove.iter().rev() {
            // Guard: index may have shifted after kill_epoch removed some entries.
            if i < self.pending.len() {
                self.pending.remove(i);
            }
        }
    }

    // ── Display gate ──────────────────────────────────────────────────────────

    /// Whether predictions should be displayed given current display mode and RTT.
    pub fn should_display(&self) -> bool {
        match self.display_mode {
            PredictDisplayMode::Always => true,
            PredictDisplayMode::Never => false,
            PredictDisplayMode::Adaptive => self.srtt_trigger,
        }
    }

    /// Whether a prediction is tentative (hidden from display).
    ///
    /// Translated from Mosh `terminaloverlay.h:68`:
    /// `bool tentative(uint64_t confirmed_epoch) const { return tentative_until_epoch > confirmed_epoch; }`
    pub fn is_tentative(&self, pred: &PendingPrediction) -> bool {
        pred.tentative_until_epoch > self.confirmed_epoch
    }

    // ── State machine ─────────────────────────────────────────────────────────

    /// Increment prediction epoch.
    ///
    /// All new predictions after this point get `tentative_until_epoch = prediction_epoch`.
    /// Since `confirmed_epoch` has not caught up, they are hidden until the server
    /// confirms one prediction from the new epoch.
    ///
    /// Translated from Mosh `terminaloverlay.cc PredictionEngine::become_tentative()`.
    pub fn become_tentative(&mut self) {
        self.prediction_epoch += 1;
    }

    /// Clear all pending predictions and increment prediction epoch.
    ///
    /// Translated from Mosh `PredictionEngine::reset()`.
    pub fn reset(&mut self) {
        self.pending.clear();
        self.become_tentative();
        // A reset (Enter / ESC / Tab / Ctrl-C / cursor-addressing / bulk) clears any
        // outstanding local cursor motion: the new epoch is tentative (hidden) until
        // the server confirms, so no speculative caret should be shown (BUG-D).
        self.cursor_motion_pending = false;
        // D-01 (BUG-E): signal that the next sync_cursor_from_confirmed call should
        // capture the new epoch-start column (the prompt boundary). Cleared once
        // captured so mid-epoch datagram confirmations don't reset the floor.
        self.needs_epoch_start_sync = true;
    }

    /// Remove all predictions with the given `tentative_until_epoch`.
    ///
    /// Used for tentative-mismatch cleanup (pruning a specific epoch's predictions).
    /// Translated from Mosh `PredictionEngine::kill_epoch()`.
    pub fn kill_epoch(&mut self, epoch: u64) {
        self.pending.retain(|p| p.tentative_until_epoch != epoch);
    }

    // ── RTT hysteresis ────────────────────────────────────────────────────────

    /// Update display trigger and underline flag based on current smoothed RTT.
    ///
    /// Hysteresis prevents flicker on link jitter:
    /// - `srtt_trigger`: activates above HIGH, deactivates below LOW **only when
    ///   no predictions are being shown** (prevents flicker mid-display).
    /// - `flagging`: activates above HIGH, deactivates below LOW (no prediction guard).
    ///
    /// Translated from Mosh `terminaloverlay.cc cull()` hysteresis block.
    pub fn update_rtt_thresholds(&mut self, rtt_ms: u64) {
        if rtt_ms > SRTT_TRIGGER_HIGH_MS {
            self.srtt_trigger = true;
        } else if self.srtt_trigger && rtt_ms <= SRTT_TRIGGER_LOW_MS && self.pending.is_empty() {
            self.srtt_trigger = false;
        }

        if rtt_ms > FLAG_TRIGGER_HIGH_MS {
            self.flagging = true;
        } else if rtt_ms <= FLAG_TRIGGER_LOW_MS {
            self.flagging = false;
        }
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    /// Determine validity of a confirmed cell vs. predicted char.
    fn check_validity(confirmed_ch: char, predicted_ch: char) -> Validity {
        if confirmed_ch == predicted_ch {
            // Trivially correct: blank→blank (no credit toward epoch advance).
            if confirmed_ch == ' ' {
                Validity::CorrectNoCredit
            } else {
                Validity::Correct
            }
        } else {
            Validity::IncorrectOrExpired
        }
    }

    /// Scan confirmed row from right-to-left for last non-blank cell.
    /// Returns col for End/Ctrl-E prediction (Open Question 3 recommendation).
    fn find_line_end(&self, row: u16, screen: &ClientScreen) -> u16 {
        for col in (0..self.term_cols).rev() {
            let cell = screen.confirmed_cell(row, col);
            if cell.ch != ' ' {
                return col + 1;
            }
        }
        0
    }
}

// ── Overlay trait ─────────────────────────────────────────────────────────────

impl Overlay for PredictionOverlay {
    /// Return a predicted cell at `(row, col)` if visible; `None` to pass through.
    ///
    /// Returns `None` when:
    /// - Display is disabled (`should_display()` is false), or
    /// - No non-tentative prediction exists at `(row, col)`.
    ///
    /// Noecho suppression (PREDICT-04) is provided structurally by the tentative
    /// epoch mechanism: `is_tentative(pred)` returns true whenever
    /// `tentative_until_epoch > confirmed_epoch`, which holds for every prediction
    /// made after an epoch reset (e.g. the Enter that precedes a `read -s` / `ssh` /
    /// `sudo` password prompt) until the server confirms an echoed character. While
    /// the server suppresses echo, `confirmed_epoch` never advances, so all such
    /// predictions remain tentative (hidden) — proven by the `noecho_suppression`
    /// unit test and the live `noecho_read_dash_s_zero_predicted_chars` integration
    /// test.
    ///
    /// Returns `Some(Cell)` with `UNDERLINE` style when `flagging` is true (RTT
    /// above FLAG_TRIGGER_HIGH_MS), plain style otherwise.
    fn cell_at(&self, row: u16, col: u16) -> Option<Cell> {
        if !self.should_display() {
            return None;
        }
        for pred in &self.pending {
            if pred.row == row && pred.col == col && !self.is_tentative(pred) {
                let style_bits = if self.flagging {
                    CellStyle(CellStyle::UNDERLINE)
                } else {
                    CellStyle(CellStyle::NONE)
                };
                return Some(Cell {
                    ch: pred.predicted_ch,
                    style: style_bits,
                    fg: None,
                    bg: None,
                });
            }
        }
        None
    }
}

// ── Input classifier ──────────────────────────────────────────────────────────

/// Classify a keystroke batch into a predictor action.
///
/// Bulk check is performed AFTER matching known escape sequences so that
/// bracketed-paste markers (`\x1b[200~` = 6 bytes) are correctly identified
/// before the `> BULK_SUPPRESS_THRESHOLD` guard fires.
///
/// Translated from Mosh `terminaloverlay.cc PredictionEngine::new_user_byte()`.
pub fn classify_input(bytes: &[u8]) -> InputAction {
    // Bracketed paste markers must be matched BEFORE the bulk guard because
    // they are 6 bytes (> BULK_SUPPRESS_THRESHOLD = 4).
    match bytes {
        b"\x1b[200~" => return InputAction::BracketedPasteStart,
        b"\x1b[201~" => return InputAction::BracketedPasteEnd,
        _ => {}
    }

    // Bulk suppression: D-15-01b. Any input > 4 bytes that is not a recognised
    // paste/escape sequence is suppressed (paste, bracketed-paste body, etc.).
    if bytes.len() > BULK_SUPPRESS_THRESHOLD {
        return InputAction::BulkSuppressed;
    }

    match bytes {
        // Backspace (DEL and BS).
        [0x7f] | [0x08] => InputAction::PredictBackspace,

        // Ctrl-A → line start; Ctrl-E → line end.
        [0x01] => InputAction::PredictLineStart,
        [0x05] => InputAction::PredictLineEnd,

        // Enter / newline → epoch reset.
        [b'\r'] | [b'\n'] => InputAction::EpochReset,

        // Tab → epoch reset (tab-stop ambiguity, D-15-01a).
        [b'\t'] => InputAction::EpochReset,

        // Cursor right: CSI C and application-mode SS3 C.
        [0x1b, b'[', b'C'] | [0x1b, b'O', b'C'] => InputAction::PredictCursorRight,

        // Cursor left: CSI D and application-mode SS3 D.
        [0x1b, b'[', b'D'] | [0x1b, b'O', b'D'] => InputAction::PredictCursorLeft,

        // Home: CSI H, CSI 1~, SS3 H.
        [0x1b, b'[', b'H'] | [0x1b, b'[', b'1', b'~'] | [0x1b, b'O', b'H'] => {
            InputAction::PredictLineStart
        }

        // End: CSI F, CSI 4~, SS3 F.
        [0x1b, b'[', b'F'] | [0x1b, b'[', b'4', b'~'] | [0x1b, b'O', b'F'] => {
            InputAction::PredictLineEnd
        }

        // Any other ESC sequence → epoch reset.
        [0x1b, ..] => InputAction::EpochReset,

        // Any other control char (< 0x20, not handled above) → epoch reset.
        [b] if *b < 0x20 => InputAction::EpochReset,

        // Single printable char or multi-byte UTF-8 scalar.
        _ => classify_printable(bytes),
    }
}

/// Classify a printable byte sequence.
///
/// Returns `PredictChar` only for clean width-1 and width-2 characters (D-15-03).
/// Ambiguous-width, combining marks, ZWJ, control chars → `EpochReset`.
///
/// Uses `UnicodeWidthChar::width()` (NOT `width_cjk()` — D-15-03).
pub fn classify_printable(bytes: &[u8]) -> InputAction {
    if let Ok(s) = std::str::from_utf8(bytes) {
        let mut chars = s.chars();
        if let (Some(ch), None) = (chars.next(), chars.next()) {
            // width() returns:
            //   None      → control character
            //   Some(0)   → combining / ZWJ / zero-width
            //   Some(1)   → narrow
            //   Some(2)   → CJK wide
            match UnicodeWidthChar::width(ch) {
                Some(1) => return InputAction::PredictChar { ch, col_width: 1 },
                Some(2) => return InputAction::PredictChar { ch, col_width: 2 },
                // None, Some(0), or >2 → epoch reset (D-15-03 conservative policy).
                _ => {}
            }
        }
    }
    InputAction::EpochReset
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use nosh_proto::datagram::{CellStyle, CursorPos, DiffRun, StateDiff};

    use super::*;

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn make_screen(cols: u16, rows: u16) -> ClientScreen {
        ClientScreen::new(cols, rows)
    }

    fn make_diff_with_char(epoch: u64, row: u16, col: u16, ch: char) -> StateDiff {
        StateDiff {
            epoch,
            cols: 80,
            rows: 24,
            cursor: CursorPos { row, col },
            runs: vec![DiffRun {
                row,
                start_col: col,
                chars: ch.to_string(),
                style: CellStyle(CellStyle::NONE),
                fg: None,
                bg: None,
            }],
        }
    }

    fn make_diff_empty(epoch: u64) -> StateDiff {
        StateDiff {
            epoch,
            cols: 80,
            rows: 24,
            cursor: CursorPos { row: 0, col: 0 },
            runs: vec![],
        }
    }

    // ── Classifier tests ──────────────────────────────────────────────────────

    #[test]
    fn classify_printable_ascii_a() {
        let action = classify_input(b"a");
        assert_eq!(
            action,
            InputAction::PredictChar { ch: 'a', col_width: 1 },
            "classify_input(b\"a\") must return PredictChar {{ch: 'a', col_width: 1}}"
        );
    }

    #[test]
    fn classify_cjk_3byte_not_bulk() {
        // '你' is 3 bytes UTF-8 — must NOT be BulkSuppressed (threshold is >4 bytes).
        let ni_hao = '你';
        let bytes = {
            let mut b = [0u8; 4];
            let s = ni_hao.encode_utf8(&mut b);
            s.as_bytes().to_vec()
        };
        assert_eq!(bytes.len(), 3, "'你' must be 3 UTF-8 bytes");
        let action = classify_input(&bytes);
        assert_eq!(
            action,
            InputAction::PredictChar { ch: '你', col_width: 2 },
            "CJK char '你' (3 UTF-8 bytes) must yield PredictChar{{col_width: 2}}, not BulkSuppressed"
        );
    }

    #[test]
    fn classify_backspace_del() {
        assert_eq!(
            classify_input(&[0x7f]),
            InputAction::PredictBackspace,
            "DEL (0x7f) must yield PredictBackspace"
        );
    }

    #[test]
    fn classify_backspace_bs() {
        assert_eq!(
            classify_input(&[0x08]),
            InputAction::PredictBackspace,
            "BS (0x08) must yield PredictBackspace"
        );
    }

    #[test]
    fn classify_ctrl_a_line_start() {
        assert_eq!(
            classify_input(&[0x01]),
            InputAction::PredictLineStart,
            "Ctrl-A (0x01) must yield PredictLineStart"
        );
    }

    #[test]
    fn classify_ctrl_e_line_end() {
        assert_eq!(
            classify_input(&[0x05]),
            InputAction::PredictLineEnd,
            "Ctrl-E (0x05) must yield PredictLineEnd"
        );
    }

    #[test]
    fn classify_csi_cursor_right() {
        assert_eq!(
            classify_input(b"\x1b[C"),
            InputAction::PredictCursorRight,
            "CSI C must yield PredictCursorRight"
        );
    }

    #[test]
    fn classify_appmode_cursor_right() {
        assert_eq!(
            classify_input(b"\x1bOC"),
            InputAction::PredictCursorRight,
            "SS3 C (app-mode right) must yield PredictCursorRight"
        );
    }

    #[test]
    fn classify_csi_cursor_left() {
        assert_eq!(
            classify_input(b"\x1b[D"),
            InputAction::PredictCursorLeft,
            "CSI D must yield PredictCursorLeft"
        );
    }

    #[test]
    fn classify_appmode_cursor_left() {
        assert_eq!(
            classify_input(b"\x1bOD"),
            InputAction::PredictCursorLeft,
            "SS3 D (app-mode left) must yield PredictCursorLeft"
        );
    }

    #[test]
    fn classify_csi_home() {
        assert_eq!(
            classify_input(b"\x1b[H"),
            InputAction::PredictLineStart,
            "CSI H must yield PredictLineStart"
        );
    }

    #[test]
    fn classify_csi_1tilde_home() {
        assert_eq!(
            classify_input(b"\x1b[1~"),
            InputAction::PredictLineStart,
            "CSI 1~ must yield PredictLineStart"
        );
    }

    #[test]
    fn classify_appmode_home() {
        assert_eq!(
            classify_input(b"\x1bOH"),
            InputAction::PredictLineStart,
            "SS3 H (app-mode Home) must yield PredictLineStart"
        );
    }

    #[test]
    fn classify_csi_end() {
        assert_eq!(
            classify_input(b"\x1b[F"),
            InputAction::PredictLineEnd,
            "CSI F must yield PredictLineEnd"
        );
    }

    #[test]
    fn classify_csi_4tilde_end() {
        assert_eq!(
            classify_input(b"\x1b[4~"),
            InputAction::PredictLineEnd,
            "CSI 4~ must yield PredictLineEnd"
        );
    }

    #[test]
    fn classify_appmode_end() {
        assert_eq!(
            classify_input(b"\x1bOF"),
            InputAction::PredictLineEnd,
            "SS3 F (app-mode End) must yield PredictLineEnd"
        );
    }

    #[test]
    fn classify_bracketed_paste_start() {
        // \x1b[200~ is 6 bytes — must be recognised BEFORE the bulk guard.
        assert_eq!(
            classify_input(b"\x1b[200~"),
            InputAction::BracketedPasteStart,
            "paste-start marker \\x1b[200~ must be BracketedPasteStart, not BulkSuppressed"
        );
    }

    #[test]
    fn classify_bracketed_paste_end() {
        assert_eq!(
            classify_input(b"\x1b[201~"),
            InputAction::BracketedPasteEnd,
            "paste-end marker \\x1b[201~ must be BracketedPasteEnd, not BulkSuppressed"
        );
    }

    #[test]
    fn classify_enter_epoch_reset() {
        assert_eq!(
            classify_input(b"\r"),
            InputAction::EpochReset,
            "CR must yield EpochReset"
        );
        assert_eq!(
            classify_input(b"\n"),
            InputAction::EpochReset,
            "LF must yield EpochReset"
        );
    }

    #[test]
    fn classify_tab_epoch_reset() {
        assert_eq!(
            classify_input(b"\t"),
            InputAction::EpochReset,
            "Tab must yield EpochReset"
        );
    }

    #[test]
    fn classify_esc_epoch_reset() {
        assert_eq!(
            classify_input(b"\x1b"),
            InputAction::EpochReset,
            "bare ESC must yield EpochReset"
        );
    }

    #[test]
    fn classify_arbitrary_csi_epoch_reset() {
        // e.g. cursor up — not predicted
        assert_eq!(
            classify_input(b"\x1b[A"),
            InputAction::EpochReset,
            "CSI A (cursor up) must yield EpochReset"
        );
    }

    #[test]
    fn classify_control_char_epoch_reset() {
        // Ctrl-C
        assert_eq!(
            classify_input(&[0x03]),
            InputAction::EpochReset,
            "Ctrl-C (0x03) must yield EpochReset"
        );
    }

    #[test]
    fn classify_bulk_suppressed() {
        // 5 bytes that are not a recognised escape sequence
        assert_eq!(
            classify_input(b"hello"),
            InputAction::BulkSuppressed,
            ">4 non-escape bytes must yield BulkSuppressed"
        );
    }

    #[test]
    fn classify_combining_mark_epoch_reset() {
        // U+0301 COMBINING ACUTE ACCENT — width Some(0) → epoch reset.
        let combining = '\u{0301}';
        let mut buf = [0u8; 4];
        let s = combining.encode_utf8(&mut buf);
        let action = classify_printable(s.as_bytes());
        assert_eq!(
            action,
            InputAction::EpochReset,
            "combining mark (width Some(0)) must yield EpochReset"
        );
    }

    #[test]
    fn classify_ambiguous_width_epoch_reset() {
        // U+FE0F VARIATION SELECTOR-16 (emoji variant) — width Some(0) → epoch reset.
        // This represents "ambiguous" sequence-modifying characters that must not be predicted.
        let vs16 = '\u{FE0F}';
        let mut buf = [0u8; 4];
        let s = vs16.encode_utf8(&mut buf);
        let action = classify_printable(s.as_bytes());
        assert_eq!(
            action,
            InputAction::EpochReset,
            "variation selector / emoji modifier (width Some(0)) must yield EpochReset"
        );
    }

    #[test]
    fn classify_no_width_cjk_used() {
        // Verify our classify_printable uses width() not width_cjk() by checking
        // that CJK chars are handled correctly with the non-CJK-context width function.
        // The important policy: we never call width_cjk.
        // This test documents that classify_printable predicts U+4E2D (中, CJK) correctly.
        let zhong = '中';
        let mut buf = [0u8; 4];
        let s = zhong.encode_utf8(&mut buf);
        let action = classify_printable(s.as_bytes());
        assert_eq!(
            action,
            InputAction::PredictChar { ch: '中', col_width: 2 },
            "CJK char '中' must yield PredictChar{{col_width: 2}}"
        );
    }

    // ── Epoch state machine tests ─────────────────────────────────────────────

    #[test]
    fn on_input_enqueues_prediction_and_advances_cursor() {
        let screen = make_screen(80, 24);
        let mut overlay = PredictionOverlay::new(PredictDisplayMode::Always, 80, 24);
        assert_eq!(overlay.predicted_cursor.col, 0, "initial cursor col must be 0");

        overlay.on_input(b"a", &screen);

        assert_eq!(
            overlay.pending.len(),
            1,
            "typing 'a' must enqueue 1 pending prediction"
        );
        assert_eq!(
            overlay.predicted_cursor.col,
            1,
            "predicted cursor must advance to col 1 after typing 'a'"
        );
        let pred = &overlay.pending[0];
        assert_eq!(pred.predicted_ch, 'a', "predicted char must be 'a'");
        assert_eq!(pred.col, 0, "prediction must be at col 0");
    }

    #[test]
    fn on_input_esc_becomes_tentative() {
        let screen = make_screen(80, 24);
        let mut overlay = PredictionOverlay::new(PredictDisplayMode::Always, 80, 24);
        let initial_epoch = overlay.prediction_epoch();
        overlay.on_input(b"\x1b", &screen);
        assert!(
            overlay.prediction_epoch() > initial_epoch,
            "ESC must increment prediction_epoch (become_tentative)"
        );
    }

    #[test]
    fn on_input_tab_becomes_tentative() {
        let screen = make_screen(80, 24);
        let mut overlay = PredictionOverlay::new(PredictDisplayMode::Always, 80, 24);
        let initial_epoch = overlay.prediction_epoch();
        overlay.on_input(b"\t", &screen);
        assert!(
            overlay.prediction_epoch() > initial_epoch,
            "Tab must increment prediction_epoch (become_tentative)"
        );
    }

    #[test]
    fn on_input_enter_becomes_tentative() {
        let screen = make_screen(80, 24);
        let mut overlay = PredictionOverlay::new(PredictDisplayMode::Always, 80, 24);
        let initial_epoch = overlay.prediction_epoch();
        overlay.on_input(b"\r", &screen);
        assert!(
            overlay.prediction_epoch() > initial_epoch,
            "Enter must increment prediction_epoch (become_tentative)"
        );
    }

    #[test]
    fn fresh_prediction_is_tentative_and_cell_at_returns_none() {
        // PREDICT-03: first char of a new epoch is tentative → cell_at returns None.
        let screen = make_screen(80, 24);
        // Simulate become_tentative to put us in a new epoch before typing.
        let mut overlay2 = PredictionOverlay::new(PredictDisplayMode::Always, 80, 24);
        overlay2.become_tentative(); // prediction_epoch = 1
        overlay2.on_input(b"a", &screen);
        // prediction's tentative_until_epoch = 1 > confirmed_epoch (0) → tentative.
        assert!(
            overlay2.cell_at(0, 0).is_none(),
            "prediction with tentative_until_epoch > confirmed_epoch must return None"
        );
    }

    #[test]
    fn cull_correct_prediction_advances_confirmed_epoch() {
        let mut screen = make_screen(80, 24);
        let mut overlay = PredictionOverlay::new(PredictDisplayMode::Always, 80, 24);

        // Type 'a' — enqueues prediction at (0,0) with epoch_required = 1.
        overlay.on_input(b"a", &screen);

        // Server confirms 'a' at (0,0) with epoch 1.
        let diff = make_diff_with_char(1, 0, 0, 'a');
        screen.apply(&diff);
        overlay.cull(&screen, 1, 50); // rtt_ms = 50 → flagging

        assert_eq!(
            overlay.confirmed_epoch(),
            0, // tentative_until_epoch was 0 (initial prediction_epoch), so max(0, 0) = 0
            "confirmed_epoch stays 0 when prediction's tentative_until_epoch is also 0"
        );
        assert_eq!(
            overlay.pending.len(),
            0,
            "confirmed prediction must be removed from pending"
        );
    }

    #[test]
    fn cull_correct_after_become_tentative_advances_epoch() {
        let mut screen = make_screen(80, 24);
        let mut overlay = PredictionOverlay::new(PredictDisplayMode::Always, 80, 24);

        // Increment prediction_epoch so new predictions have tentative_until_epoch = 1.
        overlay.become_tentative(); // prediction_epoch = 1
        overlay.on_input(b"a", &screen);

        let pred = &overlay.pending[0];
        assert_eq!(
            pred.tentative_until_epoch,
            1,
            "prediction tentative_until_epoch must be 1 after become_tentative"
        );

        // Server confirms 'a'.
        let diff = make_diff_with_char(1, 0, 0, 'a');
        screen.apply(&diff);
        overlay.cull(&screen, 1, 50);

        assert_eq!(
            overlay.confirmed_epoch(),
            1,
            "confirmed_epoch must advance to 1 after Correct confirmation"
        );
        assert_eq!(
            overlay.pending.len(),
            0,
            "prediction must be removed after confirmation"
        );
    }

    #[test]
    fn cull_mismatch_non_tentative_full_reset() {
        let mut screen = make_screen(80, 24);
        let mut overlay = PredictionOverlay::new(PredictDisplayMode::Always, 80, 24);

        // Type 'a' — enqueues non-tentative prediction (confirmed_epoch == tentative_until_epoch == 0).
        overlay.on_input(b"a", &screen);
        overlay.on_input(b"b", &screen);
        assert_eq!(overlay.pending.len(), 2, "two predictions enqueued");

        // Server sends 'x' at (0,0) — mismatch on non-tentative prediction.
        let diff = make_diff_with_char(1, 0, 0, 'x');
        screen.apply(&diff);
        let initial_prediction_epoch = overlay.prediction_epoch();
        overlay.cull(&screen, 1, 5); // rtt_ms = 5 (below triggers)

        assert_eq!(
            overlay.pending.len(),
            0,
            "non-tentative mismatch must clear all pending predictions (full reset, Pitfall 1)"
        );
        assert!(
            overlay.prediction_epoch() > initial_prediction_epoch,
            "full reset must increment prediction_epoch"
        );
    }

    #[test]
    fn cull_tolerates_dropped_datagrams() {
        // PREDICT: >= check — epoch N+2 confirms predictions requiring N+1.
        let mut screen = make_screen(80, 24);
        let mut overlay = PredictionOverlay::new(PredictDisplayMode::Always, 80, 24);

        // Increment prediction_epoch so predictions are visible after confirmation.
        overlay.become_tentative(); // prediction_epoch = 1

        // Type 'a' at cursor (0,0): epoch_required = last_applied_epoch + 1 = 1.
        overlay.on_input(b"a", &screen);
        let pred = &overlay.pending[0];
        assert_eq!(pred.epoch_required, 1, "epoch_required must be 1");

        // Datagram epoch 1 is dropped. Apply epoch 1 and 3 to screen but only cull with 3.
        let diff1 = make_diff_with_char(1, 0, 0, 'a');
        screen.apply(&diff1);
        let diff3 = make_diff_with_char(3, 0, 0, 'a');
        screen.apply(&diff3);
        overlay.cull(&screen, 3, 50); // epoch 3 >= epoch_required 1 → confirmed

        assert_eq!(
            overlay.pending.len(),
            0,
            "prediction with epoch_required=1 must be confirmed by epoch 3 (>= check, Pitfall 4)"
        );
    }

    #[test]
    fn noecho_suppression() {
        // PREDICT-04: noecho is structural — confirmed_epoch never advances when
        // server does not echo. cell_at must return None throughout.
        //
        // Invariant: confirmed_epoch() < prediction_epoch() throughout noecho.
        let mut screen = make_screen(80, 24);
        let mut overlay = PredictionOverlay::new(PredictDisplayMode::Always, 80, 24);

        // Enter a "noecho" context by triggering become_tentative.
        overlay.become_tentative(); // prediction_epoch = 1
        let initial_confirmed = overlay.confirmed_epoch();

        // Type 'a' — enqueues prediction with tentative_until_epoch = 1.
        overlay.on_input(b"a", &screen);

        // Server sends new epoch WITHOUT echoing 'a' (stty -echo).
        // Cell at (0,0) remains ' ' (space) — NOT 'a'.
        let diff = make_diff_empty(1);
        screen.apply(&diff);
        // cull: epoch_required (1) <= new_epoch (1); confirmed_cell is ' ' != 'a'
        // → IncorrectOrExpired on tentative prediction → kill_epoch(1) → pending cleared.
        overlay.cull(&screen, 1, 5);

        // After cull, type another char in new prediction state.
        overlay.on_input(b"b", &screen);

        // Server still doesn't echo.
        let diff2 = make_diff_empty(2);
        screen.apply(&diff2);
        overlay.cull(&screen, 2, 5);

        // confirmed_epoch must still be at initial value (no Correct confirmation).
        assert_eq!(
            overlay.confirmed_epoch(),
            initial_confirmed,
            "confirmed_epoch must not advance when server never echoes (noecho suppression is structural)"
        );
        // cell_at must return None for all typed positions.
        assert!(
            overlay.cell_at(0, 0).is_none(),
            "cell_at must return None during noecho (predictions are tentative)"
        );
        assert!(
            overlay.cell_at(0, 1).is_none(),
            "cell_at must return None for second typed char during noecho"
        );
        // confirmed_epoch < prediction_epoch — the key invariant.
        assert!(
            overlay.confirmed_epoch() < overlay.prediction_epoch(),
            "confirmed_epoch() must be < prediction_epoch() throughout noecho suppression"
        );
    }

    #[test]
    fn cjk_width_2_advances_cursor_by_2() {
        let screen = make_screen(80, 24);
        let mut overlay = PredictionOverlay::new(PredictDisplayMode::Always, 80, 24);

        let ni = '你';
        let mut buf = [0u8; 4];
        let s = ni.encode_utf8(&mut buf);
        overlay.on_input(s.as_bytes(), &screen);

        assert_eq!(
            overlay.predicted_cursor.col,
            2,
            "CJK width-2 char must advance predicted cursor by 2 columns"
        );
        assert_eq!(
            overlay.pending.len(),
            1,
            "one prediction must be enqueued for CJK char"
        );
        assert_eq!(
            overlay.pending[0].col_width,
            2,
            "col_width must be 2 for CJK char"
        );
    }

    #[test]
    fn cjk_at_right_edge_becomes_tentative() {
        let screen = make_screen(80, 24);
        let mut overlay = PredictionOverlay::new(PredictDisplayMode::Always, 80, 24);

        // Position cursor at col 79 (last col of 80-col terminal).
        overlay.predicted_cursor.col = 79;
        let initial_epoch = overlay.prediction_epoch();

        let ni = '你';
        let mut buf = [0u8; 4];
        let s = ni.encode_utf8(&mut buf);
        overlay.on_input(s.as_bytes(), &screen);

        assert!(
            overlay.prediction_epoch() > initial_epoch,
            "CJK at right edge must call become_tentative (Pitfall 6)"
        );
        assert_eq!(
            overlay.pending.len(),
            0,
            "CJK at right edge must NOT enqueue a prediction"
        );
    }

    #[test]
    fn rtt_hysteresis_srtt_trigger() {
        let screen = make_screen(80, 24);
        let mut overlay = PredictionOverlay::new(PredictDisplayMode::Adaptive, 80, 24);

        // Above HIGH → activate.
        overlay.cull(&screen, 0, 35);
        assert!(overlay.srtt_trigger, "srtt_trigger must be true when RTT > 30ms");

        // Below LOW but predictions present → stays active.
        overlay.on_input(b"x", &screen);
        overlay.cull(&screen, 0, 15);
        assert!(
            overlay.srtt_trigger,
            "srtt_trigger must stay true when RTT < 20ms but pending is non-empty"
        );

        // Clear predictions, then below LOW → deactivate.
        overlay.pending.clear();
        overlay.cull(&screen, 0, 15);
        assert!(
            !overlay.srtt_trigger,
            "srtt_trigger must be false when RTT < 20ms and no pending predictions"
        );
    }

    #[test]
    fn rtt_hysteresis_flagging() {
        let screen = make_screen(80, 24);
        let mut overlay = PredictionOverlay::new(PredictDisplayMode::Adaptive, 80, 24);

        // Above FLAG_HIGH → flagging.
        overlay.cull(&screen, 0, 85);
        assert!(overlay.flagging, "flagging must be true when RTT > 80ms");

        // Below FLAG_LOW → stop flagging (no prediction guard on flagging).
        overlay.cull(&screen, 0, 45);
        assert!(!overlay.flagging, "flagging must be false when RTT < 50ms");
    }

    #[test]
    fn should_display_always() {
        let overlay = PredictionOverlay::new(PredictDisplayMode::Always, 80, 24);
        assert!(overlay.should_display(), "Always mode must always return true");
    }

    #[test]
    fn should_display_never() {
        let overlay = PredictionOverlay::new(PredictDisplayMode::Never, 80, 24);
        assert!(!overlay.should_display(), "Never mode must always return false");
    }

    #[test]
    fn should_display_adaptive_follows_srtt_trigger() {
        let screen = make_screen(80, 24);
        let mut overlay = PredictionOverlay::new(PredictDisplayMode::Adaptive, 80, 24);
        assert!(
            !overlay.should_display(),
            "Adaptive must not display when srtt_trigger=false"
        );
        overlay.cull(&screen, 0, 35); // RTT=35 → srtt_trigger=true
        assert!(
            overlay.should_display(),
            "Adaptive must display when srtt_trigger=true"
        );
    }

    #[test]
    fn cell_at_returns_underline_when_flagging() {
        let screen = make_screen(80, 24);
        let mut overlay = PredictionOverlay::new(PredictDisplayMode::Always, 80, 24);
        overlay.on_input(b"a", &screen);

        // CR-03 fix: awaiting_first_cull is true after on_input on a fresh epoch.
        // Simulate the datagram arm: cull() with epoch 0 (below epoch_required=1)
        // clears awaiting_first_cull without removing the prediction from pending.
        // The prediction remains non-tentative (tentative_until_epoch=0 <= confirmed_epoch=0).
        overlay.cull(&screen, 0, 50); // clears awaiting_first_cull; prediction stays pending

        // Manually set flagging (cull at rtt=50 < FLAG_TRIGGER_HIGH=80 won't set it).
        overlay.flagging = true;
        // The prediction's tentative_until_epoch = 0 = confirmed_epoch (0), not tentative.
        if let Some(cell) = overlay.cell_at(0, 0) {
            assert_eq!(
                cell.style.0 & CellStyle::UNDERLINE,
                CellStyle::UNDERLINE,
                "cell_at must return UNDERLINE style when flagging=true"
            );
        } else {
            panic!("cell_at must return Some(Cell) when prediction is non-tentative and Always mode");
        }
    }

    #[test]
    fn cell_at_no_underline_when_not_flagging() {
        let screen = make_screen(80, 24);
        let mut overlay = PredictionOverlay::new(PredictDisplayMode::Always, 80, 24);
        overlay.on_input(b"a", &screen);
        // CR-03 fix: awaiting_first_cull is true after on_input on a fresh epoch.
        // Cull with epoch 0 clears the flag without removing the pending prediction.
        overlay.cull(&screen, 0, 5); // clears awaiting_first_cull; rtt=5 → no flagging
        overlay.flagging = false;
        if let Some(cell) = overlay.cell_at(0, 0) {
            assert_eq!(
                cell.style.0 & CellStyle::UNDERLINE,
                CellStyle::NONE,
                "cell_at must NOT have UNDERLINE style when flagging=false"
            );
        } else {
            panic!("cell_at must return Some(Cell) for non-tentative Always-mode prediction");
        }
    }

    #[test]
    fn bracketed_paste_suppresses_prediction() {
        let screen = make_screen(80, 24);
        let mut overlay = PredictionOverlay::new(PredictDisplayMode::Always, 80, 24);

        overlay.on_input(b"\x1b[200~", &screen);
        assert!(overlay.in_bracketed_paste, "in_bracketed_paste must be true after paste-start");

        overlay.on_input(b"a", &screen);
        overlay.on_input(b"b", &screen);
        assert_eq!(
            overlay.pending.len(),
            0,
            "printable chars during bracketed paste must NOT enqueue predictions"
        );

        overlay.on_input(b"\x1b[201~", &screen);
        assert!(
            !overlay.in_bracketed_paste,
            "in_bracketed_paste must be false after paste-end"
        );
    }

    #[test]
    fn bulk_suppressed_becomes_tentative() {
        let screen = make_screen(80, 24);
        let mut overlay = PredictionOverlay::new(PredictDisplayMode::Always, 80, 24);
        let initial_epoch = overlay.prediction_epoch();

        overlay.on_input(b"hello", &screen); // 5 bytes → BulkSuppressed
        assert!(
            overlay.prediction_epoch() > initial_epoch,
            "BulkSuppressed must increment prediction_epoch (become_tentative)"
        );
        assert_eq!(
            overlay.pending.len(),
            0,
            "BulkSuppressed must NOT enqueue any predictions"
        );
    }

    #[test]
    fn predicted_cursor_none_when_all_tentative() {
        let screen = make_screen(80, 24);
        let mut overlay = PredictionOverlay::new(PredictDisplayMode::Always, 80, 24);

        overlay.become_tentative(); // prediction_epoch = 1
        overlay.on_input(b"a", &screen);
        // tentative_until_epoch = 1 > confirmed_epoch (0) → tentative.
        assert_eq!(
            overlay.predicted_cursor(),
            None,
            "predicted_cursor must return None when all predictions are tentative"
        );
    }

    // ── Mandatory regression tests (CR-01, CR-02, CR-03) ─────────────────────

    /// CR-01 regression: prediction lands on NON-ZERO confirmed cursor row.
    ///
    /// Before the fix, `predicted_cursor.row` was always 0 regardless of where
    /// the confirmed cursor was. After the fix, `sync_cursor_from_confirmed` seeds
    /// the predicted row from the confirmed cursor so predictions land correctly.
    #[test]
    fn cr01_prediction_lands_on_correct_nonzero_row() {
        let mut screen = make_screen(80, 24);
        let mut overlay = PredictionOverlay::new(PredictDisplayMode::Always, 80, 24);

        // Server places cursor at row=5, col=2 (simulating a multi-line shell prompt).
        let diff = StateDiff {
            epoch: 1,
            cols: 80,
            rows: 24,
            cursor: CursorPos { row: 5, col: 2 },
            runs: vec![],
        };
        screen.apply(&diff);

        // Simulate the datagram arm: cull() then sync_cursor_from_confirmed.
        overlay.cull(&screen, 1, 5); // clears awaiting_first_cull
        overlay.sync_cursor_from_confirmed(screen.confirmed_cursor()); // seeds row=5, col=2

        // Type 'a' — prediction must land at row=5, col=2 (not row=0).
        overlay.on_input(b"a", &screen);

        assert_eq!(
            overlay.pending.len(),
            1,
            "one prediction must be enqueued"
        );
        let pred = &overlay.pending[0];
        assert_eq!(pred.row, 5, "prediction must be at row 5 (confirmed cursor row), not row 0");
        assert_eq!(pred.col, 2, "prediction must be at col 2 (confirmed cursor col)");

        // Confirm: server echoes 'a' at row=5, col=2 with epoch=2.
        let diff2 = make_diff_with_char(2, 5, 2, 'a');
        screen.apply(&diff2);
        overlay.cull(&screen, 2, 5);

        // After correct confirmation, the prediction at (5,2) must be removed from pending.
        // Note: confirmed_epoch only advances when tentative_until_epoch > confirmed_epoch.
        // In this test, tentative_until_epoch=0 == confirmed_epoch=0, so confirmed_epoch
        // stays 0 (CorrectNoCredit for blank→blank, or Correct without epoch advance).
        // The key correctness assertion is that the prediction was placed at (5, 2),
        // confirmed there, and is now gone — not at (0, 0) as the pre-fix bug caused.
        assert_eq!(
            overlay.pending.len(),
            0,
            "prediction confirmed at (5, 2) must be removed from pending (not stuck at row 0)"
        );
    }

    /// CR-02 regression: backspace removes the prediction at the vacated column.
    ///
    /// Before the fix, PredictBackspace only moved the cursor left — the prediction
    /// at the vacated column remained in `pending` and was still returned by `cell_at`.
    /// This is worse than no prediction (D-15-01 "never render worse" invariant).
    #[test]
    fn cr02_backspace_removes_char_prediction_from_overlay() {
        let screen = make_screen(80, 24);
        let mut overlay = PredictionOverlay::new(PredictDisplayMode::Always, 80, 24);

        // Type 'a' — prediction at (0, 0); cursor → col=1.
        overlay.on_input(b"a", &screen);
        // Cull to clear awaiting_first_cull so cell_at can return the prediction.
        overlay.cull(&screen, 0, 5);
        assert!(
            overlay.cell_at(0, 0).is_some(),
            "prediction must be visible at (0,0) before backspace"
        );

        // Backspace — cursor moves to col=0; prediction at col=0 must be REMOVED.
        overlay.on_input(&[0x7f], &screen);

        assert!(
            overlay.cell_at(0, 0).is_none(),
            "after backspace, cell_at(0,0) must be None — \
             deleted char must not remain in overlay (D-15-01 never-render-worse invariant)"
        );
        assert_eq!(
            overlay.pending.len(),
            0,
            "after backspace, pending must be empty — prediction at vacated column removed"
        );
        assert_eq!(
            overlay.predicted_cursor.col,
            0,
            "after backspace, predicted cursor must be at col 0"
        );
    }

    /// BUG-D: a printable prediction in a NON-tentative epoch is shown IMMEDIATELY on
    /// keystroke (no waiting for a confirming datagram). This is the headline
    /// predictive-echo behaviour: the typed char and the advanced caret appear at once.
    ///
    /// (This replaces the old CR-03 `awaiting_first_cull` test, which asserted the
    /// prediction was hidden until cull() ran — that over-suppression broke the
    /// headline feature for the common echoing-shell case. Noecho safety is now proven
    /// by `bug_d_noecho_after_reset_is_hidden` below and the live integration test.)
    #[test]
    fn bug_d_printable_visible_immediately_on_keystroke() {
        let screen = make_screen(80, 24);
        let mut overlay = PredictionOverlay::new(PredictDisplayMode::Always, 80, 24);

        // Type 'a' on a fresh, non-tentative epoch (confirmed_epoch == prediction_epoch == 0).
        overlay.on_input(b"a", &screen);

        // The predicted char must be visible WITHOUT any cull()/datagram first.
        let cell = overlay.cell_at(0, 0);
        assert!(
            cell.is_some(),
            "BUG-D: cell_at(0,0) must return Some immediately after typing 'a' (predictive \
             echo must not wait for a confirming datagram)"
        );
        assert_eq!(cell.unwrap().ch, 'a', "predicted cell must contain 'a'");

        // The predicted caret must have advanced to col 1 immediately.
        let cur = overlay.predicted_cursor();
        assert!(
            cur.is_some(),
            "BUG-D: predicted_cursor() must be Some immediately after a printable keystroke"
        );
        assert_eq!(cur.unwrap().col, 1, "BUG-D: caret must advance to col 1 after typing 'a'");
    }

    /// BUG-D: SPACE is a printable prediction and must advance the caret immediately
    /// (the user reported space did not show / advance until the next char).
    #[test]
    fn bug_d_space_advances_caret_immediately() {
        let screen = make_screen(80, 24);
        let mut overlay = PredictionOverlay::new(PredictDisplayMode::Always, 80, 24);

        overlay.on_input(b"a", &screen);
        overlay.on_input(b" ", &screen); // space
        assert_eq!(
            overlay.predicted_cursor().map(|c| c.col),
            Some(2),
            "BUG-D: caret must advance to col 2 after 'a' then space (space is predicted)"
        );
        // The space prediction is enqueued at col 1.
        assert!(
            overlay.pending.iter().any(|p| p.col == 1 && p.predicted_ch == ' '),
            "BUG-D: a space prediction must be enqueued at col 1"
        );
    }

    /// BUG-D security preservation: after an epoch reset (the Enter that precedes a
    /// `read -s` / `ssh` / `sudo` password prompt), predictions are TENTATIVE and
    /// therefore hidden until the server confirms an echoed char. Removing
    /// `awaiting_first_cull` must NOT weaken this.
    #[test]
    fn bug_d_noecho_after_reset_is_hidden() {
        let screen = make_screen(80, 24);
        let mut overlay = PredictionOverlay::new(PredictDisplayMode::Always, 80, 24);

        // Simulate the Enter that submits the command that triggers a noecho prompt.
        overlay.on_input(b"\r", &screen); // EpochReset → become_tentative
        assert!(overlay.confirmed_epoch() < overlay.prediction_epoch());

        // Now type password chars — they are tentative (tentative_until_epoch > confirmed).
        overlay.on_input(b"s", &screen);
        overlay.on_input(b"e", &screen);

        for col in 0..5u16 {
            assert!(
                overlay.cell_at(0, col).is_none(),
                "BUG-D security: password char prediction at col {col} must be HIDDEN \
                 (tentative epoch) — noecho suppression must hold without awaiting_first_cull"
            );
        }
        assert!(
            overlay.predicted_cursor().is_none(),
            "BUG-D security: predicted_cursor() must be None for tentative-only predictions"
        );
    }

    /// BUG-D symptom 3: left/right arrow keys MOVE the predicted caret, and the
    /// motion is NOT clobbered by a confirming datagram's sync_cursor_from_confirmed.
    ///
    /// Before the fix, pure cursor motion enqueued no pending cell, so
    /// `predicted_cursor()` returned None (caret never moved), and even if it had,
    /// `sync_cursor_from_confirmed` (called every datagram when pending is empty)
    /// snapped the caret back to the confirmed column — the "arrows don't move /
    /// overwrite mode" symptom.
    #[test]
    fn bug_d_arrow_motion_moves_caret_and_survives_sync() {
        let mut screen = make_screen(80, 24);
        let mut overlay = PredictionOverlay::new(PredictDisplayMode::Always, 80, 24);

        // Server places the cursor at col 5 (e.g. after "hello").
        let diff = StateDiff {
            epoch: 1,
            cols: 80,
            rows: 24,
            cursor: CursorPos { row: 0, col: 5 },
            runs: vec![DiffRun {
                row: 0,
                start_col: 0,
                chars: "hello".to_string(),
                style: CellStyle(CellStyle::NONE),
                fg: None,
                bg: None,
            }],
        };
        screen.apply(&diff);
        overlay.cull(&screen, 1, 50);
        overlay.sync_cursor_from_confirmed(screen.confirmed_cursor()); // caret at col 5

        // Press LEFT arrow (CSI D). Caret must move to col 4 and be visible.
        overlay.on_input(b"\x1b[D", &screen);
        assert_eq!(
            overlay.predicted_cursor().map(|c| c.col),
            Some(4),
            "BUG-D: left arrow must move predicted caret to col 4 (was at col 5)"
        );

        // A confirming datagram arrives (cursor STILL at col 5 server-side, motion not yet
        // applied). pending is empty, but cursor_motion_pending must protect the caret.
        let diff2 = StateDiff {
            epoch: 2,
            cols: 80,
            rows: 24,
            cursor: CursorPos { row: 0, col: 5 },
            runs: vec![],
        };
        screen.apply(&diff2);
        overlay.cull(&screen, 2, 50);
        overlay.sync_cursor_from_confirmed(screen.confirmed_cursor());

        assert_eq!(
            overlay.predicted_cursor().map(|c| c.col),
            Some(4),
            "BUG-D: predicted caret at col 4 must SURVIVE a confirming datagram \
             (sync must not snap it back to confirmed col 5 while motion is pending)"
        );

        // Press RIGHT arrow (CSI C) twice → col 6.
        overlay.on_input(b"\x1b[C", &screen);
        overlay.on_input(b"\x1b[C", &screen);
        assert_eq!(
            overlay.predicted_cursor().map(|c| c.col),
            Some(6),
            "BUG-D: two right arrows from col 4 must move predicted caret to col 6"
        );

        // Enter (epoch reset) clears motion → caret no longer overridden.
        overlay.on_input(b"\r", &screen);
        assert!(
            overlay.predicted_cursor().is_none(),
            "BUG-D: after Enter (reset), cursor_motion_pending must clear → predicted_cursor() None"
        );
    }

    /// BUG-E regression: backspace must clamp at the epoch-start column, not col 0.
    ///
    /// Before the fix, `PredictBackspace` clamped at col 0 — the predictor had no
    /// knowledge of the prompt boundary. After the fix, it clamps at the epoch-start
    /// column recorded by `sync_cursor_from_confirmed` after a fresh epoch reset
    /// (e.g. the Enter that submitted the previous command).
    #[test]
    fn bug_e_backspace_clamps_at_epoch_start_col() {
        // FAIL BEFORE FIX: cursor walks past col 5 (epoch start) toward col 0.
        // PASS AFTER FIX:  cursor stops at col 5.
        let mut screen = make_screen(80, 24);
        let mut overlay = PredictionOverlay::new(PredictDisplayMode::Always, 80, 24);

        // Simulate user pressing Enter on the previous command → epoch reset.
        // In a real session this happens via EpochReset in on_input; call reset()
        // directly to set needs_epoch_start_sync = true.
        overlay.reset(); // signals: next sync_cursor_from_confirmed captures epoch_start_col

        // Server advances cursor to col 5 (new prompt "user> ").
        let diff = make_diff_with_char(1, 0, 5, ' ');
        screen.apply(&diff);
        overlay.cull(&screen, 1, 5);
        overlay.sync_cursor_from_confirmed(screen.confirmed_cursor()); // epoch_start_col = 5

        // Type "abc" then backspace 10 times — cursor must not go below col 5.
        overlay.on_input(b"a", &screen);
        overlay.on_input(b"b", &screen);
        overlay.on_input(b"c", &screen);
        for _ in 0..10 {
            overlay.on_input(&[0x7f], &screen); // backspace
        }

        assert_eq!(
            overlay.predicted_cursor.col,
            5,
            "BUG-E: backspace must clamp at epoch-start col 5, not walk to col 0"
        );
    }

    /// BUG-E regression: cursor-left must clamp at the epoch-start column, not col 0.
    ///
    /// Before the fix, `PredictCursorLeft` clamped at col 0 — the predictor had no
    /// knowledge of the prompt boundary. After the fix, it clamps at the epoch-start
    /// column recorded by `sync_cursor_from_confirmed` after a fresh epoch reset.
    #[test]
    fn bug_e_cursor_left_clamps_at_epoch_start_col() {
        // FAIL BEFORE FIX: cursor-left walks past col 5 (epoch start) toward col 0.
        // PASS AFTER FIX:  cursor-left stops at col 5.
        let mut screen = make_screen(80, 24);
        let mut overlay = PredictionOverlay::new(PredictDisplayMode::Always, 80, 24);

        // Simulate user pressing Enter on the previous command → epoch reset.
        overlay.reset(); // signals: next sync_cursor_from_confirmed captures epoch_start_col

        // Server advances cursor to col 5 (new prompt "user> ").
        let diff = make_diff_with_char(1, 0, 5, ' ');
        screen.apply(&diff);
        overlay.cull(&screen, 1, 5);
        overlay.sync_cursor_from_confirmed(screen.confirmed_cursor()); // epoch_start_col = 5

        // Type "abc" then cursor-left (CSI D) 10 times — cursor must not go below col 5.
        overlay.on_input(b"a", &screen);
        overlay.on_input(b"b", &screen);
        overlay.on_input(b"c", &screen);
        for _ in 0..10 {
            overlay.on_input(b"\x1b[D", &screen); // cursor left
        }

        assert_eq!(
            overlay.predicted_cursor.col,
            5,
            "BUG-E: cursor-left must clamp at epoch-start col 5, not walk to col 0"
        );
    }

    /// BUG-F regression: after a noecho epoch (EpochReset), the predicted caret must
    /// be synced from the confirmed cursor so the post-`read -s` Enter advances the line.
    ///
    /// Before the fix, `EpochReset` called `reset()` which did not update
    /// `predicted_cursor`, leaving it at a stale position. After the fix, `EpochReset`
    /// calls `reset_with_cursor(screen.confirmed_cursor())` which forcibly syncs the caret.
    #[test]
    fn bug_f_enter_after_noecho_syncs_caret_from_confirmed() {
        // FAIL BEFORE FIX: predicted_cursor stays at stale col after EpochReset.
        // PASS AFTER FIX:  predicted_cursor matches the confirmed cursor after EpochReset.
        let mut screen = make_screen(80, 24);
        let mut overlay = PredictionOverlay::new(PredictDisplayMode::Always, 80, 24);

        // Server confirms cursor at row 1, col 10 (e.g. after `read -s` prompt).
        let diff = StateDiff {
            epoch: 1,
            cols: 80,
            rows: 24,
            cursor: CursorPos { row: 1, col: 10 },
            runs: vec![],
        };
        screen.apply(&diff);
        overlay.cull(&screen, 1, 5);
        overlay.sync_cursor_from_confirmed(screen.confirmed_cursor());

        // Simulate noecho typing — moves predicted_cursor to a stale position.
        // (These won't be visible due to tentative epoch, but they do advance the caret.)
        overlay.on_input(b"\r", &screen); // EpochReset first (initial state)

        // Manually stale the predicted cursor to simulate a diverged position.
        overlay.predicted_cursor = CursorPos { row: 0, col: 3 };

        // Server advances cursor (e.g. after `read -s` completes, Enter was pressed).
        let diff2 = StateDiff {
            epoch: 2,
            cols: 80,
            rows: 24,
            cursor: CursorPos { row: 2, col: 0 },
            runs: vec![],
        };
        screen.apply(&diff2);
        overlay.cull(&screen, 2, 5);

        // Simulate user pressing Enter at end of read -s prompt (EpochReset).
        overlay.on_input(b"\r", &screen);

        // After EpochReset, predicted_cursor must be synced from the confirmed cursor.
        assert_eq!(
            overlay.predicted_cursor.row,
            screen.confirmed_cursor().row,
            "BUG-F: after EpochReset, predicted_cursor.row must match confirmed cursor row"
        );
        assert_eq!(
            overlay.predicted_cursor.col,
            screen.confirmed_cursor().col,
            "BUG-F: after EpochReset, predicted_cursor.col must match confirmed cursor col"
        );
    }

    /// BUG-F: BulkSuppressed must NOT sync the predicted caret from the confirmed cursor.
    ///
    /// Before the fix, EpochReset and BulkSuppressed shared the same arm. After splitting
    /// them, BulkSuppressed must still call plain `reset()` (no cursor sync), because
    /// there is no reliable confirmed cursor available at that point.
    #[test]
    fn bug_f_bulk_suppressed_does_not_sync_caret() {
        // FAIL BEFORE FIX: (n/a — this verifies BulkSuppressed does NOT sync).
        // PASS AFTER FIX:  BulkSuppressed leaves predicted_cursor unchanged (no force-sync).
        let mut screen = make_screen(80, 24);
        let mut overlay = PredictionOverlay::new(PredictDisplayMode::Always, 80, 24);

        // Establish a confirmed cursor at col 10.
        let diff = StateDiff {
            epoch: 1,
            cols: 80,
            rows: 24,
            cursor: CursorPos { row: 0, col: 10 },
            runs: vec![],
        };
        screen.apply(&diff);
        overlay.cull(&screen, 1, 5);
        overlay.sync_cursor_from_confirmed(screen.confirmed_cursor()); // predicted at col 10

        // Move predicted cursor to col 3 (simulating some typing and cursor motion).
        overlay.predicted_cursor = CursorPos { row: 0, col: 3 };

        // Trigger BulkSuppressed — must NOT sync predicted_cursor to the confirmed col 10.
        // "hello" = 5 bytes > BULK_SUPPRESS_THRESHOLD (4), classified as BulkSuppressed.
        overlay.on_input(b"hello", &screen);

        assert_eq!(
            overlay.predicted_cursor.col,
            3,
            "BUG-F: BulkSuppressed must NOT sync predicted_cursor from confirmed cursor \
             (predicted_cursor must remain at col 3, not snap to confirmed col 10)"
        );
    }

    /// CR-01 / confirmed_cursor() getter: screen.confirmed_cursor() returns correct position.
    #[test]
    fn screen_confirmed_cursor_getter_returns_correct_position() {
        let mut screen = make_screen(80, 24);

        // Initial state: cursor at (0, 0).
        let cursor = screen.confirmed_cursor();
        assert_eq!(cursor.row, 0, "initial confirmed_cursor.row must be 0");
        assert_eq!(cursor.col, 0, "initial confirmed_cursor.col must be 0");

        // After apply with a non-trivial cursor, getter returns the updated position.
        let diff = StateDiff {
            epoch: 1,
            cols: 80,
            rows: 24,
            cursor: CursorPos { row: 3, col: 15 },
            runs: vec![],
        };
        screen.apply(&diff);

        let cursor2 = screen.confirmed_cursor();
        assert_eq!(cursor2.row, 3, "confirmed_cursor.row must be 3 after apply");
        assert_eq!(cursor2.col, 15, "confirmed_cursor.col must be 15 after apply");
    }
}
