# Phase 14: Client Predictor — Confirmed Rendering - Context

**Gathered:** 2026-06-01
**Status:** Ready for planning

<domain>
## Phase Boundary

The client renders the CONFIRMED terminal screen from received state-sync datagrams through a
single screen-composition path (`ClientScreen` + `render_to_stdout`). Display moves to the
datagram path; the reliable `PtyData` stream keeps flowing ONLY to advance the cold-reattach
ack counter. This proves the datagram display path end-to-end BEFORE the Phase 15 speculative
overlay. Requirement: PREDICT-01. No speculative/predicted echo this phase (Phase 15); no
OSC52/title/loss-banner behavior (Phase 16) — only the no-op `ConnectionLossOverlay` stub.

</domain>

<decisions>
## Implementation Decisions

### Render architecture (D-14-01)
- **D-14-01:** **Framebuffer-diff compositor (Mosh `Display` model).** `ClientScreen` holds (a)
  a CONFIRMED grid (updated by applying datagram `StateDiff`s) and (b) a PHYSICAL grid (what is
  currently on the terminal). `render_to_stdout()` composes `desired = confirmed + overlays`,
  diffs `desired` against `physical`, emits MINIMAL ANSI (cursor moves + SGR + changed cells),
  then sets `physical = desired`. Flicker-free; idempotent (re-rendering after a datagram
  resend/loss emits nothing new); and gives Phase 15 prediction + `ConnectionLossOverlay` a
  clean composition seam (they add overlay layers to `desired`). Rejected: direct diff-replay
  (redundant re-emit on acked-epoch resend; no overlay seam) and full-repaint (flicker, waste).
- **D-14-01a (compositor built now):** The render path is a compositor from the start —
  `desired = confirmed base ⊕ overlay layers`. Phase 14 has two layers: the confirmed grid and
  a no-op `ConnectionLossOverlay` stub (criterion #4). Phase 15 adds the speculative-echo layer;
  Phase 16 activates the loss overlay. The single path (`render_to_stdout`) is the only writer
  to stdout for display.

### Display authority & startup/gap (D-14-02)
- **D-14-02:** **Datagram-only display.** Once datagrams are active, display is purely the
  datagram-fed `ClientScreen` — NO direct `stdout.write_all` for display (criterion #1). At
  startup the screen is blank until the first datagram, which (per Phase 13 acked-epoch=0
  baseline, D-13-01b) is a full keyframe arriving within ~1 tick. Datagram gaps/loss self-heal
  via Phase 13's acked-epoch resend. One source of truth for display. Rejected: PtyData-display
  fallback (two display paths + handoff flicker + reintroduces direct stdout writes).

### PtyData on the client post-datagram (D-14-03)
- **D-14-03:** The client STILL parses `PtyData` frames off the reliable stream to advance
  `highest_applied` and keep sending periodic `Ack { seq }` (D-08 continuous acking) so
  cold-reattach replay still works (criterion #3) — but `PtyData` payload is NOT written to
  stdout anymore (display comes from datagrams). The client may discard `PtyData` content for
  display purposes (no client-side scrollback this milestone). The reattach `Ack`/`highest_applied`
  mechanism MUST NOT regress.
- **D-14-03a (epoch-ack):** This phase is where the REAL client begins emitting the Phase 13
  datagram epoch-ack (D-13-01c) — the client acks the last-applied `epoch` so the server's
  acked-epoch baseline advances. (Phase 13 added the server side + a test-client ack; Phase 14
  wires the real client ack.) Keep this DISTINCT from the reliable-stream `Ack { seq }`.

### ClientScreen types (D-14-04)
- **D-14-04:** **Reuse nosh-proto types.** `ClientScreen`'s grid uses the same cell/style
  vocabulary as the Phase 11 `StateDiff`/`DiffRun` (`fg`/`bg` `Option<u8>`, `CellStyle`
  bitflags), mirroring the Phase 12 server `TerminalState`. Applying a diff is a direct map, no
  translation; confirmed grid and wire format stay in lockstep.

### Apply semantics (D-14-05)
- **D-14-05:** Apply a `StateDiff` to the confirmed grid only if its `epoch` > last-applied epoch
  (D-11-03 monotonic staleness check); a full-keyframe (post-resume) replaces the confirmed grid.
  Resize diffs carry new dimensions → resize the confirmed + physical grids.

### Locked by REQUIREMENTS/roadmap (NOT relitigated)
- `run_pump` gets a `conn.read_datagram()` arm routing `StateDiff` → `ClientScreen.render_to_stdout()`.
- End-to-end test: datagram-rendered screen matches the raw PTY output's visible characters.
- `highest_applied` keeps advancing from `PtyData` (reattach Ack intact).
- `ConnectionLossOverlay` exists as a no-op stub wired into the render path.

### Claude's Discretion
- The minimal-ANSI diff algorithm details (cursor-move optimization, SGR run coalescing).
- Physical-grid representation; how overlays are represented as layers in the compositor.
- How the end-to-end test captures "visible characters" for comparison (e.g. drive a server
  TerminalState and a ClientScreen with the same byte stream and compare grids).

</decisions>

<specifics>
## Specific Ideas

- The compositor is the load-bearing seam for the whole predictor: Phase 15 adds a speculative
  layer to `desired` and the same reconcile loop renders it; epoch-confirmation removes
  predictions when the confirmed grid catches up. Build `render_to_stdout` with that in mind.
- Idempotent rendering is what makes datagram loss invisible: a resent/duplicate diff produces
  the same `desired`, which diffs to zero changes against `physical`.

</specifics>

<canonical_refs>
## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### Requirement & success criteria
- `.planning/REQUIREMENTS.md` — **PREDICT-01**. `.planning/ROADMAP.md` Phase 14 section (4 criteria).

### Wire format & server model this consumes
- `crates/nosh-proto/src/datagram.rs` (Phase 11 — `StateDiff`/`DiffRun`/`CellStyle`, `fg`/`bg`
  `Option<u8>`, `epoch`; `decode_datagram`). ClientScreen reuses these types (D-14-04).
- Phase 12 server `TerminalState` (crates/nosh-server/src/terminal.rs) — the server-side mirror;
  keep the client grid semantics consistent with it.
- Phase 13 `13-CONTEXT.md` — acked-epoch model + epoch-ack (D-13-01); the client ack lands here.

### Client integration sites
- `crates/nosh-client/src/client.rs` — `run_pump` loop (add `conn.read_datagram()` arm),
  current `PtyData`→stdout write path (remove from display; keep counting for `highest_applied`),
  `Ack { seq }` sender (~524), `RawModeGuard`/stdout setup (~280-420).
- `crates/nosh-client/src/platform.rs` — resize watcher (feeds dimensions; resize diffs).

### Architecture
- `CLAUDE.md` — datagram = state-sync display path; "single screen-composition path, never direct
  stdout once predictor exists". `.planning/research/FEATURES.md`/`ARCHITECTURE.md`/`PITFALLS.md`
  (Mosh SSP/Display model; predictive-echo groundwork).

</canonical_refs>

<code_context>
## Existing Code Insights

### Reusable Assets
- `nosh_proto::decode_datagram` + `StateDiff` types (Phase 11) — directly applied to the grid.
- The existing `Ack { seq }` / `highest_applied` machinery (client.rs ~511-530) — unchanged for
  reattach; the new epoch-ack is a parallel, distinct signal.
- `RawModeGuard` already sets up stdout for ANSI (ENABLE_VIRTUAL_TERMINAL_PROCESSING on Windows).

### Established Patterns
- `run_pump` `tokio::select!` arms (mirror the reliable-stream/Ack arms for the new datagram arm).
- crossterm for terminal size; ANSI emitted directly to stdout.

### Integration Points
- New `conn.read_datagram()` arm in `run_pump` → decode → `ClientScreen::apply(diff)` →
  `render_to_stdout()`. PtyData arm: advance `highest_applied`, do NOT write to stdout.
- Client emits the datagram epoch-ack after applying (D-14-03a).

</code_context>

<deferred>
## Deferred Ideas

- Speculative local echo overlay (predicted keystrokes ahead of confirmation) — Phase 15.
- ConnectionLossOverlay activation (the >5s silence banner) + OSC52/title — Phase 16.
- Client-side scrollback — M5.

</deferred>

---

*Phase: 14-client-predictor-confirmed-rendering*
*Context gathered: 2026-06-01*
