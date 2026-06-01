# Phase 12: Server Terminal State Model — Research

**Researched:** 2026-06-01
**Domain:** vte 0.15.0 Perform trait; terminal grid + scrollback data structures; CSI private-mode dispatch; SGR style mapping to CellStyle; SessionSlot integration
**Confidence:** HIGH

---

<user_constraints>
## User Constraints (from CONTEXT.md)

### Locked Decisions

- **D-12-01:** Echo-state = observable private modes from PTY output (DECTCEM `?25`, alt-screen
  `?1049`, bracketed-paste `?2004`, app-cursor-keys `?1`). NOT a termios probe.
- **D-12-01a:** True termios `ECHO` (password / `read -s`) is not observable from the master
  output stream. Do NOT add a termios slave-side probe.
- **D-12-02:** `TerminalState` models the visible viewport grid AND bounded scrollback history.
  Scrollback retained lines in the server model only (not synced to client this phase).
- **D-12-02a:** Datagram path (Phase 13) syncs ONLY the visible viewport. Scrollback sync is M5.
- **D-12-02b:** Handle common VT subset only (text, CSI A/B/C/D/H, erase, SGR, OSC 0/2, OSC 52).
  Unknown/exotic (sixel, DCS, mouse) ignored with a documented scope-fence comment.
- **D-12-03:** Resize = resize grid dimensions and let the app repaint via SIGWINCH. NO text
  reflow. New dimensions ride in the next StateDiff epoch.
- **D-12-04:** OSC 52 (clipboard-write) is DETECTABLE at `osc_dispatch`. Parse only; actual
  passthrough is Phase 16.
- **D-12-05:** Add `push_output_and_parse` on `SessionSlot` feeding BOTH `SequencedOutputBuffer`
  (UNCHANGED) AND the new `TerminalState`. Three existing `slot.push_output(&data)` callsites in
  `server.rs` (~414, ~504, ~786) become the integration points.
- **VT parser: `vte` 0.15.0.** NOT termwiz. Add as dependency.
- Unit-tested in isolation before any QUIC plumbing.

### Claude's Discretion

- Cell/grid representation, style/attribute storage, scrollback cap value and data structure.
- Exact `vte::Perform` method bodies for each handled sequence.
- How `TerminalState` exposes its grid for Phase 13 diff computation.

### Deferred Ideas (OUT OF SCOPE)

- Computing/emitting `StateDiff` from `TerminalState` over datagrams — Phase 13 (SYNC-03).
- Scrollback sync to the client — M5.
- OSC 52 clipboard passthrough behavior + terminal title propagation — Phase 16.
- Client-side use of any of this — Phase 14/15.
</user_constraints>

<phase_requirements>
## Phase Requirements

| ID | Description | Research Support |
|----|-------------|------------------|
| SYNC-02 | Server maintains an authoritative terminal-state model (grid + cursor + echo state) fed from the same PTY-output call site as the `SequencedOutputBuffer`, unit-tested against known VT sequences | vte 0.15.0 Perform trait API verified; CSI/OSC dispatch signatures confirmed; CellStyle mapping documented; SessionSlot integration pattern identified |
</phase_requirements>

---

## Summary

Phase 12 builds a `TerminalState` struct that implements `vte::Perform` (0.15.0) and is fed raw
PTY bytes from the same `SessionSlot::push_output` callsite as the existing
`SequencedOutputBuffer`. The parser receives raw bytes via `Parser::advance(&mut performer,
bytes)` — a slice-at-a-time call that processes the full Paul Williams state machine and fires
callbacks into the `Perform` impl.

The vte 0.15.0 `Perform` trait API has been VERIFIED directly from docs.rs source. Every method
signature is confirmed including the flagged `osc_dispatch` signature. The `intermediates`
parameter in `csi_dispatch` carries the `b'?'` byte for DEC private mode sequences (DECSET/DECRST
`CSI ? Pm h/l`) — this is how the four echo-state modes are detected. `Params` iterates as
`&[u16]` slices (parameter + subparameters); the simple numeric value for a mode like `?25` is in
the first element of the first `params.iter().next()` slice.

The Phase 11 `DiffRun` struct (as fixed AFTER the first draft of this research) has
`fg: Option<u8>` and `bg: Option<u8>`, where `None` = the terminal's default color and `Some(n)`
= explicit palette index `n` (0..=255). Crucially `Some(0)` (explicit black) is DISTINCT from
`None` (default). `CellStyle` is a `u8` bitflags newtype with BOLD/ITALIC/UNDERLINE/REVERSE
defined. The `TerminalState` cell style struct must use exactly these types — `Cell.fg`/`bg` as
`Option<u8>` — to allow Phase 13 diff extraction with zero conversion AND to preserve the
default-vs-explicit (including explicit black) distinction end-to-end.

**Primary recommendation:** Implement `TerminalState` in a new file
`crates/nosh-server/src/terminal.rs`, integrate via `SessionSlot::push_output_and_parse`, and
unit-test with raw byte sequences against expected grid/cursor/mode state in
`crates/nosh-server/src/terminal.rs` `#[cfg(test)]` module.

---

## Architectural Responsibility Map

| Capability | Primary Tier | Secondary Tier | Rationale |
|------------|-------------|----------------|-----------|
| VT byte parsing (vte Perform) | Server (session substrate) | — | PTY output arrives server-side; parsing must happen where bytes are produced |
| Terminal grid state | Server | — | Authoritative state lives server-side; client uses diffs from Phase 13 |
| Echo-state tracking | Server | — | Observable via server-side PTY output stream only |
| Scrollback history | Server | — | D-12-02a: never synced to client until M5 |
| SessionSlot integration | Server (registry.rs) | — | `push_output_and_parse` replaces existing `push_output` callsites |
| Unit tests | Server crate test mod | — | Isolated; no network/session deps |

---

## Standard Stack

### Core

| Library | Version | Purpose | Why Standard |
|---------|---------|---------|--------------|
| `vte` | 0.15.0 | VT/ANSI byte-stream parser (Paul Williams state machine) | Alacritty project; locked by CLAUDE.md/CONTEXT.md; `Perform` trait is the clean extension point; `Parser::advance(&mut performer, bytes)` slice API |

[VERIFIED: crates.io via `cargo search vte`] — latest version is 0.15.0.

### No Additional Dependencies Required

`vte` is the only new dependency. It has no required features for this phase (the default
`std` feature is fine — enables `Vec<u8>` for the OSC buffer instead of `ArrayVec`).

**Installation (add to `crates/nosh-server/Cargo.toml`):**
```toml
vte = "0.15"
```

No workspace-level coordination needed; `vte` is not used by other crates yet.

---

## Package Legitimacy Audit

| Package | Registry | Age | Downloads | Source Repo | slopcheck | Disposition |
|---------|----------|-----|-----------|-------------|-----------|-------------|
| `vte` | crates.io | ~8 yrs (alacritty project) | — | github.com/alacritty/vte | [OK] | Approved |

**Note:** There is a separate `vte@1.0.0` on npm (ISC license, unrelated to Rust VTE) — this
creates no risk since the project is a Rust workspace and `cargo add vte` targets crates.io.
slopcheck confirmed `[OK]` for the crates.io package.

**Packages removed due to slopcheck [SLOP] verdict:** none
**Packages flagged as suspicious [SUS]:** none

---

## Architecture Patterns

### System Architecture Diagram

```
PTY master fd
    |
    v
pty_io (blocking thread) ──> out_tx (tokio channel)
                                      |
                                      v
                             session pump (async)
                                      |
                                      |──> slot.push_output_and_parse(&data)
                                      |         |
                                      |         |──> SequencedOutputBuffer.push(&data)  [UNCHANGED]
                                      |         |       (cold-reattach replay path)
                                      |         |
                                      |         └──> TerminalState.advance(&data)
                                      |                   |
                                      |                   |──> Parser::advance(&mut self, bytes)
                                      |                   |       |──> Perform::print(c)          [cell write]
                                      |                   |       |──> Perform::execute(byte)      [C0 ctrl]
                                      |                   |       |──> Perform::csi_dispatch(...)  [cursor/erase/SGR/modes]
                                      |                   |       |──> Perform::osc_dispatch(...)  [title/OSC52]
                                      |                   |       └──> Perform::esc_dispatch(...)  [ESC sequences]
                                      |                   |
                                      |                   └──> TerminalState fields updated:
                                      |                           - grid: Vec<Vec<Cell>>
                                      |                           - scrollback: VecDeque<Vec<Cell>>
                                      |                           - cursor: CursorPos
                                      |                           - size: (cols, rows)
                                      |                           - echo_state: EchoState (4 private modes)
                                      |                           - title: Option<String>
                                      |                           - osc52_pending: Option<(selection, data)>
                                      |
                                      └──> Phase 13 (later): snapshot() → StateDiff
```

### Recommended Project Structure

No new files/directories beyond one source file and its test module:

```
crates/nosh-server/src/
├── terminal.rs       # NEW: TerminalState + impl Perform + #[cfg(test)]
├── registry.rs       # MODIFIED: add terminal_state field to SessionSlot; add push_output_and_parse
├── server.rs         # MODIFIED: 3 callsites push_output → push_output_and_parse
└── lib.rs            # MODIFIED: pub mod terminal;
```

---

## vte 0.15.0 API — VERIFIED

### Perform Trait (VERIFIED: docs.rs/vte/0.15.0/src/vte/lib.rs)

```rust
pub trait Perform {
    fn print(&mut self, _c: char) {}
    fn execute(&mut self, _byte: u8) {}
    fn hook(&mut self, _params: &Params, _intermediates: &[u8],
            _ignore: bool, _action: char) {}
    fn put(&mut self, _byte: u8) {}
    fn unhook(&mut self) {}
    fn osc_dispatch(&mut self, _params: &[&[u8]], _bell_terminated: bool) {}
    fn csi_dispatch(&mut self, _params: &Params, _intermediates: &[u8],
                    _ignore: bool, _action: char) {}
    fn esc_dispatch(&mut self, _intermediates: &[u8], _ignore: bool, _byte: u8) {}
    fn terminated(&self) -> bool { false }
}
```

**The flagged `osc_dispatch` signature is confirmed:** `params: &[&[u8]], bell_terminated: bool`.
[VERIFIED: docs.rs/vte/0.15.0/src/vte/lib.rs.html lines 761-826]

### Parser Struct (VERIFIED: docs.rs/vte/0.15.0/src/vte/lib.rs)

```rust
pub struct Parser<const OSC_RAW_BUF_SIZE: usize = MAX_OSC_RAW> { ... }
```

- With `std` feature (default): OSC buffer is `Vec<u8>` (unbounded); `OSC_RAW_BUF_SIZE` is
  ignored. Use `Parser::default()` or `Parser::new()`.
- `advance` method: `pub fn advance<P: Perform>(&mut self, performer: &mut P, bytes: &[u8])`
  — processes a whole slice in one call; handles partial UTF-8 across calls.

[VERIFIED: docs.rs/vte/0.15.0/src/vte/lib.rs.html lines 54-128]

### Params Type (VERIFIED: docs.rs/vte/0.15.0)

- `Params::iter()` returns `ParamsIter` yielding `&[u16]` slices.
- Each `&[u16]` slice is one parameter + its subparameters (colon-separated SGR subparams).
- For a plain numeric parameter (no subparams), the slice is `&[value]` (length 1).
- `params.iter().next()` yields the first parameter slice.

[VERIFIED: docs.rs/vte/0.15.0/vte/struct.Params.html]

---

## Pattern 1: Detecting DEC Private Modes in `csi_dispatch`

**What:** `CSI ? Pm h` (DECSET) / `CSI ? Pm l` (DECRST) — enable/disable terminal private modes.
The `?` character (byte `0x3F`) is in the "intermediate bytes" range (`0x3C..=0x3F`) and is
collected into the `intermediates` array by vte's state machine.

**Detection pattern:**

```rust
// Source: docs.rs/vte/0.15.0/src/vte/lib.rs (CSI dispatch action + params documentation)
fn csi_dispatch(
    &mut self,
    params: &vte::Params,
    intermediates: &[u8],
    _ignore: bool,
    action: char,
) {
    // DEC private mode set/reset: CSI ? Pm h / CSI ? Pm l
    if intermediates == b"?" {
        let enable = action == 'h';
        let disable = action == 'l';
        if enable || disable {
            for param in params.iter() {
                let mode = param[0]; // u16; subparams[1..] are irrelevant for mode numbers
                match mode {
                    25 => self.echo_state.cursor_visible = enable,    // DECTCEM
                    1049 => self.echo_state.alt_screen = enable,      // alternate screen
                    2004 => self.echo_state.bracketed_paste = enable, // bracketed paste
                    1 => self.echo_state.app_cursor_keys = enable,    // application cursor keys
                    _ => { /* scope fence: unknown private mode, ignore */ }
                }
            }
        }
        return; // handled as private mode; do not fall through to standard CSI
    }
    // ... standard CSI handling (cursor motion, erase, SGR, etc.)
}
```

**Key facts:**
- `intermediates == b"?"` is the correct test: vte passes `b"?"` (a `&[u8]` with one element
  `0x3F`) when the sequence contains a `?` before the parameter digits.
- `action` is `'h'` for DECSET (enable), `'l'` for DECRST (disable).
- Numeric mode is `param[0]` — a `u16`.
- Multiple modes can be combined in one sequence: `CSI ? 25 ; 1049 h` — each yields a separate
  `&[u16]` slice from `params.iter()`.

[VERIFIED: docs.rs/vte/0.15.0/src/vte/lib.rs — intermediates collected for 0x3C-0x3F range]

---

## Pattern 2: Standard CSI Cursor Motion and Erase

**In-scope sequences for D-12-02b:**

| Sequence | action | intermediates | params meaning |
|----------|--------|---------------|----------------|
| CSI n A | `'A'` | `b""` | n = rows up (default 1) |
| CSI n B | `'B'` | `b""` | n = rows down (default 1) |
| CSI n C | `'C'` | `b""` | n = cols right (default 1) |
| CSI n D | `'D'` | `b""` | n = cols left (default 1) |
| CSI row ; col H | `'H'` | `b""` | params[0][0]=row, params[1][0]=col (1-based; 0=1) |
| CSI n J | `'J'` | `b""` | n: 0=below, 1=above, 2=all, 3=all+scrollback |
| CSI n K | `'K'` | `b""` | n: 0=right, 1=left, 2=whole-line |
| CSI ... m | `'m'` | `b""` | SGR attributes |

**Default param value:** When vte receives a CSI sequence with an omitted parameter (e.g., `CSI A`
with no digit), the `Params` struct contains a default value of `0` for that position. For cursor
motion, `0` should be treated as `1` (standard VT behavior).

[ASSUMED — default param behavior based on VT standard + training knowledge; verify in tests]

---

## Pattern 3: SGR Mapping to CellStyle / fg / bg

Phase 11 `DiffRun` (current source) uses:
- `style: CellStyle(u8)` — BOLD (0x01), ITALIC (0x02), UNDERLINE (0x04), REVERSE (0x08)
- `fg: Option<u8>` — `None` = terminal-default foreground; `Some(n)` = palette index `n` (0..=255)
- `bg: Option<u8>` — `None` = terminal-default background; `Some(n)` = palette index `n` (0..=255)

`Some(0)` is explicit palette index 0 (black) and is DISTINCT from `None` (terminal default).

[VERIFIED: `crates/nosh-proto/src/datagram.rs` lines 93-120 — current source read directly 2026-06-01;
fg/bg confirmed `Option<u8>`]

**SGR parameter → cell attribute mapping:**

| SGR param | Effect |
|-----------|--------|
| 0 | Reset all — style=NONE, fg=None, bg=None |
| 1 | Bold → `CellStyle::BOLD` |
| 3 | Italic → `CellStyle::ITALIC` |
| 4 | Underline → `CellStyle::UNDERLINE` |
| 7 | Reverse → `CellStyle::REVERSE` |
| 22 | Bold off → clear `CellStyle::BOLD` |
| 23 | Italic off |
| 24 | Underline off |
| 27 | Reverse off |
| 30–37 | Standard fg colors → `fg = Some(idx)` where idx = param-30 (0..=7) |
| 38; 5; n | 256-color fg → `fg = Some(n as u8)` |
| 39 | Default fg → `fg = None` (terminal default; NOT Some(0)) |
| 40–47 | Standard bg colors → `bg = Some(idx)` where idx = param-40 (0..=7) |
| 48; 5; n | 256-color bg → `bg = Some(n as u8)` |
| 49 | Default bg → `bg = None` (terminal default; NOT Some(0)) |
| 90–97 | Bright fg → `fg = Some(idx)` where idx = param-90+8 (8..=15) |
| 100–107 | Bright bg → `bg = Some(idx)` where idx = param-100+8 (8..=15) |

[ASSUMED — SGR standard; verify in unit tests]

**Important:** `Cell.fg`/`bg` MUST be `Option<u8>` matching `DiffRun.fg`/`bg` exactly so Phase 13
diff extraction has zero conversion overhead. `None` is the terminal-default sentinel — NEVER
store an explicit `Some(0)` to mean "default". `Some(0)` is a real, distinct value (explicit
black); the default-vs-explicit distinction must survive into Phase 13.

**SGR subparameters:** `CSI 38 ; 5 ; n m` — vte passes `38` as `params[0][0]`, `5` as
`params[1][0]`, `n` as `params[2][0]`. The implementation must walk the parameter list across
multiple `iter()` items for 256-color parsing.

[ASSUMED — SGR subparam iteration; aligns with Params iter pattern verified above]

---

## Pattern 4: OSC Dispatch — Title and Clipboard

**Confirmed `osc_dispatch` signature:**
```rust
fn osc_dispatch(&mut self, params: &[&[u8]], bell_terminated: bool)
```

`params` is a slice of byte-slice segments. OSC sequences use `;` as separator; vte splits on
`;` and provides each segment as a `&[u8]`.

| OSC | `params[0]` | `params[1]` | Action |
|-----|-------------|-------------|--------|
| OSC 0 | `b"0"` | title bytes | Set icon + window title |
| OSC 2 | `b"2"` | title bytes | Set window title only |
| OSC 52 | `b"52"` | selection (e.g. `b"c"`) + `;` + base64 data... | Clipboard write |

**For OSC 52** the full structure is: `OSC 52 ; Pc ; Pd BEL` where `Pc` is clipboard selection
and `Pd` is base64 data. vte delivers this as:
- `params[0]` = `b"52"`
- `params[1]` = `b"c"` (or `b"p"` etc. — selection)
- `params[2]` = base64 data bytes

**Detection:**
```rust
fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
    if params.is_empty() { return; }
    match params[0] {
        b"0" | b"2" => {
            // Set terminal title
            if let Some(title_bytes) = params.get(1) {
                if let Ok(title) = std::str::from_utf8(title_bytes) {
                    self.title = Some(title.to_owned());
                }
            }
        }
        b"52" => {
            // OSC 52: clipboard write — D-12-04: detect, do not forward (Phase 16)
            // Scope-fence: parse and store pending; actual forward is Phase 16
            let selection = params.get(1).copied().unwrap_or(b"c");
            let data = params.get(2).copied().unwrap_or(b"");
            self.osc52_pending = Some((selection.to_vec(), data.to_vec()));
        }
        _ => { /* scope fence: unknown OSC, ignore */ }
    }
}
```

[VERIFIED: osc_dispatch signature at docs.rs/vte/0.15.0/src/vte/lib.rs lines 761-826;
OSC parameter structure is CITED from VT standard and corroborated by WebFetch of vte source]

---

## Pattern 5: Grid and Cell Data Structure

**Goal:** Design a `Cell` type that maps directly to `DiffRun` fields so Phase 13 can extract
diffs without any conversion step.

**Recommended `Cell`:**
```rust
/// Single terminal cell matching Phase 11 DiffRun's style representation exactly.
#[derive(Clone, PartialEq, Eq)]
pub struct Cell {
    /// Unicode scalar value displayed in this cell. '\0' or ' ' means blank.
    pub ch: char,
    /// SGR attributes — same type as DiffRun.style.
    pub style: CellStyle,
    /// fg palette index; None = terminal-default color, Some(n) = palette index n
    /// (incl. Some(0) = explicit black). SAME type as DiffRun.fg.
    pub fg: Option<u8>,
    /// bg palette index; None = terminal-default color, Some(n) = palette index n
    /// (incl. Some(0) = explicit black). SAME type as DiffRun.bg.
    pub bg: Option<u8>,
}

impl Default for Cell {
    fn default() -> Self {
        Cell { ch: ' ', style: CellStyle(CellStyle::NONE), fg: None, bg: None }
    }
}
```

**Grid storage:**
```rust
pub struct TerminalState {
    cols: u16,
    rows: u16,
    /// Viewport: outer Vec is rows (0 = top), inner Vec is columns.
    /// Length is always cols * rows (resized by push_output_and_parse on resize).
    grid: Vec<Vec<Cell>>,
    /// Scrollback: bounded ring; lines scroll in from the top when the viewport
    /// fills. Bounded by SCROLLBACK_LINE_CAP (Claude's discretion: 10_000 lines).
    scrollback: std::collections::VecDeque<Vec<Cell>>,
    cursor: CursorPos,
    /// Observable terminal modes (D-12-01).
    echo_state: EchoState,
    /// Window title set by OSC 0/2.
    title: Option<String>,
    /// Last parsed OSC 52 payload (D-12-04). Replaced on each new OSC 52.
    osc52_pending: Option<(Vec<u8>, Vec<u8>)>,
    /// The vte parser (holds state machine across advance() calls).
    parser: vte::Parser,
}
```

**Scrollback cap (Claude's discretion):** 10,000 lines — enough for a day's shell output,
matching the spirit of `SequencedOutputBuffer`'s 64 KiB byte cap.

**Resize behavior (D-12-03):**
- Truncate or extend each row to the new `cols` width (extend with default cells).
- Truncate or extend the grid to the new `rows` height. On shrink, lines that scroll off
  the top go into scrollback. On grow, add blank rows at the bottom.
- NO reflow — row wrapping is NOT preserved across resize. Apps repaint after SIGWINCH.
- Scrollback lines retain their original column count (no resize reflow on history).

---

## Pattern 6: `push_output_and_parse` Integration

**Current `SessionSlot`** (registry.rs line 344):
```rust
pub fn push_output(&self, chunk: &[u8]) -> u64 {
    self.output_buf.lock().unwrap().push(chunk)
}
```

**New method:**
```rust
/// Feed PTY output into BOTH the sequenced replay buffer AND the terminal state model.
/// Returns the assigned sequence number from the replay buffer (D-10 unchanged).
///
/// LOCK ORDER: acquires output_buf lock then terminal_state lock in sequence.
/// Both locks are held only for the duration of the field mutation — never across `.await`.
pub fn push_output_and_parse(&self, chunk: &[u8]) -> u64 {
    let seq = self.output_buf.lock().unwrap().push(chunk);
    self.terminal_state.lock().unwrap().advance(chunk);
    seq
}
```

**`TerminalState::advance`:**
```rust
pub fn advance(&mut self, bytes: &[u8]) {
    // self.parser is owned by TerminalState; borrow split required.
    // vte Parser::advance takes (&mut self, performer: &mut P, bytes: &[u8])
    // where P: Perform. TerminalState itself implements Perform.
    // Borrow conflict: both self.parser and the Perform impl need &mut self.
    // Solution: take the parser out, advance with self as performer, put back.
    let mut parser = std::mem::take(&mut self.parser);
    parser.advance(self, bytes);
    self.parser = parser;
}
```

**Note on borrow conflict:** `Parser::advance` takes `(&mut self, performer: &mut P)`. If
`TerminalState` owns the `Parser` AND implements `Perform`, advancing requires a two-mutable-
reference split. `std::mem::take` solves this cleanly since `Parser: Default`. This is a known
pattern for self-referential performer/parser combinations in vte users.

[ASSUMED — borrow-split via mem::take is the standard pattern; cite alacritty's own usage; verify
in implementation that Parser::Default works as expected]

---

## Don't Hand-Roll

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| VT state machine transitions | Custom byte-by-byte CSI parser | `vte::Parser::advance` | The Paul Williams state machine has 9 states and 25 transitions; DEC intermediates, partial UTF-8, and edge cases are already handled; getting it wrong causes silent state corruption |
| UTF-8 multibyte buffering | Manual partial-UTF-8 accumulation | `vte::Parser::advance` | vte buffers partial UTF-8 across `advance()` calls (the `partial_utf8` field in `Parser`); duplicating this causes mojibake at chunk boundaries |
| OSC string assembly | Manual `\x1b]` / `\x07` or `\x1b\\` scanner | `vte::Parser::advance` + `osc_dispatch` | OSC strings can be terminated by BEL (0x07) OR ST (ESC `\`); vte handles both; the `bell_terminated` param distinguishes them |
| DCS (device control) ignoring | Tracking DCS state | `hook`/`put`/`unhook` no-ops | Default Perform no-ops already ignore DCS; just don't override them |

**Key insight:** vte was built specifically so terminal emulator authors don't implement state
machines — override only the callbacks for sequences in scope.

---

## Common Pitfalls

### Pitfall 1: Borrow conflict with self-owned Parser
**What goes wrong:** `TerminalState` owns `vte::Parser` and also `impl Perform`. Calling
`self.parser.advance(self, bytes)` fails with "cannot borrow `self` as mutable more than once".
**Why it happens:** `advance` borrows `self.parser` as mutable AND takes `performer: &mut P`
which is also `self`.
**How to avoid:** `std::mem::take(&mut self.parser)` before the call, restore after.
`Parser` implements `Default` (verified), so `take` produces a clean parser state.
**Warning signs:** Compiler error E0499 "cannot borrow `self` as mutable because it is also
borrowed as mutable".

### Pitfall 2: Cursor 1-based vs 0-based in CSI H (CUP)
**What goes wrong:** `CSI 1 ; 1 H` means row 1, col 1 (top-left) in VT100, but the grid is
0-indexed internally. Off-by-one in `H` dispatch moves the cursor one cell off.
**Why it happens:** VT100 cursor addressing is 1-based; `0` and `1` both mean "1" (minimum).
**How to avoid:** In `csi_dispatch` for `'H'`: `row = params[0].max(1) - 1`, same for col.
Test with `\x1b[1;1H` (top-left) and `\x1b[24;80H` (bottom-right for 80x24).

### Pitfall 3: Omitted CSI parameters default to 0, not 1
**What goes wrong:** `CSI A` (cursor up, no digit) → vte delivers params with first value `0`.
The model should treat `0` as `1` for cursor motion commands.
**Why it happens:** vte stores omitted params as `0` (the field is zero-initialized).
**How to avoid:** `let n = params.iter().next().and_then(|p| p.first().copied()).unwrap_or(0);`
then `let n = n.max(1) as i32;`.
**Warning sign:** Tests with bare `CSI A` (no count) show cursor not moving.

### Pitfall 4: SGR 0 (reset) must clear fg/bg back to None
**What goes wrong:** After a color sequence, `CSI 0 m` or bare `CSI m` (no params = SGR 0)
must reset fg AND bg to `None` (terminal default). Only resetting the style bits leaves stale
color.
**Why it happens:** Partial SGR reset implementations forget the color fields.
**How to avoid:** Treat no-params SGR as SGR 0; handle `0` param explicitly: set style=NONE,
fg=None, bg=None before processing any other params in the sequence. Never reset to `Some(0)`
(that would be explicit black, not default).

### Pitfall 5: ED 3 (erase display + scrollback) must not panic
**What goes wrong:** `CSI 3 J` (erase display and scrollback) should clear scrollback. If
scrollback is a `VecDeque` this is a `clear()` call. Forgetting to handle `3` with a no-op is
fine per D-12-02b scope-fence, but clearing with an out-of-bounds operation panics.
**How to avoid:** Treat ED 3 as: clear visible grid + clear scrollback (or just scope-fence it
as exotic). Either is correct; document the choice.

### Pitfall 6: SGR 38/48 subparam parsing across Params iter items
**What goes wrong:** `CSI 38 ; 5 ; 201 m` — 256-color fg. The `38` and `5` and `201` are
three separate parameters (not subparameters), so `params.iter()` yields three separate
`&[u16]` slices. Trying to read them as one slice's elements finds only `[38]`.
**Why it happens:** Subparameters (colon-separated in modern SGR) use `extend()`; old-style
semicolons use `push()`. Most terminals still emit semicolon-separated `38;5;n`.
**How to avoid:** Walk `params.iter()` as a stateful sequence; when you see `38`, grab the
NEXT two params (check `5`, then read color index → `fg = Some(n as u8)`). Same for `48`.
**Warning sign:** Colors not applying after `ESC[38;5;nnn m` in tests.

### Pitfall 7: OSC 52 vs OSC 0 wrong `params[0]` check
**What goes wrong:** `params[0] == b"52"` vs `params[0] == b"0"` — these are BYTE SLICES,
not integers. `b"52"` is the two bytes `[0x35, 0x32]`, not the number 52.
**Why it happens:** Confusing integer comparison with slice comparison.
**How to avoid:** Use byte slice literals in match arms: `b"52"`, `b"0"`, `b"2"`. Works
because vte passes the raw ASCII digits as bytes.

### Pitfall 8: `push_output_and_parse` must not regress reattach replay
**What goes wrong:** `SequencedOutputBuffer::push` is called inside `push_output_and_parse`
but an error in the new `TerminalState::advance` (panic or early return) could skip the
`push` call if it comes second.
**Why it happens:** Wrong ordering — if `TerminalState::advance` is called first and panics,
the replay buffer never gets the chunk.
**How to avoid:** Always call `self.output_buf.lock().unwrap().push(chunk)` FIRST to get the
seq number. `TerminalState::advance` is called second. Since `advance` has no error path
(all CSI/OSC dispatch errors are silently ignored), this ordering is safe.

---

## Code Examples

### Complete Perform Trait Implementation Skeleton

```rust
// Source: docs.rs/vte/0.15.0 (verified Perform trait signature)
use nosh_proto::datagram::{CellStyle, CursorPos};

impl vte::Perform for TerminalState {
    fn print(&mut self, c: char) {
        // Write c at cursor position, advance cursor right.
        // Wraps to next line when cursor.col >= self.cols.
        // Scrolls viewport (push top row into scrollback) when cursor.row >= self.rows.
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            b'\r' => { self.cursor.col = 0; }
            b'\n' | b'\x0B' | b'\x0C' => { self.lf(); } // linefeed + scroll
            b'\x08' => { // backspace
                if self.cursor.col > 0 { self.cursor.col -= 1; }
            }
            b'\x07' => { /* BEL: ignore in state model */ }
            _ => { /* scope fence: other C0 control codes, ignore */ }
        }
    }

    fn csi_dispatch(
        &mut self,
        params: &vte::Params,
        intermediates: &[u8],
        _ignore: bool,
        action: char,
    ) {
        if intermediates == b"?" {
            // DEC private mode set/reset (D-12-01)
            // ...
            return;
        }
        match action {
            'A' => { /* cursor up */ }
            'B' => { /* cursor down */ }
            'C' => { /* cursor right */ }
            'D' => { /* cursor left */ }
            'H' | 'f' => { /* cursor position (CUP) — 1-based → 0-based */ }
            'J' => { /* erase in display */ }
            'K' => { /* erase in line */ }
            'm' => { /* SGR — walk params; fg/bg are Option<u8>, None=default Some(n)=palette */ }
            _ => { /* scope fence: other CSI, ignore */ }
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        if params.is_empty() { return; }
        match params[0] {
            b"0" | b"2" => { /* title */ }
            b"52" => { /* clipboard — D-12-04 */ }
            _ => { /* scope fence */ }
        }
    }

    fn esc_dispatch(&mut self, _intermediates: &[u8], _ignore: bool, byte: u8) {
        match byte {
            b'c' => { /* RIS: full reset — clear grid, reset cursor, reset modes, fg/bg=None */ }
            _ => { /* scope fence */ }
        }
    }

    // hook / put / unhook: default no-ops (DCS not in scope D-12-02b)
    // terminated: default false
}
```

### Test Pattern (Isolation — No Network)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn ts(cols: u16, rows: u16) -> TerminalState {
        TerminalState::new(cols, rows)
    }

    #[test]
    fn print_chars_advances_cursor() {
        let mut state = ts(80, 24);
        state.advance(b"abc");
        assert_eq!(state.cursor(), CursorPos { row: 0, col: 3 });
        assert_eq!(state.cell(0, 0).ch, 'a');
        assert_eq!(state.cell(0, 1).ch, 'b');
        assert_eq!(state.cell(0, 2).ch, 'c');
    }

    #[test]
    fn cursor_up_csi_a() {
        let mut state = ts(80, 24);
        state.advance(b"\x1b[5;10H");  // cursor to row=5, col=10 (1-based)
        state.advance(b"\x1b[2A");     // cursor up 2
        assert_eq!(state.cursor(), CursorPos { row: 2, col: 9 }); // 0-based
    }

    #[test]
    fn decset_alt_screen_sets_echo_state() {
        let mut state = ts(80, 24);
        assert!(!state.echo_state().alt_screen);
        state.advance(b"\x1b[?1049h");
        assert!(state.echo_state().alt_screen);
        state.advance(b"\x1b[?1049l");
        assert!(!state.echo_state().alt_screen);
    }

    #[test]
    fn osc52_detectable() {
        let mut state = ts(80, 24);
        // OSC 52 ; c ; SGVsbG8= BEL  ("Hello" in base64)
        state.advance(b"\x1b]52;c;SGVsbG8=\x07");
        let osc52 = state.osc52_pending();
        assert!(osc52.is_some());
        let (sel, data) = osc52.unwrap();
        assert_eq!(sel, b"c");
        assert_eq!(data, b"SGVsbG8=");
    }

    #[test]
    fn osc2_title_captured() {
        let mut state = ts(80, 24);
        state.advance(b"\x1b]2;My Title\x07");
        assert_eq!(state.title(), Some("My Title"));
    }

    #[test]
    fn erase_in_display_clears_below() {
        let mut state = ts(80, 24);
        state.advance(b"abc\x1b[J"); // CSI 0 J = erase from cursor to end of screen
        // Row 0, cols 3-79 cleared; row 1+ cleared; cursor stays
        assert_eq!(state.cell(0, 0).ch, 'a');
        assert_eq!(state.cell(0, 3).ch, ' ');
    }

    #[test]
    fn sgr_bold_and_reset() {
        let mut state = ts(80, 24);
        state.advance(b"\x1b[1mA\x1b[0mB");
        let a = state.cell(0, 0);
        let b = state.cell(0, 1);
        assert_eq!(a.style.0 & CellStyle::BOLD, CellStyle::BOLD);
        assert_eq!(b.style.0, CellStyle::NONE);
    }

    #[test]
    fn default_color_is_none_not_some_zero() {
        let mut state = ts(80, 24);
        state.advance(b"a");                 // default-color write
        assert_eq!(state.cell(0, 0).fg, None);   // default, NOT Some(0)
        assert_eq!(state.cell(0, 0).bg, None);
    }

    #[test]
    fn explicit_black_is_some_zero_distinct_from_default() {
        let mut state = ts(80, 24);
        state.advance(b"\x1b[30mB");         // SGR 30 → explicit black fg
        assert_eq!(state.cell(0, 0).fg, Some(0)); // explicit black, distinct from None
        // and resetting returns to default None
        state.advance(b"\x1b[39mC");         // SGR 39 → default fg
        assert_eq!(state.cell(0, 1).fg, None);
    }

    #[test]
    fn sgr_256_color_fg() {
        let mut state = ts(80, 24);
        state.advance(b"\x1b[38;5;201mZ");
        assert_eq!(state.cell(0, 0).fg, Some(201));
    }
}
```

---

## State of the Art

| Old Approach | Current Approach | When Changed | Impact |
|--------------|------------------|--------------|--------|
| `vte` 0.11.x (u8 params) | `vte` 0.15.0 (`Params` struct + `u16`) | ~2021 | Params are now `u16` (supports values > 255); iteration is `&[u16]` slice, not raw `u8` iterator |
| `Parser<usize>` with explicit OSC size | `Parser<const OSC_RAW_BUF_SIZE>` default via `Parser::default()` | 0.13+ | With `std` feature (default), OSC buffer is a `Vec`; just use `Parser::default()` |

**Not deprecated — still current:**
- `Perform` trait with default no-op methods (all methods have defaults; override only what's needed)
- `Parser::advance` slice API (not byte-by-byte; whole `&[u8]` slice at once)

---

## Assumptions Log

| # | Claim | Section | Risk if Wrong |
|---|-------|---------|---------------|
| A1 | Default param value for omitted CSI parameters is `0` (not `1`) in vte 0.15.0 | Pattern 2 / Pitfall 3 | Cursor motion with omitted count does wrong delta; test would catch |
| A2 | SGR 30–37 maps to palette indices 0–7 as `Some(idx)` (standard ANSI color mapping) | Pattern 3 | Colors wrong; unit test would catch |
| A3 | `std::mem::take` on `vte::Parser` produces a correctly-initialized (blank-state) parser suitable for temporary use as borrowing split | Pattern 6 (advance impl) | Parser state would be lost; verify by confirming `Parser: Default` resets state |
| A4 | SGR `38;5;n` uses three separate Params items (not subparams) in real terminal output | Pattern 3 / Pitfall 6 | 256-color fg/bg parsing broken; unit test with `\x1b[38;5;201m` would catch |
| A5 | `CSI 3 J` (erase display + scrollback) passes param `3` in `params[0][0]` | Pitfall 5 | Misidentified as standard ED case; test would catch |

---

## Open Questions (RESOLVED)

1. **`Parser::Default` reset behavior (A3)**
   - What we know: `Parser` derives `Default`; `std::mem::take` replaces with `Default::default()`.
   - What's unclear: Does `Default` produce a clean ground-state parser, or a partially
     initialized one? (It should produce ground-state per derive, but not confirmed in source.)
   - Recommendation: Write a test that advances a partial escape sequence, calls `take`,
     and confirms the new parser starts fresh. If `take` resets state, the borrow-split
     is safe to use in production. If not, use `Mutex<Parser>` or restructure ownership.
   - Status: RESOLVED in implementation — the `Parser` struct derives `Default` with all
     fields zero-initialized (state = State::Ground), so `take` produces a ground-state parser.
     The in-flight sequence in the taken parser is lost (the partial sequence was in the old
     state that was moved into `self.parser` via restore). This is safe for the `advance` pattern
     because: we only call `take` at the start of `advance`, not mid-sequence. The same bytes
     slice is passed to the taken parser immediately.

2. **`CursorPos` type reuse from `nosh_proto`**
   - What we know: `nosh_proto::datagram::CursorPos` exists with `row: u16, col: u16` (0-based).
   - What's unclear: Should `TerminalState` import `CursorPos` from `nosh_proto`, or define
     its own? `nosh-server` already depends on `nosh-proto`, so the import is legal.
   - Recommendation: Reuse `nosh_proto::datagram::CursorPos` directly — avoids type conversion
     at Phase 13 diff extraction time.
   - **RESOLVED:** Plan 12-01 imports and reuses `nosh_proto::datagram::CursorPos` directly
     (Task 1 action + acceptance criterion grepping for `nosh_proto::datagram`). Same applies
     to `CellStyle`, and `Cell.fg`/`bg` mirror `DiffRun`'s `Option<u8>` for zero conversion.

3. **Scrollback interaction with resize (D-12-03 Claude's discretion)**
   - What we know: D-12-03 says no reflow. Scrollback lines have their original column count.
   - What's unclear: Should scrollback lines be padded/trimmed on resize, or kept as-is?
   - Recommendation: Keep scrollback lines as-is (original column count preserved). The
     Phase 13 diff path operates only on the visible viewport, so scrollback line width
     mismatch has no immediate impact.
   - **RESOLVED:** Plan 12-02 Task 1 specifies `TerminalState::resize` resizes grid dimensions
     only (truncate/extend rows to `cols` with default cells; on shrink top rows scroll into
     scrollback; NO reflow per D-12-03). Scrollback lines are kept as-is (original column count
     preserved); the Phase 13 viewport-only diff path is unaffected.

---

## Environment Availability

Step 2.6: SKIPPED — no external dependencies beyond the Rust toolchain. `vte 0.15.0` is a
pure-Rust crate with no native dependencies. The Rust toolchain (`cargo`) is already in use
in this workspace.

---

## Security Domain

No new security surface in this phase. `TerminalState` is a pure in-memory state accumulator
fed from already-received PTY bytes (same data that feeds `SequencedOutputBuffer`). No network
input, no privilege escalation surface, no env-variable handling.

The CLAUDE.md security invariants (env sanitization, no SSH_AUTH_SOCK forwarding) apply at the
PTY-spawn callsite (session.rs), not here.

OSC 52 (clipboard-write) is detected only — no forwarding or clipboard access this phase.

---

## Sources

### Primary (HIGH confidence — VERIFIED)
- `docs.rs/vte/0.15.0/src/vte/lib.rs.html` — Perform trait signatures (lines 761–826), Parser struct (lines 54–70), `advance` method (lines 108–128)
- `docs.rs/vte/0.15.0/vte/struct.Params.html` — Params API (iter → &[u16] slices)
- `docs.rs/vte/0.15.0/src/vte/params.rs.html` — Params internals (MAX_PARAMS=32, push/extend, ParamsIter)
- `crates/nosh-proto/src/datagram.rs` (read directly 2026-06-01) — `CellStyle`, `DiffRun.fg/bg: Option<u8>`, `CursorPos` types confirmed
- `crates/nosh-server/src/registry.rs` (read directly) — `SessionSlot::push_output` signature, `SequencedOutputBuffer` pattern
- `crates/nosh-server/src/server.rs` (read directly) — 3 `push_output` callsites at lines ~414, ~504, ~786 confirmed
- `crates.io cargo search vte` — version 0.15.0 confirmed as current

### Secondary (MEDIUM confidence — cited/corroborated)
- `docs.rs/vte/0.15.0/vte/trait.Perform.html` — method-level docs confirming the `ignore` flag meaning for csi_dispatch
- `github.com/alacritty/vte` — README + source structure confirming CSI `?` intermediate collection in `0x3C..=0x3F` range
- VT100/ECMA-48 standard (via source analysis) — OSC parameter separator is `;`, confirming `params[0]=b"52"` for OSC 52

### Tertiary (LOW confidence — training knowledge, marked ASSUMED)
- SGR color mapping (30–37 → Some(0..=7); 38;5;n → Some(n)) — standard but not re-verified
- Default param value behavior for omitted CSI params — standard but A1 flagged

---

## Metadata

**Confidence breakdown:**
- vte 0.15.0 Perform API: HIGH — verified directly from docs.rs source
- CSI intermediate/params dispatch model: HIGH — verified from docs.rs source
- OSC dispatch structure: HIGH — verified from source; corroborated by VT standard
- Phase 11 CellStyle/DiffRun types: HIGH — read from current source (fg/bg are Option<u8>)
- SGR mapping: MEDIUM — standard knowledge, flagged as ASSUMED
- Borrow-split pattern via mem::take: MEDIUM — well-known Rust pattern, A3 documents the verification step

**Research date:** 2026-06-01
**Valid until:** 2026-09-01 (vte 0.15.x API is stable; OSC/CSI protocol is immutable)
</content>
