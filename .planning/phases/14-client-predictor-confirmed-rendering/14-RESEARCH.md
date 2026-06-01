# Phase 14: Client Predictor — Confirmed Rendering - Research

**Researched:** 2026-06-02
**Domain:** Framebuffer-diff compositor, ANSI terminal rendering, datagram apply loop, run_pump integration
**Confidence:** HIGH

---

<user_constraints>
## User Constraints (from CONTEXT.md)

### Locked Decisions

- **D-14-01:** Framebuffer-diff compositor (Mosh Display model). `ClientScreen` holds (a) a CONFIRMED grid and (b) a PHYSICAL grid. `render_to_stdout()` computes `desired = confirmed + overlays`, diffs `desired` against `physical`, emits MINIMAL ANSI (cursor moves + SGR + changed cells), then sets `physical = desired`.
- **D-14-01a (compositor built now):** Phase 14 has two layers: the confirmed grid and a no-op `ConnectionLossOverlay` stub. Phase 15 adds the speculative-echo layer; Phase 16 activates the loss overlay. The single path (`render_to_stdout`) is the only writer to stdout for display.
- **D-14-02:** Datagram-only display. Once datagrams are active, display is purely the datagram-fed `ClientScreen` — NO direct `stdout.write_all` for display. At startup the screen is blank until the first datagram.
- **D-14-03:** The client STILL parses `PtyData` frames off the reliable stream to advance `highest_applied` and keep sending periodic `Ack { seq }` — but `PtyData` payload is NOT written to stdout anymore.
- **D-14-03a (epoch-ack):** This phase wires the REAL client datagram epoch-ack (D-13-01c) — the client acks the last-applied `epoch` so the server's acked-epoch baseline advances. Kept DISTINCT from the reliable-stream `Ack { seq }`.
- **D-14-04:** ClientScreen grid uses the same cell/style vocabulary as `StateDiff`/`DiffRun` (`fg`/`bg` `Option<u8>`, `CellStyle` bitflags), mirroring the Phase 12 server `TerminalState`. Direct map, no translation.
- **D-14-05:** Apply a `StateDiff` only if `epoch > last_applied_epoch`; full-keyframe (post-resume, empty baseline) replaces the confirmed grid. Resize diffs carry new dimensions → resize confirmed + physical grids.

### Claude's Discretion

- The minimal-ANSI diff algorithm details (cursor-move optimization, SGR run coalescing).
- Physical-grid representation; how overlays are represented as layers in the compositor.
- How the end-to-end test captures "visible characters" for comparison (drive a server `TerminalState` and a `ClientScreen` with the same byte stream and compare grids).

### Deferred Ideas (OUT OF SCOPE)

- Speculative local echo overlay (Phase 15).
- ConnectionLossOverlay activation (>5s silence banner) + OSC52/title (Phase 16).
- Client-side scrollback (M5).
</user_constraints>

<phase_requirements>
## Phase Requirements

| ID | Description | Research Support |
|----|-------------|------------------|
| PREDICT-01 | The client renders the confirmed terminal screen from received state-sync datagrams (display routed through a single screen-composition path, never direct `stdout` writes once the predictor exists), matching raw PTY output | ClientScreen struct + render_to_stdout compositor path; conn.read_datagram() arm in run_pump; end-to-end grid comparison test |
</phase_requirements>

---

## Summary

Phase 14 is pure Rust integration work inside `nosh-client` — no new crate dependencies are required. The client already has all the building blocks it needs: `StateDiff`/`DiffRun`/`CellStyle` types in `nosh-proto::datagram`, `Cell`-like semantics from the Phase 12 `TerminalState` (which uses the same `Option<u8>` fg/bg color model), `encode_epoch_ack` for the real epoch-ack, and a `tokio::select!` pump loop (`run_pump` in `main.rs`) that is ready for a new `conn.read_datagram()` arm.

The central deliverable is a new `ClientScreen` struct (in a new `screen.rs` module inside `nosh-client`) that owns the confirmed grid, a physical grid (what is currently painted on the terminal), and a no-op overlay seam. `render_to_stdout` diffs desired against physical and emits minimal ANSI — the key insight being that this is NOT a VT parser but a VT EMITTER: given a `desired: &[Vec<Cell>]`, it walks the diff and emits cursor-move + SGR + literal chars. The existing `crossterm` dependency handles raw mode and terminal sizing; actual ANSI byte emission is hand-rolled because it needs to be minimal (only changed cells) rather than high-level.

The run_pump integration removes the `stdout.write_all(&data)` call from the `PtyData` arm (keeping `highest_applied` counting), adds a `conn.read_datagram()` arm that calls `screen.apply(diff); screen.render_to_stdout()`, and emits `encode_epoch_ack(last_applied_epoch)` as a datagram after each apply. The `conn` reference must be added to `run_pump`'s signature.

**Primary recommendation:** Build `ClientScreen` as a self-contained module with a `Cell` type that mirrors `nosh_server::terminal::Cell` (same fields, same `nosh_proto::datagram` derived types), implement the confirmed-grid apply (D-14-05 monotonic epoch check), implement the framebuffer diff emitter using hand-rolled ANSI with crossterm for cursor positioning, then wire the datagram arm into `run_pump` and verify end-to-end by driving both a `TerminalState` (server side) and a `ClientScreen` (client side) with the same PTY byte stream and asserting grid equality.

---

## Architectural Responsibility Map

| Capability | Primary Tier | Secondary Tier | Rationale |
|------------|-------------|----------------|-----------|
| Confirmed terminal state (grid + cursor) | Client (nosh-client::screen) | — | Client must hold the last-applied confirmed state to diff against physical; server already has its own TerminalState |
| Physical screen state (what terminal shows) | Client (nosh-client::screen) | — | The diff algorithm needs to know what ANSI has already been emitted to avoid redundant writes |
| Overlay composition (desired = confirmed + overlays) | Client (nosh-client::screen) | — | Compositor layer seam owned by the client for Phase 15/16 extension; server never sees overlay state |
| ANSI framebuffer diff emission | Client (nosh-client::screen) | crossterm (cursor positioning) | Minimal diff is client-only logic; crossterm provides cursor_to(), terminal size; raw ANSI SGR by hand |
| Datagram receive + apply | Client (main.rs run_pump) | nosh-client::screen | Quinn conn.read_datagram() arm in run_pump feeds into ClientScreen::apply(); separation maintained |
| Epoch-ack emission | Client (main.rs run_pump) | nosh-proto::encode_epoch_ack | After apply, encode_epoch_ack(last_epoch) sent as datagram — same arm, one call |
| Reliable-stream Ack advancement | Client (main.rs run_pump) | — | PtyData arm unchanged: still increments highest_applied, still sends periodic Ack{seq}; no display write |

---

## Standard Stack

### Core (all already in nosh-client Cargo.toml — no new dependencies)

| Library | Version | Purpose | Why Standard |
|---------|---------|---------|--------------|
| `nosh-proto` (workspace) | 0.1.0 | `StateDiff`, `DiffRun`, `CellStyle`, `CursorPos`, `decode_datagram`, `encode_epoch_ack` | The wire format is Phase 11; D-14-04 mandates reuse |
| `crossterm` | 0.29 | Terminal cursor positioning for ANSI emitter; terminal size | Already in nosh-client; provides `cursor::MoveTo`, `terminal::size()`; DO NOT use for writing cell content |
| `tokio` (workspace) | 1.x | Async runtime for select! arm | quinn integration |
| `quinn` (workspace) | 0.11.9 | `conn.read_datagram()` for receiving StateDiff datagrams | Existing transport layer |
| `bytes` (workspace) | 1.x | `conn.send_datagram(Bytes)` for epoch-ack | encode_epoch_ack returns Bytes; send_datagram accepts Bytes |

[VERIFIED: crates/nosh-client/Cargo.toml — all above are current dependencies]

### No New Dependencies

Phase 14 requires zero new crates. The `crossterm` crate (already present) provides `cursor::MoveTo` for the ANSI emitter. SGR byte sequences are hand-rolled (they are trivial, ~10 lines) because the high-level crossterm API does not offer minimal-diff mode.

**Why NOT crossterm's `SetForegroundColor` / `SetBackgroundColor` for SGR?**  
crossterm's color API is designed for single-call styling, not for run-coalesced minimal diffs. Hand-rolling `\x1b[{attrs}m` avoids the crossterm serialization path and produces shorter output (e.g., `\x1b[0m` reset once per run boundary vs. per-cell overhead). [ASSUMED: judgment call; crossterm's style API has no "emit once for a run" primitive]

### Alternatives Considered

| Instead of | Could Use | Tradeoff |
|------------|-----------|----------|
| Hand-rolled ANSI SGR emitter | `termwiz::render` | termwiz owns a full terminal emulator with render path, but Phase 14 only needs a tiny emit-changed-cells loop. Adding termwiz (a large crate) is overkill when the diff emitter is ~100 lines. [ASSUMED: size judgment] |
| `crossterm::cursor::MoveTo` for cursor | Raw `\x1b[{row+1};{col+1}H` | Either works; crossterm's MoveTo is cleaner and already in scope |

---

## Package Legitimacy Audit

> No new packages are installed in Phase 14. All dependencies are already in the workspace Cargo.toml and have been in use since earlier phases.

| Package | Registry | Status | Disposition |
|---------|----------|--------|-------------|
| `crossterm` | crates.io | Already in nosh-client (0.29) — slopcheck flagged as [SUS] (false positive: proximity to "crossbeam") | Approved — confirmed well-known terminal library at crates.io/crates/crossterm, 30M+ downloads |
| `nosh-proto`, `quinn`, `bytes`, `tokio` | workspace | In use since Phase 0–11 | Approved |

**Packages removed due to slopcheck [SLOP] verdict:** none
**Packages flagged as suspicious [SUS]:** crossterm (false positive — "crossterm" is not a typosquat of "crossbeam"; it is the primary Rust cross-platform terminal library, extensively referenced in the Rust TUI ecosystem. The slopcheck alert is purely name-similarity based.)

---

## Architecture Patterns

### System Architecture Diagram

```
PTY bytes (from server)
        │
        ▼
  QUIC bidi stream  ──────────────────────────────┐
  (PtyData frames)                                │
        │                                         │
        │  run_pump: PtyData arm                  │  run_pump: datagram arm
        │  • highest_applied += 1                 │  • decode_datagram(bytes) → StateDiff
        │  • DO NOT write to stdout               │  • if diff.epoch > last_applied_epoch:
        │  • periodic Ack{seq} via send_ack       │      screen.apply(diff)
        │                                         │      render_to_stdout()
        │                                         │      send_datagram(encode_epoch_ack(epoch))
        ▼                                         ▼
  highest_applied ──────────────────►   ClientScreen
  (reattach Ack)                        ┌────────────────────────────────────┐
                                        │ confirmed: Vec<Vec<Cell>>          │
                                        │ physical:  Vec<Vec<Cell>>          │
                                        │ last_applied_epoch: u64            │
                                        │ overlays: [ConnectionLossOverlay]  │
                                        └───────────┬────────────────────────┘
                                                    │ render_to_stdout()
                                                    ▼
                                        desired = confirmed ⊕ overlays (no-op this phase)
                                        diff(desired, physical) → emit ANSI
                                        physical = desired
                                                    │
                                                    ▼
                                               stdout (terminal)
```

### Recommended Project Structure

```
crates/nosh-client/src/
├── main.rs          # run_pump: add conn param, add datagram arm, remove PtyData→stdout
├── client.rs        # unchanged (helpers, no display logic)
├── platform.rs      # unchanged (ResizeWatcher, quit_signal)
├── lib.rs           # add pub mod screen;
└── screen.rs        # NEW: ClientScreen, Cell, Overlay trait, ConnectionLossOverlay stub
                     #      render_to_stdout, apply(StateDiff), resize(cols, rows)
```

### Pattern 1: ClientScreen Cell Type

The `Cell` type in `screen.rs` mirrors `nosh_server::terminal::Cell` exactly (same fields, no conversion needed when applying a `DiffRun`). It does NOT import from `nosh_server` — it re-declares the same fields using the shared types from `nosh_proto::datagram`.

```rust
// Source: inferred from nosh_server/src/terminal.rs Cell struct (same design, no import)
use nosh_proto::datagram::{CellStyle, CursorPos};

#[derive(Clone, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub style: CellStyle,
    pub fg: Option<u8>,
    pub bg: Option<u8>,
}

impl Default for Cell {
    fn default() -> Self {
        Cell { ch: ' ', style: CellStyle(CellStyle::NONE), fg: None, bg: None }
    }
}
```

[ASSUMED: declaring a local `Cell` in screen.rs rather than pub-re-exporting from nosh_server; the design decision is that nosh-client should not depend on nosh-server directly (nosh-server is a dev-dependency only)]

Note: `nosh-server` is in `[dev-dependencies]` of nosh-client's Cargo.toml — it is not a production dependency. Therefore `ClientScreen` MUST declare its own `Cell` type (or import `nosh_proto::datagram` types directly) and MUST NOT reference `nosh_server::terminal::Cell` in production code.

### Pattern 2: StateDiff Apply — Monotonic Epoch Guard (D-14-05)

```rust
// Source: D-14-05 (CONTEXT.md), D-11-03 (SYNC-01 datagram.rs doc)
pub fn apply(&mut self, diff: &StateDiff) {
    // D-14-05: monotonic staleness check — only apply newer epochs.
    if diff.epoch <= self.last_applied_epoch {
        return; // stale or duplicate datagram — idempotent discard
    }

    // Resize if dimensions changed (resize diffs carry updated cols/rows).
    if diff.cols != self.cols || diff.rows != self.rows {
        self.resize(diff.cols, diff.rows);
    }

    // Full-keyframe detection: when last_applied_epoch == 0, the server has
    // reset the acked baseline (post-resume), so this diff is a full-screen
    // repaint. No special handling needed — runs cover all changed cells.

    // Apply each DiffRun to the confirmed grid.
    for run in &diff.runs {
        let row = run.row as usize;
        if row >= self.confirmed.len() {
            continue; // out-of-bounds guard
        }
        let row_cells = &mut self.confirmed[row];
        let mut col = run.start_col as usize;
        for ch in run.chars.chars() {
            if col >= row_cells.len() {
                break; // out-of-bounds guard
            }
            row_cells[col] = Cell {
                ch,
                style: run.style,
                fg: run.fg,
                bg: run.bg,
            };
            col += 1;
        }
    }

    // Update cursor from the diff.
    self.confirmed_cursor = diff.cursor;
    self.last_applied_epoch = diff.epoch;
}
```

[ASSUMED: the post-resume "full-keyframe" case needs no special path — the server's empty `last_acked_snapshot` at reattach produces a full-screen diff naturally (per D-13-01b and 13-02-SUMMARY.md). No `is_keyframe` flag is needed.]

### Pattern 3: Framebuffer Diff Emitter (render_to_stdout)

The Mosh Display model: compare `desired` against `physical` cell-by-cell, group consecutive changed cells into runs sharing the same SGR attributes, emit cursor-move + SGR + chars for each run, set `physical = desired`.

```rust
// Source: Mosh's terminal/display.cc concept, adapted for nosh's Cell model.
// crossterm::cursor::MoveTo provides cursor positioning.
// SGR hand-rolled for minimal output.

use std::io::Write;
use crossterm::QueueableCommand;
use crossterm::cursor::MoveTo;

pub fn render_to_stdout<W: Write>(&mut self, out: &mut W) -> std::io::Result<()> {
    // Compose desired = confirmed ⊕ overlays.
    // Phase 14: overlays is a no-op, so desired == confirmed.
    let desired = self.compose_desired();
    let desired_cursor = self.confirmed_cursor;

    let rows = desired.len().min(self.physical.len());
    let cols = if rows > 0 { desired[0].len().min(self.physical[0].len()) } else { 0 };

    let mut last_row: Option<u16> = None;
    let mut last_col: Option<u16> = None;
    let mut last_sgr: Option<(CellStyle, Option<u8>, Option<u8>)> = None;

    for row in 0..rows {
        for col in 0..cols {
            let want = &desired[row][col];
            let have = &self.physical[row][col];
            if want == have {
                continue; // no change — skip
            }

            // Need to move cursor to (row, col) if not already there.
            let need_move = last_row != Some(row as u16) || last_col != Some(col as u16);
            if need_move {
                out.queue(MoveTo(col as u16, row as u16))?;
                last_row = Some(row as u16);
                last_col = Some(col as u16);
            }

            // Emit SGR if attributes changed.
            let want_sgr = (want.style, want.fg, want.bg);
            if last_sgr != Some(want_sgr) {
                emit_sgr(out, want.style, want.fg, want.bg)?;
                last_sgr = Some(want_sgr);
            }

            // Write the character.
            let mut buf = [0u8; 4];
            let s = want.ch.encode_utf8(&mut buf);
            out.write_all(s.as_bytes())?;

            // Advance tracked position (single-width chars only this phase).
            last_col = Some(last_col.unwrap_or(col as u16) + 1);
        }
    }

    // Position cursor at the confirmed cursor position.
    out.queue(MoveTo(desired_cursor.col, desired_cursor.row))?;
    out.flush()?;

    // Update physical grid.
    for (row, (des_row, phys_row)) in desired.iter().zip(self.physical.iter_mut()).enumerate() {
        for (des_cell, phys_cell) in des_row.iter().zip(phys_row.iter_mut()) {
            *phys_cell = des_cell.clone();
        }
    }
    // Update physical cursor tracking.
    self.physical_cursor = desired_cursor;

    Ok(())
}
```

[ASSUMED: cursor advance logic for single-width chars — wide char handling is deferred to Phase 15 per CONTEXT.md]

### Pattern 4: SGR Emission (hand-rolled)

```rust
// Emit a minimal SGR sequence for the given cell attributes.
// Resets to \x1b[0m then applies active attributes.
// Called only when attributes change between adjacent cells.
fn emit_sgr<W: Write>(
    out: &mut W,
    style: CellStyle,
    fg: Option<u8>,
    bg: Option<u8>,
) -> std::io::Result<()> {
    // Always reset first (SGR 0), then re-apply.
    let mut parts: Vec<u8> = vec![b'0'];

    if style.0 & CellStyle::BOLD != 0     { parts.push(b'1'); }
    if style.0 & CellStyle::ITALIC != 0   { parts.push(b'3'); }
    if style.0 & CellStyle::UNDERLINE != 0 { parts.push(b'4'); }
    if style.0 & CellStyle::REVERSE != 0  { parts.push(b'7'); }

    // 256-color fg: \x1b[38;5;Nm
    // default fg: SGR 39 is NOT needed after SGR 0 (reset restores default)
    if let Some(n) = fg {
        // Write as "38;5;N"
        write!(out, "\x1b[38;5;{n}m")?;
        // ... remaining attrs in separate sequence if needed
    }
    // Similar for bg.
    // ... (full implementation is ~30 lines)
    Ok(())
}
```

[ASSUMED: exact SGR byte layout — the implementation detail. The pattern is well-understood ANSI; the exact sequencing (combine all attrs in one CSI vs. split) is left to the implementer.]

### Pattern 5: run_pump conn parameter threading

`run_pump` currently has signature:
```rust
async fn run_pump(
    send: &mut quinn::SendStream,
    recv: &mut quinn::RecvStream,
    highest_applied: &mut u64,
    resize: &mut platform::ResizeWatcher,
    _seq_baseline: u64,
) -> anyhow::Result<PumpOutcome>
```

Phase 14 must add `conn: &quinn::Connection` to enable `conn.read_datagram()` and `conn.send_datagram(...)`. The `conn` is already in scope in both `fresh_session` and `reattach_session` callers.

```rust
// AFTER Phase 14:
async fn run_pump(
    conn: &quinn::Connection,  // NEW
    send: &mut quinn::SendStream,
    recv: &mut quinn::RecvStream,
    highest_applied: &mut u64,
    resize: &mut platform::ResizeWatcher,
    _seq_baseline: u64,
) -> anyhow::Result<PumpOutcome>
```

[VERIFIED: client.rs and main.rs — conn is available in both fresh_session and reattach_session callers; threading it through run_pump is the minimal change]

### Pattern 6: Datagram Arm in run_pump select!

```rust
// New datagram arm — mirrors sync.rs test pattern exactly.
// Source: crates/nosh-client/tests/sync.rs (read_datagram loop)
datagram = conn.read_datagram() => {
    match datagram {
        Ok(bytes) => {
            if let Ok(diff) = nosh_proto::datagram::decode_datagram(&bytes) {
                if diff.epoch > screen.last_applied_epoch() {
                    screen.apply(&diff);
                    screen.render_to_stdout(&mut stdout).unwrap_or_else(|e| {
                        tracing::warn!("render_to_stdout error: {e}");
                    });
                    // D-14-03a: emit datagram epoch-ack (distinct from reliable Ack{seq}).
                    let ack_payload = nosh_proto::datagram::encode_epoch_ack(diff.epoch);
                    let _ = conn.send_datagram(ack_payload); // best-effort
                }
            }
            // Non-StateDiff datagrams (unknown tags): silently discard.
        }
        Err(_) => {
            return Ok(PumpOutcome::TransportDrop);
        }
    }
}
```

[VERIFIED: encode_epoch_ack and decode_datagram are in nosh_proto::datagram (datagram.rs lines 161-168, 430-447)]

### Pattern 7: PtyData arm — remove display write, keep counter

```rust
// BEFORE (current main.rs ~637-644):
Ok(Message::PtyData { data }) => {
    stdout.write_all(&data).await?;   // ← REMOVE THIS LINE
    stdout.flush().await?;             // ← REMOVE THIS LINE
    *highest_applied = highest_applied.saturating_add(1);
}

// AFTER (Phase 14):
Ok(Message::PtyData { data }) => {
    // D-14-03: advance reattach counter but do NOT write to stdout.
    // Display comes exclusively from datagrams via screen.render_to_stdout().
    let _ = data; // content discarded for display (no client-side scrollback this milestone)
    *highest_applied = highest_applied.saturating_add(1);
}
```

[VERIFIED: current PtyData arm in main.rs lines 637-644 — the write_all and flush calls are the two lines to remove]

### Pattern 8: Overlay Seam

Phase 14 introduces the overlay trait seam so Phase 15 can slot in without restructuring:

```rust
/// A screen overlay layer. Renders on top of the confirmed grid in compose_desired().
/// Phase 14: only ConnectionLossOverlay exists, and it is a no-op (returns None for all cells).
pub trait Overlay {
    /// Return Some(cell) to override the cell at (row, col), or None to pass through.
    fn cell_at(&self, row: u16, col: u16) -> Option<Cell>;
}

/// No-op connection-loss overlay stub (Phase 14).
/// Phase 16 activates this when no datagram is received for >5s.
pub struct ConnectionLossOverlay;

impl Overlay for ConnectionLossOverlay {
    fn cell_at(&self, _row: u16, _col: u16) -> Option<Cell> {
        None // no-op: pass through confirmed grid cell
    }
}
```

[ASSUMED: trait-based overlay seam — the exact API shape is Claude's discretion per CONTEXT.md. Static dispatch (enum) vs. dynamic dispatch (dyn Overlay) is also discretionary. Static is preferred in a hot path.]

### Anti-Patterns to Avoid

- **Writing PtyData to stdout after Phase 14 lands:** violates D-14-02 (single display path). The `stdout.write_all(&data)` call MUST be removed.
- **Holding a lock on TerminalState across an await point:** not applicable client-side (no server TerminalState), but the same discipline applies if any future mutex is added to ClientScreen.
- **Calling render_to_stdout from multiple places:** the CLAUDE.md mandate is "single screen-composition path, never direct stdout once predictor exists." Only the datagram arm's `render_to_stdout` call writes display.
- **Applying a stale epoch:** if `diff.epoch <= last_applied_epoch`, discard silently. The epoch is a monotonic counter; out-of-order or duplicate datagrams are expected under loss. [VERIFIED: datagram.rs doc "The client applies this diff only if `epoch > last_applied_epoch`"]
- **Sending epoch-ack on the reliable stream instead of as a datagram:** epoch-ack is `conn.send_datagram(encode_epoch_ack(epoch))`, not a `Message` variant. The two channels are DISTINCT. [VERIFIED: datagram.rs TAG_CLIENT_EPOCH (0x02), TAG_STATE_DIFF (0x01) are both datagram-channel payloads]
- **Resizing only the confirmed grid without resizing physical:** physical must also be resized (to blanks) so the next render_to_stdout diffes against the correct dimensions. Failure to resize physical produces corrupt output after a terminal resize.
- **Using `tokio::io::stdout()` and then calling `queue(MoveTo...)`:** crossterm's `QueueableCommand` works on `std::io::Write`, not `tokio::io::AsyncWrite`. The render path writes synchronously to a buffered `std::io::Stdout` (or a `Vec<u8>` in tests), then flushes. Use `std::io::stdout()` with a `BufWriter` in the render path, NOT `tokio::io::stdout()`.

---

## Don't Hand-Roll

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| Cursor positioning | Custom `\x1b[row;colH` formatter | `crossterm::cursor::MoveTo` + `QueueableCommand` | Already in scope; handles 1-based vs 0-based correctly; cross-platform |
| SGR attribute encoding | (none to avoid) | Hand-roll ~30 lines | This IS the correct approach — crossterm's high-level API is not suited for minimal-diff output; the SGR encoding is simple enough |
| Terminal size query | Raw ioctl/GetConsoleScreenBufferInfo | `crossterm::terminal::size()` | Already used in main.rs; cross-platform |
| VT parsing on the client | Custom VT parser | Not needed (no VT parsing client-side) | The client receives a parsed `StateDiff` from the server — it does NOT re-parse raw VT bytes |
| Datagram wire format | Custom framing | `nosh_proto::datagram::decode_datagram` | Phase 11 already built this; do NOT reimplement |

**Key insight:** The client-side terminal logic is an EMITTER, not a parser. The server parses VT (via `vte`) and produces `StateDiff`. The client applies `StateDiff` to its grid and emits minimal ANSI to the local terminal. These roles must not be confused.

---

## Runtime State Inventory

> Phase 14 is not a rename/refactor/migration phase. Omitted.

---

## Common Pitfalls

### Pitfall 1: tokio::io::stdout vs std::io::stdout in render path

**What goes wrong:** `crossterm::QueueableCommand` is implemented on `std::io::Write`. If you try to use it with `tokio::io::AsyncWrite` (e.g., `tokio::io::stdout()`), it will not compile.

**Why it happens:** The `run_pump` loop uses `tokio::io::stdout()` for async I/O. But the render path is synchronous (it produces a buffer of ANSI bytes).

**How to avoid:** Have `render_to_stdout` accept a `&mut impl std::io::Write` (or write to a `Vec<u8>`, then flush to tokio stdout with `stdout.write_all(buf).await?`). Either keep a separate `std::io::BufWriter<std::io::Stdout>` for the render path, or buffer to `Vec<u8>` and do a single async flush.

**Warning signs:** Compile error `the trait Write is not implemented for tokio::io::Stdout`.

### Pitfall 2: Physical Grid Not Resized on Resize Diff

**What goes wrong:** A resize diff changes `diff.cols` / `diff.rows`. The confirmed grid is resized correctly but the physical grid is not, so the diff loop compares cells at mismatched indices.

**Why it happens:** Physical grid remembers what was emitted to the terminal. After a resize, the terminal is blank — so physical must be reset to all-default cells at the new dimensions.

**How to avoid:** In `apply()`, when `diff.cols != self.cols || diff.rows != self.rows`, resize BOTH confirmed and physical. Physical should be reset to default cells (not copied) so the next render repaints the entire new screen.

**Warning signs:** Garbled display or index-out-of-bounds panic after the user resizes the terminal window.

### Pitfall 3: Epoch-Ack Sent on Wrong Channel (Reliable Stream vs Datagram)

**What goes wrong:** Developer calls `nosh_proto::write_message(send, Message::EpochAck {...})` (which doesn't exist) or tries to pack the epoch-ack into a `PtyData` frame, rather than `conn.send_datagram(encode_epoch_ack(epoch))`.

**Why it happens:** The two channels (reliable stream and datagrams) are parallel. The epoch-ack is a datagram (TAG_CLIENT_EPOCH = 0x02), not a stream message.

**How to avoid:** Always use `conn.send_datagram(encode_epoch_ack(diff.epoch))` in the datagram arm. The `Ack{seq}` on the reliable stream is for cold-reattach replay and must remain separate.

**Warning signs:** Server receives no epoch-acks → server baseline never advances → every diff is a full-screen repaint.

### Pitfall 4: render_to_stdout Called Outside Datagram Arm (Multiple Display Writers)

**What goes wrong:** A second call to `stdout.write_all` or a second call to `screen.render_to_stdout` from somewhere other than the datagram arm causes display corruption.

**Why it happens:** CLAUDE.md invariant: "All output to the local terminal goes through `ClientScreen.render_to_stdout()` — never direct `stdout.write_all` once the predictor exists." Multiple writers will interleave on the terminal.

**How to avoid:** Search for `stdout.write_all` in main.rs after Phase 14 and confirm the only remaining write is the `render_to_stdout` flush path.

**Warning signs:** Characters appearing twice, or server output interleaved with ANSI from the compositor.

### Pitfall 5: Screen Blank Until First Datagram (Expected Behavior)

**What goes wrong:** Tester sees blank screen for ~16ms after session open and reports a bug.

**Why it happens:** D-14-02 (datagram-only display). At startup the physical and confirmed grids are both blank. The first datagram (a full-screen diff, since `last_acked_snapshot` on the server is empty at session open) arrives within one 16ms tick.

**How to avoid:** This is intentional. Document in the code and test that "startup blank" is correct. The 16ms interval from Phase 13 guarantees the first diff arrives very quickly.

**Warning signs:** None — this is not a bug.

### Pitfall 6: Stale-Epoch Discard Silently Drops All Output

**What goes wrong:** `last_applied_epoch` is initialized to `u64::MAX` or some large value by mistake. Every received diff is `diff.epoch <= last_applied_epoch` and discarded. Screen stays blank forever.

**Why it happens:** Off-by-one in initialization.

**How to avoid:** Initialize `last_applied_epoch = 0`. The server starts at epoch 1 (Phase 13 increments epoch on first diff), so the first diff with `epoch = 1 > 0` is correctly applied.

**Warning signs:** Screen stays blank; epoch-ack for epoch 0 is never sent; server logs show epoch advancing but client shows nothing.

### Pitfall 7: crossterm MoveTo is 0-based Column, 1-based in raw ANSI

**What goes wrong:** If hand-rolling `\x1b[row;colH` instead of using `MoveTo`, row and col are 1-based. Using 0-based values causes cursor to always be off by one.

**Why it happens:** ANSI cursor positioning is 1-based; crossterm::cursor::MoveTo is 0-based (it adds 1 internally).

**How to avoid:** Use `crossterm::cursor::MoveTo(col as u16, row as u16)` (0-based) where MoveTo's first arg is COLUMN, second is ROW (note: reversed from mathematical convention).

**Warning signs:** Cursor consistently one row/column off from expected position.

---

## Code Examples

### End-to-End Test Strategy: Grid Comparison

The CONTEXT.md specifies: "drive a server TerminalState and a ClientScreen with the same byte stream and compare grids." This is the recommended unit test pattern:

```rust
// Source: inferred from terminal.rs tests + datagram.rs round-trip tests
// Pattern: deterministic in-process test, no QUIC, no tokio::test needed for pure logic

#[test]
fn grid_match_after_diff_round_trip() {
    // Server side: feed PTY bytes to TerminalState.
    let mut server_ts = nosh_server::terminal::TerminalState::new(80, 24);
    let pty_bytes = b"hello world\r\nline two\r\n";
    server_ts.advance(pty_bytes);

    // Extract diff vs empty baseline (epoch 1, no last_acked_snapshot).
    let diff = extract_diff_from_terminal_state(&server_ts, 1, &[]);

    // Client side: apply the diff to a ClientScreen.
    let mut screen = ClientScreen::new(80, 24);
    screen.apply(&diff);

    // Assert each cell matches.
    for row in 0..24u16 {
        for col in 0..80u16 {
            let server_cell = server_ts.cell(row, col);
            let client_cell = screen.confirmed_cell(row, col);
            assert_eq!(client_cell.ch, server_cell.ch,
                "mismatch at ({row},{col})");
            assert_eq!(client_cell.fg, server_cell.fg);
            assert_eq!(client_cell.bg, server_cell.bg);
            assert_eq!(client_cell.style, server_cell.style);
        }
    }
}
```

Note: `extract_diff_from_terminal_state` is a test helper that uses `compute_diff_runs` from `nosh_server::server` and `nosh_proto::datagram::encode_datagram` / `decode_datagram` — or it can call the server's existing `build_state_diff` via a test-accessible wrapper.

However: `nosh-server` is a `[dev-dependency]` of `nosh-client`. The grid-comparison test in `crates/nosh-client/tests/` CAN import `nosh_server::terminal::TerminalState` (it's available in dev-deps). This avoids duplicating the VT parsing logic.

[VERIFIED: nosh-server is in nosh-client's [dev-dependencies] — Cargo.toml line 49]

### ANSI SGR Emission Pattern (complete)

```rust
// Emit SGR for a changed cell. Called only when style/fg/bg differ from previous cell.
// Source: VT100/ANSI SGR specification; mirrors the codes used in nosh_server::terminal
fn emit_sgr<W: std::io::Write>(
    out: &mut W,
    style: nosh_proto::datagram::CellStyle,
    fg: Option<u8>,
    bg: Option<u8>,
) -> std::io::Result<()> {
    use nosh_proto::datagram::CellStyle;
    // Build the CSI params list. Start with reset (0).
    let mut params = String::from("0");
    if style.0 & CellStyle::BOLD != 0     { params.push_str(";1"); }
    if style.0 & CellStyle::ITALIC != 0   { params.push_str(";3"); }
    if style.0 & CellStyle::UNDERLINE != 0 { params.push_str(";4"); }
    if style.0 & CellStyle::REVERSE != 0  { params.push_str(";7"); }
    // 256-color fg: 38;5;N
    if let Some(n) = fg { params.push_str(&format!(";38;5;{n}")); }
    // 256-color bg: 48;5;N
    if let Some(n) = bg { params.push_str(&format!(";48;5;{n}")); }
    write!(out, "\x1b[{params}m")
}
```

[ASSUMED: using format! in render path — for ~16ms tick frequency this is fine; micro-optimization would use a pre-built byte array]

### Overlay Composition (compose_desired)

```rust
fn compose_desired(&self) -> Vec<Vec<Cell>> {
    // Phase 14: ConnectionLossOverlay is a no-op. Clone confirmed as desired.
    // Phase 15: speculative overlay cells overwrite confirmed cells here.
    // Phase 16: loss overlay row 0 cells overwrite confirmed cells here.
    let mut desired = self.confirmed.clone();
    let rows = desired.len();
    let cols = if rows > 0 { desired[0].len() } else { 0 };
    for overlay in &self.overlays {
        for row in 0..rows {
            for col in 0..cols {
                if let Some(cell) = overlay.cell_at(row as u16, col as u16) {
                    desired[row][col] = cell;
                }
            }
        }
    }
    desired
}
```

[ASSUMED: clone-based composition — correct for correctness; Phase 15 may optimize if the overlay is sparse]

---

## State of the Art

| Old Approach | Current Approach | When Changed | Impact |
|--------------|------------------|--------------|--------|
| PtyData → stdout (direct write) | PtyData → highest_applied only; datagram → ClientScreen → render_to_stdout | Phase 14 | Single display path; enables prediction overlay without display path duplication |
| No client terminal model | ClientScreen (confirmed + physical grids) | Phase 14 | Enables idempotent rendering; duplicate diffs produce no ANSI output |
| No epoch-ack from real client | encode_epoch_ack in datagram arm | Phase 14 | Server baseline advances; subsequent diffs are sparse rather than full-screen |

**Deprecated/outdated after Phase 14:**
- `stdout.write_all(&data)` in the PtyData arm of run_pump: removed. Must not be re-added in any future phase without re-establishing the overlay seam first.

---

## Assumptions Log

| # | Claim | Section | Risk if Wrong |
|---|-------|---------|---------------|
| A1 | Local `Cell` in screen.rs (not re-exported from nosh_server) | Standard Stack / Pattern 1 | If nosh-server becomes a non-dev dep, could reuse. Risk is LOW — nosh-server is and should remain a dev-dependency only |
| A2 | Overlay trait as `trait Overlay { fn cell_at(...) -> Option<Cell> }` | Pattern 8 | Phase 15 may prefer enum-based dispatch or a Vec<OverlideCell> approach; but any seam works as long as the compositor is the single composition point |
| A3 | `compose_desired` uses full clone of confirmed | compose_desired example | For a 80x24 grid this is ~1920 Cell clones (~15KB). Fine for 16ms ticks. Phase 15 may optimize if overlay is sparse |
| A4 | render_to_stdout writes to std::io::Write, flushes via tokio afterward | Pitfall 1 / Pattern 3 | Exact buffer strategy (BufWriter vs Vec<u8>) is Claude's discretion |
| A5 | Wide char handling deferred (single-width chars only) | Pattern 2 (apply) | CONTEXT.md defers wide chars to Phase 15 explicitly |
| A6 | `send_datagram` for epoch-ack is best-effort (ignore error in datagram arm) | Pattern 6 | If epoch-ack fails, server just falls back to full-screen diffs next tick. No session loss. |

---

## Open Questions

1. **Test helper: exposing compute_diff_runs / build_state_diff for grid-comparison test**
   - What we know: `compute_diff_runs` and `build_state_diff` are free functions in `nosh_server::server` (per 13-02-SUMMARY.md). The test can call them via dev-dep imports.
   - What's unclear: `build_state_diff` takes `slot: &SessionSlot` as its first arg — no `SessionSlot` is available in a unit test without spawning a full server.
   - Recommendation: The grid-comparison test should use a simpler approach — manually construct a `StateDiff` by calling `compute_diff_runs` directly (extract current cells vs. empty baseline), then `encode_datagram`, then `decode_datagram`, then apply to `ClientScreen`. Or: add a small test helper `fn diff_from_terminal_state(ts: &TerminalState, epoch: u64) -> StateDiff` that wraps `compute_diff_runs` in the test module. Either approach avoids needing `SessionSlot`.

2. **Screen dimensions at startup before first datagram**
   - What we know: The client calls `crossterm::terminal::size()` at startup (main.rs line 416) and sends `SessionOpen { cols, rows }`. The server creates its `TerminalState` at those dimensions.
   - What's unclear: Should `ClientScreen::new(cols, rows)` use those same startup dimensions, or should it start 0x0 and resize on first diff?
   - Recommendation: Initialize `ClientScreen::new(cols, rows)` with the same startup dimensions passed to `SessionOpen`. This avoids a spurious resize on the first diff if the server sends a diff at the same dimensions.

3. **Idempotent render on duplicate datagrams: physical grid update timing**
   - What we know: If a datagram is resent (loss recovery), apply() returns early (`diff.epoch <= last_applied_epoch`). render_to_stdout is not called. Physical grid is unchanged. This is correct and intentional.
   - What's unclear: What if the connection drops and reconnects, and the first post-resume diff has a very large epoch? The physical grid will not match physical terminal state (the terminal may have been partially repainted or the resize may have cleared it).
   - Recommendation: On reconnect/resume, reset the physical grid to all-default cells (force a full repaint on next render). This is symmetric with the server's `last_acked_snapshot = empty` reset on resume (D-13-01b). Add this to the `reattach_session` path: `screen.reset_physical()`.

---

## Environment Availability

| Dependency | Required By | Available | Version | Fallback |
|------------|------------|-----------|---------|----------|
| `crossterm` 0.29 | ANSI cursor positioning, raw mode | Already in nosh-client Cargo.toml | 0.29 | — |
| `nosh-proto` | StateDiff, encode_epoch_ack, decode_datagram | Workspace dependency | 0.1.0 | — |
| `nosh-server` (dev-dep) | Grid-comparison integration test | Already in [dev-dependencies] | 0.1.0 | — |
| Linux PTY + /bin/sh | Integration test (sync.rs pattern) | Linux only — confirmed available | — | Skip with have_sh() guard |

**Missing dependencies with no fallback:** None.

---

## Validation Architecture

> `nyquist_validation` is explicitly `false` in `.planning/config.json`. Section omitted.

---

## Security Domain

> Phase 14 is a client-side display/rendering phase. ASVS categories relevant:

| ASVS Category | Applies | Standard Control |
|---------------|---------|-----------------|
| V5 Input Validation | YES — StateDiff from network must be bounds-checked before grid apply | MAX_RUNS guard already in decode_datagram; row/col out-of-bounds guard in apply() |
| V2 Authentication | No — no new auth surface | — |
| V4 Access Control | No | — |
| V6 Cryptography | No | — |

### Known Threat Patterns for This Stack

| Pattern | STRIDE | Standard Mitigation |
|---------|--------|---------------------|
| Malformed StateDiff with out-of-bounds row/col | Tampering | `row >= self.confirmed.len()` and `col >= row_cells.len()` guards in apply(); decode_datagram MAX_RUNS guard already in nosh-proto |
| Large StateDiff forcing excessive allocation in ClientScreen | DoS | MAX_RUNS=4096 in decode_datagram prevents >4096 runs; confirmed grid is bounded by terminal dimensions (cols x rows) |
| Stale epoch replay (old datagram accepted) | Tampering | D-14-05 monotonic check: `if diff.epoch <= self.last_applied_epoch { return; }` |
| Epoch-ack spoofing (server accepting forged acks) | Tampering | Server-side: `acked > last_acked_epoch` guard (13-02-SUMMARY.md). Client cannot help here beyond sending correct acks |
| Physical terminal side-effects from malformed char | Tampering | Only chars from diff.chars are written; these are UTF-8 from the server's terminal model (TerminalState tracks Unicode scalars). No injection vector beyond what the remote shell can produce |

---

## Sources

### Primary (HIGH confidence)

- `crates/nosh-proto/src/datagram.rs` — StateDiff, DiffRun, CellStyle, CursorPos, decode_datagram, encode_epoch_ack (directly read, all APIs confirmed)
- `crates/nosh-server/src/terminal.rs` — Cell struct, TerminalState, SgrState, EchoState (directly read; defines the server-side grid semantics ClientScreen must mirror)
- `crates/nosh-client/src/main.rs` — run_pump full source, PtyData arm, Ack interval, resize handling, escape state machine (directly read)
- `crates/nosh-client/src/client.rs` — RawModeGuard, helper functions (directly read)
- `crates/nosh-client/tests/sync.rs` — datagram read loop pattern, encode_epoch_ack emission pattern (directly read)
- `.planning/phases/13-server-datagram-sender/13-02-SUMMARY.md` — server datagram sender design, ResumeComplete gate, build_state_diff, compute_diff_runs patterns (directly read)
- `.planning/phases/13-server-datagram-sender/13-03-SUMMARY.md` — integration test patterns for datagram arm (directly read)
- `.planning/phases/13-server-datagram-sender/13-PATTERNS.md` — Pattern Map with exact code templates for select! arm, epoch-ack, datagram arm (directly read)
- `crates/nosh-client/Cargo.toml` — confirmed: crossterm 0.29, nosh-proto, nosh-server (dev-dep), quinn, bytes, tokio all present
- `.planning/phases/14-client-predictor-confirmed-rendering/14-CONTEXT.md` — locked decisions D-14-01 through D-14-05 (directly read)

### Secondary (MEDIUM confidence)

- Mosh `Display` model (framebuffer-diff compositor concept) — `[ASSUMED]` from training knowledge; the concept is well-documented in the Mosh paper (Keith Winstein and Hari Balakrishnan, 2012) and corroborated by the CONTEXT.md description of the approach.

### Tertiary (LOW confidence)

- SGR emission approach (hand-rolled vs crossterm) — `[ASSUMED]` judgment call based on crossterm API review.

---

## Metadata

**Confidence breakdown:**
- Standard stack: HIGH — confirmed via direct Cargo.toml read; no new packages
- Architecture patterns: HIGH — confirmed from codebase reads (datagram.rs, terminal.rs, main.rs, sync.rs)
- Pitfalls: MEDIUM — Pitfalls 1-4 and 7 are VERIFIED from codebase patterns; Pitfall 5-6 are ASSUMED from design analysis
- Diff algorithm: MEDIUM — Mosh Display model is assumed from training knowledge; the specific implementation is Claude's discretion

**Research date:** 2026-06-02
**Valid until:** Stable (this phase's tech stack is pinned to the existing workspace; no ecosystem churn risk)
