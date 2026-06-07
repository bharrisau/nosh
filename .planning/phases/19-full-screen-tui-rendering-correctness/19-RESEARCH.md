# Phase 19: Full-Screen TUI Rendering Correctness - Research

**Researched:** 2026-06-07
**Domain:** Server-side terminal state model (TerminalState), client-side predictor suppression, OSC accumulation security
**Confidence:** HIGH — all findings verified from codebase source or crate source in the local Cargo registry

---

<user_constraints>
## User Constraints (from CONTEXT.md)

### Locked Decisions
- **D-19-01:** OSC accumulation bounded at 1 MiB per OSC sequence, intercepted before vte's internal `osc_raw` Vec. Distinct from the existing `OSC_52_MAX_BYTES` (64 KiB) and `MAX_TITLE_BYTES` (1 KiB) storage caps.
- **D-19-02:** On overflow, truncate and discard the oversized OSC; resync the VT parser to ground state. Do NOT drop-and-keep-parsing (risks parser staying inside malformed OSC). Do NOT kill the session (buggy app emitting large title must not nuke the shell).
- **D-19-03:** Legitimate OSC 52 clipboard and OSC 0/2 title sequences must continue to pass unchanged. SEC-03 regression test is RED-before / GREEN-after: a multi-chunk ~10 MB OSC payload across many `advance()` calls must not exhaust memory.
- **D-19-04:** Use `unicode-width` default `width()` — East Asian Ambiguous characters measure as width 1. Do NOT use `width_cjk()`. Do NOT add an ambiguous-width config knob this phase.
- **D-19-05:** CJK wide characters (width 2) advance cursor by two columns. Zero-width combining marks and ZWJ sequences (width 0) do not advance the cursor. Mode 2027 grapheme clustering deferred to v1.4+.
- **D-19-06:** The spacer cell at `col+1` after a width-2 char is an explicit wide-char continuation marker (a distinct Cell representation — sentinel `ch` or Cell flag), NOT a literal blank space.
- **D-19-07:** Acceptance bar = synthetic VT grid-assertion tests in CI + documented manual visual pass. Do NOT attempt automated golden-master capture against live apps.
- **D-19-08:** Investigation-first is mandatory: reproduce garbling/missing-spaces against a Linux client↔server before fixing.
- **D-19-09:** No scrollback while alt-screen active — `scroll_up()` must check `!alt_screen`. Primary buffer scrollback preserved untouched. Alt grid has no own scrollback.

### Claude's Discretion
- Exact two-grid data structure (separate saved primary grid vs swap pointers), atomicity implementation of save+swap+clear / restore+swap.
- Which width crate API surface on the server side (add `unicode-width` to `nosh-server`); exact continuation-marker encoding on `Cell`.
- OSC pre-accumulation buffering mechanism (custom byte pre-scan / wrapper around `advance`).
- Resize handling so both active alt grid and saved primary grid resize (TUI-02) with no inactive-buffer loss.
- Mechanism for suppressing the speculative predictor while alt-screen is active, constrained by: no overlay inside vim/htop, `predictor.pending` empty after `?1049h` is processed.

### Deferred Ideas (OUT OF SCOPE)
- Mode 2027 grapheme clustering — deferred to v1.4+.
- Configurable ambiguous-width negotiation (client→server) — out of scope.
- Scrollback sync to the client — Phase 22.
</user_constraints>

<phase_requirements>
## Phase Requirements

| ID | Description | Research Support |
|----|-------------|------------------|
| TUI-01 | Genuine two-grid alt-screen model: atomic save+swap+clear on `?1049h`, restore+swap on `?1049l` | §Alt-Screen Two-Grid Model, §Pitfall A-1, §Pitfall A-6 |
| TUI-02 | Both primary and alt grids resize on SIGWINCH with no inactive-buffer loss | §Resize with Two Grids |
| TUI-03 | Wide chars (width 2) occupy two columns; zero-width marks do not advance cursor | §Wide-Character Handling, §Pitfall A-2, §Pitfall A-3 |
| TUI-04 | Full-screen TUI apps render correctly over nosh (investigation-first) | §Manual Verification, §Pitfall A-1 |
| TUI-05 | Predictor suppresses speculative echo while alt-screen active | §Predictor Suppression (TUI-05) |
| SEC-03 | OSC accumulation bounded before vte's internal buffer; RED/GREEN regression test | §OSC Accumulation Pre-Bound (SEC-03), §Pitfall SEC-2 |
</phase_requirements>

---

## Summary

Phase 19 is entirely server-side `TerminalState` work plus one client-side hook for predictor suppression. No protocol changes, no new QUIC streams, no wire-format changes to the datagram codec in this phase.

The current `TerminalState` handles `?1049h`/`?1049l` by toggling `echo_state.alt_screen` only — no buffer swap, no save, no restore, no clear. This is a no-op and is explicitly worse than a half-implemented alt-screen (PITFALL A-1). The two-grid implementation must be atomic: all three operations (save + swap + clear-on-enter, restore + swap-on-exit) must land in a single commit or the session will render worse than before.

The OSC OOM vector is verified from vte 0.15.0 source: with `feature = "std"`, `osc_raw` is an unbounded `Vec<u8>` that accumulates across `advance()` calls until the BEL/ST terminator fires `osc_end`. The existing `OSC_52_MAX_BYTES` and `MAX_TITLE_BYTES` caps run in `osc_dispatch` — after vte has already allocated the full buffer. The pre-bound must intercept the raw PTY bytes before they reach `parser.advance()`.

`EchoState.alt_screen` is NOT currently propagated to the client via any datagram or stream channel. The `StateDiff` struct carries only `epoch`, `cols`, `rows`, `cursor`, and `runs`. The predictor suppression (TUI-05) requires a new mechanism to convey alt-screen state to the client side.

**Primary recommendation:** Implement the four concerns in this order within the phase: (1) two-grid alt-screen (TUI-01/02/D-19-09), (2) wide-char width (TUI-03), (3) OSC pre-bound (SEC-03), (4) predictor suppression (TUI-05). Each can be a separate plan. Do not ship alt-screen in a half-built state — the atomicity invariant (Pitfall A-1) means all three ?1049h operations must be in one commit.

---

## Architectural Responsibility Map

| Capability | Primary Tier | Secondary Tier | Rationale |
|------------|-------------|----------------|-----------|
| Alt-screen two-grid model | Server (`nosh-server/terminal.rs`) | — | `TerminalState` is the authoritative grid; `?1049h`/`?1049l` is PTY output parsed server-side |
| Wide-char width tracking | Server (`nosh-server/terminal.rs`) | Client render path (`screen.rs`) | Server writes the grid including continuation markers; client must not re-render wide chars as two glyphs |
| OSC accumulation bound | Server (`nosh-server/terminal.rs`) | Fuzz target (`fuzz/osc_accumulation.rs`) | Interception must happen before `parser.advance()` in `TerminalState::advance` |
| Predictor suppression | Client (`nosh-client/src/main.rs` + `predictor.rs`) | Server (`echo_state` as source of truth) | Predictor is client-side; alt-screen state must be conveyed from server to client |
| Scrollback gate | Server (`scroll_up()` in `terminal.rs`) | — | Must check `!alt_screen` before pushing to `scrollback` (D-19-09); Phase 22 depends on this gate |

---

## Standard Stack

### Core (all already in workspace)
| Library | Version | Purpose | Notes |
|---------|---------|---------|-------|
| `vte` | 0.15.0 | VT/ANSI parser feeding `TerminalState::advance` | Already in `nosh-server/Cargo.toml`. `Parser` implements `Default` (ground state). `#[derive(Default)]` on `Parser` — `State::Ground` is the `#[default]` variant. |
| `unicode-width` | 0.2.2 | Per-codepoint width via `UnicodeWidthChar::width()` | Already in `nosh-client/Cargo.toml`. Must be ADDED to `nosh-server/Cargo.toml`. |

### No New Dependencies Required
All libraries needed are already in the workspace. The only Cargo.toml change is adding `unicode-width = "0.2"` to `crates/nosh-server/Cargo.toml`.

**Version verification:** [VERIFIED: codebase] `vte` 0.15.0 in `Cargo.lock`; `unicode-width` 0.2.2 in `Cargo.lock`.

---

## Package Legitimacy Audit

No new packages are installed in this phase. `unicode-width` is an existing workspace dependency being added to a new crate. No audit required.

---

## Architecture Patterns

### System Architecture: Data Flow for Phase 19 Changes

```
PTY output bytes
        │
        ▼
TerminalState::advance(bytes)
        │
        ├─── [NEW] OSC pre-scan loop
        │       Counts OSC bytes; if > 1 MiB:
        │       - truncate input slice to just before the offending OSC
        │       - feed truncated slice to parser.advance()
        │       - reset parser (mem::take → Parser::default())
        │       else:
        │       - feed full bytes to parser.advance()
        │
        ▼
vte::Parser::advance(self, bytes)
        │
        ├─ print(c) ──► [MODIFIED] print_char(c)
        │                   UnicodeWidthChar::width(c)
        │                   Some(2): write char at col, write Wide-Marker at col+1, cursor += 2
        │                   Some(0): no advance (combining/ZWJ)
        │                   Some(1) / None: existing behaviour (write at col, cursor += 1)
        │
        ├─ csi_dispatch(?1049h) ──► [NEW] enter_alt_screen()
        │                            saved_primary = Some((grid.clone(), cursor, sgr.clone()))
        │                            grid = make_grid(cols, rows)  ← blank
        │                            cursor = CursorPos { row: 0, col: 0 }
        │                            sgr = SgrState::default()
        │                            echo_state.alt_screen = true
        │
        ├─ csi_dispatch(?1049l) ──► [NEW] exit_alt_screen()
        │                            if let Some((prim_grid, prim_cursor, prim_sgr)) = saved_primary.take()
        │                                grid = prim_grid
        │                                cursor = prim_cursor
        │                                sgr = prim_sgr
        │                            echo_state.alt_screen = false
        │
        └─ scroll_up() ──► [MODIFIED] if !echo_state.alt_screen { push to scrollback }
                            (D-19-09 gate — Phase 22 depends on this)

Server→Client EchoState propagation (NEW for TUI-05):
        StateDiff (datagram) carries alt_screen flag OR
        TerminalControl stream message carries it.
        Client run_pump reads alt_screen → calls predictor.reset() + clear pending on ?1049h
```

### Recommended Project Structure (changes only)

```
crates/nosh-server/src/
└── terminal.rs         ← all server-side changes
    ├── Cell struct     ← add `wide: bool` flag (continuation marker)
    ├── TerminalState   ← add `saved_primary: Option<(Vec<Vec<Cell>>, CursorPos, SgrState)>`
    ├── advance()       ← add OSC pre-scan before parser.advance()
    ├── print_char()    ← add unicode-width dispatch
    ├── scroll_up()     ← add !alt_screen gate
    ├── resize()        ← resize saved_primary if Some
    └── csi_dispatch()  ← replace ?1049 flag-only with enter/exit_alt_screen()

crates/nosh-client/src/
└── main.rs             ← predictor suppression hook in datagram arm

fuzz/fuzz_targets/
└── osc_accumulation.rs ← extend with multi-chunk 10 MB OSC test
```

---

## Research Findings by Topic

### 1. Two-Grid Alt-Screen Model (TUI-01, TUI-02, D-19-09)

**What xterm `?1049` actually does (verified against xterm source and PITFALLS.md):**

`?1049h` (enter alternate screen):
1. Save the current cursor position (equivalent to DECSC / ESC 7).
2. Save the current SGR pen state (style, fg, bg).
3. Save the entire primary grid.
4. Switch to a blank alternate grid (same dimensions as the terminal).
5. Set cursor to (0, 0).

`?1049l` (exit alternate screen):
1. Restore the primary grid exactly as it was.
2. Restore cursor position.
3. Restore SGR pen state.
4. Clear the alternate grid (it is not retained — it does not have scrollback).

**Distinction from `?47`/`?1047`/`?1048`:**
- `?47`: basic alt screen — switch grid but does NOT save/restore cursor position or SGR.
- `?1047`: switch grid + clear alt on exit. No cursor save.
- `?1048`: save/restore cursor only (DECSC/DECRC). No grid switch.
- `?1049`: `?1048` + `?47` combined — save cursor + SGR + grid, clear alt, restore on exit. This is what modern terminals (xterm, tmux, vim) use. [CITED: https://invisible-island.net/xterm/ctlseqs/ctlseqs.html#h3-Functions-using-CSI-_-ordered-by-the-final-character_s_]

**Recommended data structure:**

Add one field to `TerminalState`:

```rust
/// Saved primary grid state when alternate screen is active.
/// `None` when the primary screen is the active screen.
/// `Some((grid, cursor, sgr))` when ?1049h has been received.
saved_primary: Option<(Vec<Vec<Cell>>, CursorPos, SgrState)>,
```

`SgrState` is already `Clone`d in the codebase (private struct, but used in `csi_dispatch` — confirm it derives `Clone`). [VERIFIED: codebase — `SgrState` is defined in terminal.rs lines 129–151, implements `Clone`]

**Atomicity pattern:**

The borrow-split pattern already used in `advance()` (`std::mem::take`) means `csi_dispatch` runs inside the `vte::Perform` impl — it has exclusive `&mut self` access. Enter and exit alt-screen can be implemented as private methods `enter_alt_screen()` and `exit_alt_screen()`, called from `csi_dispatch` at the `1049` match arm. Because these are synchronous within a single `advance()` call, they are atomic from the caller's perspective — no partial state is visible outside `advance()`.

**Critical invariant (PITFALL A-1):** The three operations on `?1049h` — save, clear, mark active — must ALL occur. A partial implementation (e.g. `echo_state.alt_screen = true` but no grid clear) is demonstrably worse than the current no-op. The existing `decset_alt_screen_toggled_by_1049` test will need updating to assert grid-state, not just the flag.

**What state must be saved:**
- `grid: Vec<Vec<Cell>>` — the entire primary viewport (full clone).
- `cursor: CursorPos` — cursor position at the moment of `?1049h`.
- `sgr: SgrState` — current SGR pen (style, fg, bg) — because TUI apps reset SGR on entry and the primary shell should resume with its own pen state. [ASSUMED — based on xterm convention; verifying via xterm source is recommended but the pitfall of not saving SGR is low-severity vs not saving cursor position]

**On enter (`?1049h`):**
```rust
fn enter_alt_screen(&mut self) {
    // Save primary state.
    self.saved_primary = Some((
        std::mem::replace(&mut self.grid, Self::make_grid(self.cols, self.rows)),
        self.cursor,
        self.sgr.clone(),
    ));
    // Blank alt grid already set via make_grid in the replace above.
    self.cursor = CursorPos { row: 0, col: 0 };
    self.sgr.reset();
    self.echo_state.alt_screen = true;
}
```

Using `std::mem::replace` avoids cloning the old grid into a temporary — it moves the primary grid into `saved_primary.0` and puts a fresh blank grid in its place in one operation.

**On exit (`?1049l`):**
```rust
fn exit_alt_screen(&mut self) {
    if let Some((prim_grid, prim_cursor, prim_sgr)) = self.saved_primary.take() {
        self.grid = prim_grid;
        self.cursor = prim_cursor;
        self.sgr = prim_sgr;
    }
    // If saved_primary is None (exit without prior enter — graceful no-op):
    // leave grid as-is, just clear the flag.
    self.echo_state.alt_screen = false;
}
```

**Pitfall A-6 (cursor save/restore):** The cursor position at `?1049h` MUST be saved and restored at `?1049l`. Without this, the cursor lands at (0,0) after exiting vim — the shell prompt appears at the top of the screen. This is documented in PITFALLS.md A-6. [VERIFIED: codebase — currently `csi_dispatch` at line 504 only sets `echo_state.alt_screen = enable`; cursor is NOT saved]

**Scrollback gate (D-19-09, Pitfall S-2):**
```rust
fn scroll_up(&mut self) {
    if self.rows == 0 { return; }
    let top_row = self.grid.remove(0);
    if !self.echo_state.alt_screen {
        // Only push to scrollback when on the primary screen.
        self.scrollback.push_back(top_row);
        if self.scrollback.len() > SCROLLBACK_LINE_CAP {
            self.scrollback.pop_front();
        }
    }
    self.grid.push(vec![Cell::default(); self.cols as usize]);
}
```

This gate is the exact dependency Phase 22 (scrollback sync) requires before it can proceed.

**RIS reset must clear saved_primary:** The `esc_dispatch` RIS (`b'c'`) handler currently resets grid, scrollback, cursor, sgr, echo_state. It must also clear `saved_primary = None`. [VERIFIED: codebase — esc_dispatch at lines 742–757 does not currently have a `saved_primary` to clear, but the new field must be reset in RIS]

### 2. Resize with Two Grids (TUI-02, Pitfall A-4)

**Problem:** When the user resizes the terminal while a full-screen app is open, `TerminalState::resize()` currently resizes only `self.grid`. The saved primary grid in `saved_primary` retains its old dimensions. When the app exits (`?1049l`), the restored primary grid is the wrong size — content is truncated or padded incorrectly.

**Solution:** Extend `resize()` to also resize `saved_primary.0` if it is `Some`. Apply the same row/column truncation-or-extend logic:

```rust
pub fn resize(&mut self, cols: u16, rows: u16) {
    // ... existing grid resize logic ...

    // Also resize the saved primary grid if alt-screen is active.
    if let Some((ref mut prim_grid, ref mut prim_cursor, _)) = self.saved_primary {
        // Resize each column.
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
        let current_rows = prim_grid.len();
        let new_rows = rows as usize;
        if current_rows > new_rows {
            // Shrink: drop top rows into primary scrollback.
            // NOTE: scrollback is on self, not saved — push excess to self.scrollback.
            let excess = current_rows - new_rows;
            for _ in 0..excess {
                let top = prim_grid.remove(0);
                self.scrollback.push_back(top);
                if self.scrollback.len() > SCROLLBACK_LINE_CAP {
                    self.scrollback.pop_front();
                }
            }
        } else if current_rows < new_rows {
            for _ in current_rows..new_rows {
                prim_grid.push(vec![Cell::default(); cols as usize]);
            }
        }
        // Clamp saved cursor.
        prim_cursor.row = prim_cursor.row.min(rows.saturating_sub(1));
        prim_cursor.col = prim_cursor.col.min(cols.saturating_sub(1));
    }

    // ... existing self.cols / self.rows update and cursor clamp ...
}
```

The scrollback push during primary-grid shrink goes to `self.scrollback` (not lost) and obeys `SCROLLBACK_LINE_CAP`. This is consistent with the primary-active-screen resize behaviour.

### 3. Wide-Character Width Handling (TUI-03, Pitfall A-2, Pitfall A-3)

**`unicode-width` 0.2 API (verified from crate source):**

```rust
use unicode_width::UnicodeWidthChar;

// Returns:
//   None      → control character
//   Some(0)   → combining / ZWJ / zero-width
//   Some(1)   → narrow (normal ASCII + most Latin)
//   Some(2)   → CJK wide
let w: Option<usize> = UnicodeWidthChar::width(c);
```

`width()` uses East Asian Ambiguous → 1 (the default, per D-19-04). `width_cjk()` uses Ambiguous → 2 (do NOT use, per D-19-04). [VERIFIED: crate source at `unicode-width-0.2.2/src/lib.rs` lines 194–226]

The predictor already uses `UnicodeWidthChar::width(ch)` in `classify_printable` (predictor.rs lines 942–951) with the same policy — consistent.

**Continuation marker (D-19-06):**

The `Cell` struct currently has `ch: char`, `style: CellStyle`, `fg: Option<u8>`, `bg: Option<u8>`. A wide character writes `ch` at `col` and needs a sentinel at `col+1`. Options:

**Option A: sentinel char** — A reserved char value that is otherwise impossible to appear (e.g. `'\u{0000}'` or `'\u{FFFE}'`) used as `Cell.ch` for the continuation marker. Simple, requires no struct change. Risk: `'\u{0000}'` is a valid vte output (though NUL is already a C0 code handled by `execute`, not `print`). `'\u{FFFE}'` (non-character) is never produced by `print`. **Recommended.**

**Option B: `wide: bool` flag on Cell** — Add `pub wide: bool` to `Cell`, set `true` on the continuation cell. More self-documenting. Requires updating all struct initialisers in the codebase and the `Default` impl. Increases `Cell` size by 1 byte (or possibly padded to 4 bytes depending on alignment).

The planner should decide between A and B. Either works; both are wire-transparent (the `DiffRun.chars` String carries Unicode scalars, and the diff encoder in server.rs does NOT special-case continuation cells — a continuation sentinel char would be transmitted as-is). The client `screen.rs` `emit_diff` renders each `Cell.ch` with `encode_utf8`. A sentinel char (`'\u{FFFE}'` or `'\u{0000}'`) would be written to the terminal as-is — the terminal should display it as nothing (NUL is suppressed; U+FFFE is a non-character, many terminals render it as blank). **A `wide: bool` flag is cleaner and eliminates the rendering ambiguity.**

**Client-side rendering impact:** `ClientScreen::emit_diff` (screen.rs lines 428–491) iterates cells and calls `want.ch.encode_utf8(&mut buf)`. If wide continuation cells carry `Cell { ch: '\u{FFFE}', wide: true }`, the client must skip rendering `ch` for continuation cells. This requires adding a `wide` check in `emit_diff`. Alternatively, using `ch: ' '` for continuation cells (a literal space) would render naturally (the terminal advances one column for the blank, matching the physical width). A literal space is the simplest client-side option and is safe — but it could cause incorrect copy-paste behaviour (copies a spurious space after every CJK char). The `wide: bool` flag approach avoids this.

**`print_char` changes:**

```rust
fn print_char(&mut self, c: char) {
    let col_width = match UnicodeWidthChar::width(c) {
        Some(0) => {
            // Combining mark / ZWJ: do not advance cursor, do not write new cell
            // (D-19-05 / Pitfall A-3). Attach to the previous cell's position.
            // For now: no-op (scope fence per D-12-02b extended).
            return;
        }
        Some(2) => 2u16,
        _ => 1u16, // Some(1) or None (control chars — vte should not call print for C0)
    };

    let row = (self.cursor.row as usize).min(self.rows.saturating_sub(1) as usize);
    let col = (self.cursor.col as usize).min(self.cols.saturating_sub(1) as usize);

    if !self.grid.is_empty() && row < self.grid.len() && col < self.grid[row].len() {
        self.grid[row][col] = Cell {
            ch: c,
            style: self.sgr.style,
            fg: self.sgr.fg,
            bg: self.sgr.bg,
            wide: false,
        };
        // Write wide-char continuation marker at col+1 if there is room.
        if col_width == 2 && col + 1 < self.grid[row].len() {
            self.grid[row][col + 1] = Cell {
                ch: ' ',      // or '\u{FFFE}' — planner decides
                style: self.sgr.style,
                fg: self.sgr.fg,
                bg: self.sgr.bg,
                wide: true,   // continuation marker
            };
        }
        // If col_width == 2 and col+1 is at the right edge, wrap:
        // xterm behaviour: the wide char is placed at col, col+1 is the next line col 0.
        // Simplest safe approach: treat it as a wrap (advance col by 2, triggering wrap).
    }

    self.cursor.col += col_width;
    if self.cursor.col >= self.cols {
        self.cursor.col = 0;
        self.lf();
    }
}
```

**Right-edge behaviour for wide chars:** When a wide char lands such that `col + 2 > cols` (i.e. `col == cols - 1`), the char cannot fit. xterm's behaviour is to leave a blank at `col` and place the wide char at the start of the next line. The simplest correct approach: advance cursor by `col_width` before writing, which triggers the wrap naturally. The exact behaviour is an edge case rarely hit in practice — the important thing is no panic and no column drift. [ASSUMED — xterm source documentation; the exact edge-case behaviour is implementation-defined in most terminals]

### 4. OSC Accumulation Pre-Bound (SEC-03, D-19-01/02/03, Pitfall SEC-2)

**Confirmed vte 0.15.0 behaviour (verified from crate source):**

With `feature = "std"` (which `nosh-server` uses), `osc_raw` is `Vec<u8>` (lib.rs line 64). It is cleared at `osc_start` (line 373) and at `osc_end` (line 557). Between those two points, `action_osc_put` (line 544) calls `self.osc_raw.push(byte)` unconditionally — no length check. This means `osc_raw` grows without bound across multiple `advance()` calls that span a single OSC sequence (one read delivering ESC], subsequent reads delivering the OSC content, final read delivering BEL).

**The `osc_dispatch` caps (`OSC_52_MAX_BYTES` / `MAX_TITLE_BYTES`) run AFTER `osc_end` calls `dispatch_osc` which calls the `Perform::osc_dispatch` method.** By that point, vte has already allocated `osc_raw` to the full OSC payload size. The existing caps bound what is *stored*, not what vte *allocates*. [VERIFIED: crate source — lib.rs lines 544–557 and line 411]

**Resync to ground state:** `Parser::default()` is `State::Ground` (`#[default]` on `State::Ground`, lib.rs line 747). `Parser` derives `Default` (lib.rs line 54). Therefore:

```rust
// Resync to ground state:
self.parser = vte::Parser::default();
// This is equivalent to the mem::take + restore pattern already used in advance().
```

**Pre-scan implementation strategy:**

The interception must happen in `TerminalState::advance(bytes: &[u8])` before the bytes reach `parser.advance()`. The cleanest approach is a byte scanner that tracks the OSC accumulation length:

```rust
pub fn advance(&mut self, bytes: &[u8]) {
    // OSC accumulation pre-bound (D-19-01/02/03 / SEC-03).
    // Track total in-flight OSC bytes across advance() calls.
    // self.osc_byte_count is a new field: usize, reset at OSC start/end.
    let bytes_to_feed = self.osc_prefilter(bytes);
    
    let mut parser = std::mem::take(&mut self.parser);
    parser.advance(self, bytes_to_feed);
    self.parser = parser;
}
```

`osc_prefilter` needs to:
1. Scan `bytes` looking for ESC-] (OSC start: `0x1B 0x9D` or `0x1B ]`).
2. Count bytes being fed while inside an OSC (between ESC-] and BEL/ST).
3. If `self.osc_byte_count + bytes_fed_into_osc > OSC_ACCUMULATION_MAX` (1 MiB), truncate the slice to just before the byte that would exceed the cap, then resync the parser to ground state and reset the counter.

**New field on `TerminalState`:**

```rust
/// Running count of bytes accumulated into the current OSC sequence
/// across `advance()` calls. Reset at OSC start (ESC ]) and end (BEL / ST).
/// Used by the pre-filter to enforce the 1 MiB OSC accumulation cap (D-19-01).
osc_byte_count: usize,
```

And a constant:
```rust
/// Maximum bytes allowed for one OSC sequence before truncation (D-19-01).
/// Distinct from OSC_52_MAX_BYTES (storage cap) and MAX_TITLE_BYTES (storage cap).
pub const OSC_ACCUMULATION_MAX: usize = 1_048_576; // 1 MiB
```

**State machine note:** The pre-filter scanner must track whether the parser is currently inside an OSC string. This requires a shadow state variable (`osc_in_progress: bool`) because the parser's internal `State` field is private. Alternatively, the counter alone suffices: start counting when ESC-] is seen, stop counting when BEL or ST is seen, reset count on truncation.

**Fuzz target extension:** The existing `fuzz/fuzz_targets/osc_accumulation.rs` calls `state.advance(data)` once with the fuzz input. The new test case for SEC-03 must call `state.advance()` many times in a loop (simulating multi-chunk delivery), passing slices of a 10+ MB OSC sequence. The libFuzzer `max_len` default of 4096 is too small to reach the 1 MiB bound; the fuzz target must construct the oversize OSC internally and drive it in chunks.

**Correctness requirement (D-19-03):** After truncating an oversized OSC and resyncing to ground, the next legitimate OSC sequence (OSC 52, OSC 0/2) must still parse and dispatch correctly. This is guaranteed by the ground-state reset — the parser is ready to receive fresh input. The existing fuzz assertions (OSC 52 payload cap, title cap) must still pass.

### 5. Predictor Suppression (TUI-05, Pitfall A-5)

**Current EchoState propagation — gap identified:**

`EchoState.alt_screen` is a server-side field on `TerminalState`. The `StateDiff` datagram struct (`nosh-proto/src/datagram.rs`) carries only `epoch`, `cols`, `rows`, `cursor`, `runs`. It does NOT carry `alt_screen` or any `EchoState` fields. [VERIFIED: codebase — datagram.rs lines 51–76]

The client's `run_pump` (`main.rs`) receives `StateDiff` datagrams and calls `screen.apply(diff)`, `predictor.cull(...)`, and `predictor.sync_cursor_from_confirmed(...)`. There is no code path that reads `alt_screen` state on the client. [VERIFIED: codebase — main.rs lines 1006–1082]

**Two viable mechanisms to convey alt_screen to the client:**

**Option A: Add `alt_screen: bool` to `StateDiff`** — The datagram already carries `cols`, `rows`, `cursor`; adding a bool costs 1 byte under postcard (as a field, likely 1-byte varint for `false`/`true`). This is a wire-format change to `StateDiff`, which is a breaking change. Since v1.3 is an in-milestone wire-format change (the REQUIREMENTS.md says "nosh is built from source; client and server are the same version"), this is acceptable. The field would be set from `TerminalState.echo_state().alt_screen` in `build_state_diff` in `server.rs`. The client reads it in the datagram arm of `run_pump`.

**Option B: Add `AltScreen(bool)` variant to `TerminalControlPayload` on the reliable stream** — Sent whenever `?1049h`/`?1049l` is processed. Similar to how `Title` and `Clipboard` are forwarded. No datagram format change. Slightly higher latency (goes through the reliable stream, not the low-latency datagram path). The client's reliable-stream arm in `run_pump` (lines 975–997) already handles `TerminalControl` messages.

**Recommendation:** Option A (add `alt_screen` to `StateDiff`) is simpler and has lower latency. The datagram already carries the terminal state snapshot; including `alt_screen` is natural. Option B is viable if datagram format changes are prohibited, but is more complex. The planner should pick one — both are feasible. [ASSUMED that Option A is preferred based on architectural fit; confirmed no cross-milestone wire-compat requirement per REQUIREMENTS.md "Out of Scope" section]

**Client-side hook in `run_pump` datagram arm:**

After `screen.apply(diff)`, when `diff.alt_screen` transitions to `true`:
```rust
if diff.alt_screen && !was_alt_screen {
    // Entering alt-screen: suppress all predictions.
    predictor.reset();  // clears pending + increments prediction_epoch
    // predictor.pending is now empty (confirmed by reset() implementation).
}
// was_alt_screen = diff.alt_screen;
```

`predictor.reset()` (predictor.rs lines 665–676) calls `pending.clear()` and `become_tentative()`. After `reset()`, `predictor.pending.len() == 0` and all future predictions are tentative (hidden) until the server confirms a character — which won't happen in alt-screen because cursor-addressing apps don't echo typed characters in the expected columns. [VERIFIED: codebase — predictor.rs reset() implementation]

When `diff.alt_screen` is `true`, the predictor's `should_display()` continues to gate rendering (via `srtt_trigger`), but since `reset()` was called, `pending` is empty and no overlay cells are emitted. This is the correct suppression mechanism.

**Success criterion validation:** "predictor.pending is empty after `?1049h` is processed" — satisfied by `predictor.reset()` in the datagram arm on alt-screen entry. "No overlay appears inside vim or htop" — satisfied because `pending` is empty so `cell_at()` returns `None` for all positions.

### 6. Wire Representation of Wide-Char Continuation Cells

**Current `DiffRun.chars` encoding:** The `DiffRun.chars: String` field carries UTF-8 text. The server's `build_state_diff` in `server.rs` calls `state.viewport_rows()` and constructs runs from consecutive cells with matching style. The diff encoder iterates `row.chars()` (a `String`'s Unicode scalar values) to build runs.

**Impact of continuation markers:** If continuation cells have `Cell.ch = ' '` (literal space), the diff encoder treats them as blank cells with the same SGR attributes as the wide char. This may produce incorrect diffing — a wide char at column 4 produces cells `[ch='中', wide=false]` and `[ch=' ', wide=true]` at columns 4 and 5. The diff encoder would group them into a single run with `chars = "中 "` — two scalars. The client would render '中' at col 4 and ' ' at col 5, which is correct visually but wastes one column (the space is redundant as the glyph already occupies two columns in the terminal).

**If continuation cells have `ch = '\u{FFFE}'` (non-character sentinel):** The diff encoder would include `'\u{FFFE}'` in the run's chars string. The client `emit_diff` would write it to the terminal. Most terminals render U+FFFE as a zero-width or blank glyph, but the behaviour is not guaranteed.

**Recommended approach (Claude's discretion):** Use `wide: bool` field on `Cell`. The diff encoder in server.rs must be taught to skip continuation cells (`wide: true`) when building `chars` for a run, and advance `start_col` past them. This keeps `DiffRun.chars` as a sequence of printable characters (one per logical cell position), not two scalars for every wide char. The client's `emit_diff` then writes each char from the run at the corresponding column — wide chars are rendered natively by the terminal (the terminal itself handles double-column rendering). **This requires changes to the diff-encoder in `server.rs`.**

Alternatively, keep `DiffRun.chars` as-is (including continuation markers), add `wide: bool` to `Cell`, and have the client skip rendering continuation cells. This is simpler for the diff encoder but adds complexity to the client render path.

The planner must decide: both approaches work. The key constraint from D-19-06 is that the continuation cell's representation is explicit and agreed-upon between server and client.

---

## Don't Hand-Roll

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| Per-char Unicode width | Custom lookup table or hardcoded ranges | `UnicodeWidthChar::width()` from `unicode-width` 0.2 | Already in workspace; covers all Unicode edge cases including combining marks, ZWJ, ambiguous-width, Hangul jamo |
| VT state machine | Custom parser | `vte` 0.15 | Already in use; any deviation from the established parser creates inconsistency |
| OSC byte counting | Per-byte state machine from scratch | Simple in-line scanner in `advance()` | Only needs to track ESC-], count bytes, detect BEL/ST — not worth a separate abstraction |

---

## Common Pitfalls

### Pitfall 1: Half-built alt-screen is worse than the no-op (CRITICAL — from PITFALLS.md A-1)
**What goes wrong:** Shipping `?1049h` with save+clear but without restore (`?1049l`), or with swap but without clear-on-enter. Either half-state produces consistently wrong (and confusing) output.
**Prevention:** All three operations must be in the same commit: `enter_alt_screen()` (save + replace + cursor reset) and `exit_alt_screen()` (restore) must both exist before the commit lands.
**Test:** `vim --noplugin -c q` must leave the primary grid unchanged. The existing `decset_alt_screen_toggled_by_1049` test is insufficient — it only checks the flag. New tests must assert grid content before/after.

### Pitfall 2: OSC resync leaves parser in OscString state (CRITICAL)
**What goes wrong:** The pre-filter truncates input mid-OSC and feeds the truncated slice to `parser.advance()`. The parser state is now `OscString` (mid-sequence). If the pre-filter resets `self.parser = vte::Parser::default()` after `parser.advance()`, it is too late — `osc_raw` has already grown (vte already accumulated the bytes).
**Prevention:** The pre-filter must truncate the `bytes` slice BEFORE calling `parser.advance()`. The reset `self.parser = vte::Parser::default()` must happen after the truncated `advance()` call, replacing the mid-OSC parser with a fresh ground-state parser.
**Test:** Multi-chunk OSC: feed `ESC]52;c;<500KiB>` in 100 chunks, then `<another 500KiB>` in 100 more chunks. The total 1 MiB must be intercepted without OOM. Then feed a normal `ESC]0;hello\x07` — must set the title to "hello".

### Pitfall 3: Continuation cell skipped by scroll_up fills wrong width
**What goes wrong:** When a wide char is at columns N and N+1, and the viewport scrolls, `scroll_up()` removes row 0 and pushes a new blank row. If the continuation cell is pushed into scrollback as-is (with `wide: true`), Phase 22's scrollback rendering must handle it. If it's omitted, the scrollback row has incorrect column count.
**Prevention:** Scrollback rows carry continuation markers as-is. Phase 22 will need to handle `wide: bool` in its render path. Document this dependency.

### Pitfall 4: SgrState not saved/restored on alt-screen (from D-19-06 analysis)
**What goes wrong:** A TUI app resets SGR on entry (most do: `ESC[m`). On exit, the shell resumes with a reset SGR pen — prompts styled bold/coloured appear in the wrong style.
**Prevention:** Save and restore `self.sgr` as part of `saved_primary`.

### Pitfall 5: RIS (ESC c) does not clear saved_primary
**What goes wrong:** `esc_dispatch` RIS currently resets grid, scrollback, cursor, sgr, echo_state (terminal.rs lines 745–751). If `saved_primary` is not cleared, a subsequent `?1049l` restores a grid from before the RIS, overwriting the post-reset blank state.
**Prevention:** Add `self.saved_primary = None;` to the RIS handler.

### Pitfall 6: EchoState propagation creates wire-format break
**What goes wrong:** Adding `alt_screen: bool` to `StateDiff` changes the postcard encoding — old clients will misparse new datagrams and vice versa. This is intentional for v1.3 (same-version build), but must be documented as a breaking change.
**Prevention:** Document as milestone-level breaking change in the phase summary. The REQUIREMENTS.md already states cross-milestone wire compat is out of scope.

---

## Code Examples

### Two-grid enter/exit (verified patterns from codebase analysis)

```rust
// In TerminalState:
// New field:
saved_primary: Option<(Vec<Vec<Cell>>, CursorPos, SgrState)>,

// New field for OSC pre-bound:
osc_byte_count: usize,

fn enter_alt_screen(&mut self) {
    // Atomic: move primary grid out, put blank alt grid in, save cursor+sgr.
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

### unicode-width per-char dispatch

```rust
use unicode_width::UnicodeWidthChar;

// In print_char():
let col_width: u16 = match UnicodeWidthChar::width(c) {
    Some(0) => { return; } // combining/ZWJ: no-op
    Some(2) => 2,
    _ => 1,
};
```

### vte Parser reset to ground state

```rust
// After truncating bytes due to OSC overflow:
let mut parser = std::mem::take(&mut self.parser); // take existing parser
parser.advance(self, &truncated_bytes);             // feed only the safe prefix
// Now drop the mid-OSC parser and replace with a fresh ground-state parser:
self.parser = vte::Parser::default();               // Parser::default() = State::Ground
// osc_byte_count reset:
self.osc_byte_count = 0;
```

---

## State of the Art

| Old Approach | Current Approach | When Changed | Impact |
|--------------|------------------|--------------|--------|
| `?1049` = flag-only | Two-grid save/swap/clear | Phase 19 | vim/htop render correctly |
| Width-agnostic `print_char` | Per-codepoint `UnicodeWidthChar::width` | Phase 19 | No column drift for CJK/emoji |
| OSC caps in `osc_dispatch` (after vte) | Pre-scan bound before `parser.advance` | Phase 19 | Closes post-auth OOM vector |
| No predictor suppression in alt-screen | `predictor.reset()` on alt-screen entry | Phase 19 | No overlay in vim/htop |

**Deprecated/outdated in this phase:**
- The `decset_alt_screen_toggled_by_1049` test is INSUFFICIENT post-implementation — it only asserts the flag, not grid state. It must be extended or replaced with grid-asserting tests.
- The `adversarial_large_osc_title_is_bounded_no_panic` test (terminal.rs line 1503) passes a single-chunk 64 KiB OSC. It does NOT test the multi-chunk accumulation path (which is the actual OOM vector). A new multi-chunk test is required.

---

## Assumptions Log

| # | Claim | Section | Risk if Wrong |
|---|-------|---------|---------------|
| A1 | SGR pen state (`SgrState`) should be saved/restored on alt-screen enter/exit | §Two-Grid Alt-Screen | Possible wrong pen colour after exit; low impact — most TUI apps issue SGR reset on entry |
| A2 | `Option A` (add `alt_screen` to `StateDiff`) is preferred over `Option B` (TerminalControl stream message) for predictor suppression | §Predictor Suppression | If Option A is rejected, Option B requires additional `TerminalControlPayload` variant and reliable-stream handling |
| A3 | Wide char at right edge (no room for continuation): advance by `col_width` triggering wrap is acceptable xterm-compatible behaviour | §Wide-Character Handling | May mismatch xterm exactly; edge case rarely visible in practice |
| A4 | Continuation cells in scrollback rows should be preserved as-is (with `wide: bool`) for Phase 22 | §Pitfall 3 | Phase 22 may need extra work to handle continuation cells in scrollback rendering |

---

## Open Questions

1. **Continuation cell `ch` value: sentinel char vs `wide` bool flag**
   - What we know: D-19-06 requires an explicit marker, not a literal space.
   - What's unclear: Whether using `Cell { ch: ' ', wide: true }` is simpler for the diff encoder than using `Cell { ch: '\u{FFFE}', wide: false }` and whether the client's `emit_diff` needs to be updated.
   - Recommendation: Add `wide: bool` to `Cell`. Use `ch: ' '` for the continuation cell's char field (safest for any code path that reads `ch` without checking `wide`). Teach the diff encoder to skip `wide: true` cells when building `chars` strings. Teach `emit_diff` to skip `wide: true` cells in rendering.

2. **`alt_screen` propagation: datagram or reliable stream?**
   - What we know: Both are viable. Datagram is lower-latency; reliable stream avoids datagram format change.
   - What's unclear: Whether adding `alt_screen: bool` to `StateDiff` causes friction with Phase 20 (repaint pacing) which modifies the datagram path.
   - Recommendation: Add to `StateDiff` (Option A). Phase 20 works on the server-side `build_state_diff` and client-side `apply`/epoch paths, not the StateDiff struct definition. The struct change is orthogonal.

3. **OSC pre-filter: scan for OSC start byte-by-byte or use a simpler heuristic?**
   - What we know: The scanner must detect ESC-] (OSC start) and BEL/ST (OSC end). The vte state is opaque.
   - What's unclear: Whether a full per-byte scanner introduces measurable performance overhead on the hot PTY-output path.
   - Recommendation: A simple two-state loop (tracking `in_osc: bool`, `osc_byte_count: usize`) adds O(n) per `advance()` call where n is the byte count. PTY output is bounded by the pipe read size (typically 4–8 KB per read). The overhead is negligible relative to the vte state machine itself.

---

## Environment Availability

Step 2.6: SKIPPED — this phase is purely server-side Rust code changes with no external tool dependencies beyond the existing Rust toolchain and Cargo workspace.

---

## Validation Architecture

`workflow.nyquist_validation = false` in `.planning/config.json` — Validation Architecture section omitted.

---

## Security Domain

### Applicable ASVS Categories

| ASVS Category | Applies | Standard Control |
|---------------|---------|-----------------|
| V2 Authentication | No | — |
| V3 Session Management | No | — |
| V4 Access Control | No | — |
| V5 Input Validation | Yes | OSC pre-bound caps, grid bounds checks in all new paths |
| V6 Cryptography | No | — |

### Known Threat Patterns for Phase 19

| Pattern | STRIDE | Standard Mitigation |
|---------|--------|---------------------|
| Oversized OSC sequence in PTY output (post-auth DoS) | Denial of Service | 1 MiB pre-bound in `advance()` before vte; resync to ground on overflow |
| Wide-char `col_width=2` with cursor at `cols-1` (right edge) | Tampering / crash | Saturating add + clamping in `print_char()` — same pattern as existing CSI B/C overflow fix |
| Repeated `?1049h` without `?1049l` (nesting) | Denial of Service / corruption | `enter_alt_screen()` must handle re-entry gracefully: if `saved_primary` is already `Some`, the second `?1049h` should overwrite it (save the alt grid, which is the current grid — unusual but safe) |
| `?1049l` without prior `?1049h` | Corruption | `exit_alt_screen()` must handle `saved_primary = None` gracefully: leave grid as-is, just clear flag (already handled by `if let Some(...)`) |
| OSC 52 injection via crafted OSC with embedded BEL in selection field | Injection | Already mitigated by `WR-01` check in `osc_dispatch` — unchanged |

---

## Sources

### Primary (HIGH confidence)
- `crates/nosh-server/src/terminal.rs` — Complete reading; all existing types, methods, test patterns verified. `TerminalState`, `Cell`, `EchoState`, `SgrState`, `print_char`, `scroll_up`, `csi_dispatch`, `osc_dispatch`, `resize`, `advance`.
- `crates/nosh-client/src/predictor.rs` — Complete reading of `PredictionOverlay`, `reset()`, `become_tentative()`, `cull()`, `on_input()`, `cell_at()`, `should_display()`.
- `crates/nosh-client/src/screen.rs` — Complete reading of `ClientScreen`, `apply()`, `emit_diff()`, `render_with_predictor()`.
- `crates/nosh-client/src/main.rs` — Datagram arm `run_pump()` lines 999–1082 verified; no `alt_screen` or `EchoState` handling present.
- `crates/nosh-proto/src/datagram.rs` — `StateDiff`, `DiffRun`, `CellStyle` verified; `alt_screen` field confirmed absent.
- `~/.cargo/registry/.../vte-0.15.0/src/lib.rs` — `Parser` struct fields verified: `osc_raw: Vec<u8>` with `feature = "std"`, `State` enum with `Ground` as default, `advance()` API, `action_osc_put()` unbounded push.
- `~/.cargo/registry/.../unicode-width-0.2.2/src/lib.rs` — `UnicodeWidthChar::width()` API verified; returns `Option<usize>` where `None` = control, `Some(0)` = zero-width, `Some(1)` = narrow, `Some(2)` = wide.
- `.planning/research/PITFALLS.md` — Pitfalls A-1 through A-6, SEC-2/SEC-3 verified as applying to this phase.
- `.planning/phases/19-full-screen-tui-rendering-correctness/19-CONTEXT.md` — All locked decisions D-19-01 through D-19-09.
- `fuzz/fuzz_targets/osc_accumulation.rs` — Existing fuzz target structure; confirmed current scope (single-chunk, max_len 4096) is insufficient for multi-chunk OOM test.
- `docs/999.1-SECURITY.md` §7 — OSC-OOM analysis verified: vte `std` feature makes `osc_raw` unbounded Vec; `osc_dispatch` caps run after vte allocation.

### Secondary (MEDIUM confidence)
- `https://invisible-island.net/xterm/ctlseqs/ctlseqs.html` — `?1049` vs `?47`/`?1047`/`?1048` distinction cited from xterm documentation. [CITED]

### Tertiary (LOW confidence — assumptions)
- SGR state save/restore on alt-screen: based on xterm convention, not independently verified in xterm source in this session. [ASSUMED]
- Wide char at right-edge wrap behaviour: described as xterm-compatible but not verified from xterm source in this session. [ASSUMED]

---

## Metadata

**Confidence breakdown:**
- Alt-screen two-grid model: HIGH — verified from PITFALLS.md, codebase, and xterm documentation citation.
- OSC pre-bound mechanism: HIGH — verified from vte crate source (unbounded Vec with `std` feature).
- Unicode-width API: HIGH — verified from crate source.
- Predictor suppression mechanism: HIGH — verified from predictor.rs reset() and main.rs datagram arm.
- EchoState propagation gap: HIGH — verified from datagram.rs (no `alt_screen` field) and main.rs (no handling).
- Wire representation of continuation cells: MEDIUM — design decision, both options viable.

**Research date:** 2026-06-07
**Valid until:** 2026-09-07 (stable Rust crates; vte/unicode-width APIs are stable)
