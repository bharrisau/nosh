# Phase 15: Client Predictor — Speculative Overlay - Research

**Researched:** 2026-06-02
**Domain:** Rust client-side speculative echo; Mosh overlay model translation; unicode-width; quinn RTT API
**Confidence:** HIGH

---

<user_constraints>
## User Constraints (from CONTEXT.md)

### Locked Decisions

**D-15-01 — Prediction scope:**
Predict: printable chars, backspace, left/right arrow (`\x1b[C`/`\x1b[D`), Home/End (incl. Ctrl-A/Ctrl-E).
Epoch-reset (display nothing) on: Tab, word-motion, Enter/`\r`, ESC, any CSI cursor-addressing/erase/alternate-screen, any non-printing control key.

**D-15-01b — Bulk/paste suppression:**
Suppress prediction during bracketed paste (`CSI ?2004h`/`l`) and on bulk input (>~4 bytes in one read batch).

**D-15-01c — No-echo suppression (security):**
Track server's confirmed echo state; display zero predicted characters when server is not echoing. Validated adversarially.

**D-15-02 — Adaptive RTT thresholds + hysteresis (Mosh values):**
Show predictions when smoothed RTT > ~30ms / stop below ~20ms. Underline when RTT > ~80ms / stop below ~50ms.
`--predict always|adaptive|never`, default adaptive. Invisible on loopback.

**D-15-03 — Unicode-width policy:**
`unicode-width` (`UnicodeWidthChar::width()`) for column advance. Predict clean width-1 and width-2 (CJK). Epoch-reset on ambiguous-width / combining / ZWJ / emoji.

**D-15-04 — Validation matrix (gates phase done):**
vim insert, `read -s`, CJK, less/htop, bracketed paste, Ctrl-C mid-line, simulated-loss, Home/End motion.

### Claude's Discretion

- Module layout (`predictor.rs` vs folding into `screen.rs`).
- `PendingPrediction` / `Validity` state machine internals.
- `VecDeque` cull bookkeeping.
- Exact numeric RTT constants within Mosh-derived ranges; SRTT smoothing factor.
- Bulk-input batch-size threshold (~4 bytes — Claude may tune).
- How the simulated-loss test harness injects datagram drops.

### Deferred Ideas (OUT OF SCOPE)

- nosh-specific RTT threshold tuning (D-15-02a).
- Tab/word-motion prediction.
- ConnectionLossOverlay activation, OSC52, terminal title — Phase 16.
- Windows-host predictive-echo validation — Phase 17.
- Client-side scrollback — M5.

</user_constraints>

---

<phase_requirements>
## Phase Requirements

| ID | Description | Research Support |
|----|-------------|------------------|
| PREDICT-02 | Speculatively echo printable chars, backspace, left/right cursor motion with per-prediction tracking | PendingPrediction/Validity state machine (§ Architecture Patterns) |
| PREDICT-03 | Conservative by design — control sequences reset epoch; no prediction on fresh row before first server confirmation | `become_tentative()` and `cull()` model (§ Mosh Model Translation) |
| PREDICT-04 | Prediction suppressed during `stty -echo` / `read -s` — noecho detection via confirmed StateDiff | Noecho detection mechanism (§ Echo-State / Noecho Detection) |
| PREDICT-05 | Underline unconfirmed predictions only above RTT flag threshold; adaptive default; `--predict` CLI override | quinn `conn.rtt()` API + Mosh thresholds (§ SRTT Estimation) |
| PREDICT-06 | Correct cursor advance for CJK wide chars; epoch-reset on ambiguous/ZWJ/emoji | `unicode-width` 0.2.2 API (§ unicode-width Integration) |

</phase_requirements>

---

## Summary

Phase 15 implements the speculative echo overlay — the headline M4 differentiator. The research
grounds every design decision in the Mosh `terminaloverlay.cc` source (fetched directly from
`mobile-shell/mosh@master`) and the current `nosh-client` codebase (Phase 14 compositor, Phase 11
wire format). Confidence is HIGH because all claims are traced to primary sources with file/line
citations or direct crate registry verification.

**Key findings:**

1. The Mosh `PredictionEngine` / `ConditionalOverlayCell` model translates cleanly to Rust. The
   critical insight is that Mosh's "confirmed epoch" (`confirmed_epoch`) is a u64 that advances
   only when a `Correct` (non-trivial) prediction is confirmed by the server. This is the noecho
   suppression mechanism — if the server never echoes (stty -echo), `confirmed_epoch` never
   advances past the first epoch, so tentative predictions stay hidden forever. This is structural,
   not an explicit noecho flag.

2. Quinn's `Connection::rtt()` returns `Duration` — a "current best estimate of latency." This is
   the smoothed RTT already maintained by quinn's internal congestion control; it is directly usable
   for the Mosh threshold comparisons without implementing a separate EWMA.

3. `unicode-width` 0.2.2 provides `UnicodeWidthChar::width() -> Option<usize>` returning
   `None` for control characters, `Some(0)` for combining/zero-width, `Some(1)` for narrow,
   `Some(2)` for CJK wide. The conservative predicate is exactly: predict only when `Some(1)` or
   `Some(2)`; epoch-reset on `None` or `Some(0)`.

4. The Phase 14 compositor seam (`Overlay` trait on `screen.rs:78`) is already the correct hook
   point. `PredictionOverlay` implements `Overlay::cell_at(row, col) -> Option<Cell>` and returns
   a `Cell` with `style.0 | CellStyle::UNDERLINE` when the prediction is unconfirmed-and-above-RTT-flag.

5. The `EscapeState` machine in `main.rs:90-165` processes input BEFORE `send_input` and
   returns `EscapeResult { bytes_to_forward, quit }`. The predictor hooks AFTER the escape
   machine but BEFORE the `send_input` call, at `main.rs:726-731` in the stdin arm.

**Primary recommendation:** Implement `PredictionOverlay` in a new `crates/nosh-client/src/predictor.rs`, model closely on Mosh's `PredictionEngine` with the five-enum `Validity` type and a per-prediction `epoch_required` field, and hook into the Phase 14 `Overlay` trait seam.

---

## Architectural Responsibility Map

| Capability | Primary Tier | Secondary Tier | Rationale |
|------------|-------------|----------------|-----------|
| Speculative echo state machine | Client process | — | Client-local speculation; server never knows about predicted cells |
| Noecho detection | Client process | — | Inferred from server StateDiff — if server echo is absent, confirmed_epoch never advances |
| RTT measurement | Client process (quinn) | — | `conn.rtt()` already computed by quinn's CC; no app-level SRTT needed |
| Overlay composition | Client process (ClientScreen) | — | Phase 14 compositor seam is the sole display path |
| Keystroke routing | Client reliable stream | — | Keystrokes stay on bidi stream; prediction is display-only |
| Unicode width classification | Client process | — | Pure lookup table; runs on the keystroke path inline |
| Validation tests | Client integration tests | — | Adversarial test harness drives server+client; assertions on ClientScreen state |

---

## Standard Stack

### Core (Phase 15 changes)

| Library | Version | Purpose | Why Standard |
|---------|---------|---------|--------------|
| `unicode-width` | 0.2.2 | `UnicodeWidthChar::width()` for column advance | Unicode Standard Annex #11 compliant; used by alacritty, ratatui, termwiz |

### Already Present (no new deps for these)

| Library | Version | Purpose | Notes |
|---------|---------|---------|-------|
| `nosh-proto` | workspace | `StateDiff`, `CellStyle::UNDERLINE`, `encode_epoch_ack` | Phase 11/14 wire format; `UNDERLINE = 0x04` already exists |
| `nosh-client` (screen.rs) | — | `Overlay` trait, `ConnectionLossOverlay` pattern | Phase 14 compositor; seam exists |
| `quinn` | 0.11.9 | `Connection::rtt()` → `Duration` for adaptive RTT | Phase 14 conn already in scope |
| `tokio` | 1.52.x | `run_pump` select loop | Phase 14 pump |
| `clap` | workspace | `--predict always\|adaptive\|never` arg | existing Args struct |

### Supporting

No additional supporting libraries. The predictor is self-contained using existing workspace deps.

### Alternatives Considered

| Instead of | Could Use | Tradeoff |
|------------|-----------|----------|
| `unicode-width` for width | Custom `wcwidth` table | Reinventing a maintained crate; no benefit |
| `conn.rtt()` direct | Custom EWMA on datagram timestamps | quinn's RTT is already a smoothed estimate; adding a second EWMA adds noise without benefit |
| Separate `predictor.rs` module | Folding into `screen.rs` | `screen.rs` is already 835 lines; separation maintains single-responsibility |

**Installation:**

```toml
# Add to crates/nosh-client/Cargo.toml [dependencies]
unicode-width = "0.2"
```

**Version verification:**

```
cargo search unicode-width
→ unicode-width = "0.2.2"  (confirmed 2026-06-02)
```

---

## Package Legitimacy Audit

| Package | Registry | Age | Downloads | Source Repo | slopcheck | Disposition |
|---------|----------|-----|-----------|-------------|-----------|-------------|
| `unicode-width` | crates.io | ~10 yrs | Very high (rust-lang org, used by compiler) | github.com/unicode-rs/unicode-width | [OK] | Approved |

**Packages removed due to slopcheck [SLOP] verdict:** none
**Packages flagged as suspicious [SUS]:** none

[VERIFIED: crates.io registry — `cargo search unicode-width` returned 0.2.2; slopcheck rated [OK]]
[CITED: https://github.com/unicode-rs/unicode-width — official rust-lang org repository]

---

## Architecture Patterns

### System Architecture Diagram

```
                    stdin read
                        │
                  ┌─────▼──────┐
                  │ EscapeState │  (main.rs:625)
                  │  machine   │
                  └─────┬──────┘
                        │ bytes_to_forward
                  ┌─────▼──────────────┐
                  │  PredictionOverlay  │  NEW predictor.rs
                  │  .on_input(bytes)   │◄──── conn.rtt() for display gate
                  └──┬──────────────┬──┘
                     │ mark dirty   │ send unchanged
                     │              ▼
                     │        client::send_input()  ──► server bidi stream
                     │
                  ┌──▼──────────────────────────────────┐
                  │  ClientScreen (screen.rs)            │
                  │  confirmed ⊕ [ConnectionLossOverlay] │
                  │             ⊕ [PredictionOverlay]    │◄── cell_at(r,c)
                  │  render_to_stdout()                  │
                  └─────────────────────────────────────┘
                                   │
                             stdout (ANSI)
                                   │
                    ┌──────────────▼────────────────┐
                    │  datagram arm: StateDiff recv  │
                    │  screen.apply(diff)            │
                    │  predictor.cull(diff)          │ confirmed_epoch advance
                    │  send epoch_ack                │
                    └───────────────────────────────┘
```

### Recommended Project Structure

The only new file is `predictor.rs`:

```
crates/nosh-client/src/
├── predictor.rs        [NEW] PredictionOverlay, PendingPrediction, Validity,
│                             PredictDisplayMode, InputClassifier
├── screen.rs           [MODIFY] add PredictionOverlay to overlay stack
├── main.rs             [MODIFY] --predict flag, hook predictor in stdin arm,
│                                call predictor.cull() in datagram arm
└── lib.rs              [MODIFY] pub mod predictor
```

---

## Mosh Model Translation: The Core State Machine

[CITED: https://github.com/mobile-shell/mosh — src/frontend/terminaloverlay.{h,cc} fetched directly via GitHub API 2026-06-02]

### Validity Enum (terminaloverlay.h:56)

Mosh defines five validity states. The Rust translation:

```rust
// crates/nosh-client/src/predictor.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Validity {
    /// Still waiting for server epoch confirmation.
    Pending,
    /// Server confirmed this exact cell content (non-trivial).
    Correct,
    /// Server confirmed but it's trivially correct (blank→blank) — no credit toward
    /// advancing confirmed_epoch.
    CorrectNoCredit,
    /// Server state differs from prediction, or prediction expired.
    IncorrectOrExpired,
    /// Prediction is inactive (not in use).
    Inactive,
}
```

Source: `enum Validity { Pending, Correct, CorrectNoCredit, IncorrectOrExpired, Inactive }` in terminaloverlay.h:56.

### PendingPrediction

```rust
pub struct PendingPrediction {
    /// Screen position.
    pub row: u16,
    pub col: u16,
    /// Predicted cell content.
    pub predicted_ch: char,
    pub col_width: u16,         // 1 or 2 (from unicode-width)
    /// The server epoch at or above which this prediction is confirmed.
    /// Maps to Mosh's `expiration_frame`.
    pub epoch_required: u64,
    /// Minimum epoch to DISPLAY this prediction (tentative gate).
    /// Only display if confirmed_epoch >= tentative_until_epoch.
    /// Maps to Mosh's `tentative_until_epoch`.
    pub tentative_until_epoch: u64,
    /// Whether this is a cursor move (not a cell char) prediction.
    pub is_cursor_move: bool,
}
```

**Key mapping from Mosh:**
- `expiration_frame` (Mosh) → `epoch_required` (nosh): the StateDiff epoch that confirms this prediction
- `tentative_until_epoch` (Mosh) → same name: predictions with `tentative_until_epoch > confirmed_epoch` are hidden (not displayed), matching Mosh's `tentative()` method: `bool tentative(uint64_t confirmed_epoch) const { return tentative_until_epoch > confirmed_epoch; }`

### PredictionOverlay Struct

```rust
pub struct PredictionOverlay {
    /// Monotonic epoch tracking which predictions have been confirmed by server.
    /// Advances when a Correct (non-trivial) prediction is confirmed.
    /// Noecho suppression falls out structurally: if server never echoes, no
    /// Correct confirmation arrives, confirmed_epoch stays at 0, all predictions
    /// remain tentative (hidden).
    confirmed_epoch: u64,
    /// Current prediction epoch (increments on become_tentative / epoch reset).
    prediction_epoch: u64,
    /// All active predictions.
    pending: VecDeque<PendingPrediction>,
    /// Display mode from --predict flag.
    display_mode: PredictDisplayMode,
    /// Whether predictions are currently being displayed (RTT above show threshold).
    srtt_trigger: bool,
    /// Whether unconfirmed predictions should be underlined (RTT above flag threshold).
    flagging: bool,
    /// Whether bracketed paste mode is active (suppress all prediction).
    in_bracketed_paste: bool,
    /// Current predicted cursor position (separate from confirmed cursor).
    predicted_cursor: CursorPos,
}
```

### `become_tentative()` — Epoch Reset

[CITED: terminaloverlay.cc — `PredictionEngine::become_tentative()` at line ~890]

```cpp
void PredictionEngine::become_tentative( void ) {
  if ( display_preference != Experimental ) {
    prediction_epoch++;
  }
}
```

Rust translation:
```rust
fn become_tentative(&mut self) {
    self.prediction_epoch += 1;
    // All pending predictions now have tentative_until_epoch == old prediction_epoch.
    // Since confirmed_epoch < prediction_epoch, they are hidden until the server
    // confirms one prediction from the new epoch, advancing confirmed_epoch.
}
```

Every call to `become_tentative()` increments `prediction_epoch`. New predictions after this point get `tentative_until_epoch = prediction_epoch`. Since `confirmed_epoch` has not yet caught up, these predictions are hidden (`tentative_until_epoch > confirmed_epoch`). The predictor resumes displaying when a server diff arrives confirming a prediction from the new epoch, which sets `confirmed_epoch = tentative_until_epoch` of that prediction.

### `cull()` — Confirmation Flow

[CITED: terminaloverlay.cc — `PredictionEngine::cull()` at line ~470]

The Rust `cull()` equivalent runs in the datagram arm after `screen.apply(diff)`. The confirmed grid is now updated; compare each `PendingPrediction` against `screen.confirmed_cell(row, col)`:

```rust
fn cull(&mut self, screen: &ClientScreen, new_epoch: u64) {
    // 1. Update RTT thresholds (called before each cull).
    // 2. Scan all pending predictions:
    //    - epoch_required <= new_epoch → prediction is due for confirmation.
    //      Compare screen.confirmed_cell(row, col).ch against predicted_ch:
    //        match AND not trivially blank (CorrectNoCredit) → confirmed_epoch = tentative_until_epoch; remove prediction.
    //        match AND trivially blank → remove prediction (no epoch advance).
    //        mismatch → IncorrectOrExpired → call reset() (full overlay reset, not just this cell).
    //    - epoch_required > new_epoch → still Pending; leave in place.
    // 3. Remove all predictions where Correct/CorrectNoCredit (already consumed).
}
```

**Critical Pitfall 4 mapping (datagram loss ≠ confirmation loss):**

The `epoch_required` check is `epoch_required <= new_epoch`, NOT `epoch_required == new_epoch`. If datagram epoch N is lost and epoch N+2 arrives, any prediction with `epoch_required <= N+2` is confirmed by epoch N+2. This tolerates multiple consecutive drops without a reset — matching RESEARCH.md Pitfall 4 requirement.

**Incorrect prediction handling — full reset, not partial:**

From Mosh `cull()`: when an `IncorrectOrExpired` NON-tentative cell is found, the response is `reset()` (clear all predictions), not just `j->reset()` (clear that cell). This is the correct behavior — a mismatch on any confirmed cell means the server state diverged and all speculative work is invalid. In Rust:

```rust
case IncorrectOrExpired (non-tentative) → self.reset(); return;
case IncorrectOrExpired (tentative) → self.kill_epoch(tentative_until_epoch); // prune that epoch's predictions
```

### Noecho Suppression — Structural, Not Explicit

[CITED: FEATURES.md + direct Mosh source analysis — confirmed 2026-06-02]

This is the most important insight for PREDICT-04. Mosh does NOT track a noecho flag explicitly. The mechanism is structural:

1. User types 'a' in `read -s` → `on_input('a')` → creates a `PendingPrediction` with `tentative_until_epoch = current_prediction_epoch`.
2. Server receives 'a' via PTY → PTY's `stty -echo` suppresses echo → server's TerminalState does NOT change at that position → StateDiff epoch advances but the cell at cursor position is unchanged (still shows the prompt, not 'a').
3. Client receives StateDiff with new epoch → `cull()` runs → prediction for 'a' has `epoch_required <= new_epoch` → compare: `screen.confirmed_cell(row, col).ch != 'a'` → **IncorrectOrExpired** → `reset()`.
4. After reset, `prediction_epoch` is incremented (via `become_tentative`), `confirmed_epoch` is still 0 → all new predictions are tentative → nothing is displayed.
5. The user types more chars, each triggers a new prediction, each is quickly culled as IncorrectOrExpired → `confirmed_epoch` never advances → nothing is ever displayed.

**What to test:** After entering a `read -s` context, type characters, run `cull()` with each incoming StateDiff, assert that `pending` list is empty and zero cells are displayed. The invariant to assert is `confirmed_epoch < prediction_epoch` throughout the noecho period.

**Security validation (D-15-01c):** The adversarial test must actually run a `read -s` on the server PTY and confirm the client screen contains no predicted characters. This is a phase-gate requirement per CONTEXT.md.

---

## SRTT Estimation

[VERIFIED: https://docs.rs/quinn/latest/quinn/struct.Connection.html — `rtt()` method returning `Duration`]
[VERIFIED: https://docs.rs/quinn/latest/quinn/struct.PathStats.html — `path.rtt: Duration` field]

### Quinn RTT API

Quinn exposes RTT via two equivalent paths:

```rust
// Path 1: direct method (simplest)
let rtt: Duration = conn.rtt();

// Path 2: via PathStats
let rtt: Duration = conn.stats().path.rtt;
```

Both return "current best estimate of this connection's latency (round-trip-time)." Quinn's internal RTT is computed using QUIC's built-in RTT measurement (RFC 9002 §5) — it is already a smoothed estimate equivalent to SRTT. No additional EWMA is needed.

**Note:** `PathStats` has no separate `min_rtt` or `srtt` field — only `rtt: Duration`. For the Mosh threshold comparisons, use `conn.rtt().as_millis() as u64` directly.

### Mosh Threshold Constants

[CITED: deepwiki.com/mobile-shell/mosh/4.2-predictive-overlay-system — thresholds verified against FEATURES.md research]
[CITED: terminaloverlay.cc — `cull()` method showing `send_interval > SRTT_TRIGGER_HIGH` logic]

```rust
const SRTT_TRIGGER_HIGH_MS: u64 = 30;   // Activate predictions above this
const SRTT_TRIGGER_LOW_MS: u64 = 20;    // Deactivate below this (when no active predictions)
const FLAG_TRIGGER_HIGH_MS: u64 = 80;   // Start underlining above this
const FLAG_TRIGGER_LOW_MS: u64 = 50;    // Stop underlining below this
```

### RTT Update Logic (inside `cull()`)

```rust
fn update_rtt_thresholds(&mut self, rtt_ms: u64) {
    // Hysteresis: srtt_trigger activates above HIGH, deactivates below LOW
    // (but only when no predictions are currently being shown).
    if rtt_ms > SRTT_TRIGGER_HIGH_MS {
        self.srtt_trigger = true;
    } else if self.srtt_trigger && rtt_ms <= SRTT_TRIGGER_LOW_MS && self.pending.is_empty() {
        self.srtt_trigger = false;
    }

    // Flagging (underline): no "active prediction" guard (flagging can turn off faster).
    if rtt_ms > FLAG_TRIGGER_HIGH_MS {
        self.flagging = true;
    } else if rtt_ms <= FLAG_TRIGGER_LOW_MS {
        self.flagging = false;
    }
}
```

This is a direct translation of Mosh's `cull()` hysteresis block (terminaloverlay.cc:~490-510).

### Display Gate

```rust
fn should_display(&self) -> bool {
    match self.display_mode {
        PredictDisplayMode::Always => true,
        PredictDisplayMode::Never => false,
        PredictDisplayMode::Adaptive => self.srtt_trigger,
    }
}
```

When `!should_display()`, `PredictionOverlay::cell_at()` returns `None` for all positions — the overlay contributes nothing to the compositor.

---

## Input Classification

[CITED: terminaloverlay.cc — `new_user_byte()` at line ~580, direct source fetch 2026-06-02]

The Mosh source uses its own VT parser to classify each input byte. For nosh, `vte` is server-only; the client predictor needs only a minimal byte-level input classifier, NOT a full VT parser.

### Input Classes

Based on the Mosh `new_user_byte()` source, the classification is:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputAction {
    /// Predict this printable char at cursor, advance cursor by col_width.
    PredictChar { ch: char, col_width: u16 },
    /// Predict backspace: move cursor left 1, blank that cell (or shift row).
    PredictBackspace,
    /// Predict cursor left 1 column (←).
    PredictCursorLeft,
    /// Predict cursor right 1 column (→).
    PredictCursorRight,
    /// Predict cursor to start of line (Home / Ctrl-A → col=0).
    PredictLineStart,
    /// Predict cursor to end of confirmed row content (End / Ctrl-E).
    PredictLineEnd,
    /// Reset the prediction epoch; display nothing until server confirms.
    EpochReset,
    /// Begin bracketed paste — suppress all prediction.
    BracketedPasteStart,
    /// End bracketed paste — re-enable prediction (after server confirms).
    BracketedPasteEnd,
    /// Input suppressed (bulk batch > threshold bytes).
    BulkSuppressed,
}
```

### Classifier Logic

```rust
fn classify_input(bytes: &[u8]) -> InputAction {
    // Bulk suppression: D-15-01b. Threshold: >4 bytes in one read batch.
    if bytes.len() > 4 {
        return InputAction::BulkSuppressed;  // → EpochReset before send
    }

    // Single byte or known sequences:
    match bytes {
        [0x7f] | [0x08]                  => InputAction::PredictBackspace,
        [0x01]                           => InputAction::PredictLineStart,    // Ctrl-A
        [0x05]                           => InputAction::PredictLineEnd,      // Ctrl-E
        [b'\r'] | [b'\n']               => InputAction::EpochReset,          // Enter
        [0x1b, b'[', b'C']              => InputAction::PredictCursorRight,  // CSI C (→)
        [b'\x1b', b'O', b'C']           => InputAction::PredictCursorRight,  // App-mode →
        [0x1b, b'[', b'D']              => InputAction::PredictCursorLeft,   // CSI D (←)
        [b'\x1b', b'O', b'D']           => InputAction::PredictCursorLeft,   // App-mode ←
        [0x1b, b'[', b'H'] | [0x1b, b'[', b'1', b'~']  // CSI H / CSI 1~
            | [0x1b, b'O', b'H']        => InputAction::PredictLineStart,    // Home
        [0x1b, b'[', b'F'] | [0x1b, b'[', b'4', b'~']  // CSI F / CSI 4~
            | [0x1b, b'O', b'F']        => InputAction::PredictLineEnd,      // End
        // Bracketed paste markers.
        b"\x1b[200~"                    => InputAction::BracketedPasteStart,
        b"\x1b[201~"                    => InputAction::BracketedPasteEnd,
        // Any other ESC sequence → epoch reset.
        [0x1b, ..]                      => InputAction::EpochReset,
        // Any other control char (< 0x20, not handled above) → epoch reset.
        [b] if *b < 0x20               => InputAction::EpochReset,
        // Single printable char (or multi-byte UTF-8 char).
        _ => classify_printable(bytes),
    }
}

fn classify_printable(bytes: &[u8]) -> InputAction {
    // Must be valid UTF-8 and a single Unicode scalar.
    if let Ok(s) = std::str::from_utf8(bytes) {
        let mut chars = s.chars();
        if let (Some(ch), None) = (chars.next(), chars.next()) {
            // Use unicode-width to determine column width.
            match unicode_width::UnicodeWidthChar::width(ch) {
                Some(1) => return InputAction::PredictChar { ch, col_width: 1 },
                Some(2) => return InputAction::PredictChar { ch, col_width: 2 },
                // width=0 (combining/ZWJ), None (control), or >2 → epoch reset.
                _ => {}
            }
        }
    }
    InputAction::EpochReset
}
```

**Note on multi-byte input in one read:** When the OS delivers a multi-byte UTF-8 char as a single `stdin.read()` call (e.g., CJK character = 3 bytes), it arrives as `bytes.len() == 3`. The bulk threshold of `> 4 bytes` is chosen so CJK single-char input (`len==3`) is NOT treated as bulk. If the threshold were `> 3`, CJK would be suppressed. At `> 4`, a 3-byte CJK char passes to `classify_printable` which correctly identifies it as `Some(2)` wide.

---

## unicode-width Integration

[VERIFIED: crates.io registry — `unicode-width` 0.2.2, slopcheck [OK], confirmed 2026-06-02]
[CITED: https://docs.rs/unicode-width/0.2.2/unicode_width/trait.UnicodeWidthChar.html]

### API

```rust
use unicode_width::UnicodeWidthChar;

// Returns Option<usize>:
// None      → control character (do not predict)
// Some(0)   → combining mark / ZWJ / zero-width (do not predict → epoch reset)
// Some(1)   → narrow (width-1) → predict with col_width=1
// Some(2)   → wide / CJK (width-2) → predict with col_width=2
let w: Option<usize> = UnicodeWidthChar::width(ch);

// CJK-aware variant (ambiguous-width chars treated as 2):
let w_cjk: Option<usize> = UnicodeWidthChar::width_cjk(ch);
```

**Do NOT use `width_cjk()`** for nosh. Ambiguous-width characters should trigger epoch-reset (conservative, D-15-03), not be treated as width-2. Use `width()` only.

### Conservative Predicate

```rust
fn is_predictable(ch: char) -> Option<u16> {
    match UnicodeWidthChar::width(ch) {
        Some(1) => Some(1),  // narrow → predict
        Some(2) => Some(2),  // CJK wide → predict
        _       => None,     // control/combining/ZWJ/ambiguous → epoch reset
    }
}
```

### Cursor Advance for CJK

When `col_width == 2` and the predicted cursor is at column C:
- Place the predicted char at column C.
- The "continuation cell" at column C+1 is conventionally blank (wide char occupies two cells).
- Advance predicted cursor to C+2.
- If C+1 >= terminal width → `become_tentative()` (line wrap is unpredictable for wide chars).

---

## Overlay Integration with Phase 14 ClientScreen

[CITED: crates/nosh-client/src/screen.rs — `Overlay` trait at line 78, `overlays: Vec<Box<dyn Overlay>>` at line 117, `compose_desired()` at line 271]

### The Seam

`screen.rs:78`:
```rust
pub trait Overlay {
    fn cell_at(&self, row: u16, col: u16) -> Option<Cell>;
}
```

`screen.rs:117`:
```rust
overlays: Vec<Box<dyn Overlay>>,
```

Phase 14 pre-loads `ConnectionLossOverlay` (no-op) at index 0. Phase 15 adds `PredictionOverlay` at index 1 (applied after ConnectionLossOverlay, before final render).

### PredictionOverlay::cell_at()

```rust
impl Overlay for PredictionOverlay {
    fn cell_at(&self, row: u16, col: u16) -> Option<Cell> {
        if !self.should_display() {
            return None;
        }
        // Find a non-tentative active prediction at (row, col).
        for pred in &self.pending {
            if pred.row == row && pred.col == col
                && !self.is_tentative(pred)
                && pred.is_active()
            {
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

fn is_tentative(&self, pred: &PendingPrediction) -> bool {
    pred.tentative_until_epoch > self.confirmed_epoch
}
```

### Cursor Position for Render

`render_to_stdout` currently uses `self.confirmed_cursor` (from `screen.rs:304`). Phase 15 must pass the PREDICTED cursor position when there are active non-tentative cursor predictions. The cleanest approach: add a `cursor_override() -> Option<CursorPos>` method to the `Overlay` trait (or a separate `PredictionOverlay::predicted_cursor()` method consulted by `render_to_stdout`).

The simplest approach without modifying the Overlay trait: extend `ClientScreen` to accept a cursor override from the predictor, checked after overlay composition. This requires a small addition to `ClientScreen::render_to_stdout()`.

---

## run_pump Integration

[CITED: crates/nosh-client/src/main.rs — `run_pump` at line 608, stdin arm at lines 721-737, datagram arm at lines 678-717]

### Stdin Arm (add predictor hook)

Current code at `main.rs:724-734`:
```rust
let result = escape.process(&stdin_buf[..n]);
if result.quit { return Ok(PumpOutcome::UserQuit); }
if !result.bytes_to_forward.is_empty()
    && client::send_input(send, &result.bytes_to_forward).await.is_err()
{
    return Ok(PumpOutcome::TransportDrop);
}
```

Phase 15 adds predictor between escape machine and send_input:
```rust
let result = escape.process(&stdin_buf[..n]);
if result.quit { return Ok(PumpOutcome::UserQuit); }

// Hook predictor AFTER escape machine, BEFORE send_input.
// The escape machine has already consumed ~. and ~~; bytes_to_forward
// are the raw keystrokes going to the server.
if !result.bytes_to_forward.is_empty() {
    predictor.on_input(&result.bytes_to_forward, &screen);
    // Mark screen dirty → render will pick up new overlay state.
    let mut buf: Vec<u8> = Vec::new();
    screen.render_to_stdout(&mut buf).unwrap_or_else(|e| tracing::warn!("render: {e}"));
    if !buf.is_empty() { /* async flush */ }

    if client::send_input(send, &result.bytes_to_forward).await.is_err() {
        return Ok(PumpOutcome::TransportDrop);
    }
}
```

### Datagram Arm (add cull)

Current code at `main.rs:681-704` applies diff and re-renders. Add `predictor.cull()` call:
```rust
if diff.epoch > screen.last_applied_epoch() {
    screen.apply(&diff);
    let rtt_ms = conn.rtt().as_millis() as u64;
    predictor.cull(&screen, diff.epoch, rtt_ms);  // confirm/prune predictions
    let mut buf: Vec<u8> = Vec::new();
    screen.render_to_stdout(&mut buf).unwrap_or_else(|e| tracing::warn!("render: {e}"));
    /* async flush */
    let ack_payload = nosh_proto::datagram::encode_epoch_ack(diff.epoch);
    let _ = conn.send_datagram(ack_payload);
}
```

### CLI Arg Addition

```rust
// Add to Args struct (main.rs:282):
/// Prediction display mode. Adaptive (default) shows predictions only on
/// high-latency links (>30 ms RTT). Always or never override.
#[arg(long, default_value = "adaptive")]
predict: PredictMode,  // enum: Always, Adaptive, Never
```

---

## Common Pitfalls

### Pitfall 1: IncorrectOrExpired Triggers Only Partial Reset

**What goes wrong:** When a non-tentative cell prediction mismatches the server's confirmed state, only that cell's prediction is cleared — other predictions remain visible. The user sees partially-stale predictions mixed with confirmed content.

**Why it happens:** Thinking of predictions as independent entities. Mosh's lesson (and direct source code evidence) is that a confirmed mismatch on any non-tentative cell means the entire overlay is invalid. `reset()` clears ALL predictions; `kill_epoch()` clears predictions from a specific epoch.

**How to avoid:** In `cull()`, when `Validity::IncorrectOrExpired` and the prediction is NOT tentative → call `self.reset()` and return immediately. Do not clear individual predictions.

**Warning signs:** `VecDeque::remove(i)` called inside a for loop on mismatch; no early return from `cull()` on first non-tentative mismatch.

### Pitfall 2: Epoch Confirmation Requires the Wrong Epoch Number

**What goes wrong:** The implementation confirms predictions when `diff.epoch == pred.epoch_required` (exact match). A dropped datagram with `epoch == pred.epoch_required` means the prediction stays Pending forever, eventually expiring and causing a visible flicker.

**Why it happens:** Confusing "epoch required for confirmation" with "exact epoch that confirms."

**How to avoid:** Confirmation check is `diff.epoch >= pred.epoch_required` (greater-than-or-equal), not equality. Any datagram with a fresh enough epoch confirms all older pending predictions.

**Warning signs:** `if diff.epoch == pred.epoch_required` in the cull loop.

### Pitfall 3: Predicted Cursor Used for Overlay — But Physical Cursor Not Updated

**What goes wrong:** The predictor advances its local `predicted_cursor` as the user types. `render_to_stdout()` uses `self.confirmed_cursor` (from the confirmed grid). The terminal shows the cursor at the server's confirmed position — lagging behind the predicted characters — making the predicted text appear at the wrong position relative to the cursor.

**Why it happens:** The overlay replaces CELL content but the cursor position comes from `confirmed_cursor` (screen.rs:304). These must be coordinated.

**How to avoid:** Add a cursor override mechanism so when non-tentative cursor predictions exist, `render_to_stdout()` emits `MoveTo(predicted_cursor.col, predicted_cursor.row)` as the final cursor position instead of `confirmed_cursor`. Implement as a method on `PredictionOverlay` that `run_pump` passes to the screen's render step, or extend `ClientScreen::render_to_stdout` to accept an optional cursor override.

**Warning signs:** Predicted chars appear at correct grid positions but terminal cursor is always at the server's last-confirmed cursor position.

### Pitfall 4: Backspace Prediction Shifts vs. Overwrites

**What goes wrong:** Backspace in insert mode (the common shell case) shifts all content to the right of cursor left by one and leaves the rightmost cell blank. Implementing it as "blank the cell to the left of cursor" misses the shift and produces a corrupt row for all characters after the deleted one.

**Why it happens:** Mosh's source (terminaloverlay.cc:~630) shows two modes: `predict_overwrite` (simple — blank previous cell) and insert mode (complex — shift all cells left). For a shell prompt context, the insert mode is closer to reality.

**How to avoid:** For the initial implementation, use the conservative Mosh approach: predict cursor move left by 1, mark the vacated column as `unknown` (the content there after backspace depends on whether readline is in insert vs. overwrite mode). Marking as `unknown` means `apply()` won't render the unknown cell content — only the cursor move is displayed. This avoids the shift complexity while still providing the cursor-feel benefit of backspace prediction.

**Warning signs:** Backspace prediction blanks `cell[cursor.col - 1]` without shifting; visible corruption when user types after backspace.

### Pitfall 5: Bracketed Paste Detection Must Track Server-Side State

**What goes wrong:** Bracketed paste mode (`CSI ?2004h`) is enabled by the server-side shell (e.g., bash, fish) via PTY output. The client cannot know it is in bracketed paste mode until the server's TerminalState diff reflects it. If the client only looks at its own input stream for `\x1b[200~` (paste start), it may miss mode changes that happen before any paste.

**Why it happens:** The client needs to detect BOTH:
1. When the user starts a paste (`\x1b[200~` in the input stream) — D-15-01b.
2. When the server has enabled bracketed paste mode (`\x1b[?2004h` in PTY output) — the server's TerminalState tracks this.

For Phase 15, the server's `TerminalState` already tracks the bracketed paste DEC mode (from Phase 12 vte implementation). However, the StateDiff wire format does not currently carry mode bits.

**How to avoid:** Detect paste start/end from the CLIENT'S OWN INPUT stream: `\x1b[200~` (start) and `\x1b[201~` (end). In raw mode, these sequences arrive from the terminal emulator in the stdin stream when the user pastes. This is sufficient for Phase 15 (the client detects its own paste). The server-side mode tracking is secondary.

**Warning signs:** Prediction runs during a paste because `\x1b[200~` from stdin is not classified as `BracketedPasteStart`.

### Pitfall 6: Wide-Char Prediction at Terminal Right Edge

**What goes wrong:** User types a CJK wide char when `predicted_cursor.col == terminal_width - 1`. The char occupies columns N and N+1, but column N+1 is beyond the terminal width. The terminal wraps, and the actual cursor position after the wide char is unpredictable (depends on terminal emulator wrap behavior).

**Why it happens:** Wide chars at the rightmost column trigger wrap behavior that varies between emulators (some eat the right column as the wide-char left half, some push to a new line).

**How to avoid:** When `predicted_cursor.col + col_width > terminal_width`, call `become_tentative()` instead of predicting. Matches Mosh's `become_tentative()` for last-column predictions.

**Warning signs:** No check of `predicted_cursor.col + col_width` against terminal width before inserting a wide-char prediction.

---

## Don't Hand-Roll

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| Unicode column width | Custom wcwidth table | `unicode-width` 0.2.2 | Correct UAX#11 implementation; handles all edge cases including Unicode 15.x |
| RTT smoothing | Application-level EWMA on datagram timestamps | `conn.rtt()` | Quinn already computes SRTT per RFC 9002; adding a second EWMA introduces noise |
| VT input parser | Full vte parser for client-side input | Byte-level classifier (`classify_input`) | Client only needs to classify ~15 specific byte sequences; full vte is overkill and would couple to the server-side vte version |

---

## Code Examples

### Confirmed Epoch Advance in cull()

```rust
// Source: direct translation of Mosh terminaloverlay.cc cull() confirmed_epoch advance
// When a Correct (non-trivial) prediction is confirmed:
if validity == Validity::Correct {
    if pred.tentative_until_epoch > self.confirmed_epoch {
        self.confirmed_epoch = pred.tentative_until_epoch;
        // All predictions with tentative_until_epoch <= new confirmed_epoch
        // are now displayable (tentative gate removed).
    }
}
```

### Tentative Check

```rust
// Source: Mosh terminaloverlay.h:68
// bool tentative( uint64_t confirmed_epoch ) const {
//   return tentative_until_epoch > confirmed_epoch;
// }
fn is_tentative(&self, pred: &PendingPrediction) -> bool {
    pred.tentative_until_epoch > self.confirmed_epoch
}
```

### RTT Threshold Check

```rust
// Source: terminaloverlay.cc cull() hysteresis block
let rtt_ms = conn.rtt().as_millis() as u64;
if rtt_ms > SRTT_TRIGGER_HIGH_MS {
    self.srtt_trigger = true;
} else if self.srtt_trigger
    && rtt_ms <= SRTT_TRIGGER_LOW_MS
    && self.pending.is_empty()   // only turn off when no predictions shown
{
    self.srtt_trigger = false;
}
```

### unicode-width column advance

```rust
// Source: https://docs.rs/unicode-width/0.2.2/unicode_width/trait.UnicodeWidthChar.html
use unicode_width::UnicodeWidthChar;

fn advance_cursor_col(col: u16, ch: char, term_width: u16) -> Option<u16> {
    match UnicodeWidthChar::width(ch) {
        Some(1) => Some(col + 1),
        Some(2) if col + 2 <= term_width => Some(col + 2),
        _ => None, // epoch reset
    }
}
```

---

## Adversarial Test Strategy

[CITED: D-15-04 validation matrix from 15-CONTEXT.md]

`nyquist_validation` is `false` in `.planning/config.json` — the Validation Architecture section is omitted. However, the D-15-04 matrix is a hard phase gate, so tests are mandatory and documented here for the planner.

### Test Harness Pattern

The existing Phase 14 integration test pattern spawns a real `nosh-server` and connects a `nosh-client`. For Phase 15, add a `PredictionHarness` that:

1. Wraps a `ClientScreen` + `PredictionOverlay`.
2. Feeds fake `StateDiff` datagrams (constructed via `encode_datagram`) with controlled epochs.
3. Feeds fake keystrokes to `predictor.on_input()`.
4. Exposes `predicted_cell(row, col)` and `predictor.confirmed_epoch()` for assertions.

**Simulated-loss test:** Inject StateDiff with epoch N, then epoch N+2 (skipping N+1). Assert predictions expecting epoch N are confirmed by N+2 (not reset).

### D-15-04 Matrix — Test Cases

| Case | Setup | Assert |
|------|-------|--------|
| vim `iHello<Esc>` | Feed 'i' → epoch reset. Feed 'H','e','l','l','o' (each with server confirm between). Feed `\x1b` (ESC) → epoch reset. | After ESC: `pending.is_empty()` or all tentative. Zero corrupt cells in screen. |
| `read -s` noecho | Server StateDiff never echoes typed chars at cursor pos. Feed chars, feed new-epoch StateDiff without char change at cursor. | `cell_at(cursor_row, cursor_col) == None` for all typed chars. |
| CJK `你好` | Feed 3-byte UTF-8 sequence for '你' (width=2). | `predictor.predicted_cursor.col += 2`. Cell at (row, col) shows '你'. Cell at (row, col+1) shows blank (continuation). |
| `less` (cursor-addressing) | Feed `\x1b[H` (cursor home CSI) → epoch reset. | `pending.is_empty()` immediately after. |
| Bracketed paste | Feed `\x1b[200~`, then chars, then `\x1b[201~`. | During paste: `cell_at()` returns `None` for all chars. After paste end: epoch reset. |
| Ctrl-C mid-line | Feed `\x03` → epoch reset. | `pending.is_empty()` or all tentative after Ctrl-C. |
| Simulated 30% loss | Feed 5 keystrokes, confirm epochs 1,3,5 (2,4 dropped). | All predictions for epochs 1,3,5 confirmed. Epochs 2,4 predictions also confirmed by 3,5 (>= check). Zero stale predictions after epoch 5. |
| Home / End | Feed `\x1b[H` (Home) → `predicted_cursor.col = 0`. Feed `\x1b[F` (End) → `predicted_cursor.col = row_content_length`. | Cursor lands on correct column after server confirm. |

---

## State of the Art

| Old Approach | Current Approach | When Changed | Impact |
|--------------|------------------|--------------|--------|
| Mosh custom UDP SSP | QUIC RFC 9221 datagrams | nosh design (2024) | No separate port, QUIC-authenticated, same MTU handling |
| Client runs full terminal emulator | Client uses server StateDiff (struct diff, no vte client-side) | nosh design | No double-parse; prediction against cell grid not byte stream |
| vte client-side input parsing | Byte-level classifier | Phase 15 design | Simpler, no coupling to server vte version |

**Note on `wcwidth` vs `unicode-width`:** Mosh uses POSIX `wcwidth()` for column width, which is locale-dependent and can return wrong values for ambiguous-width chars. The `unicode-width` Rust crate always uses the UAX#11 non-CJK context by default (same as `wcwidth()` in non-CJK locale), but is locale-independent. Behavior for nosh: `width()` (not `width_cjk()`) matches `wcwidth()` in a typical UTF-8 non-CJK locale. Correct for the conservative policy.

---

## Open Questions

1. **Predicted cursor in render_to_stdout**
   - What we know: `render_to_stdout` emits `MoveTo(confirmed_cursor.col, confirmed_cursor.row)` at line 351. No mechanism currently passes a predicted cursor to this function.
   - What's unclear: The cleanest extension point — add a cursor override parameter, or add a `Overlay::cursor_override() -> Option<CursorPos>` method.
   - Recommendation: Add a `fn predicted_cursor(&self) -> Option<CursorPos>` to `PredictionOverlay` and extend `ClientScreen` with a method to accept an external cursor position for the final MoveTo. Keep the `Overlay` trait unchanged to avoid breaking `ConnectionLossOverlay`.

2. **Backspace insert vs. overwrite prediction complexity**
   - What we know: Mosh models full insert-mode backspace (shifting cells). The nosh implementation can start with cursor-only backspace (move left, mark unknown) to avoid the complexity.
   - Recommendation: Start with cursor-only backspace (move predicted cursor left 1, no cell prediction). This is D-15-01 compliant and avoids the shift complexity. If user feedback reveals the cursor-lag is noticeable, add the cell prediction in a follow-on.

3. **Home/End "end of content" detection**
   - What we know: End (and Ctrl-E) moves the cursor to the end of the current line's content. The confirmed grid has `confirmed_cell(row, col)` but the "end of typed content" is not directly tracked.
   - Recommendation: For End/Ctrl-E prediction, scan the confirmed row from right-to-left to find the last non-blank non-predicted cell and set `predicted_cursor.col` to that position + 1. This is an approximation; epoch-reset on mismatch corrects any error.

---

## Project Constraints (from CLAUDE.md)

- **Language:** Rust (locked); no new language dependencies.
- **QUIC transport:** quinn 0.11.9; datagram display path only; keystrokes on reliable stream.
- **Single display path:** `render_to_stdout` is the only writer to stdout; prediction is an overlay, not a separate display path.
- **Security:** Never forward `SSH_AUTH_SOCK` via env; env sanitization on every shell open (Phase 2 requirement, not Phase 15 scope but must not regress).
- **vte is server-side only:** The client predictor must NOT add a vte dependency; classification is byte-level only.
- **`CellStyle::UNDERLINE = 0x04`** already exists in `nosh-proto::datagram::CellStyle` — reuse it.
- **No direct stdout writes:** All display through `ClientScreen::render_to_stdout`. Prediction hook must not add any `stdout.write_all` calls outside that path.

---

## Assumptions Log

| # | Claim | Section | Risk if Wrong |
|---|-------|---------|---------------|
| A1 | Backspace prediction in insert mode (shell prompt) is cursor-only (move left), not full row-shift | Code Examples / Pitfall 4 | If readline always inserts, the predicted row content would be incorrect after a backspace followed by more typing — visible as extra character overlap until server confirms |
| A2 | Home/End scan of confirmed row provides adequate "end of content" estimate | Open Questions | If row content has trailing spaces that were once typed content, cursor lands one position early — minor and corrected by server confirm |
| A3 | The existing Phase 12 `TerminalState` on the server tracks DEC mode `?2004` (bracketed paste enable). Not needed for Phase 15 client detection but assumed for completeness. | Pitfall 5 | If server does not track bracketed paste mode, Phase 16 (QoL) may need to add it — no Phase 15 impact |

**Claims that were verified or cited: all library APIs and Mosh source citations are [VERIFIED] or [CITED]. The assumption table covers only design choices not traceable to primary sources.**

---

## Environment Availability

Step 2.6: SKIPPED (Phase 15 is code-only changes; no external tools or services beyond existing nosh build environment; `unicode-width` is a Cargo dependency only).

---

## Sources

### Primary (HIGH confidence)
- Mosh `terminaloverlay.cc` — fetched via GitHub API from `mobile-shell/mosh@master` `src/frontend/terminaloverlay.cc` 2026-06-02. Contains: `PredictionEngine::cull()`, `become_tentative()`, `new_user_byte()`, `kill_epoch()`, `reset()`, `active()`.
- Mosh `terminaloverlay.h` — fetched same path. Contains: `Validity` enum, `ConditionalOverlay::tentative()`, `expiration_frame`, `tentative_until_epoch`.
- `crates/nosh-client/src/screen.rs` — `Overlay` trait (line 78), `overlays: Vec<Box<dyn Overlay>>` (line 117), `compose_desired()` (line 271), `render_to_stdout()` (line 302), `confirmed_cursor` (line 351).
- `crates/nosh-client/src/main.rs` — `EscapeState` machine (lines 90-165), `run_pump` (line 608), stdin arm (lines 721-737), datagram arm (lines 678-717).
- `crates/nosh-proto/src/datagram.rs` — `CellStyle::UNDERLINE = 0x04` (line 137), `StateDiff` (line 51), `encode_epoch_ack` (line 161).
- `crates/nosh-client/Cargo.toml` — confirmed `unicode-width` is NOT yet present; must be added.
- `https://docs.rs/quinn/latest/quinn/struct.Connection.html` — `rtt()` method, `stats()` → `ConnectionStats`.
- `https://docs.rs/quinn/latest/quinn/struct.PathStats.html` — `path.rtt: Duration` (only RTT field; no min_rtt or srtt).
- `https://docs.rs/unicode-width/0.2.2/unicode_width/trait.UnicodeWidthChar.html` — `width() -> Option<usize>`, `width_cjk() -> Option<usize>`.

### Secondary (MEDIUM confidence)
- deepwiki.com/mobile-shell/mosh/4.2-predictive-overlay-system — SRTT thresholds (30/20/80/50ms), Validity enum states, cull() overview. Cross-verified against Mosh source.
- `.planning/research/FEATURES.md` — RTT thresholds, noecho mechanism description.
- `.planning/research/PITFALLS.md` — Pitfall 1 (cursor-addressing), Pitfall 2 (CJK), Pitfall 3 (paste), Pitfall 4 (datagram loss).

### Tertiary (LOW confidence)
- None.

---

## Metadata

**Confidence breakdown:**
- Mosh model translation: HIGH — primary source (terminaloverlay.cc/h fetched directly)
- quinn RTT API: HIGH — docs.rs verified
- unicode-width API: HIGH — docs.rs + crates.io verified; slopcheck [OK]
- Noecho suppression mechanism: HIGH — directly inferred from Mosh source
- Input classifier byte patterns: MEDIUM — verified for common cases; edge cases (application-mode cursor key variants) may need runtime adjustment
- Adversarial test strategy: MEDIUM — pattern established from Phase 14 test harness; Phase 15 specifics are new

**Research date:** 2026-06-02
**Valid until:** 2026-08-01 (Mosh model is stable; quinn/unicode-width APIs are stable)
