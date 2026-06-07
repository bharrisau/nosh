# Research Summary: nosh v1.3 (M5) — Channel Multiplexing, Scrollback Sync & TUI Rendering Correctness

v1.3 fixes two broken behaviours (alt-screen is a no-op flag; repaint pacing was reverted after two bugs) and adds two new capabilities (channel multiplexing and native scrollback sync) that together close the usability gap with tmux for full-screen TUI apps and large-terminal sessions. The four features interact at exactly two seams: (a) the alt-screen state gates whether lines pushed off the viewport enter primary scrollback, so the alt-screen fix must land before the scrollback channel can be correct; and (b) the repaint-pacing epoch cadence is a security invariant the predictor depends on, so the one-epoch-per-tick rule is non-negotiable and was the root cause of the 999.4 revert. Everything else is cleanly separable. No new transport infrastructure is required — quinn 0.11.9 already exposes native multistream and all needed datagram APIs. The one new workspace dependency is `unicode-segmentation 1.12.0`.

---

## Recommended Build Order

The three researchers who analysed dependencies (FEATURES, ARCHITECTURE, PITFALLS) all converge on the same sequence. The STACK researcher listed mux first, reflecting its role as a protocol foundation, but did not account for the alt-screen → scrollback correctness dependency. The reconciled order is:

**1. Alt-screen buffer (Phase A) — first**

Self-contained change inside `nosh-server/src/terminal.rs`. No wire-format or client changes. Must be atomic: save+swap+clear on `?1049h`, restore+swap on `?1049l`, resize both grids. The scrollback suppression gate (`echo_state.alt_screen` check in `scroll_up()`) lives here too. Completing this before repaint pacing ensures the burst delivers correct content from day one rather than accelerating delivery of a garbled alt-screen.

**2. Repaint pacing (Phase B) — second**

Depends on alt-screen being correct (the most visible pacing payoff is vim/htop startup). Does NOT depend on mux or scrollback. Requires the `ClientScreen::apply()` monotonic guard change from `<=` to `<` (so same-epoch burst datagrams all apply). One epoch is assigned at the start of each tick; the burst drain loop calls `encode_datagram` only — never `build_state_diff` a second time. The noecho gate (`noecho_read_dash_s_zero_predicted_chars`) is a required passing CI test before merge.

Alt-screen and repaint do not gate each other. The ordering A→B means the burst lands on a correct grid from day one; reversing it would cause the burst to accelerate delivery of residual primary-buffer content into what should be a cleared alt canvas.

**3. Channel mux foundation (Phase C) — third**

Greenfield. Adds `ChannelOpen`/`ChannelAccept`/`ChannelReject` variants to the `Message` enum (appended after `TerminalControl`, discriminant 11+) and a second stream accept loop. The discriminant-stability enforcement test must be the first commit of this phase. Scrollback declares a channel type but does not implement it yet. Mux does not depend on alt-screen or repaint.

**4. Scrollback sync (Phase D) — last**

Depends on mux (for the scrollback stream) and alt-screen (for the suppression gate). Server-side `TerminalState.scrollback` already exists; this phase adds the sequence-number index, wire types, server handler, and client scrollback-viewport mode. Must run on a separate tokio task (not inline in the pump) to avoid head-of-line blocking.

---

## Load-Bearing Decisions

All four researchers agree on every item below.

| Decision | Agreed position | Consequence |
|----------|----------------|-------------|
| **New quinn streams per channel, NOT in-stream framing** | Unanimous. QUIC streams are cheap to open and give independent HOL domains. In-stream mux (SSH-connection-style) would couple the reattach replay stream with new channel state and reintroduce application-level HOL blocking. | The control channel is the first bidi stream; PTY data moves to a second bidi stream; scrollback gets its own bidi stream. |
| **QUIC stream flow control is uniform/endpoint-wide — scrollback needs an app-level credit protocol** | Unanimous. Quinn `TransportConfig.stream_receive_window` is a static setting applying to ALL streams; there is no per-stream-instance setter. A slow scrollback consumer can fill the connection's aggregate receive window without a credit protocol. | Scrollback channel carries `ScrollbackCredit { lines: u32 }` client→server to pace delivery. Shell output stream needs no app-level flow control. |
| **Alt-screen = add `alt_grid` + `saved_cursor` to `TerminalState`; termwiz `Surface` is NOT an alt-screen abstraction** | Unanimous. `termwiz::surface::Surface` is a compositing/diff surface with no `?1049h`/`?1049l` semantics. The fix is a second `Vec<Vec<Cell>>` field in `TerminalState`, swapped in-place on mode transitions. | `viewport_rows()` always reads `self.grid` (the active buffer) unchanged; `build_state_diff` requires no change. Both grids must be resized together. |
| **`Cell.ch` must change from `char` to `String`; add `unicode-segmentation`** | Unanimous. Multi-codepoint grapheme clusters (emoji + variation selectors, ZWJ sequences) cannot fit in a `char`. `DiffRun.chars: String` in nosh-proto already uses the correct representation — aligning `Cell.ch` to `String` makes the server→proto diff path zero-copy. | Breaking change to `Cell`. Add `unicode-segmentation = "1.12"`. Cluster splitting: `unicode_segmentation::UnicodeSegmentation::graphemes()`; width per cluster: `termwiz::cell::grapheme_column_width()` (already transitive via portable-pty). |
| **Repaint pacing: one epoch per tick shared across burst datagrams** | Unanimous. Per-datagram epoch increment was the 999.4 noecho-epoch security regression. All burst datagrams in a single tick share the same `current_epoch`. | Client `ClientScreen::apply()` guard changes from `<=` to `<` so same-epoch datagrams are all applied rather than discarded after the first. |
| **Repaint pacing: burst loop calls `encode_datagram` only, never `build_state_diff` again** | Unanimous. Re-calling `build_state_diff` during burst drain was the 999.4 infinite-spin bug: `last_acked_snapshot` does not advance during the burst (epoch acks arrive in a separate `select!` arm), so `fresh_runs` is recomputed from an unchanged baseline every iteration, refilling `pending_deferred` faster than it drains. | Burst loop: call `build_state_diff` once → send first payload → drain `deferred` queue via `encode_datagram` while `datagram_send_buffer_space() > cap`. |
| **Scrollback never over datagrams; gate on `!alt_screen`** | Unanimous. Datagrams are loss-tolerant/unordered; a lost line leaves a silent hole in history. `scroll_up()` must check `echo_state.alt_screen` before pushing to `scrollback`. | Scrollback channel uses the reliable bidi stream + existing postcard codec. New `Message` variants appended in discriminant order. |

---

## Stack Additions

No new transport crates. One new dependency; one constraint on version use.

| Change | Detail | Verify at implementation |
|--------|--------|--------------------------|
| **Add** `unicode-segmentation = "1.12"` to workspace and `nosh-server/Cargo.toml` | Grapheme cluster iteration (UAX#29). Required alongside `unicode-width 0.2` which measures width-per-cluster but does not split strings into clusters. | Confirm 1.12.0 is current on crates.io |
| **Use** `termwiz::cell::grapheme_column_width(s, version)` for per-cluster width | More accurate than `unicode-width` for emoji and variation selectors; already in the transitive dep graph via `portable-pty`. Do NOT add termwiz as an explicit workspace dep (version-skew risk if portable-pty bumps). | Confirm `grapheme_column_width` is pub in the transitive termwiz version |
| **Do not add** termwiz as an explicit dep | `portable-pty 0.9` already pulls termwiz 0.23.3 as a transitive dep. Explicit pin risks skew. | — |
| **Do not add** any new QUIC crate | `quinn 0.11.9` covers `open_bi`/`accept_bi`/`open_uni`/`accept_uni`, `set_priority`, `datagram_send_buffer_space()`, and per-endpoint `stream_receive_window`. | Confirm `datagram_send_buffer_space()` pub surface on 0.11.9 |
| **Append** new `Message` variants after `TerminalControl` (current discriminant 10) | postcard encodes by source-order index. Inserting anywhere else silently corrupts the wire format. New discriminants 11+: channel control; 14+: scrollback (exact numbering TBD at implementation). | Discriminant-stability test must be added as Phase C's first commit |

---

## Feature Scope at a Glance

| Feature area | Table stakes (must ship) | Done looks like |
|--------------|--------------------------|-----------------|
| **Alt-screen buffer** | Real two-grid `TerminalState` with save+swap+clear on `?1049h`, restore on `?1049l`; `resize()` updates both grids; `scroll_up()` gated on `!alt_screen`; predictor reset on alt-screen enter | `vim --noplugin -c q` over nosh: blank canvas on open, primary content restored on exit; no predictor overlay inside vim |
| **Repaint pacing** | Burst drain loop in `diff_interval` arm; one epoch per tick; `encode_datagram`-only drain; `datagram_send_buffer_space()` as budget gate; `apply()` guard `<=` → `<` | Full 80×24 vim startup visible in ≤ 2 RTT at 150 ms RTT; `noecho_read_dash_s_zero_predicted_chars` and `burst_drains_when_grid_differs_from_acked_baseline` both pass CI |
| **Channel mux** | Control-first OPEN/ACCEPT/REJECT on control stream (channel 0); even/odd ID parity (client/server); per-channel app-level flow-control windows; clean half-close + full-close; TTY and SCROLLBACK types declared; PFWD/AFWD typed but rejected; channels reset on cold reattach (not replayed) | Discriminant-stability test passes; simultaneous open race test passes; PTY input latency < 5 ms during a saturated scrollback channel |
| **Scrollback sync** | Server-side sequence numbers on existing `VecDeque`; `ScrollbackRequest`/`ScrollbackPage` on dedicated reliable stream; `ScrollbackCredit` app-level flow control; paged delivery (not bulk dump); `epoch_at_snapshot` in handshake; separate tokio task for scrollback sender | User Shift-PageUp shows shell history excluding vim content; scrollback survives cold reattach; no PTY stall during scrollback fetch |

Should-have (aim for v1.3, not blocking): original column-width metadata on scrollback lines (resize reflow); scrollback-view mode disables predictor overlay.

Defer to v1.4+: port/agent forwarding channels (mux types declared and rejected; implementation is M5+); Mode 2027 grapheme clustering (wcwidth-per-codepoint is the safe v1.3 baseline); OSC 999.7 pre-filter (adjacent but explicitly deferred).

---

## Security-Critical Invariants

These constrain implementation choices across all four phases. Treat violations as blockers.

**Noecho suppression gate (`noecho_read_dash_s_zero_predicted_chars` is required CI)**
The predictor's noecho suppression is structural, not a flag: `confirmed_epoch` never advances during a `read -s` window because `cull()` always finds a mismatch. Any change to epoch cadence or datagram timing must not break this. The 999.4 revert demonstrated the failure mode. This test must run as a required (non-`#[ignore]`) CI check before any datagram-timing change merges. The one-epoch-per-tick rule is the mechanism; the `apply()` `<` guard change is required to make same-epoch burst datagrams apply without epoch advancement.

**Discriminant-order append-only (enforcement test as Phase C's first commit)**
postcard encodes enum variants by source-order index. Inserting a variant before an existing one silently shifts all subsequent discriminants, corrupting the wire format for every deployed connection. The existing code has two "append-only" comments in `messages.rs` from prior near-misses. A `#[test] fn message_discriminant_order_is_stable()` test must be written as the very first commit of the mux phase. This also covers SEC-3: `ChannelOpen` misread as `ReattachErr` by an old client would be a protocol correctness failure.

**Scrollback cap + channel send-buffer bound (post-auth OOM)**
`SCROLLBACK_LINE_CAP = 10_000` is the per-session cap. The scrollback sync channel adds a second buffer (in-flight send buffer on the QUIC stream). The `ScrollbackCredit` flow-control protocol is the mechanism; the scrollback sender task must use a bounded `mpsc::channel` and drop oldest lines rather than blocking the pump if the consumer is slow. Do not raise `SCROLLBACK_LINE_CAP` without a measured reason.

**OSC 999.7 (adjacent, deferred, must not be blocked)**
`vte` accumulates OSC bytes in an unbounded `Vec<u8>` until the terminator arrives; application-level caps fire only in `osc_dispatch`, after vte has already allocated the full buffer. An `ESC]52;c;<100MB>` sequence allocates 100 MB server-side per authenticated session. Phase 999.7 is deferred from M5. M5 terminal-model work must not structurally block the future OSC pre-filter. If M5 ships without 999.7, update `docs/999.1-SECURITY.md`.

**Pre-auth connection cap not bypassed by secondary streams**
The secondary stream accept loop added in Phase C must not open new streams before authentication completes and must not circumvent the `AuthLimits` semaphore.

**`SSH_AUTH_SOCK` never forwarded via environment**
No new M5 channel type should forward the agent socket path via the environment. Agent forwarding is a future dedicated channel.

---

## Watch Out For

**R-1 / R-2 (infinite-spin and noecho-epoch) — the 999.4 revert, documented root causes**
Both bugs must be designed out architecturally before the first burst line ships. Write `burst_drains_when_grid_differs_from_acked_baseline` as a RED-before-fix test. Run `noecho_read_dash_s_zero_predicted_chars` as a required CI gate. These are not hypothetical — they already caused a production revert in this codebase.

**A-1 / A-6 (half-built alt-screen is worse than the no-op)**
A partial implementation (swaps grids but doesn't restore cursor on `?1049l`) is harder to diagnose than the current uniformly wrong behaviour. Alt-screen must be atomic: save+swap+clear on enter, restore on exit, both grids resized together — all in one phase.

**M-1 / SEC-3 (discriminant shift silently corrupts wire format)**
Inserting a new `Message` variant anywhere except after the last existing one compiles cleanly but corrupts all deployed connections. Write the enforcement test first.

**M-5 (channels are ephemeral per-connection, not replayed on cold reattach)**
`SequencedOutputBuffer` replays raw `PtyData` bytes, not `ChannelOpen`/`ChannelAccept` frames. After cold reattach all channel state is reset; the client re-opens channels after `ReattachOk`. Distinct from QUIC migration, where all open streams survive transparently.

**M-6 / S-4 (scrollback sender must be a separate tokio task)**
Scrollback channel writes inside the main pump's `select!` loop will stall PTY output and datagram ticks when the client is slow. Use a dedicated task + bounded `mpsc::channel`.

**A-2 / A-3 (wide-char column drift and ZWJ non-advancement)**
`print_char()` currently advances `cursor.col` by 1 unconditionally. A CJK wide char (width 2) must advance by 2 and write a placeholder at `col + 1`. A ZWJ or combining mark (width 0) must not advance the cursor at all. Both require unit tests.

---

## Open Questions for Requirements

1. **Wire-format versioning for `StateDiff` width field.** Adding `width: u8` to cell runs is a breaking change for v1.2 clients. Options: version field in `StateDiff` header; treat as a milestone-level breaking change; width optional/defaulted. Decision needed before Phase A.

2. **Channel re-establishment on cold reattach.** Channels are ephemeral per-connection and are not replayed. On cold reattach, who initiates re-opening the TTY and scrollback channels — client or server? At what scrollback sequence number does the client resume? RFC 4254 and quicshell both assume a fresh connection; nosh's session-persistence model requires a defined answer. Suggested: client sends `ChannelOpen { type: ScrollbackSync, resume_seq: u64 }` after `ReattachOk`.

3. **Keystrokes typed while in scrollback view.** Options: (a) buffered and delivered on return to live view; (b) immediately return to live view; (c) discarded with notice. Mosh/tmux use (b); ET uses (a). Affects the client-side scrollback state machine.

4. **Scrollback client UI trigger.** Key binding for scrollback view: Shift-PageUp (tmux-compatible), configurable, or something else? Affects key handling in `client.rs` and onboarding documentation.

5. **Whether 999.7 OSC pre-filter rides this milestone.** The change is targeted and bounded; deferring past M5 leaves an authenticated-user OOM vector open. Requirements should decide: include in M5, or update the security doc and defer.

6. **Scrollback reflow on resize.** `TerminalState::resize()` preserves scrollback lines at original column count. Requirements should specify whether per-line column-width metadata in `ScrollbackPage` is mandatory for v1.3 or deferred (Pitfall S-3).

7. **`epoch_at_snapshot` synchronisation contract.** The scrollback/live-grid consistency race (Pitfall S-5) requires an `epoch_at_snapshot` field in the scrollback response so the client knows when to transition from scrollback-replay to live-grid rendering. Requirements should specify this as mandatory in `ScrollbackPage` (or a `ScrollbackBegin` framing message).

---

## Pointers

| Topic | File | Key sections |
|-------|------|--------------|
| quinn 0.11.9 API surface (open_bi, accept_bi, set_priority, datagram_send_buffer_space, TransportConfig) | STACK.md | "quinn 0.11 Multistream and Flow Control" |
| unicode-segmentation + grapheme_column_width pattern | STACK.md | "Alternate-Screen Buffer & Unicode Width" |
| termwiz Surface is not an alt-screen abstraction (verified) | STACK.md | "Alternate-Screen Buffer & Unicode Width" |
| Scrollback wire format, sequence-number scheme, ScrollbackCell | STACK.md | "Scrollback Storage and Sync" |
| Message discriminant invariant, current last variant (discriminant 10) | STACK.md | "What NOT to Add" / version notes |
| Feature table-stakes / differentiators / anti-features for all four areas | FEATURES.md | Each feature's table |
| Prior art comparison (SSH, Mosh, ET, quicshell, nosh v1.2 vs v1.3 target) | FEATURES.md | "Prior Art Comparison" table |
| Datagram protocol interactions (StateDiff width field, apply() guard, burst epoch) | FEATURES.md | per-feature dependency notes |
| Exact file/line references for every touched component | ARCHITECTURE.md | "Integration Map" table |
| Burst drain architecture (one build_state_diff call per tick, encode_datagram drain loop) | ARCHITECTURE.md | "Repaint Pacing — Designing Out the Two Known Traps" |
| Alt-screen swap implementation detail (field names, resize interaction, predictor self-correction) | ARCHITECTURE.md | "Alternate-Screen Buffer in the Terminal Model" |
| Reattach and migration impact of secondary streams | ARCHITECTURE.md | "Decision: Channel Transport" |
| Full sign-off checklist (per-feature, per invariant) | PITFALLS.md | "Looks Done But Isn't" |
| All pitfalls with exact line references and prevention tests | PITFALLS.md | All sections |
| 999.4 / 999.5 / 999.6 / 999.7 ROADMAP entries (primary ground truth for reverts) | `.planning/milestones/v1.2-ROADMAP.md` (archived) | 999.x entries |

---
*Synthesized: 2026-06-07. Confidence: HIGH across all four areas — all findings are grounded in direct source-code reading and verified API docs, not assumptions.*
