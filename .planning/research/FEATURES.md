# Feature Landscape: nosh v1.3 (M5)

**Domain:** Channel multiplexing, scrollback sync, full-screen TUI rendering, repaint pacing for a QUIC remote shell
**Researched:** 2026-06-07
**Confidence:** HIGH (grounded in ROADMAP investigation notes, prior art from quicshell spec, RFC 4254, Mosh architecture, and the existing codebase)

---

## Executive Summary

v1.3 tackles four related problems. Channel multiplexing provides the architectural plumbing — a control-first OPEN/ACCEPT/REJECT handshake on a designated control stream — that makes scrollback sync (and future forwarding) a clean incremental addition rather than a protocol entanglement. Scrollback sync is the first and only M5 consumer of the mux layer; it is a fundamentally different feature from the live state-sync grid because it operates over a reliable stream, not datagrams, and is paged on request rather than pushed continuously. TUI rendering correctness fixes a broken existing behaviour (alternate-screen is currently a no-op flag, not a real buffer), which blocks daily use of vim, htop, and Claude Code over nosh. Repaint pacing eliminates the visible "painting top-down" effect during full-screen repaints by bursting multiple datagrams in a single tick — a feature that was implemented, found two bugs, and was reverted from 999.4 with documented root causes and fixes-by-design.

The four features interact at two seams: (a) the alternate-screen state gates scrollback eligibility (lines scrolled off inside an alt-screen session must not enter the primary scrollback), and (b) the repaint-pacing burst must not change epoch cadence, which is the noecho security invariant the predictor depends on. Both interactions are documented in the ROADMAP and are treated as mandatory gate conditions here.

---

## 1. Channel Multiplexing and Per-Channel Flow Control

### Table Stakes

| Feature | Why Expected | Complexity |
|---------|--------------|------------|
| Control-first OPEN/ACCEPT/REJECT on a dedicated control stream (stream id 0 by convention) | Without this, adding any new logical channel type requires a protocol version bump or side-band hack; the ROADMAP and quicshell prior art both name this as the correct primitive | Medium |
| Channel IDs with clear initiator parity (even = client-initiated, odd = server-initiated) | Prevents ID collision when both sides can open channels concurrently; quicshell spec V1 uses this rule | Low |
| Per-channel flow-control windows (initial window in ACCEPT, WINDOW_UPDATE delta messages) | Prevents a slow-consuming channel (e.g. large scrollback fetch) from stalling a faster channel (e.g. the live PTY stream) | Medium |
| Clean channel teardown: half-close (EOF) + full-close (CLOSE), with exit-status for exec channels | Applications depend on knowing when the remote end is done writing; SSH RFC 4254 §5.3 and quicshell CTRL signals define this | Low |
| Channel type registry: at minimum TTY and SCROLLBACK types defined for v1.3 | Consumers (scrollback sync) can only be wired in if the type namespace exists | Low |

### Differentiators

| Feature | Value | Complexity |
|---------|-------|------------|
| REJECT carries no descriptive payload | Prevents an attacker-observable capability enumeration oracle — a server that rejects PFWD channels reveals which forwarding types it supports; uniform opaque REJECT hides this | Low |
| Channels survive QUIC connection migration | Because nosh uses QUIC connection IDs for roaming, the same QUIC connection continues across IP changes; logical channels bound to that connection stay alive with zero extra work. Cold reattach is a different case — channels do NOT survive a full cold reattach (the connection is a new QUIC connection); the server re-opens channels as part of reattach negotiation | Medium |
| Single-epoch per QUIC connection: no per-channel handshake state machine during cold reattach | On cold reattach the single control stream is re-established over the new QUIC connection; open channels must be re-announced by the server (or deferred to client request) | Medium |

### Anti-Features

| Anti-Feature | Why Avoid | Instead |
|--------------|-----------|---------|
| Running channel OPEN/ACCEPT on the existing PTY reliable stream | Entangles session lifecycle with channel lifecycle; a rejected port-forward OPEN could block PTY output | Dedicate stream id 0 (the first accepted bidi stream) as the control channel; bind the PTY/data to a separate stream |
| Per-channel encryption keys, nonces, and rekey counters (as quicshell v1 does) | QUIC+TLS already provides per-connection encryption with forward secrecy; adding a second layer of per-channel crypto duplicates the security apparatus with no benefit in nosh's threat model | Rely on QUIC/TLS for encryption; use channel IDs for logical separation only |
| Dropping SSH-style per-channel flow control in favour of QUIC stream flow control alone | SSH/QUIC (draft-bider) drops per-channel flow control because QUIC streams have their own; however nosh uses a datagram-based channel (the state-sync channel) alongside reliable streams, and the datagram channel has no QUIC stream-level flow control. Per-channel windows on reliable streams are still needed so a large scrollback transfer does not exhaust the connection's stream send window at the expense of the control channel | Keep per-channel flow-control windows on reliable-stream channels; datagrams remain unwindowed (their loss-tolerant nature is the design) |
| Building port forwarding or agent forwarding in M5 | Out of scope; the mux layer must leave clean room for them (correct type registry, clean REJECT semantics) but must not implement them | Annotate TTY and SCROLLBACK as v1.3 types; mark PFWD/AFWD as future types that MUST be rejected by v1.3 peers |

### Expected User-Observable Behaviour

The user does not see channel multiplexing directly. The observable consequence is that a scrollback-fetch request (a client scrolling up) does not freeze the live interactive PTY stream — keystrokes continue to echo normally while the scrollback page is being transferred. The per-channel flow control window is the mechanism that prevents the scrollback stream from consuming the full QUIC connection send budget.

On QUIC connection migration (Wi-Fi to cellular), logical channels continue without interruption because the QUIC connection ID is preserved. On cold reattach, the client re-establishes the control stream, re-issues the SessionOpen (or Reattach) message, and the server opens a fresh TTY channel. Any in-progress scrollback fetch is abandoned and must be re-requested.

### Complexity and Dependencies

- Existing protocol (`nosh-proto/src/messages.rs`) multiplexes everything over a single stream; M5 introduces a second stream class (the control stream vs data streams). This is an additive change — existing `Message` variants continue on the PTY stream; new channel-management messages go on the control stream.
- The discriminant-ordering invariant on `Message` (append-only, never reorder) applies to any new message types as well.
- Per-channel flow control interacts with `datagram_send_buffer_space()` for the datagram channel only at the tick level; reliable-stream channels use QUIC stream send window already.
- Scrollback is the only M5 consumer; port/agent forwarding channels MUST be typed but MUST NOT be implemented.

---

## 2. Scrollback Sync

### Table Stakes

| Feature | Why Expected | Complexity |
|---------|--------------|------------|
| Server retains a bounded ring buffer of scrollback lines beyond the visible grid | Mosh's defining pain point is the absence of this; any Mosh successor that advertises native scrollback must have it | Low (already partially present: `TerminalState.scrollback` is a `VecDeque<Vec<Cell>>` capped at 10,000 lines) |
| Client can request a page of scrollback history (request/response over a reliable channel) | The state-sync datagram protocol only carries the live viewport; scrollback must be fetched separately, on demand, via a reliable stream | Medium |
| Scrollback page carries: line index, cells with attributes, and a "total lines available" hint | Client needs to know how far back the buffer goes to render the scroll bar and stop paging | Medium |
| Alternate-screen apps (vim, htop) do NOT pollute the primary scrollback | Lines scrolled off inside the alternate-screen grid must go into the alt-screen's own buffer (if any) or be discarded — not appended to the primary scrollback that the user sees when they scroll up after quitting the TUI app | Medium |
| History is bounded and the bound is enforced with drop-oldest semantics | Unbounded history is a server-side OOM path; 10,000 lines matching the current `SCROLLBACK_LINE_CAP` constant is reasonable and already codified | Low |

### Differentiators

| Feature | Value | Complexity |
|---------|-------|------------|
| Scrollback persists across QUIC migration | Because the session stays alive across IP change, the server-side scrollback buffer is unchanged; the client simply re-requests any page it hasn't already rendered | Low (falls out of QUIC migration design) |
| Scrollback survives cold reattach (within session lifetime) | The server-side `TerminalState` with its `scrollback` VecDeque is held in the `SessionSlot` which survives the client disconnect; on reattach the client can immediately request scrollback pages without waiting for the shell to re-emit them | Low (falls out of session persistence design) |
| Incremental paging (client requests one page at a time, not a bulk dump) | Avoids transferring megabytes of history on every scroll; the client fetches the next page only when the user scrolls far enough | Medium |

### Anti-Features

| Anti-Feature | Why Avoid | Instead |
|--------------|-----------|---------|
| Scrollback delivered over datagrams | Datagrams are unreliable (drop-on-loss) and MTU-bounded; a lost scrollback datagram is undetected and leaves a hole in the history view. Scrollback is exactly the wrong use case for datagrams — it is large, ordered, and correctness-critical | Use a dedicated reliable QUIC stream (a scrollback channel opened via the control channel mux) |
| Pushing all scrollback to the client on connect/reattach | A session with 10,000 lines of history would transfer up to ~80 MB of cell data (10k × 220 cols × 4 bytes/cell); this delays the live view and wastes bandwidth if the user never scrolls | Pull on demand: client requests pages; server responds with pages |
| Scrollback that includes alternate-screen content | vim and htop write to the alternate screen; their output (status bars, file contents) is not meaningful out of context and should not appear when the user scrolls up after quitting the app | Gate scrollback writes on `echo_state.alt_screen == false`; when `?1049h` is active, pushed-off lines go to a separate (discardable) alt-screen overflow, not the primary scrollback |
| Predicting content inside the scrollback view | The predictor operates on the live viewport cursor; it has no valid model for the scrollback buffer state and must not attempt to predict in scroll-back mode | Disable prediction (hold speculative overlay) while the client is in scrollback-view mode; resume on return to the live viewport |
| Infinite scrollback | OOM risk on long-lived sessions; tmux defaults to 2,000 lines, ghostty to 10,000 | The existing 10,000-line `SCROLLBACK_LINE_CAP` cap is the right bound |

### Expected User-Observable Behaviour

The user presses the scrollback key binding (Shift-PageUp or a configurable key). The display freezes on the current live view and begins showing historical lines above it, paged in as the user continues scrolling. Lines are rendered with correct attributes (bold, colour) matching what was on screen when they scrolled off. The live PTY continues running; keystrokes typed while in scrollback view are buffered and delivered when the user returns to the live view (or discarded with a notice — TBD, but this is a UX decision outside this feature's core correctness).

Quitting the alternate-screen app (vim, htop) and then scrolling up shows the shell's command history, not vim's file contents. This is the correct behaviour that matches what users expect from tmux or a standard terminal.

After a QUIC migration (Wi-Fi switch), the user can immediately continue scrolling — no re-request is needed because the QUIC connection (and the server session) are alive.

After a cold reattach (suspend+resume), the scrollback buffer from before the disconnect is still available and can be paged in on the next scroll request.

### Complexity and Dependencies

- The server's `TerminalState.scrollback` VecDeque already exists and is populated. The missing piece is the request/response protocol (a new message type pair on the scrollback reliable channel) and the client-side scrollback viewport rendering.
- The alt-screen gate (`echo_state.alt_screen`) is already tracked in `TerminalState`; the scrollback write path in `vte::Perform::print` (and the line-scroll path) needs to check it.
- The scrollback channel is the first non-PTY logical channel; its existence validates the mux design.
- The client-side predictor must detect when the client is in scrollback-view mode and suppress the speculative overlay. This is a new predictor mode (no interaction with `noecho` or `epoch` logic, just a separate "viewing history" flag).
- Scrollback line format can reuse the existing `Cell` struct from `nosh-server/src/terminal.rs`; a new proto type is needed to serialise `Vec<Cell>` as a scrollback page for the wire.

---

## 3. Full-Screen TUI Rendering Correctness

This feature is a bug fix of a known, documented breakage (999.5 in ROADMAP). The symptom is that full-screen TUI apps (vim, htop, Claude Code) render as garbled output with missing spaces and shifted content. The ROADMAP investigation identified two suspected root causes; this section treats those as ground truth and maps the expected behaviour around them.

### Table Stakes

| Feature | Why Expected | Complexity |
|---------|--------------|------------|
| Genuine alternate-screen buffer: a second grid that `?1049h` switches to and `?1049l` restores from | Every modern terminal emulator (xterm, iTerm2, Ghostty, WezTerm, foot) provides this. xterm's ctlseqs define `?1049` as: save cursor (DECSC), switch to alternate buffer, clear it. `?1049l` restores the primary buffer and cursor. The current `nosh-server/src/terminal.rs:504` assigns `self.echo_state.alt_screen = enable` with no grid swap — a no-op that leaves the TUI writing into the primary grid | High |
| Clear the alternate-screen grid on entry (`?1049h`) | The xterm spec says the alternate buffer is "cleared first" when entered via mode 1049. A TUI that assumes a blank canvas will render on top of whatever was in the primary grid if this is not done | Medium (follows from having a real alt grid) |
| Restore the primary grid and cursor on exit (`?1049l`) | Users expect to see their shell output from before the TUI ran when the TUI exits. Without a save/restore, the primary grid is overwritten by the TUI and the previous context is lost | Medium (follows from having a real alt grid) |
| Alternate-screen content is not included in the state-sync diff | The datagram state-sync sends the current grid to the client; it must send whichever grid is active — the alt grid while in alt-screen mode, the primary grid when not. The diff must be keyed to the active buffer | Medium |
| Cell-width accuracy for ASCII and common Latin characters | The most common case; the current model likely handles this correctly for the basic visible ASCII range | Low (likely already correct) |

### Differentiators

| Feature | Value | Complexity |
|---------|-------|------------|
| Correct wide-char (CJK, emoji) cell width using `wcwidth` semantics | Wide characters occupy two cells; writing a wide char at column N must mark column N+1 as occupied/blank to prevent the next character from overwriting the second half. Mitchell Hashimoto's analysis shows wcwidth-per-codepoint is the de-facto standard that shells and editors agree on | Medium |
| Grapheme cluster awareness for combining marks | Zero-width joiners and combining characters should not advance the cursor; the cell should accumulate the full grapheme | High (Mode 2027 is not universally expected yet; wcwidth-per-codepoint is the safe baseline) |
| Correct handling at column boundaries for wide chars | If a wide char starts at the last column, it must either wrap or be clipped — not write a half-char that shifts subsequent columns | Medium |

### Anti-Features

| Anti-Feature | Why Avoid | Instead |
|--------------|-----------|---------|
| Mode 2027 grapheme clustering as the default | Applications and shells still largely assume wcwidth-per-codepoint; implementing mode 2027 by default causes misalignment with apps that don't opt in | Implement wcwidth-per-codepoint as the baseline; Mode 2027 can be a future opt-in |
| Scrollback lines from the alternate-screen grid | As noted in feature 2, alt-screen lines must not enter primary scrollback | Gate scrollback writes on `alt_screen == false` |
| Re-implementing a full terminal emulator | nosh is a remote shell, not an emulator. The server-side terminal state model needs to be authoritative enough that the client can render the correct screen; it does not need to render fonts, handle mouse reporting beyond passthrough, or implement sixel graphics | Extend the existing `TerminalState` model with a second grid and correct wide-char width tracking; stay within the declared scope fence in `terminal.rs` |
| Trying to predict inside full-screen TUI apps | Mosh explicitly notes that the predictive engine avoids predicting in full-screen apps because the cursor semantics are complex and wrong predictions are visually jarring | The existing `EchoState.alt_screen` flag is the gate; confirm the predictor checks it before allowing speculative overlay |

### Expected User-Observable Behaviour

The user runs `vim file.txt` over nosh. The terminal clears to a blank canvas (not overwritten on top of the previous shell output). vim's interface renders correctly: status bar at the bottom, file content in the body, cursor at the correct position. Characters with box-drawing glyphs (e.g. window borders in htop) are correctly single-width. CJK and emoji characters occupy two columns without shifting subsequent text.

On `:q` from vim, the shell prompt and previous command output reappear exactly as they were before vim was launched. The scrollback history from the shell session is available to scroll through; vim's content is not in it.

Claude Code's TUI (which uses box-drawing glyphs and ANSI colour extensively) renders without the "spaces missing, screen garbled" symptom. The investigation-first approach mandated by the ROADMAP (reproduce on Linux client↔server before fixing) is the correct first step.

### Complexity and Dependencies

- The highest-complexity item is adding a second grid to `TerminalState` and routing all `vte::Perform` callbacks through `self.active_grid()` (a method that returns either `&mut self.grid` or `&mut self.alt_grid` depending on `self.echo_state.alt_screen`). All existing `print`, `csi_dispatch`, and `execute` handlers that index `self.grid` directly must be updated.
- The cursor save/restore required by `?1049` (DECSC/DECRC) adds a `saved_cursor: CursorPos` field alongside a `saved_alt_cursor: CursorPos`.
- The datagram sender (Phase 13: `build_state_diff`) must be updated to diff against the active grid, not always `self.grid`. This is likely a one-line change if the grid accessor is clean.
- Wide-char tracking adds a `width: u8` field to `Cell` (1 or 2) and a "continuation" marker for the right half of a wide char. The diff encoding in `nosh-proto/src/datagram.rs` must also carry width information so the client renders correctly.
- The client `screen.rs` `emit_diff` must handle continuation cells (skip the second half; the first half's width tells the renderer to advance two columns).
- The predictor (`predictor.rs`) must check `echo_state.alt_screen` (available via the datagram's echo-state field, or as a separate flag in `StateDiff`) and suppress speculative overlay when in alt-screen mode. This matches Mosh's documented behaviour.
- All of this should be covered by tests that compare `TerminalState` grid output against a reference terminal for standard TUI sequences.

---

## 4. Repaint Pacing

This feature fixes a known performance deficiency documented in ROADMAP 999.4 and 999.6. The root cause, the two bugs that caused the revert, and the fixes-by-design are all documented in the ROADMAP. This section maps the feature in terms of user-observable behaviour and testable requirements.

### Table Stakes

| Feature | Why Expected | Complexity |
|---------|--------------|------------|
| Full-screen repaints (vim startup, multi-line paste) land in approximately 1 RTT | The current one-datagram-per-tick policy at 16 ms intervals means a full-screen repaint at 150 ms RTT takes ~N ticks × 16 ms — visible as a top-down progressive paint wave. Users expect the screen to appear fully formed on arrival | Medium |
| Burst multiple datagrams per tick when a large state delta is pending | The mechanism: instead of capping at one datagram per tick, drain the pending-deferred queue up to the available `datagram_send_buffer_space()` budget in a single tick | Medium |
| One epoch per tick even when bursting | The noecho security invariant (`noecho_read_dash_s_zero_predicted_chars` test) requires that `current_epoch` does not advance per-datagram within a burst. All datagrams in a single tick's burst carry the same epoch number. This was the bug in the 999.4 revert | Low (design is documented; implementation must follow it) |
| The burst loop does not recompute `fresh_runs` mid-drain | The second bug in the 999.4 revert: `build_state_diff` compared against `last_acked_snapshot` (which does not advance during a burst), causing `fresh_runs` to be refilled from a non-empty grid every iteration — infinite spin. Fix: when `pending_deferred` is non-empty, skip `fresh_runs` recomputation and drain only the deferred queue | Medium |

### Differentiators

| Feature | Value | Complexity |
|---------|-------|------------|
| Direction-ordered burst drain (top-to-bottom, consistent with cell-walk order) | The 999.4 investigation noted that the paint direction (top-down for vim startup, bottom-up for paste) is determined by `build_state_diff`'s cell-walk and deferral order. A consistent top-to-bottom order avoids visual confusion | Low |
| Congestion-aware burst cap (`datagram_send_buffer_space()` as the gate) | The ROADMAP confirms: no QUIC flow-control ceiling on datagrams (not ack-gated; send buffer is 1 MiB). The correct limiter is `datagram_send_buffer_space()`. A fixed-count fallback (e.g. max 16 datagrams per burst) is acceptable if the space check is unreliable | Low |

### Anti-Features

| Anti-Feature | Why Avoid | Instead |
|--------------|-----------|---------|
| Incrementing `current_epoch` once per datagram in a burst | The 999.4 root cause. It causes `confirmed_epoch` to advance during a `read -s` window, tripping the noecho invariant. One epoch per tick; all burst datagrams share it | One epoch assignment at the start of the tick; all datagrams in that tick's burst carry the same epoch |
| Recomputing `fresh_runs` inside the burst drain loop | The 999.4 infinite-spin bug. `last_acked_snapshot` does not change during burst (acks arrive on a different `select!` arm), so `fresh_runs` is always non-empty, the deferred queue never drains, and the pump task hangs | When `pending_deferred` is non-empty, drain it without recomputing fresh_runs |
| Reliable-stream fallback for full-screen repaints | Phase 11 deferred this strategy; the ROADMAP confirms datagrams are the correct channel (loss-tolerant; latest-state-wins). A stream fallback reintroduces HOL blocking | Stay on datagrams for state-sync; the burst policy is sufficient |
| Sleeping between burst datagrams | Adds artificial latency; the point is to deliver the full state in one RTT | Send all burst datagrams immediately within the tick; the QUIC congestion controller handles pacing |

### Expected User-Observable Behaviour

The user runs `vim` over nosh at 150 ms RTT. Instead of watching the screen paint top-down over several seconds, the entire vim interface appears fully formed within approximately 1 RTT (~150–200 ms) after the TUI is launched. Similarly, pasting 50 lines of text results in all lines appearing simultaneously (or within 1 RTT), not progressively.

The `read -s` password prompt continues to work correctly: entering a password does not cause predicted characters to appear (the noecho invariant is preserved), because the burst datagrams share the tick's epoch and do not advance `confirmed_epoch` mid-tick.

The mandatory test gates from the ROADMAP are:
- `noecho_read_dash_s_zero_predicted_chars` passes (noecho invariant)
- A `burst_drains_when_grid_differs_from_acked_baseline` test passes (fails before fix, passes after)
- Existing `auth.rs` integration tests pass

### Complexity and Dependencies

- The change is localised to the server session pump in `nosh-server/src/server.rs` (the `select!` loop around line 580 in the ROADMAP's annotation). The burst logic replaces the single-datagram-send with a drain loop.
- The epoch is assigned once at the start of the tick (before the burst loop begins), not inside the loop. This is a two-line invariant to check in code review.
- The `datagram_send_buffer_space()` method is confirmed available on `quinn::Connection` (verified against quinn docs).
- The `pending_deferred` queue is already present in the server pump from Phase 13/999.4 work; the burst loop reads it without reconstructing it.
- Dependencies: the alternate-screen feature (feature 3) must be complete first, because the repaint-pacing bug manifests most visibly on full-screen TUI repaints and the fix should be validated on a correctly rendering alt-screen grid.

---

## Prior Art Comparison

| System | Channel Multiplexing | Scrollback | Alt-Screen | Repaint Pacing |
|--------|---------------------|------------|------------|----------------|
| **SSH (RFC 4254)** | Control-first: `SSH_MSG_CHANNEL_OPEN` on the connection; server replies with CONFIRMATION or FAILURE. Channel types: session, x11, forwarded-tcpip, direct-tcpip. Per-channel flow-control windows (initial window + WINDOW_ADJUST). Channels are logical on one TCP connection | No native scrollback; client's local terminal stores the byte stream from the pipe; scroll = reading the local buffer | Byte-stream passthrough; the client terminal handles `?1049h/l` natively | Not applicable; TCP stream is continuous |
| **Mosh** | No channel multiplexing; single SSP state-sync object per connection | No scrollback; by design — SSP syncs the current terminal state only. Lines that scroll off the grid are gone from the server's state. The workaround is running inside tmux | The SSP terminal model does track alt-screen mode and suppresses prediction in full-screen apps. The predictive engine explicitly avoids predicting in contexts where cursor semantics are complex | SSP sends at 8 ms collection intervals; sends the latest state object, not individual cells. Loss-tolerant — a late/lost packet is simply superseded by the next |
| **Eternal Terminal** | No channel multiplexing as a protocol primitive; port forwarding is available but implemented outside the main session stream | Native scrollback: ET syncs raw PTY bytes over its `BackedReader`/`BackedWriter` reliable byte stream; the client's local terminal accumulates scrollback from the stream exactly as SSH does | Byte-stream passthrough to client terminal; local terminal handles alt-screen natively | Not applicable; TCP-based resumption |
| **quicshell (haukened)** | Full control-first mux: OPEN on control channel (id 0), parity rule (even = client, odd = server), ACCEPT carries `initial_window` credit, REJECT is uniform/opaque. Channel types: TTY, EXEC (v1); QFTP/PFWD reserved and must be rejected. Per-channel sequence numbers per direction. Per-channel crypto keys, nonces, rekey (nosh does not need this — QUIC/TLS covers it) | Not specified in the v1 spec | Not specified | Not specified |
| **nosh current (v1.2)** | Single reliable stream carries all `Message` variants; no logical channel abstraction | `TerminalState.scrollback` VecDeque (10,000 lines) exists server-side but is never transmitted to client | `?1049h/l` sets `echo_state.alt_screen` flag only — no second grid, no save/restore, no clear-on-enter. **This is the bug** | One state-diff datagram per 16 ms tick, MTU-capped. Burst attempted in 999.4 and reverted. **This is the deficiency** |
| **nosh target (v1.3)** | Control stream (id 0) with OPEN/ACCEPT/REJECT; TTY and SCROLLBACK channel types; per-channel flow-control windows on reliable channels | Scrollback channel (reliable stream) with request/response paging; alt-screen gate; 10,000-line cap | Two grids (primary + alt); `?1049h` = save cursor + swap to cleared alt; `?1049l` = restore cursor + swap back; diff always from active grid | Burst multiple datagrams per tick (one epoch per tick, no fresh_runs recompute mid-drain) |

---

## Dependencies on Existing Predictor and Datagram Invariants

### Predictor

- The speculative overlay must be suppressed when `echo_state.alt_screen == true`. The ROADMAP notes this is Mosh's explicit design. The existing `EchoState` struct and `StateDiff` carry this flag; the predictor must check it before applying predictions.
- Scrollback-view mode is a new predictor state: when the client is displaying historical lines (not the live viewport), no predictions should be rendered. This is independent of the noecho and alt-screen gates.
- The repaint-pacing epoch invariant (one epoch per tick) must not be broken; all burst datagrams in a tick carry the same epoch value. The `confirmed_epoch` advancement path in the predictor is the correctness dependency.

### Datagram Protocol

- The `StateDiff` datagram format (Phase 11 / `nosh-proto/src/datagram.rs`) carries `dims`, `cursor`, and cell runs. Wide-char support adds a `width: u8` field to the cell run format. This is a wire-format change and must be version-gated or treated as a breaking change relative to v1.2 clients.
- The burst sends multiple datagrams per tick, each a valid `StateDiff` with the same epoch. The client's latest-state-wins deduplication already handles receiving multiple datagrams with the same epoch; the one with the largest set of changed cells (i.e. the last one sent in the burst, which covers the full screen state) takes priority. This must be confirmed against the client's `sync.rs` logic.
- The active-grid switch (alt vs primary) means the datagram sender must reference `self.active_grid()` rather than `self.grid` directly. The diff baseline (`last_acked_snapshot`) must also be keyed to the correct buffer.

### Stream Protocol

- The control stream (stream id 0) is a new concept; the existing server.rs and client.rs open a single bidi stream for the session. M5 changes the connection setup to open (or accept) stream id 0 as the control stream first, then open/accept the PTY data stream as a second bidi stream.
- Postcard discriminant ordering: new `Message` variants for channel control (ChannelOpen, ChannelAccept, ChannelReject, ChannelData, ChannelWindowUpdate, ChannelClose) must be appended after the existing variants (current highest discriminant is `TerminalControl` at index 9).

---

## Sources

- quicshell spec (haukened/quicshell, docs/spec.md) — control-first mux, OPEN/ACCEPT/REJECT, parity rule, per-channel flow control, CTRL signals — HIGH confidence (fetched directly)
- RFC 4254 §5 (SSH Connection Protocol: Channel Mechanism) — SSH channel open/confirm/fail, flow-control windows, WINDOW_ADJUST, channel types — HIGH confidence (fetched directly from rfc-editor.org)
- draft-bider-ssh-quic-09 — SSH/QUIC design decision to drop per-channel flow control in favour of QUIC stream-level flow control; channel open/reject behaviour — MEDIUM confidence (IETF draft, fetched)
- Mosh architecture (mosh.org, SSP design) — no scrollback by design (state-sync vs byte-stream), prediction suppression in full-screen apps, 8 ms collection interval — HIGH confidence (multiple sources agree)
- Eternal Terminal (eternalterminal.dev/howitworks) — native scrollback via BackedReader/BackedWriter byte-stream model — MEDIUM confidence (official site, limited detail on scrollback protocol)
- xterm ctlseqs (invisible-island.net) — `?1047`/`?1048`/`?1049` semantics: save cursor, switch to alternate buffer, clear on enter — HIGH confidence (official specification)
- ghostty-web buffer/scrollback analysis (deepwiki.com) — primary vs alt screen separation, 10,000-line default, alt-screen has no scrollback, scrollback resets on alt-screen enter — MEDIUM confidence (secondary analysis of Ghostty implementation)
- Mitchell Hashimoto: Grapheme Clusters in Terminals — wcwidth-per-codepoint as de-facto standard, Mode 2027, drift bugs — HIGH confidence (authoritative, widely cited)
- quinn Connection docs (docs.rs/quinn/latest) — `datagram_send_buffer_space()`, `max_datagram_size()`, `send_datagram()`, `open_bi()`/`accept_bi()` — HIGH confidence (official docs, fetched)
- ROADMAP.md 999.4 and 999.6 entries — two root causes of the 999.4 revert (infinite-spin + noecho-epoch), documented fixes-by-design, mandatory gate tests — HIGH confidence (primary project artefact, treated as ground truth)
- ROADMAP.md 999.5 entry — alt-screen no-op flag as the primary suspect for TUI garbling; cell-width drift as secondary suspect; investigation-first approach mandated — HIGH confidence (primary project artefact)
- nosh-server/src/terminal.rs — current `TerminalState` struct, `EchoState`, `SCROLLBACK_LINE_CAP`, alt-screen as a flag-only no-op at line 504 — HIGH confidence (read directly from codebase)
- nosh-proto/src/messages.rs — current `Message` enum, discriminant-ordering invariant, existing `TerminalControl` at discriminant 9 — HIGH confidence (read directly from codebase)
