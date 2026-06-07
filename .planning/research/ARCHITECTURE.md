# Architecture: nosh v1.3 (M5) Integration

**Domain:** Integration research for channel multiplexing, scrollback, alt-screen, and repaint pacing into an existing QUIC remote shell
**Researched:** 2026-06-07
**Confidence:** HIGH — based on reading actual source files, not assumptions

---

## Summary

v1.3 adds four features to a working system. The integration difficulty varies greatly: repaint pacing and alt-screen are well-contained server-side changes; scrollback is a new feature built on top of the existing mux layer; channel multiplexing is the only greenfield foundational piece and its transport decision has meaningful knock-on effects.

The existing architecture has one load-bearing invariant that governs nearly every decision below: **the single reliable bidirectional QUIC stream carries all sequenced output** (`PtyData` frames in `SequencedOutputBuffer`) and these are replayed verbatim on cold reattach. Any new reliable channel that also needs replay must either be folded into this buffer or have its own replay mechanism. Datagrams (`StateDiff`) are ephemeral and carry no replay obligation.

---

## Integration Map

| Feature | Files Touched | New vs Modified | Primary Concern |
|---------|--------------|-----------------|-----------------|
| Channel mux — proto | `crates/nosh-proto/src/messages.rs` | Modified (new variants appended) | Discriminant ordering invariant |
| Channel mux — server | `crates/nosh-server/src/server.rs` | Modified (new `select!` arms, new stream accept loop) | Interaction with `run_session` / `run_reattach_session` pump |
| Channel mux — client | `crates/nosh-client/src/client.rs` | Modified (open control channel, accept mux channels) | Reattach path must re-open channels |
| Scrollback transport | `crates/nosh-proto/src/messages.rs` | Modified (new variants: `ScrollbackRequest`, `ScrollbackPage`) | Discriminant ordering |
| Scrollback server | `crates/nosh-server/src/terminal.rs` | Modified (`scrollback` field already exists; add page-query method) | `alt_screen` gate; buffer ownership during reattach |
| Scrollback server | `crates/nosh-server/src/server.rs` | Modified (handle scrollback request messages in pump) | Routing through mux layer |
| Scrollback client | `crates/nosh-client/src/client.rs` | Modified (scrollback UI + paging request) | New display mode distinct from confirmed grid |
| Alt-screen buffer | `crates/nosh-server/src/terminal.rs` | Modified (`TerminalState`: new `alt_grid`, `saved_cursor`; real `?1049h`/`?1049l` handler) | `build_state_diff` must read correct active grid; epoch reset on switch |
| Alt-screen client | `crates/nosh-client/src/screen.rs` | Possibly modified (full clear on alt-screen enter/exit, emit_connect_clear pattern) | Physical grid reset on buffer switch |
| Repaint pacing | `crates/nosh-server/src/server.rs` | Modified (burst loop in `diff_interval` arm) | Both documented traps; must not touch `build_state_diff` internals |
| Repaint pacing | `crates/nosh-proto/src/datagram.rs` | No change (encode/decode path is unchanged) | None |

---

## Decision: Channel Transport — New Quinn Streams vs In-Stream Framing

**Recommendation: new quinn streams, opened after an OPEN/ACCEPT/REJECT handshake on a reserved control stream (channel id 0).**

### Rationale

Quinn already gives logical multiplexing: `conn.open_bi()` / `conn.accept_bi()` open independent reliable streams. Each stream has its own flow-control window, its own head-of-line blocking domain, and QUIC-level framing. Using a separate stream for scrollback means a scrollback page transfer cannot delay keystrokes (they are on the primary stream) and vice versa.

The alternative — application-level framing inside the existing single stream — would require multiplexing logic on top of quinn's reliable ordering. Today the single stream carries `PtyData`/`Resize`/`Ack`/`TerminalControl` frames interleaved. Adding a scrollback channel to that same stream means a scrollback page response can sit in front of a `PtyData` keystroke response in the reliable send queue, reintroducing HOL blocking at the application level even though QUIC avoids it at the transport level. Worse, the existing `SequencedOutputBuffer` replay mechanism on reattach only replays `PtyData` chunks (seq-numbered). Interleaving scrollback frames inside the same stream would require the replay logic to understand and skip non-`PtyData` frames, or every frame type becomes part of the replay transcript. That is architecturally messy and fragile.

### The Control Channel Pattern (borrowed from quicshell)

Reserve stream pair 0 as the control channel. Before either side opens a secondary stream, it sends `ChannelOpen { channel_type: ScrollbackSync, channel_id: u32 }` on the control stream and waits for `ChannelAccept { channel_id }` or `ChannelReject { channel_id, reason: String }`. This prevents either side from silently opening streams the peer does not understand. The control channel is always the first stream opened by the client after auth (the current bidi stream), so backwards compatibility is automatic: the server side continues reading `SessionOpen` or `Reattach` as the first frame on stream 0; the protocol version signals whether additional streams are expected. For v1.3, no negotiation is needed — both sides know M5 is in play.

### Reattach and Migration Impact

Cold reattach (`run_reattach_session`) replays buffered `PtyData` from `SequencedOutputBuffer` on the primary stream. Secondary streams (scrollback) are stateless request/response — there is nothing to replay. The client simply re-opens the scrollback channel after reattach completes (`ResumeComplete` gate). This is clean: new streams can be opened on the re-used QUIC connection at any time, and migration preserves all open streams at the transport layer automatically (QUIC connection migration carries all streams). Secondary streams do not need their own reattach token or replay buffer.

### Flow Control

Each quinn stream has QUIC-level flow control independently. Scrollback pages (potentially large) consume only the scrollback stream's flow-control window, not the primary stream's. The primary stream's send-side backpressure is unaffected. Per-channel application-level flow control (as mentioned in the M5 brief) can be added on top: a `ScrollbackCredit { bytes }` message lets the server pace page sends without saturating the stream's QUIC window. For v1.3, QUIC's built-in stream flow control is sufficient; per-channel app-level windows are an M5+ extension.

---

## Scrollback Architecture

### Storage

`TerminalState.scrollback` (a `VecDeque<Vec<Cell>>`, capped at `SCROLLBACK_LINE_CAP = 10_000` lines) already exists and is populated by `scroll_up()` whenever the viewport scrolls. No new storage field is needed.

**Critical gate:** Scrollback is only meaningful on the primary screen. When `echo_state.alt_screen == true`, scrollback accumulation is suppressed (alt-screen applications like vim do not scroll the primary scrollback). The `scroll_up()` method must check `self.echo_state.alt_screen` and skip the `scrollback.push_back` when true. This is a one-line guard.

### Wire Protocol

Two new `Message` variants appended after the current last variant (`TerminalControl`, discriminant 10):

```
ScrollbackRequest { from_line: u64, max_lines: u16 }   // client → server
ScrollbackPage    { from_line: u64, lines: Vec<Vec<DiffRun>> }  // server → client
```

`from_line` is a 0-based line index into the server's scrollback buffer (line 0 = oldest retained). `max_lines` caps the page size. The server sends as many lines as it has from `from_line` onward, up to `max_lines`. Empty response signals end-of-history.

These travel on the dedicated scrollback reliable stream (not the primary stream), so they never enter `SequencedOutputBuffer` and never participate in reattach replay.

### Relationship to `SequencedOutputBuffer`

Scrollback is entirely decoupled from `SequencedOutputBuffer`. The output buffer tracks raw PTY bytes for replay; the scrollback is a decoded cell grid derived from those bytes by `TerminalState`. The two are consistent by construction (both fed from `push_output_and_parse`). On cold reattach, replaying `PtyData` chunks re-drives `TerminalState.advance()`, rebuilding the scrollback model — the scrollback channel simply queries whatever the model holds after replay completes.

### Client Rendering

Scrollback is a separate display mode from the live grid. When the user scrolls up, the client suspends datagram rendering to the terminal (or renders into a scrollback-viewport mode), sends `ScrollbackRequest` on the scrollback stream, and renders received `ScrollbackPage` content above the confirmed grid. On any new keystroke or scroll-down, the client returns to live rendering and resumes datagram application. The `ClientScreen.confirmed` grid and `PredictionOverlay` are untouched during scrollback viewing.

---

## Alternate-Screen Buffer in the Terminal Model

### Where the Real Buffer Pair Lives

`TerminalState` in `crates/nosh-server/src/terminal.rs` currently has a single `grid: Vec<Vec<Cell>>` for the viewport. The `echo_state.alt_screen` flag is set by `?1049h`/`?1049l` but the grid is unchanged (line 504, `self.echo_state.alt_screen = enable`).

The fix adds two new fields to `TerminalState`:

```rust
alt_grid: Vec<Vec<Cell>>,       // the alternate-screen grid
saved_cursor: CursorPos,        // cursor saved on ?1049h entry (Xterm behavior)
```

`grid` remains the **active viewport** — `viewport_rows()`, `compute_diff_runs`, and `build_state_diff` all read `grid` without change. Buffer switching swaps the contents:

- `?1049h` (enter alt): save `cursor` → `saved_cursor`; swap `grid` ↔ `alt_grid`; clear the new active grid (the alt buffer starts blank per xterm semantics); set `echo_state.alt_screen = true`.
- `?1049l` (leave alt): swap `grid` ↔ `alt_grid`; restore `cursor ← saved_cursor`; set `echo_state.alt_screen = false`.

The `alt_grid` is the same `Vec<Vec<Cell>>` shape as `grid`, initialized identically in `TerminalState::new()`.

### Resize Interaction

`resize()` must resize **both** `grid` and `alt_grid`. Currently it only touches `grid`. The alt buffer must track the same dimensions or a switch after a resize will restore a stale-sized grid.

### Interaction with `build_state_diff` and Epoch Reset

`build_state_diff` snapshots `slot.with_terminal_state(|ts| ts.viewport_rows()...)`. Because `viewport_rows()` always reads `self.grid` (the active buffer), switching between primary and alt automatically delivers the correct content — no changes to `build_state_diff` are needed.

However, the client's confirmed grid must be completely reset on a buffer switch, because the client's `confirmed` grid holds the previous buffer's content. The mechanism: increment `current_epoch` on the tick immediately after a buffer switch and include a `StateDiff` with full-screen content (empty `last_acked_snapshot` baseline is sufficient — the first diff after reattach already does this). To signal a buffer switch to the client without a new message type, the server can rely on the diff converging: the full `alt_grid` content arrives within a few datagram ticks after the switch, and `ClientScreen.apply()` merges it correctly.

For the predictor: a buffer switch is semantically equivalent to Enter/Ctrl-C — it resets the echo epoch. The server's `build_state_diff` will see a totally different grid, the client's `confirmed` will converge to it, and the predictor's pending queue will mismatch and reset via `cull()`. No explicit epoch-reset signal is needed; the state-diff self-correction handles it.

**One exception:** the `PredictionOverlay` must detect when `confirmed` changes dramatically (large mismatch on `cull()`) and call `reset()` quickly. This already happens — `cull()` calls `reset()` on any mismatch. The predictor will naturally suppress echoes until the alt-grid state stabilises.

### Client Physical Grid Reset

When the client receives a diff whose content differs significantly from `physical` (e.g. vim startup clearing the screen), `emit_diff` will emit the minimum needed ANSI to catch up. For a buffer switch, this typically means a full-screen repaint within one or two datagram ticks. The `emit_connect_clear` mechanism (writing `\x1b[2J\x1b[H`) is the sanctioned way to get a known-clean terminal state; that can be called at alt-screen-exit if the client detects a full-screen transition. However this requires the client to know a buffer switch occurred, which today it does not. A simpler approach: the diff convergence within 1-2 RTTs is acceptable for M5; explicit alt-screen notification can be a follow-on if the repaint seam is visible.

---

## Repaint Pacing — Designing Out the Two Known Traps

### Background

Currently `server.rs` emits at most one `StateDiff` datagram per 16 ms tick in the `diff_interval` arm. A full 80×24 screen (~13 MTUs) takes ~200 ms × RTT to converge. The fix is to burst multiple datagrams per tick until the pending-deferred queue drains or the QUIC send buffer is exhausted.

The 999.4 revert proved two failure modes that must be designed in, not discovered at runtime.

### Trap 1: Infinite Spin from Recomputing `fresh_runs`

**Root cause (confirmed by reading `build_state_diff`):** `fresh_runs` is computed by `compute_diff_runs(&cells, last_acked_snapshot)` inside `build_state_diff`. During a burst loop, `last_acked_snapshot` does NOT advance (epoch acks are processed in the separate `datagram` arm of the outer `select!`, which does not run while the `diff_interval` arm is executing synchronously). Each burst iteration therefore recomputes an identical `fresh_runs` from the same baseline, repopulates `pending_deferred`, and the loop never drains.

**Architecture that avoids it:** Do not call `build_state_diff` more than once per tick for the same screen state. Instead, restructure the burst as a drain loop *inside* the `diff_interval` arm that only calls `encode_datagram` repeatedly on the already-computed `all_runs` from the single `build_state_diff` call:

```
diff_interval arm:
  1. Call build_state_diff ONCE → get (first_payload, deferred, epoch, sent_cells).
  2. Send first_payload.
  3. While deferred is non-empty AND datagram_send_buffer_space() > cap:
       a. Call encode_datagram(&StateDiff { epoch, runs: deferred, ... }, cap)
          → (next_payload, next_deferred)
       b. Send next_payload.
       c. deferred = next_deferred.
  4. pending_deferred = deferred (carry remainder to next tick).
```

`fresh_runs` is computed exactly once per tick regardless of burst depth. The burst drains `deferred` only — it does not re-diff against `last_acked_snapshot`. This is correct: `deferred` already contains the runs that did not fit, sorted cursor-first by the prior `encode_datagram` call. Re-sorting is not needed because deferred runs are already in priority order.

**Epoch on burst datagrams:** All burst datagrams within a single tick share the SAME epoch — the one incremented at the top of `build_state_diff` on that tick. The client's `apply()` uses monotonic epoch (`diff.epoch <= last_applied_epoch` discards older diffs). With all burst datagrams sharing the same epoch, only the first one per tick advances `last_applied_epoch`; subsequent ones in the burst are still applied because `apply()` checks `<=` (strictly less than or equal), not `<`. Wait — actually `apply()` at line 213 reads `if diff.epoch <= self.last_applied_epoch { return; }` which means equal epoch is DISCARDED. This means burst datagrams sharing an epoch would be silently dropped after the first one.

The correct fix: encode each burst datagram with a **distinct epoch** — epoch, epoch+1, epoch+2, ... within the burst — but all computed from the same `cells` snapshot (no re-diff). Increment `current_epoch` once per burst datagram, not once per tick. BUT this re-introduces the noecho-epoch trap (Trap 2 below). The resolution is in Trap 2.

Alternatively: do not reuse `build_state_diff` for burst datagrams at all. After the first `build_state_diff` call gives `(payload, deferred, epoch, sent_cells)`, the burst sends the deferred chunks using `encode_datagram` directly with `epoch + burst_index`. The burst loop only calls `encode_datagram`, not `build_state_diff`.

### Trap 2: Noecho-Epoch Security Interaction

**Root cause:** The 999.4 implementation incremented `current_epoch` once per burst datagram. The client's `cull()` in `PredictionOverlay` uses `confirmed_epoch` (advanced by the predictor when a non-trivial correct prediction lands). The predictor's `cull()` is driven by `screen.last_applied_epoch()`. If burst datagrams each carry a different epoch, the client applies them in order and `last_applied_epoch` advances rapidly. During a `read -s` window, `confirmed_epoch` should NOT advance (the server doesn't echo typed chars, so the predictor's `cull()` finds mismatches and stays reset). But `last_applied_epoch` advancing from the rapidly arriving burst datagrams means the predictor sees `epoch > epoch_required` for predictions it made at the old epoch — and `cull()` evaluates them as IncorrectOrExpired (the server confirmed a different cell content). This causes `confirmed_epoch` to advance via `CorrectNoCredit` paths even during noecho, breaking `noecho_read_dash_s_zero_predicted_chars`.

**Architecture that avoids it — ONE epoch per tick:**

All burst datagrams within a single tick share the SAME epoch value. This requires changing the client's `apply()` monotonic guard from `<=` to `<`:

```rust
// In ClientScreen::apply():
if diff.epoch < self.last_applied_epoch {  // changed from <=
    return;
}
```

With `<` instead of `<=`, burst datagrams sharing an epoch are all applied (each arriving burst datagram applies its `runs` patch to the confirmed grid). The `last_applied_epoch` is set to the shared epoch once (after the first burst datagram applies), and subsequent same-epoch datagrams still pass the guard and apply their runs. The `physical` grid is updated incrementally by `render_to_stdout` calls interleaved with burst arrivals (since datagrams are processed in the `read_datagram` arm of the client's `select!` loop, not synchronously with server sends).

This preserves the noecho invariant: during a `read -s` window, the epoch increments once per tick (not per burst datagram), and the predictor's `confirmed_epoch` path through `cull()` is unchanged — it still evaluates char predictions against the confirmed grid content, and if server echoes nothing, `confirmed_epoch` stays frozen.

**Mandatory test:** `noecho_read_dash_s_zero_predicted_chars` in `crates/nosh-client/tests/predict.rs` must pass without modification. The `<` guard change in `apply()` must be tested for correctness: same-epoch burst datagrams must all apply their runs.

### Budget Gate: `datagram_send_buffer_space()`

`quinn::Connection::datagram_send_buffer_space()` (confirmed public on quinn 0.11.9) returns the number of bytes available in the datagram send buffer (1 MiB default). The burst loop checks this before each additional burst datagram:

```rust
while !deferred.is_empty() {
    let space = conn.datagram_send_buffer_space();
    if space < cap {
        break; // no room for another MTU-sized datagram
    }
    let (next_payload, next_deferred) = encode_datagram(&burst_diff, cap)?;
    conn.send_datagram(next_payload)?;
    deferred = next_deferred;
}
```

This is the correct bound. QUIC datagrams are not flow-controlled by the peer (no ack-gating), so `datagram_send_buffer_space()` reflects local send-buffer availability only. The 1 MiB buffer holds ~700 MTU-sized datagrams; for a full 80×24 screen (~13 MTUs) the burst will drain in a single tick with room to spare. Congestion control still applies at the QUIC layer — quinn will pace datagram sends into the network automatically.

**Fixed-count fallback:** If `datagram_send_buffer_space()` is ever unavailable (e.g. future quinn API change), a fixed burst cap of 16 datagrams per tick (16 × 1200 bytes = ~19 KB) is a safe fallback that covers full-screen repaints without risking unbounded loops.

---

## Suggested Build Order

The four features have the following dependency graph:

- **Alt-screen buffer** (`terminal.rs` change) must precede repaint pacing, because accurate diff output for full-screen TUIs requires a correct buffer model. If repaint pacing ships first on a broken alt-screen model, you accelerate delivery of garbled content.
- **Repaint pacing** depends on alt-screen being correct (the burst is most valuable for full-screen TUI transitions), and it depends on the `apply()` monotonic guard change in `screen.rs`. No dependency on scrollback or channel mux.
- **Channel mux** is a foundation for scrollback (scrollback channel rides the mux stream). Channel mux does NOT depend on alt-screen or repaint pacing.
- **Scrollback** depends on channel mux (uses the new stream type) and on the alt-screen scrollback suppression gate (otherwise scrollback fills with vim's alt-screen output).

**Recommended phase order:**

### Phase A: Alternate-Screen Buffer (`terminal.rs` + `server.rs` epoch handling)

Scope: Add `alt_grid: Vec<Vec<Cell>>` and `saved_cursor: CursorPos` to `TerminalState`. Replace the `?1049h`/`?1049l` no-op with real swap/clear/restore logic. Update `resize()` to size both grids. Add `alt_screen` suppression gate to `scroll_up()`. No changes to the wire protocol or client.

Why first: this is a pure server-side, in-process terminal model change. It has tests (`decset_alt_screen_toggled_by_1049` already exists; add `alt_screen_grid_is_separate_from_primary`). No network or client changes. Completing it before repaint pacing means the burst delivers correct content.

**Invariants:** `viewport_rows()` always reads `self.grid` (the active buffer) — unchanged. `build_state_diff` unchanged. `SequencedOutputBuffer` unchanged. `PtyData` on the wire carries raw bytes (including the `?1049h` sequence) for replay — the terminal model re-derives the correct state on reattach by re-driving `advance()`.

### Phase B: Repaint Pacing (burst datagrams per tick)

Scope: Restructure the `diff_interval` arm in both `run_session` and `run_reattach_session` in `server.rs`. Change `apply()` monotonic guard in `screen.rs` from `<=` to `<`. Add burst drain loop using `datagram_send_buffer_space()`. One epoch per tick shared across all burst datagrams.

Why second: alt-screen is fixed, so burst delivery is useful. The `apply()` guard change is low-risk. The two known traps are designed out architecturally, not discovered at runtime.

**Mandatory gates before merge:** `noecho_read_dash_s_zero_predicted_chars` passes; add `burst_drains_when_grid_differs_from_acked_baseline` (was reverted in 999.4 — re-add it, fails before fix, passes after); confirm `auth.rs` integration tests pass.

### Phase C: Channel Multiplexing Foundation

Scope: Add `ChannelOpen`, `ChannelAccept`, `ChannelReject` variants to `Message` (appended after `TerminalControl`, discriminant 11+). Add secondary stream accept loop on the server; add control-channel open logic on the client. No scrollback-specific logic yet — just the mux plumbing. Scrollback stream type is declared but not yet used.

Why third: scrollback needs this foundation. Mux does not depend on alt-screen or burst pacing. Building mux before scrollback lets the mux layer be tested with a simple echo channel before adding scrollback complexity.

**Reattach impact:** Secondary streams are re-opened by the client after `ResumeComplete` is received. The primary stream reattach protocol (`Reattach`/`ReattachOk`/replay) is unchanged. `SequencedOutputBuffer` is unchanged.

**Migration impact:** QUIC connection migration carries all open streams automatically at the transport layer. No application-layer handling needed for secondary streams during migration.

### Phase D: Scrollback Sync

Scope: Add `ScrollbackRequest` and `ScrollbackPage` variants to `Message` (appended, discriminant 14+). Add `alt_screen` gate to `scroll_up()` in `terminal.rs` (if not done in Phase A). Add scrollback page-query handler to `server.rs` (reads `TerminalState.scrollback`, encodes as `Vec<Vec<DiffRun>>`). Add scrollback client UI and rendering path.

Why last: depends on mux (Phase C) and alt-screen suppression gate (Phase A). The `TerminalState.scrollback` field and cap already exist; this phase adds the query API and wire encoding. Client rendering is a new display mode (separate from `ClientScreen.confirmed`); the confirmed grid and predictor are untouched.

---

## Invariants That Must Not Break

| Invariant | Where Enforced | Risk in v1.3 |
|-----------|---------------|-------------|
| Discriminant ordering in `Message` — NEVER reorder or insert, only append | `nosh-proto/src/messages.rs`, test `variant_name_never_leaks_token_bytes` | Every phase adds new variants; must append only |
| `PtyData` advances `highest_applied` in `SequencedOutputBuffer` | `crates/nosh-server/src/registry.rs`, `push_output_and_parse` | No phase touches `SequencedOutputBuffer` directly |
| Keystrokes travel only on the reliable stream, never datagrams | `server.rs` `msg` arm: `PtyData { data }` → `in_tx.send` | Mux phase must not accidentally route keystrokes to secondary streams |
| Noecho suppression: `confirmed_epoch` must not advance during `read -s` | `predictor.rs` `cull()` state machine; `noecho_read_dash_s_zero_predicted_chars` test | Repaint pacing `apply()` guard change (`<=` → `<`) must not break this |
| One epoch per tick (shared across burst datagrams) | New invariant introduced in Phase B | Every burst datagram in a single tick must carry the same epoch value |
| `build_state_diff` called at most once per tick | New invariant introduced in Phase B burst redesign | The burst loop must call only `encode_datagram`, never `build_state_diff`, after the first call |
| `datagram_send_buffer_space()` is the burst budget gate | Phase B | Do not use a fixed byte count that ignores actual buffer state |
| `SSH_AUTH_SOCK` never forwarded via environment | `session.rs` env sanitization | No new channel type should forward the agent socket path |
| Scrollback suppressed in alt-screen | Phase A / Phase D | `scroll_up()` must gate on `echo_state.alt_screen`; otherwise vim output contaminates primary scrollback |
| `ReattachErr` must remain fieldless (no-oracle invariant) | `messages.rs` comment, `Message::ReattachErr` variant | Channel mux must not add a reason field to `ReattachErr` |
| Pre-auth connection cap | `server.rs` `AuthLimits` semaphore | Secondary stream accept must not bypass the cap |
| `alt_grid` sized same as `grid` after every resize | Phase A `resize()` | Both grids must track the same `(cols, rows)` at all times |

---

## Sources

- `/home/bharris/github.com/bharrisau/nosh/crates/nosh-proto/src/messages.rs` — discriminant ordering, current 10 variants, append-only invariant documented at lines 56–62
- `/home/bharris/github.com/bharrisau/nosh/crates/nosh-proto/src/datagram.rs` — `encode_datagram`, `decode_datagram`, `StateDiff`, `ClientEpoch` wire types
- `/home/bharris/github.com/bharrisau/nosh/crates/nosh-server/src/server.rs` — `build_state_diff` (lines 294–360), one-datagram-per-tick `diff_interval` arm (lines 676–713), epoch-ack arm (lines 717–745), `SequencedOutputBuffer` integration, `run_session` / `run_reattach_session` full pump loops
- `/home/bharris/github.com/bharrisau/nosh/crates/nosh-server/src/terminal.rs` — `TerminalState` struct (lines 161–185), `alt_screen` no-op at lines 503–505, `scrollback` field (line 170, `VecDeque<Vec<Cell>>`), `scroll_up()` (lines 291–302), `SCROLLBACK_LINE_CAP = 10_000`
- `/home/bharris/github.com/bharrisau/nosh/crates/nosh-client/src/screen.rs` — `ClientScreen.apply()` monotonic guard at line 213 (`diff.epoch <= self.last_applied_epoch`), `emit_diff`, `render_with_predictor`, `emit_connect_clear`
- `/home/bharris/github.com/bharrisau/nosh/crates/nosh-client/src/predictor.rs` — `PredictionOverlay`, `cull()` mismatch/reset path, `confirmed_epoch` advancement logic, `noecho_read_dash_s_zero_predicted_chars` test reference
- `/home/bharris/github.com/bharrisau/nosh/.planning/ROADMAP.md` — Phase 999.5 (alt-screen investigation, line 102–118), Phase 999.6 (burst pacing with both trap descriptions, lines 111–118), Phase 999.4 (revert lessons, lines 98–99)
- `/home/bharris/github.com/bharrisau/nosh/.planning/PROJECT.md` — v1.3 M5 scope and context
