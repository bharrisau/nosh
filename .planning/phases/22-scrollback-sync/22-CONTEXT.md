# Phase 22: Scrollback Sync - Context

**Gathered:** 2026-06-12
**Status:** Ready for planning
**Mode:** Autonomous smart-discuss (non-interactive — recommended defaults auto-accepted)

<domain>
## Phase Boundary

Deliver shell history that has scrolled off the visible grid from the server's
existing `TerminalState.scrollback` buffer to the client, over a dedicated
**reliable** QUIC channel (the `ChannelType::Scrollback` channel that rides the
Phase 21 mux layer), paged on demand, gated to exclude alt-screen content, and
surviving both QUIC migration and cold reattach.

In scope (SCROLL-01…SCROLL-05):
- A scrollback request/page protocol over the Scrollback channel: server streams
  lines from `TerminalState.scrollback` on demand; client renders them above the
  current viewport.
- A `ScrollbackPage` reliable-stream wire type carrying an `epoch_at_snapshot`
  field; a `ScrollbackCredit` application-level flow-control message paces
  delivery.
- A dedicated server-side scrollback sender **tokio task** fed by a **bounded
  `mpsc::channel`**, isolated from the session pump (no PTY-input latency spike,
  no missed diff ticks during transfer).
- The `!alt_screen` gate on `scroll_up()` (already present in `terminal.rs`) is
  the correctness boundary for SCROLL-03 — verified, not re-implemented.
- Client scrollback-view UX: Shift-PageUp enters/pages up, Shift-PageDown pages
  down, any other keystroke snaps back to live and is forwarded to the shell,
  reaching the live view auto-exits scrollback mode.
- Migration transparency (channel survives via connection IDs) and cold-reattach
  recovery (client **re-opens** the Scrollback channel after `ResumeComplete`;
  channel state is never byte-replayed; `TerminalState.scrollback` survives in
  the `SessionSlot`).

Out of scope: scrollback search / copy-mode text selection (later UX add — see
REQUIREMENTS "Out of Scope"); port/agent forwarding and file transfer (other mux
consumers, deferred); raising `SCROLLBACK_LINE_CAP` (locked at 10,000).

LOCKED by ROADMAP success criteria + Security note (NOT grey areas — treat as
hard constraints): reliable-stream-only delivery enforced at the type level
(scrollback sender accepts only `SendStream`, never `send_datagram`);
`ScrollbackPage` carries `epoch_at_snapshot`; `ScrollbackCredit` flow control;
separate tokio task with a bounded `mpsc::channel` that drops oldest lines rather
than blocking the pump; `SCROLLBACK_LINE_CAP = 10_000` unchanged; `scroll_up()`
gated on `!alt_screen`; channel re-opened (not replayed) after cold reattach.
</domain>

<decisions>
## Implementation Decisions

### Request / paging protocol (SCROLL-01, SCROLL-02, S-4)
- **Client-driven pull, not server push.** The client requests history; the
  server never bulk-dumps. Add a `ScrollbackRequest { channel_id, from_line,
  count }` control/stream message (client → server) that names a contiguous range
  of lines (indexed from the newest scrollback line backwards, 0 = line just
  above the live viewport). The server replies with `ScrollbackPage` frames over
  the Scrollback channel's reliable stream. Alternative considered: a single
  "give me everything" stream — rejected (violates S-4 paging requirement and the
  bounded-buffer mandate).
- **`ScrollbackPage` carries `epoch_at_snapshot`** (LOCKED) plus the page's line
  range, the line count actually available (so the client knows when it has hit
  the top of history), and per-line content. The client applies historical lines
  up to (but not including) `epoch_at_snapshot`, then gates its transition back to
  the live datagram grid on receiving a datagram with `epoch >= epoch_at_snapshot`
  (mirrors the existing CR-01 `epoch_snapshots` pattern in `server.rs`).
- **`epoch_at_snapshot` is read from the server's existing `current_epoch`
  datagram counter** at the moment the page is snapshotted, under the same
  `terminal_state` mutex acquisition, so the scrollback snapshot and the epoch are
  mutually consistent (no torn read between scrollback contents and live grid).
- **Pages are encoded as `Message` variants appended after the last existing
  variant** (`ChannelClose`), preserving the postcard discriminant order (MUX-06
  invariant). A discriminant-stability assertion covers the new variants, matching
  the Phase 21 precedent.

### Flow control & sender isolation (SCROLL-02, M-6, S-4)
- **Reuse the Phase 21 256 KiB per-channel byte-credit window** as the default for
  the Scrollback channel; the `ScrollbackCredit` message is the channel-typed
  credit grant (consistent with `ChannelCredit`, byte-granular). No bespoke
  windowing scheme — variable-size pages map cleanly onto byte credits.
- **Default page size: 256 lines per `ScrollbackRequest`.** At ~80–200 cols this
  is a few KB to ~50 KB — comfortably inside one credit window, one screenful-plus
  of context per page, and small enough that a slow client never holds large
  server-side buffers. (Discretion item — see below.)
- **Server-side scrollback sender runs as a dedicated `tokio::spawn` task** (the
  channel task model already established in `nosh-server/src/channel.rs`), fed by a
  **bounded `mpsc::channel`** from the pump. If that channel is full the pump
  **drops the oldest queued lines rather than blocking** (LOCKED, M-6). Bounded
  pending-send buffer cap: a few hundred lines (default 512) so a stalled client
  cannot grow server memory without bound (S-4).
- **PTY-input-responsiveness regression test is required**: flood the scrollback
  channel to its credit/buffer limit and assert PTY input latency and the 16 ms
  diff tick are unaffected (the M-6 isolation proof).
- **Reliable-only enforced at the type level** (LOCKED, S-1): the scrollback
  sender's function signature accepts a `quinn::SendStream` (or the channel
  wrapper over one) and has no access to `Connection::send_datagram`. An in-order /
  no-gap test under simulated loss proves sequential integrity.

### Alt-screen gate & buffer correctness (SCROLL-03, S-2)
- **The `!alt_screen` gate on `scroll_up()` already exists** (`terminal.rs`
  ~line 631) and `resize()` carries the same D-19-09 gate; this phase **verifies**
  it rather than adding it. Required unit test (verbatim from ROADMAP criterion 3):
  write to the primary buffer, activate alt screen, force scroll lines, deactivate
  alt screen, assert only primary lines appear in `TerminalState.scrollback`.
- **Server reads scrollback through a `TerminalState` accessor** (e.g.
  `scrollback_lines(from, count)`), never by exposing the `VecDeque` directly, so
  the cap and the alt-screen invariant stay enforced inside `terminal.rs`.

### Client viewport rendering & keybindings (SCROLL-04)
- **Keybindings: Shift-PageUp enters scrollback view and pages up;
  Shift-PageDown pages down** (LOCKED). The client detects these as raw CSI byte
  sequences (`ESC [ 5 ; 2 ~` / `ESC [ 6 ; 2 ~`) in its existing stdin escape
  machine — consistent with how the client already parses raw input bytes (it does
  not use crossterm key events for the data path).
- **Any non-paging keystroke while in scrollback view immediately snaps back to
  the live viewport AND is delivered to the shell** (LOCKED) — the keystroke is
  not swallowed. Reaching the live view (paging down past line 0) auto-exits
  scrollback mode (LOCKED).
- **Scrollback rendering model: a client-side scroll offset over a locally
  retained line buffer.** The client keeps the fetched historical lines in a local
  buffer and a current view offset; entering scrollback redraws the screen from
  that buffer; live datagram application is suspended for display only while in
  scrollback (the connection keeps acking epochs so the server pump never stalls).
  This replaces the current `main.rs` line ~934 stub that discards channel data
  "no scrollback this milestone".
- **Prefetch is lazy/on-demand**: the client requests the next page when the user
  pages near the top of what it currently holds, not eagerly. Edge cases — paging
  up at the very top of history is a no-op (page reports 0 further lines
  available); paging down at the live boundary exits scrollback.

### Mobility & cold reattach (SCROLL-05, MUX-05)
- **Migration is transparent**: the Scrollback channel rides the same QUIC
  connection and survives IP change via connection IDs with no application action
  (inherited from the mux layer).
- **Cold reattach: the client re-opens the Scrollback channel after
  `ResumeComplete`** (LOCKED) — never byte-replayed. The server's
  `TerminalState.scrollback` lives in the `SessionSlot` and survives the orphan
  window, so history is viewable immediately after reattach. A reattach test must
  assert scrollback is fetchable post-resume.

### Claude's Discretion
- Exact default **page size (256 lines)** and **pending-send buffer cap
  (512 lines)** — tune during planning/implementation against the credit window;
  any value that keeps a page inside one 256 KiB credit grant and the server
  buffer bounded is acceptable.
- Exact **new `Message`/wire variant names and field layouts** for
  `ScrollbackRequest` / `ScrollbackPage` / `ScrollbackCredit` (names above are
  indicative; final shape is the implementer's, subject to: append-only
  discriminants, `epoch_at_snapshot` present, byte-granular credit).
- **Line-width / reflow handling (S-3)**: recommended default is to send each
  scrollback line with its **original column width** as per-line metadata and let
  the client render variable-width lines (no server-side reflow) — scrollback
  lines are stored at their original width and `SCROLLBACK_LINE_CAP`/resize already
  preserve that. Final choice (per-line width metadata vs. truncate-to-current)
  is at implementer discretion provided post-resize content is not silently
  mangled; a resize test should cover it.
- Whether `ScrollbackRequest` travels on the channel's own data stream or the
  control stream — implementer's call, provided it does not reintroduce the M-2
  control/data flow-control deadlock.
</decisions>

<code_context>
## Existing Code Insights

### Reusable Assets
- `crates/nosh-proto/src/messages.rs` — `ChannelType::Scrollback` variant already
  exists (Phase 21). The mux control messages (`ChannelOpen`/`ChannelAccept`/
  `ChannelReject`/`ChannelCredit`/`ChannelClose`) and the append-only discriminant
  discipline are in place; new scrollback wire variants append after `ChannelClose`.
- `crates/nosh-server/src/channel.rs` — established per-channel `tokio::spawn` task
  model with bounded `mpsc::Receiver<ChannelEvent>`, 256 KiB `INITIAL_CREDIT`,
  `read_varint_u32`, credit-pause/replenish loop. The scrollback sender task is a
  new channel handler following this exact shape (echo loop is the template).
- `crates/nosh-client/src/channel.rs` — client channel task with `EvenIdAllocator`
  (even client ids), credit-drain/advertise loop, control_tx mpsc (pump is sole
  control-stream writer — invariant A4). The scrollback client channel follows this.
- `crates/nosh-server/src/terminal.rs` — `TerminalState.scrollback:
  VecDeque<Vec<Cell>>`, `SCROLLBACK_LINE_CAP = 10_000`, `scroll_up()` with the
  `!alt_screen` gate (line ~631) and the matching `resize()` D-19-09 gate. Add a
  read accessor here; do not expose the `VecDeque`.
- `crates/nosh-server/src/server.rs` — `current_epoch` datagram counter and the
  CR-01 `epoch_snapshots` VecDeque (snapshot-at-send-time) — the precedent and the
  source of `epoch_at_snapshot`.
- `crates/nosh-server/src/registry.rs` — `SessionSlot` owns
  `terminal_state: Mutex<TerminalState>` and survives the orphan/reattach window;
  this is where scrollback persists across cold reattach.
- `crates/nosh-proto/src/datagram.rs` — `StateDiff.epoch: u64` is the live-grid
  epoch the client compares against `epoch_at_snapshot`.

### Established Patterns
- postcard enum encoding is positional — append-only `Message` discriminants,
  guarded by a discriminant-stability test (MUX-06, Phase 21 precedent in codec.rs).
- Channels are independent QUIC streams; per-channel byte-credit flow control;
  channel tasks never write the control stream directly (route via control_tx).
- Client data path reads raw stdin bytes through an escape machine
  (`crates/nosh-client/src/main.rs`), not crossterm key events — Shift-PageUp/Down
  are matched as raw CSI sequences there.
- Cold-reattach re-establishes channels over the control channel after resume
  (MUX-05); channel state is per-connection, never byte-replayed.

### Integration Points
- Server pump: tap accepted-channel dispatch to spawn the scrollback sender task on
  `ChannelType::Scrollback` accept; feed it from a bounded mpsc owned by the pump.
- Client pump (`main.rs` ~line 934): replace the "data discarded — no scrollback
  this milestone" stub with the scrollback view buffer + offset rendering and the
  Shift-PageUp/Down escape-sequence handling.
- Reattach path: client re-issues `ChannelOpen{Scrollback}` after `ResumeComplete`.
</code_context>

<specifics>
## Specific Ideas

- Implementation order implied by the goal and pitfalls: (1) reliable-only
  type-level enforcement + new append-only wire variants (`ScrollbackRequest` /
  `ScrollbackPage` w/ `epoch_at_snapshot` / `ScrollbackCredit`) + discriminant
  test; (2) `TerminalState` read accessor + the verbatim alt-screen-exclusion unit
  test (SCROLL-03); (3) server scrollback sender as a separate bounded-mpsc tokio
  task with drop-oldest backpressure + the M-6 PTY-latency isolation test;
  (4) client scrollback view buffer, offset rendering, Shift-PageUp/Down escape
  parsing, snap-back-to-live-on-keystroke; (5) `epoch_at_snapshot` handoff test
  (no duplicate / no missing lines at the boundary); (6) cold-reattach re-open +
  post-resume fetchability test; (7) lossy in-order/no-gap test (S-1).
- Honour every Security-note constraint as a test, not just code: reliable-only
  (type signature + lossy test), drop-oldest bounded channel, cap unchanged,
  alt-screen gate, channel re-open on reattach.

</specifics>

<deferred>
## Deferred Ideas

- Scrollback search and copy-mode / text selection — explicitly out of scope for
  v1.3 (REQUIREMENTS "Out of Scope"); v1.3 scrollback is view + page only.
- Server-side scrollback reflow to current width — if per-line-width client-side
  rendering proves insufficient UX, a true reflow is a later enhancement; not in
  this phase.
- Raising `SCROLLBACK_LINE_CAP` above 10,000 — only with a measured reason
  (operator metric on `scrollback.len()` approaching the cap is the trigger), not
  in this phase.
- A `tracing::warn!` when `scrollback.len()` nears the cap (operator
  observability, PITFALLS S-4) — nice-to-have; can fold in here or defer.

</deferred>

<canonical_refs>
## Canonical References

- `.planning/ROADMAP.md` — Phase 22 success criteria (1–5) + Security note
  (pitfalls S-1…S-5, M-6) — the authoritative locked contract.
- `.planning/REQUIREMENTS.md` — SCROLL-01…SCROLL-05 and the v1.3 "Out of Scope"
  table (no search/selection; no cap raise without measured reason).
- `.planning/research/PITFALLS.md` — S-1 (datagram ban), S-2 (alt-screen buffer),
  S-3 (resize reflow widths), S-4 (memory cap / paging), S-5 (epoch_at_snapshot
  handoff), M-6 (sender task isolation / drop-oldest).
- `.planning/phases/21-channel-multiplexing-foundation/21-CONTEXT.md` — the mux
  layer this phase consumes (channel-id parity, per-stream binding, 256 KiB credit,
  append-only discriminants, cold-reattach re-establishment).
- `crates/nosh-proto/src/messages.rs`, `crates/nosh-server/src/channel.rs`,
  `crates/nosh-client/src/channel.rs`, `crates/nosh-server/src/terminal.rs`,
  `crates/nosh-server/src/server.rs`, `crates/nosh-server/src/registry.rs` —
  integration points named above.
- `CLAUDE.md` — load-bearing transport decisions (datagrams = loss-tolerant state
  sync; reliable streams carry control/scrollback/forwarding).
</canonical_refs>
</content>
</invoke>
