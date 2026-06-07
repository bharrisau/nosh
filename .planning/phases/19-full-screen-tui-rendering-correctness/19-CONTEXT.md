# Phase 19: Full-Screen TUI Rendering Correctness - Context

**Gathered:** 2026-06-07
**Status:** Ready for planning

<domain>
## Phase Boundary

Make the server-side `TerminalState` model (`crates/nosh-server/src/terminal.rs`) faithful enough that full-screen TUI apps (vim, htop, Claude Code) render correctly over nosh. Four concerns, all server-side, no protocol or client dependencies:

1. **Genuine two-grid alternate screen** — replace the current no-op `alt_screen` bool + single `grid` with a real save/swap/clear-on-enter, restore/swap-on-exit model for `?1049h`/`?1049l`, atomic.
2. **Width-accurate character handling** — `print_char` currently always advances the cursor by 1; add wcwidth-per-codepoint so CJK width-2 chars advance 2 (with a spacer at col+1) and zero-width combining/ZWJ marks advance 0.
3. **Predictor suppression in alt-screen** — the speculative local-echo predictor must produce no overlay while the alternate screen is active.
4. **OSC OOM bound (SEC-03)** — bound OSC accumulation before it reaches vte's unbounded internal `osc_raw` buffer, closing the post-auth OOM vector.

Scope anchor is fixed by ROADMAP Phase 19 and REQUIREMENTS TUI-01..TUI-05 + SEC-03. Discussion below clarifies HOW within this boundary.

</domain>

<decisions>
## Implementation Decisions

### OSC accumulation bound (SEC-03)
- **D-19-01:** Bound total OSC accumulation at **1 MiB** per OSC sequence, intercepted **before** vte's internal `osc_raw` Vec. This is the memory ceiling; it is distinct from and larger than the existing `OSC_52_MAX_BYTES` (64 KiB) and `MAX_TITLE_BYTES` (1 KiB) *storage* caps, which bound what is stored, not what vte allocates while parsing.
- **D-19-02:** On overflow (OSC exceeds 1 MiB), **truncate the oversized OSC, discard it, and resync the VT parser to ground state** so subsequent PTY output still renders correctly. Do NOT drop-and-keep-parsing (risks the parser staying "inside" a malformed OSC) and do NOT kill the session (a single buggy app emitting a large title must not nuke the user's live shell — this is trusted post-auth output).
- **D-19-03:** Legitimate OSC 52 clipboard and OSC 0/2 title sequences must continue to pass unchanged. The SEC-03 regression test is RED-before / GREEN-after: a multi-chunk ~10 MB OSC payload across many `advance()` calls must not exhaust memory, while OSC 52 / title behaviour still works.

### Wide-character width policy (TUI-03)
- **D-19-04:** Use `unicode-width`'s **default** `width()` — East Asian Ambiguous characters measure as **width 1**. Matches xterm/most modern terminals and the client's existing `unicode-width` 0.2 dependency, so server and client agree by default. Do NOT use `width_cjk()` and do NOT add an ambiguous-width config knob this phase (config negotiation would be scope creep over the wcwidth baseline).
- **D-19-05:** CJK wide characters (width 2) advance the cursor by two columns. Zero-width combining marks and ZWJ sequences (width 0) do not advance the cursor. Mode 2027 grapheme clustering stays **deferred to v1.4+** — wcwidth-per-codepoint is the v1.3 baseline.
- **D-19-06:** The spacer cell at `col+1` after a width-2 char is an **explicit wide-char continuation marker** (a distinct Cell representation — e.g. a sentinel `ch` or a Cell flag) that the client renders as nothing, NOT a literal blank space. Rationale: unambiguous cursor math, copy, and diffing; the client knows col+1 belongs to the wide glyph at col. May require a field/sentinel on the `Cell` struct and a matching client-render path — keep the server/client representation consistent on the wire.

### TUI-04 verification bar
- **D-19-07:** Acceptance bar = **synthetic VT grid-assertion tests in CI + a documented manual visual pass**. Automated tests assert grid state over crafted deterministic VT byte sequences (alt-screen enter/exit, wide chars, cursor addressing) as the regression net. A documented manual visual comparison against a reference terminal covers the three named live apps (vim, htop, Claude Code). Do NOT attempt automated golden-master capture against live apps (flaky across app/OS versions).
- **D-19-08:** Investigation-first is mandatory (per ROADMAP criterion 4): reproduce garbling/missing-spaces against a Linux client↔server **before** fixing. The manual visual pass will surface as a `human_needed` verification item at execute time.

### Alt-screen ↔ scrollback interaction (gates Phase 22)
- **D-19-09:** While the alternate screen is active, there is **no scrollback** — `scroll_up()` must check `!alt_screen` and skip the scrollback push when alt-screen is active. Alt-screen content never enters history. The primary buffer's scrollback is preserved untouched while alt-screen is active and restored intact on `?1049l`. Matches xterm/tmux. The alt grid does NOT get its own scrollback. This is the exact gate Phase 22 (scrollback sync) depends on.

### Predictor suppression (TUI-05) — Claude's Discretion
- The mechanism for suppressing the speculative predictor while alt-screen is active is at planning discretion, constrained by success criterion 5: no overlay inside vim/htop, and `predictor.pending` empty after `?1049h` is processed. The planner should determine how the client learns alt-screen state (the server's `EchoState.alt_screen` is the source of truth) and wire suppression + a flush of pending predictions on entry.

### Claude's Discretion
- Exact two-grid data structure (separate saved primary grid vs swap pointers), atomicity implementation of save+swap+clear / restore+swap.
- Which width crate API surface on the server side (add `unicode-width` to `nosh-server`); exact continuation-marker encoding on `Cell`.
- OSC pre-accumulation buffering mechanism (custom byte pre-scan / wrapper around `advance`).
- Resize handling so both active alt grid and saved primary grid resize (TUI-02) with no inactive-buffer loss.

</decisions>

<specifics>
## Specific Ideas

- Behaviour should match xterm/tmux conventions for alt-screen and scrollback (no alt-screen history; primary frozen and restored intact).
- Server and client must agree on the wide-char continuation-cell representation so the diff/render path stays consistent.

</specifics>

<canonical_refs>
## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### Phase scope & requirements
- `.planning/ROADMAP.md` — Phase 19 section: goal, 6 success criteria, security note (Pitfalls A-1..A-6, SEC-2/SEC-3 govern this phase; alt-screen must be atomic).
- `.planning/REQUIREMENTS.md` — TUI-01 (two-grid alt screen), TUI-02 (both grids resize), TUI-03 (wide/zero-width chars), TUI-04 (live-app correctness), TUI-05 (predictor suppression), SEC-03 (OSC OOM bound).

### Pitfalls & research (v1.3)
- `.planning/research/PITFALLS.md` — Pitfalls A-1 through A-6 (alt-screen atomicity, width accuracy) and SEC-2/SEC-3 (OSC OOM). A-1: do not ship a half-built alt-screen (swap without clear, or clear without restore) — demonstrably worse than the current no-op.
- `.planning/research/ARCHITECTURE.md`, `.planning/research/STACK.md`, `.planning/research/SUMMARY.md` — v1.3 architecture/stack/summary context for the terminal model work.

### Security (OSC OOM)
- `docs/999.1-SECURITY.md` §7 (OSC-OOM) — the analysis that vte's `std` feature makes `osc_raw` an unbounded `Vec<u8>`; the bound must be applied before vte. Referenced directly in `crates/nosh-server/Cargo.toml` lines 32–47.
- **`docs/999.7-SECURITY.md`** — does NOT yet exist. ROADMAP requires the 999.7 OSC mitigation to be in place OR this doc updated **before Phase 19 closes**. Planner must create/update it as part of phase exit.

### Code under change
- `crates/nosh-server/src/terminal.rs` — `TerminalState`, `EchoState.alt_screen` (line 115), `print_char` (line 306, single-column advance), `scroll_up` (line 292), `osc_dispatch` (line 682), `csi_dispatch` ?1049 handling (line 504), resize (line 236). Existing caps: `OSC_52_MAX_BYTES` (line 56), `MAX_TITLE_BYTES`.
- `crates/nosh-client/src/predictor.rs` — predictor; suppression is currently structural (tentative-epoch / bracketed-paste). Needs alt-screen suppression wired.
- `fuzz/fuzz_targets/osc_accumulation.rs` — existing OSC-cap fuzz target; extend for the 1 MiB accumulation bound.

</canonical_refs>

<code_context>
## Existing Code Insights

### Reusable Assets
- `unicode-width` 0.2 already a dependency of `nosh-client` (`crates/nosh-client/Cargo.toml:34`) — add the same to `nosh-server` for width logic; versions/semantics already agree.
- Existing OSC caps in `osc_dispatch` (`OSC_52_MAX_BYTES` = 65_536, `MAX_TITLE_BYTES` = 1_024) — the new 1 MiB bound sits *above* these and intercepts earlier (before vte), it does not replace them.
- `osc_accumulation.rs` fuzz target already asserts OSC-52/title caps and no unbounded alloc — extend it for the accumulation bound.
- `advance()` uses the `std::mem::take` borrow-split pattern (terminal.rs:222) — any pre-vte OSC interception must preserve this.

### Established Patterns
- `TerminalState` is the single server-side authoritative model implementing `vte::Perform`; grid is `Vec<Vec<Cell>>`, scrollback is a capped `VecDeque<Vec<Cell>>` (`SCROLLBACK_LINE_CAP`).
- `EchoState` (terminal.rs:110) already tracks `alt_screen` as an observable flag toggled in `csi_dispatch` (line 504) — the two-grid model hangs off the same toggle point.
- `Cell` carries `ch`, `style`, `fg`, `bg` — the wide-char continuation marker likely adds to this struct; `cell()` returns a `'static` default sentinel out of bounds (do not store the ref across mutations).

### Integration Points
- Two-grid swap hooks into `csi_dispatch` ?1049 enter/exit (line 504) and must coordinate with `resize` (line 236) so both grids resize.
- `scroll_up` (line 292) gets the `!alt_screen` gate (D-19-09) — Phase 22 scrollback sync depends on this gate.
- Predictor suppression: server `EchoState.alt_screen` is the source of truth; client predictor (`predictor.rs`) consumes it — confirm how alt-screen state already reaches the client (diff/datagram echo-state path) during research.
- OSC accumulation bound wraps/precedes the vte `advance()` feed in `TerminalState::advance`.

</code_context>

<deferred>
## Deferred Ideas

- Mode 2027 grapheme clustering — deferred to v1.4+ (wcwidth-per-codepoint is the v1.3 baseline).
- Configurable ambiguous-width negotiation (client→server) — out of scope; default ambiguous→1 this phase.
- Scrollback **sync** to the client — Phase 22 (this phase only sets the `!alt_screen` gate and preserves the primary buffer correctly).

</deferred>

---

*Phase: 19-full-screen-tui-rendering-correctness*
*Context gathered: 2026-06-07*
