---
plan: 12-02
phase: 12-server-terminal-state-model
status: complete
completed: 2026-06-01
commit: 5d91cbd
---

## What Was Built

Wired `TerminalState` (from Plan 12-01) into the live session substrate.

### Key changes

**crates/nosh-server/src/registry.rs:**
- Added `use crate::terminal::TerminalState;` import
- Added `terminal_state: Mutex<TerminalState>` field to `SessionSlot` struct (lock-order
  documented: always acquire `output_buf` then `terminal_state`; never across `.await`)
- `SessionSlot::new` initializes `terminal_state` to `TerminalState::new(80, 24)` (conventional
  default; resize path corrects dimensions when client reports actual size)
- Added `push_output_and_parse(&self, chunk: &[u8]) -> u64`: acquires `output_buf` lock and
  calls `push(chunk)` FIRST (assigns seq, replay path — never fails), then acquires
  `terminal_state` lock and calls `advance(chunk)`, returns `seq` (Pitfall 8 / SYNC-02)
- Extended `resize()` to call `terminal_state.lock().unwrap().resize(cols, rows)` after
  `Session::resize` — no reflow per D-12-03, returns Session's Result
- Regression tests:
  - `push_output_and_parse_seq_replay_trim_byte_identical_to_push_output`: pure-SequencedOutputBuffer
    level, no real PTY needed; proves seq, replay_from, trim_acked are byte-identical
  - `slot_push_output_and_parse_feeds_both_buffers`: real SessionSlot (/bin/sh guarded);
    confirms seq numbers, replay chunks, and terminal_state cell content are all correct

**crates/nosh-server/src/server.rs:**
- Converted 3 PTY-output callsites from `push_output` to `push_output_and_parse`:
  - Line ~416 (interactive pump, `out_rx.recv`)
  - Line ~506 (ShellExited drain loop)
  - Line ~795 (reattach pump, `out_rx.recv`)

### Acceptance criteria

- ✓ `cargo build -p nosh-server` exits 0
- ✓ `registry.rs SessionSlot` has `terminal_state: Mutex<TerminalState>` field (struct + new
  + push_output_and_parse + resize all reference it)
- ✓ `push_output_and_parse` exists; `push` precedes `advance` (lock order correct)
- ✓ Original `push_output` method body unchanged (single `output_buf.lock().unwrap().push(chunk)`)
- ✓ `resize` touches `terminal_state`
- ✓ `grep -c 'push_output_and_parse' server.rs` == 3
- ✓ SequencedOutputBuffer regression test: seq/replay_from/trim_acked byte-identical
- ✓ Slot-level test: terminal_state advances after push_output_and_parse call

### Test results

`cargo test -p nosh-server --lib`: **65 passed, 0 failed**

## Self-Check: PASSED

### Deviations

None. All plan requirements implemented as specified.

## key-files.created

- crates/nosh-server/src/registry.rs (modified: terminal_state field + push_output_and_parse + resize hook + regression tests)
- crates/nosh-server/src/server.rs (modified: 3 callsites converted)
