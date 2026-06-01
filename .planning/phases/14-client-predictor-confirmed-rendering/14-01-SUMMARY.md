---
phase: 14-client-predictor-confirmed-rendering
plan: "01"
subsystem: nosh-client
tags: [screen, compositor, framebuffer-diff, ANSI, overlay, security]
dependency_graph:
  requires: []
  provides:
    - nosh_client::screen::ClientScreen
    - nosh_client::screen::Cell
    - nosh_client::screen::Overlay
    - nosh_client::screen::ConnectionLossOverlay
  affects:
    - crates/nosh-client/src/lib.rs
tech_stack:
  added: []
  patterns:
    - "Mosh Display model: confirmed + physical dual-grid, minimal ANSI diff"
    - "OnceLock static default cell (IN-02 pattern for OOB access)"
    - "D-14-05 monotonic epoch guard (T-14-03 replay mitigation)"
    - "Single composition seam via Overlay trait (D-14-01a extension point)"
key_files:
  created:
    - crates/nosh-client/src/screen.rs
  modified:
    - crates/nosh-client/src/lib.rs
decisions:
  - "Both tasks (model/apply and render/SGR) implemented in a single screen.rs module — cohesive seam for Phase 15/16 extension without premature splitting"
  - "Clippy clean: compose_desired refactored to enumerate() zip; apply inner loop uses (start..).zip() pattern; doc comment fixed"
  - "nosh_server references are doc-comment only in screen.rs — no production import of the dev-dep"
metrics:
  duration: "~30 minutes"
  completed: "2026-06-02"
  tasks_completed: 2
  files_changed: 2
---

# Phase 14 Plan 01: ClientScreen Compositor — Confirmed Rendering Summary

**One-liner:** `ClientScreen` Mosh Display model with confirmed/physical dual-grid, monotonic epoch guard, compose-desired overlay seam, and minimal-ANSI diff emitter — all in `crates/nosh-client/src/screen.rs`.

## What Was Built

`crates/nosh-client/src/screen.rs` (716 lines, new module) implementing the complete `ClientScreen` compositor:

**Task 1 — Model, apply, resize, read API:**
- Local `Cell` struct (field-for-field mirror of `nosh_server::terminal::Cell`, D-14-04; no production import of nosh-server)
- `Overlay` trait + `ConnectionLossOverlay` no-op stub (D-14-01a Phase 14 seam)
- `ClientScreen` struct with `confirmed`/`physical` dual-grid, `last_applied_epoch`, `overlays`
- `apply(&StateDiff)`: D-14-05 monotonic guard; resize on dimension change; OOB row/col guards (T-14-01/T-14-03)
- `resize(cols, rows)`: confirmed truncated/extended; physical reset to blank (Pitfall 2)
- `confirmed_cell(row, col)`: `OnceLock`-backed static default (IN-02 pattern; never panics)
- `last_applied_epoch()` + `size()` read API

**Task 2 — Framebuffer diff emitter:**
- `compose_desired()`: single composition loop over overlays (D-14-01a extension seam)
- `render_to_stdout<W: Write>()`: minimal ANSI diff via crossterm `MoveTo` + hand-rolled SGR; `std::io::Write` only (Pitfall 1); sets `physical = desired` after emit
- `emit_sgr()`: SGR 0 reset + BOLD/ITALIC/UNDERLINE/REVERSE bits + 256-color `38;5;N`/`48;5;N`
- `reset_physical()`: force full repaint on next render (reattach path, Plan 02)

`crates/nosh-client/src/lib.rs`: added `pub mod screen;`.

## Tests (14 unit tests, all green)

| Test | Behavior Covered |
|------|-----------------|
| `apply_fresh_writes_chars_to_confirmed_grid` | Task 1 basic apply |
| `apply_monotonic_same_epoch_is_noop` | D-14-05 / T-14-03 same-epoch guard |
| `apply_monotonic_lower_epoch_is_noop` | D-14-05 / T-14-03 lower-epoch guard |
| `apply_resize_changes_dims_and_resets_physical` | Dual-grid resize, physical reset |
| `apply_oob_row_is_skipped_no_panic` | T-14-01 SECURITY V5 row guard |
| `apply_oob_col_chars_clamped_no_panic` | T-14-01 SECURITY V5 col guard |
| `confirmed_cell_oob_returns_default_no_panic` | OnceLock default, no panic |
| `connection_loss_overlay_is_noop` | D-14-01a no-op stub |
| `render_after_apply_emits_nonempty_ansi_with_chars` | Task 2 render produces ANSI |
| `duplicate_datagram_produces_minimal_ansi` | Idempotency / Pitfall 5 |
| `emit_sgr_bold_fg_produces_correct_sequence` | SGR BOLD + fg=Some(1) → `\x1b[0;1;38;5;1m` |
| `emit_sgr_all_attributes` | All four SGR bits + fg/bg |
| `emit_sgr_none_attrs_produces_reset_only` | SGR 0 reset only |
| `reset_physical_forces_full_repaint` | reset_physical → next render re-emits all cells |

## Commits

| Task | Commit | Files |
|------|--------|-------|
| Tasks 1 + 2 (complete screen module) | `10dd4d3` | `crates/nosh-client/src/screen.rs` (new, 716 lines), `crates/nosh-client/src/lib.rs` |

## Deviations from Plan

None — plan executed exactly as written.

The two tasks share a single file and were naturally implemented together (model and renderer are tightly coupled). No architectural deviation required.

## Known Stubs

`ConnectionLossOverlay` returns `None` for all cells (intentional Phase 14 no-op). Phase 16 will activate this overlay to display a connection-loss banner after 5 s of datagram silence. The stub is tracked in the plan and is the designed extension point — not an accidental omission.

## Threat Flags

No new security surface introduced beyond what the plan's `<threat_model>` covers:
- T-14-01 (OOB run guards): implemented and unit-tested
- T-14-02 (oversized run count): decode_datagram already caps at MAX_RUNS=4096; apply allocates nothing per-run beyond the bounded grid
- T-14-03 (replay/stale epoch): `diff.epoch <= self.last_applied_epoch` guard implemented and unit-tested
- T-14-04 (terminal injection): only `Cell.ch` (single Unicode scalar) reaches stdout via render; no raw server byte stream

## Self-Check: PASSED

- `crates/nosh-client/src/screen.rs` exists: FOUND
- `crates/nosh-client/src/lib.rs` has `pub mod screen`: FOUND
- Commit `10dd4d3` exists in git log: FOUND
- `cargo build -p nosh-client`: exits 0 (clean)
- `cargo clippy -p nosh-client --lib`: exits 0 (0 warnings)
- `cargo test -p nosh-client --lib screen::`: 14/14 tests pass
- No `nosh_server` production import in `screen.rs`: CONFIRMED (doc-comment references only)
- `tokio::io` not referenced in `screen.rs`: CONFIRMED
- `diff.epoch <= self.last_applied_epoch` guard present: CONFIRMED
- `pub fn render_to_stdout` defined once: CONFIRMED
- `pub fn reset_physical` defined: CONFIRMED
- `pub struct ClientScreen` defined: CONFIRMED
- Line count 716 >= min_lines 150: CONFIRMED
