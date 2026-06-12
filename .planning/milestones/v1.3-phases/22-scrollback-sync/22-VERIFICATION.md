---
phase: 22-scrollback-sync
verified: 2026-06-12T00:00:00Z
status: passed
score: 5/5 roadmap success criteria verified
overrides_applied: 0
re_verification:
  previous_status: gaps_found
  previous_score: 4/5
  gaps_closed:
    - "Pressing Shift-PageUp displays terminal history that has scrolled off the visible grid — the client renders the fetched lines above the current viewport (ROADMAP SC #1 / SCROLL-01)"
  gaps_remaining: []
  regressions: []
gaps: []
deferred: []
---

# Phase 22: Scrollback Sync Verification Report

**Phase Goal:** Users can view shell history that has scrolled off the visible grid, served from the server's existing scrollback buffer over a dedicated reliable channel, paged on demand, gated to exclude alt-screen content, and surviving both QUIC migration and cold reattach.
**Verified:** 2026-06-12
**Status:** passed
**Re-verification:** Yes — after gap closure (plan 22-05-gap, commit 5d999d3)

## Re-Verification Summary

The single blocking gap from the prior pass — the client buffered scrollback pages but
never RENDERED them (`TODO: render scrollback view` at former main.rs:1802, SCROLL-01
display half unmet) — is **genuinely closed**. The render path is real, substantive,
wired at all required call sites, bounds-safe, and test-backed. No regressions in the six
previously-verified items. Phase goal "users can view shell history that has scrolled off
the visible grid" is now achieved.

## Gap Closure — Adversarial Findings (the render path)

1. **TODO removed; page_rx arm renders.** No `TODO`/`FIXME`/`XXX` remains in main.rs. The
   `page_rx` Active arm (main.rs:2066-2117) prepends the new page, updates
   `epoch_at_snapshot`/`total_available`/`pending_request`, then calls
   `render_scrollback_to_buf(&mut buf, lines, *offset, cols, rows)` (2102), `reset_physical()`
   (2103), and async-writes to stdout (2105-2109). VERIFIED.

2. **`render_scrollback_to_buf` is substantive, not a stub** (main.rs:216-313). Emits
   `\x1b[2J\x1b[H`, computes `bottom = total.saturating_sub(offset)` and
   `top = bottom.saturating_sub(rows)`, then per row emits `MoveTo`, SGR (bold/italic/
   underline/reverse + `38;5;N`/`48;5;N` colour), the actual `cell.ch` glyph, and blank-fills
   the remainder. Field names (`ch`/`style`/`fg`/`bg`/`cells`) match `ScrollbackCell`/
   `ScrollbackLine` in nosh-proto/messages.rs. **Bounds-safe:** `saturating_sub` throughout and
   `if line_idx < total` guards array access; `large_offset_produces_blank_rows_without_panic`
   (offset=1000 over 1 line) passes. VERIFIED.

3. **Rendered on entry AND on offset change, not only on page receipt.** Active entry renders
   an initial blank frame (2198) immediately on Shift-PageUp-from-Live; Shift-PageUp offset
   increment re-renders (2242); Shift-PageDown offset decrement re-renders (2293). So paging
   shows content immediately rather than waiting for a page to arrive. VERIFIED.

4. **Snap-back restores the live grid AND forwards the keystroke (no SCROLL-04 regression).**
   Non-paging keystroke path captures `was_active`, sets `ScrollbackView::Live`, does a full
   live repaint via `render_to_stdout` against the blank physical model (2326-2338), THEN
   processes the keystroke through the escape machine and forwards it via `send_input` (2342,
   2379). Shift-PageDown past the live boundary likewise full-repaints (2271). The integration
   test `scrollback_keybinding_snap_back` passes. VERIFIED.

5. **Epoch-gate exit (SCROLL-05) restores the live grid.** Datagram arm: when
   `diff.epoch >= epoch_at_snapshot`, sets Live (1869) and falls through to `screen.apply(&diff)`
   + `render_with_predictor` (1887/1951). Because every scrollback render calls
   `reset_physical()`, the physical model is blank and this produces a full repaint. VERIFIED.

6. **Predictor NOT used for scrollback content.** `render_scrollback_to_buf` takes no predictor
   and references none (grep over its body: zero `predict` hits). It bypasses the
   ClientScreen compositor/predictor/overlay entirely, as specified. VERIFIED.

7. **Tests.** `scrollback_render_tests` (7 tests) all pass in isolation and run (not filtered/
   ignored): `empty_lines_emits_clear_screen`, `single_line_content_appears_in_output`,
   `offset_zero_shows_most_recent_lines`, `offset_rows_shifts_viewport_up`,
   `large_offset_produces_blank_rows_without_panic`, `bold_cell_emits_bold_sgr`,
   `fg_color_cell_emits_256_color_sgr`. VERIFIED.

## Goal Achievement

### Observable Truths

| # | Truth (ROADMAP Success Criterion) | Status | Evidence |
|---|-----------------------------------|--------|----------|
| 1 | Shift-PageUp displays history; client renders lines above the viewport (SCROLL-01) | ✓ VERIFIED | `render_scrollback_to_buf` (main.rs:216) paints held cells with SGR + glyphs; wired at entry (2198), page_rx (2102), PgUp (2242), PgDn (2293). TODO removed. 7 unit tests + `scrollback_basic_fetch`/`scrollback_inorder_under_loss` integration tests pass. |
| 2 | Reliable stream only, never datagrams; separate task; bounded mpsc; ScrollbackCredit paces; PTY unaffected (SCROLL-02) | ✓ VERIFIED (re-confirmed) | Type-level reliable-only sender (channel.rs); bounded `mpsc::channel(64)` + drop-on-Full; `scrollback_pty_latency_isolation`/`scrollback_backpressure_drop_oldest` pass. |
| 3 | Alt-screen content never enters scrollback; scroll_up gated on !alt_screen; unit test (SCROLL-03) | ✓ VERIFIED (re-confirmed) | `scroll_up()` + resize-shrink gated `if !alt_screen` (terminal.rs); both alt-screen tests pass. |
| 4 | Any key while in scrollback snaps to live AND is forwarded; Shift-PageDown pages; reaching live auto-exits (SCROLL-04) | ✓ VERIFIED (re-confirmed + render) | Snap-back sets Live, full-repaints (2326), then forwards via `send_input` (2379); PgDn boundary auto-exit + repaint (2271); `scrollback_keybinding_snap_back` passes. |
| 5 | No duplicate/missing lines at handoff; epoch_at_snapshot on wire; survives cold reattach (SCROLL-05) | ✓ VERIFIED (re-confirmed) | Atomic epoch read; client seeds from `last_applied_epoch()` (2173); epoch-gated exit then full repaint (1868/1951); `scrollback_epoch_handoff_no_gap`/`scrollback_post_reattach` pass. |

**Score:** 5/5 truths verified

### Required Artifacts

| Artifact | Expected | Status | Details |
|----------|----------|--------|---------|
| `crates/nosh-proto/src/messages.rs` | Scrollback variants + ScrollbackLine/Cell | ✓ VERIFIED | Fields `ch`/`style`/`fg`/`bg`/`cells` present; round-trip tested. |
| `crates/nosh-proto/src/codec.rs` | Discriminant stability 15/16/17 | ✓ VERIFIED | Stability tests pass. |
| `crates/nosh-server/src/terminal.rs` | scrollback_lines accessor + alt-screen tests | ✓ VERIFIED | Accessor + both gates; tests pass. |
| `crates/nosh-server/src/channel.rs` | run_scrollback_sender_task reliable-only | ✓ VERIFIED | SendStream-only; atomic epoch read. |
| `crates/nosh-server/src/server.rs` | Accept arm + spawn in both session paths | ✓ VERIFIED | Spawned in run_session + run_reattach_session. |
| `crates/nosh-client/src/channel.rs` | drain task decode + credit | ✓ VERIFIED | Decodes pages; advertises credit. |
| `crates/nosh-client/src/main.rs` | ScrollbackView SM + CSI + reattach + **render** | ✓ VERIFIED | render_scrollback_to_buf (216) wired at 5 sites; snap-back + epoch-gate full repaint; TODO removed. |
| `crates/nosh-client/tests/channel_mux.rs` | integration suite | ✓ VERIFIED | 7 scrollback integration tests pass. |

### Key Link Verification

| From | To | Status | Details |
|------|----|--------|---------|
| server.rs spawn | run_scrollback_sender_task | ✓ WIRED | Both session paths. |
| run_scrollback_sender_task | TerminalState::scrollback_lines | ✓ WIRED | Inside with_terminal_state closure. |
| client main.rs | open_channel(Scrollback) | ✓ WIRED | Re-opens on reattach inside run_pump. |
| client channel.rs drain | page_tx → page_rx → view buffer | ✓ WIRED | Buffer updated AND rendered (was the gap). |
| page_rx / PgUp / PgDn / entry | render_scrollback_to_buf → stdout | ✓ WIRED | 5 call sites; each writes + flushes; reset_physical for live repaint. |

### Behavioral / Probe Results

`cargo test --workspace`: all pass after accounting for one timing flake. `channel_echo_roundtrip`
(a PTY-latency assertion: median <5ms under channel saturation) failed once under full-parallel
contention (median 6.98ms) but passed 3/3 in isolation and again in a re-run of the full
`channel_mux` suite (13/13) — it is a load-sensitive timing flake, unrelated to the scrollback
render path (which carries no latency assertion and bypasses the session pump). `scrollback_render_tests`
(7) pass in isolation. All scrollback view/epoch/integration tests pass.

### Requirements Coverage

| Requirement | Source Plan(s) | Status | Evidence |
|-------------|----------------|--------|----------|
| SCROLL-01 | 22-01/02/03/04/05-gap | ✓ SATISFIED | Server serves + client fetches/buffers + **renders** held lines to viewport. |
| SCROLL-02 | 22-01/02/04 | ✓ SATISFIED | Reliable-only, bounded mpsc, credit pacing, PTY isolation. |
| SCROLL-03 | 22-01 | ✓ SATISFIED | scroll_up + resize gated on !alt_screen. |
| SCROLL-04 | 22-03/04/05-gap | ✓ SATISFIED | Snap-back + forward + page + auto-exit + repaint. |
| SCROLL-05 | 22-01/02/03/04 | ✓ SATISFIED | Atomic epoch, wire field + seed, cold-reattach re-open, epoch-gate repaint. |

### Anti-Patterns Found

None. The previously-flagged unreferenced `TODO` at former main.rs:1802 has been removed.

### Gaps Summary

No gaps. The render path closes SCROLL-01's display half: pressing Shift-PageUp now clears the
screen and paints the held scrollback buffer (blank initially, populated as pages arrive); each
PgUp/PgDn re-renders the new offset; any keystroke or paging past the live boundary snaps back to
a full live repaint while still forwarding the keystroke to the shell. The predictor is correctly
bypassed for historical content. All five ROADMAP success criteria are satisfied and the phase
goal is achieved.

---

_Verified: 2026-06-12 (re-verification after gap closure)_
_Verifier: Claude (gsd-verifier)_
