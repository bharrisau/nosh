---
phase: 19-full-screen-tui-rendering-correctness
plan: "05"
subsystem: nosh-server/terminal
tags: [tui, vt, cursor-addressing, alt-screen, wide-char, security, osc, docs]
dependency_graph:
  requires:
    - phase: 19-01
      provides: two-grid alt-screen model (TUI-01)
    - phase: 19-02
      provides: wide-char width tracking and predictor suppression (TUI-03/05)
    - phase: 19-03
      provides: OSC_ACCUMULATION_MAX pre-bound and parser resync (SEC-03)
    - phase: 19-04
      provides: resize both grids + scroll_up gate (TUI-02, D-19-09)
  provides:
    - TUI-04 synthetic CI regression suite (cursor-addressing grid-assertion tests)
    - docs/999.7-SECURITY.md (OSC OOM mitigation documented before phase close)
  affects: [20-repaint-pacing, 21-channel-multiplexing, 22-scrollback-sync]
tech_stack:
  added: []
  patterns:
    - "Grid-assertion integration tests: CUP-addressed paint on alt-screen then verify primary restored exactly"
    - "Deterministic VT regression net: byte sequences directly encode control codes (no live apps, no timing)"
key_files:
  created:
    - docs/999.7-SECURITY.md
  modified:
    - crates/nosh-server/src/terminal.rs
key-decisions:
  - "D-19-07 automated half of TUI-04: CI gets synthetic grid-assertion tests (cursor addressing, alt-screen atomicity, wide char via CUP, ED/EL region clearing); human visual pass covers live apps"
  - "999.7-SECURITY.md created before phase close as required by ROADMAP phase-exit criterion"
requirements-completed: [TUI-04, SEC-03]
duration: 25min
completed: 2026-06-07
---

# Phase 19 Plan 05: TUI-04 Acceptance Net + 999.7 Security Doc Summary

**Cursor-addressing grid-assertion CI suite (4 tests, 70 terminal:: passing) and docs/999.7-SECURITY.md documenting the OSC OOM mitigation, with human visual pass checkpoint pending operator confirmation.**

## Performance

- **Duration:** ~25 min (autonomous tasks); human checkpoint pending
- **Started:** 2026-06-07
- **Completed (autonomous):** 2026-06-07
- **Tasks completed (autonomous):** 2 of 3 (Task 3 is a human-verify checkpoint)
- **Files modified:** 2

## Accomplishments

- Added 4 deterministic cursor-addressing grid-assertion tests to `terminal.rs`, bringing the terminal test count to 70 (103 total nosh-server lib tests). Tests exercise: CUP-addressed write landing at exact cells, alt-screen enter/CUP-paint/exit atomicity (vim workflow simulation), wide char via CUP with continuation cell, and ED/EL region boundary precision after CUP.
- Created `docs/999.7-SECURITY.md` documenting the SEC-03 OSC OOM mitigation from plan 03: threat (vte osc_raw unbounded, post-auth), mitigation (1 MiB pre-bound + parser resync), residual risks, regression test reference, and a "verify before relying on this" note about vte upgrades.
- Human checkpoint (Task 3) returned as structured state for operator completion.

## Task Commits

1. **Task 1: Synthetic VT grid-assertion regression suite** — `10131e2` (test)
2. **Task 2: Create docs/999.7-SECURITY.md** — `220a749` (docs)
3. **Task 3: Manual visual pass** — PENDING (human-verify checkpoint)

## Files Created/Modified

- `crates/nosh-server/src/terminal.rs` — Added 4 grid-assertion tests in the `// ── TUI-04 grid-assertion regression suite (19-05 D-19-07) ─────────────────` section
- `docs/999.7-SECURITY.md` — Created: threat, mitigation, residual risk, regression test reference, verify note

## Decisions Made

- Grid-assertion tests complement (do not duplicate) the unit-level per-feature tests from plans 01–03. The integration-level tests (especially `alt_screen_cup_fullscreen_paint_exit_restores_primary_exactly`) exercise the combined CUP + alt-screen path representative of a real vim session.
- The security doc uses section-numbered format consistent with `docs/999.1-SECURITY.md`, Australian English spelling, and includes an explicit "verify before relying on this" note scoped to vte upgrade behaviour.

## Deviations from Plan

None — plan executed exactly as written for the two autonomous tasks.

## Human Checkpoint Status

Task 3 (`checkpoint:human-verify`) was returned as a structured checkpoint (not auto-completed). The checkpoint requires an operator to:
1. Build release and start a nosh server + client.
2. Run `vim --noplugin`, `htop`, and `claude` (or another full-screen TUI) over the nosh session.
3. Verify correct rendering against a reference terminal (no garbling, no missing spaces, primary buffer restored after vim exit, no predictor overlay in alt-screen, CJK/emoji no column drift).
4. Record the result as "approved" or list specific rendering defects.

## Known Stubs

None. The regression tests are fully wired and deterministic. The security doc is complete. The human visual pass is intentionally a checkpoint — it cannot be automated (D-19-07).

## Threat Flags

None introduced. The changes are test additions and a documentation file — no new network endpoints, auth paths, file access patterns, or schema changes.

## Self-Check: PASSED

- `crates/nosh-server/src/terminal.rs` — modified (4 new tests added; `cargo test -p nosh-server --lib` passes: 70 terminal:: tests, 103 total)
- `docs/999.7-SECURITY.md` — created (confirmed: `test -f docs/999.7-SECURITY.md && grep -qi 'OSC_ACCUMULATION_MAX\|1 MiB\|osc' docs/999.7-SECURITY.md && echo OK` → OK)
- Commit `10131e2` — `git log --oneline` confirms (test(19-05): add TUI-04 cursor-addressing grid-assertion regression suite)
- Commit `220a749` — `git log --oneline` confirms (docs(19-05): create 999.7-SECURITY.md for OSC accumulation OOM mitigation)
