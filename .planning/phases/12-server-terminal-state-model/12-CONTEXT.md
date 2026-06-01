# Phase 12: Server Terminal State Model - Context

**Gathered:** 2026-06-01
**Status:** Ready for planning

<domain>
## Phase Boundary

The server maintains an authoritative terminal-state model (`TerminalState` implementing
`vte::Perform`) tracking cell content, cursor position, and echo state — fed from the SAME
PTY-output callsite as `SequencedOutputBuffer`, unit-tested in isolation BEFORE any QUIC
plumbing. Requirement: SYNC-02. This phase does NOT emit datagrams (Phase 13), compute/send
`StateDiff`s over the wire, or do client rendering. It only builds and tests the model.

</domain>

<decisions>
## Implementation Decisions

### Echo-state signal (D-12-01) — prediction-safety foundation for Phase 15 / SEC-01
- **D-12-01:** `TerminalState` tracks the **observable terminal private modes** that appear in
  the PTY output stream and gate prediction safety: DECTCEM cursor show/hide (`?25`),
  alt-screen (`?1049`), bracketed-paste (`?2004`), and application-cursor-keys. This set is
  the "prediction-unsafe / full-screen app" signal consumed by the Phase 15 predictor.
- **D-12-01a (boundary):** True termios `ECHO` (password / `read -s`) is NOT observable from
  the master output stream and is NOT inferred here. Actual noecho/password suppression is
  confirmed at the Phase 15 predictor via echo-confirmation (a predicted char failing to
  appear in confirmed output), not by this model. Do NOT add a termios slave-side probe this
  phase (rejected as out-of-boundary — "fed from the output callsite").

### Model scope (D-12-02)
- **D-12-02:** `TerminalState` models the visible viewport grid **AND scrollback history**
  (user override of the viewport-only recommendation). The model retains scrolled-off lines.
- **D-12-02a (datagram boundary — IMPORTANT for Phase 13):** Even though the model retains
  scrollback, the Phase 11 `StateDiff` / datagram path syncs ONLY the visible viewport.
  Scrollback *sync* remains a separate later reliable-stream feature (M5) — do not widen the
  datagram diff to scrollback. Scrollback in the model serves the server's authoritative
  record and the future scrollback-sync feature.
- **D-12-02b (VT vocabulary):** Authoritatively handle the common subset: printable text,
  CSI cursor motion (A/B/C/D/H), erase-in-display/line, SGR styles, OSC 0/2 title, OSC 52
  clipboard. Unknown/exotic sequences (sixel, DCS, mouse) are ignored with a documented
  scope-fence code comment; the predictor falls back to confirmed server output on divergence.
  Scrollback retention requires a bounded cap (memory) — choose a sane line cap (Claude's
  discretion), mirroring the spirit of `SequencedOutputBuffer`'s byte cap.

### Resize behavior (D-12-03)
- **D-12-03:** On PTY resize (existing M2 resize signal), **resize the grid dimensions and let
  the application repaint** (SIGWINCH) — matches Mosh and real terminals; do NOT attempt text
  reflow/rewrap. New dimensions ride along in the next StateDiff (Phase 11 epoch carries dims).
  Define how scrollback interacts with resize (Claude's discretion — simplest correct behavior;
  no reflow).

### OSC 52 detection (D-12-04, from success criterion #2)
- **D-12-04:** OSC 52 (clipboard-write) sequences are DETECTABLE at the `osc_dispatch`
  callsite — the model can identify them in PTY output. This phase only detects/parses them in
  the model; actual clipboard passthrough behavior is Phase 16 (QOL). OSC 0/2 (title) likewise
  parsed.

### Integration (D-12-05, from success criterion #3)
- **D-12-05:** Add `push_output_and_parse` on `SessionSlot` that feeds BOTH
  `SequencedOutputBuffer` (UNCHANGED — cold-reattach replay must not be affected) AND the new
  `TerminalState`. The 3 existing `slot.push_output(&data)` callsites in server.rs (~414, ~504,
  ~786) are the integration points. Do not regress the reattach replay path.

### Locked by REQUIREMENTS/STACK (NOT relitigated)
- VT parser: **`vte` 0.15.0** (Alacritty's Paul-Williams state machine; `Perform` trait is the
  extension point) — add as a dependency. NOT termwiz (full emulator — too heavy).
- Unit-tested in isolation before any QUIC plumbing (success criterion #1, #4).

### Claude's Discretion
- Cell/grid representation, style/attribute storage, scrollback cap value and data structure.
- Exact `vte::Perform` method bodies for each handled sequence.
- How `TerminalState` exposes its grid for the future Phase 13 diff computation (design the
  read API with Phase 11 `StateDiff` run-length viewport extraction in mind).

</decisions>

<specifics>
## Specific Ideas

- "Unit-tested in isolation before any QUIC plumbing is touched" is a hard sequencing
  constraint — `TerminalState` must be fully testable with byte input → expected grid, with no
  network/session dependencies.
- Research flag (MEDIUM confidence): VERIFY the vte 0.15.0 `Perform::osc_dispatch` signature
  (`params: &[&[u8]], bell_terminated: bool`) at docs.rs BEFORE implementing — the roadmap
  explicitly calls this out.

</specifics>

<canonical_refs>
## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### Requirement & success criteria
- `.planning/REQUIREMENTS.md` — **SYNC-02**. `.planning/ROADMAP.md` Phase 12 section (4 success
  criteria + the vte osc_dispatch research flag).

### Architecture & stack
- `CLAUDE.md` — terminal model = `vte`; datagram=loss-tolerant state-sync; env-sanitization &
  security invariants. Technology Stack table: vte 0.15.0 rationale (vs termwiz).
- `.planning/research/ARCHITECTURE.md`, `.planning/research/PITFALLS.md`,
  `.planning/research/FEATURES.md` — state model + predictive-echo context.

### Existing code to integrate with
- `crates/nosh-server/src/registry.rs` — `SequencedOutputBuffer` (lines 29-199, UNCHANGED) and
  `SessionSlot::push_output` (line 344) — the callsite that gains `push_output_and_parse`.
- `crates/nosh-server/src/server.rs` — the 3 `slot.push_output(&data)` callsites (~414, ~504,
  ~786) inside the two session pumps.
- `crates/nosh-proto/src/datagram.rs` (Phase 11 output) — the `StateDiff` shape the eventual
  diff extraction (Phase 13) will target; design `TerminalState`'s read API to suit it.
- `crates/nosh-server/Cargo.toml` — add `vte = "0.15"`.

</canonical_refs>

<code_context>
## Existing Code Insights

### Reusable Assets
- `SequencedOutputBuffer` byte-cap pattern (registry.rs) is the model for `TerminalState`'s
  bounded scrollback cap.
- The existing resize signal path (M2) feeds dimensions — `TerminalState` consumes the same.

### Established Patterns
- `SessionSlot` methods lock a Mutex briefly, mutate, release (e.g. push_output). The
  `push_output_and_parse` follows this — feed both buffers under the same call.

### Integration Points
- 3 `slot.push_output` callsites in server.rs become `push_output_and_parse` (or push_output
  internally also parses). SequencedOutputBuffer behavior must be byte-identical (reattach).

</code_context>

<deferred>
## Deferred Ideas

- Computing/emitting `StateDiff` from `TerminalState` over datagrams — Phase 13 (SYNC-03).
- Scrollback *sync* to the client over a reliable stream — M5 (this phase only retains
  scrollback in the server model).
- OSC 52 clipboard passthrough behavior + terminal title propagation — Phase 16 (QOL).
- Client-side use of any of this — Phase 14/15.

</deferred>

---

*Phase: 12-server-terminal-state-model*
*Context gathered: 2026-06-01*
