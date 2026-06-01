---
plan: 12-01
phase: 12-server-terminal-state-model
status: complete
completed: 2026-06-01
commit: 07edc56
---

## What Was Built

Added `crates/nosh-server/src/terminal.rs` containing `TerminalState` — a full `vte::Perform`
implementation that consumes raw PTY bytes and maintains an authoritative terminal grid, cursor,
bounded scrollback, and four observable DEC private-mode echo flags.

### Key artifacts

- `crates/nosh-server/Cargo.toml` — added `vte = "0.15"` dependency
- `crates/nosh-server/src/lib.rs` — added `pub mod terminal;`
- `crates/nosh-server/src/terminal.rs` — 1,236 lines (implementation + tests):
  - `Cell` struct: `ch: char`, `style: CellStyle`, `fg: Option<u8>`, `bg: Option<u8>`
    (types match `DiffRun` exactly for zero-conversion Phase 13 extraction)
  - `EchoState` struct: four `bool` fields for DECTCEM (`?25`), alt-screen (`?1049`),
    bracketed-paste (`?2004`), app-cursor-keys (`?1`)
  - `TerminalState`: viewport grid `Vec<Vec<Cell>>`, scrollback `VecDeque<Vec<Cell>>`
    bounded by `SCROLLBACK_LINE_CAP = 10_000`, cursor `CursorPos`, echo state, title,
    osc52_pending, owned `vte::Parser`
  - `advance(&mut self, bytes: &[u8])` using `std::mem::take` borrow-split pattern
  - Full `vte::Perform` impl: `print`, `execute`, `csi_dispatch` (DEC private modes +
    cursor motion + erase + SGR), `osc_dispatch` (OSC 0/2 title, OSC 52 detection),
    `esc_dispatch` (ESC c RIS reset), hook/put/unhook as default no-ops
  - Public read API: `cursor()`, `cell()`, `echo_state()`, `title()`, `osc52_pending()`,
    `size()`, `viewport_rows()`, `resize()`

### Acceptance criteria

- ✓ `cargo build -p nosh-server` exits 0
- ✓ `vte = "0.15"` in Cargo.toml
- ✓ `pub mod terminal;` in lib.rs
- ✓ `impl vte::Perform for TerminalState` present
- ✓ `nosh_proto::datagram` imports: `CellStyle`, `CursorPos`
- ✓ `Cell.fg`/`bg` are `Option<u8>`, Default sets them to `None`
- ✓ `SCROLLBACK_LINE_CAP` const present (= 10_000)
- ✓ `std::mem::take` borrow-split in `advance`

### Test results

`cargo test -p nosh-server --lib terminal`: **39 passed, 0 failed**

### Isolation check

`grep -nE 'use (quinn|tokio)|crate::(session|registry|server)' terminal.rs` — returns only
comments (lines 32–33), no actual imports. Isolation constraint satisfied.

## Self-Check: PASSED

### Deviations

None. All plan requirements implemented as specified.

## key-files.created

- crates/nosh-server/src/terminal.rs
- crates/nosh-server/Cargo.toml (modified)
- crates/nosh-server/src/lib.rs (modified)
