# Domain Pitfalls: nosh v1.3 (M5) Feature Additions

**Domain:** Adding channel multiplexing, scrollback sync, alt-screen buffer, and burst repaint-pacing to a working QUIC mobility shell with predictive echo and session persistence.
**Researched:** 2026-06-07
**Sources:** `.planning/ROADMAP.md` (999.4, 999.5, 999.6, 999.7 entries), `crates/nosh-server/src/server.rs`, `crates/nosh-server/src/terminal.rs`, `crates/nosh-client/src/predictor.rs`, `crates/nosh-proto/src/messages.rs`.

---

## Summary

Four of these features have already produced documented production failures in this specific codebase. The 999.4 burst-pacing revert was caused by two bugs introduced simultaneously: an infinite-spin from recomputing `fresh_runs` against a non-advancing `last_acked_snapshot` during burst drain, and a noecho-epoch security regression where per-datagram epoch increments advanced the client's `confirmed_epoch` during a `read -s` window. The alt-screen is currently a no-op flag (`echo_state.alt_screen = enable` only) with no saved buffer, no clear-on-enter, and no primary-buffer restore. A half-correct implementation is provably worse than the current no-op. The scrollback buffer already exists in `TerminalState` (`scrollback: VecDeque<Vec<Cell>>`, capped at `SCROLLBACK_LINE_CAP = 10_000`), but there is no sync protocol to deliver it to clients. Channel multiplexing is net-new; the current system uses exactly one bidi stream per connection with a flat `Message` enum encoded by `postcard` with discriminant-ordered variants that must never be reordered.

The pitfalls below are grounded in the actual invariants, bugs, and reverts documented in the ROADMAP 999.x entries and the source files read above. Generic Rust advice is not included.

---

## Channel Multiplexing

### Pitfall M-1: postcard discriminant shift silently corrupts the wire protocol across versions

**What goes wrong:** A new `Message` variant or a new `ChannelOpen`/`ChannelAccept`/`ChannelReject` control message is inserted anywhere except at the end of the `Message` enum (`crates/nosh-proto/src/messages.rs`). `postcard` encodes enums by their discriminant index (position in source order). Inserting a variant before an existing one shifts all following discriminants. Old clients decode the new discriminant as the wrong variant — silently, with no wire error. The current enum already has two explicit "append after X to preserve discriminant order" comments (lines 56–62, 154–160) documenting prior near-misses.

**Warning sign:** A new `Message` variant appears anywhere other than after the last existing variant (`TerminalControl`, discriminant 9). A PR that adds `ChannelOpen` between `PtyData` (1) and `Resize` (2) will compile and pass unit tests but corrupt all existing deployed connections.

**Prevention:** Add a `#[test] fn message_discriminant_order_is_stable()` test that encodes each variant by index (`postcard::to_stdvec(&msg).unwrap()[0]`) and asserts it matches the expected discriminant value hardcoded in the test. This will fail immediately if anyone reorders. Add a `// APPEND-ONLY — do NOT insert or reorder` comment directly on the enum declaration as well as on each block of new variants. For channel multiplexing, consider whether the channel framing belongs in a separate `ChannelMessage` enum on a separate QUIC stream type rather than polluting `Message` with many new variants.

**Owning phase:** The phase that defines the channel-multiplexing wire protocol (first mux phase). Test must be written before any new variants land.

---

### Pitfall M-2: flow-control deadlock between the control channel and data channels

**What goes wrong:** Channel multiplexing requires a control channel (channel id 0) to issue OPEN/ACCEPT/REJECT before any data stream is bound. If the implementation multiplexes all channels over a single QUIC bidi stream (re-using the existing one), or if the control channel back-pressure blocks while a data channel is waiting for a CREDIT frame that can only arrive on the control channel, you get a classic head-of-line deadlock: the control channel cannot progress because the read loop is blocked draining the (now-HOL-blocked) data channel.

**Warning sign:** Hanging integration test — `tokio::select!` blocks forever; one arm is stuck on `recv.read_buf` for channel data while the other arm needs a `ChannelAccept` that is behind the blocked read.

**Prevention:** Each logical channel must map to a distinct QUIC stream (or the control channel must be a separate QUIC bidi stream from the shell I/O stream). QUIC streams are independent; they do not head-of-line block each other at the QUIC layer (only at the stream-level flow-control window). Write an integration test that opens N data channels, floods channel 1 to its flow-control limit, and verifies the control channel still processes messages in under 50 ms.

**Owning phase:** Mux protocol design phase. The stream/channel topology must be decided before any implementation work.

---

### Pitfall M-3: simultaneous OPEN race — both peers assign the same channel id

**What goes wrong:** If both client and server can initiate channels and they both pick channel ids from a single shared counter, both sides can simultaneously send `ChannelOpen{id: 5}`. The receiver sees a `ChannelOpen` for an id it just sent, and neither side has a resolution rule. The connection hangs or both sides reject each other.

**Warning sign:** Intermittent test failure in concurrent-channel tests; the race only triggers when both sides open at nearly the same time.

**Prevention:** Use the SSH multiplexing convention: client-initiated channels use even ids, server-initiated channels use odd ids (or any partition that guarantees non-overlap). Each side maintains its own counter in its own namespace. Write a test that fires client-open and server-open simultaneously for the same notional resource and verifies the session survives.

**Owning phase:** Mux protocol design phase.

---

### Pitfall M-4: accept-before-open and rejected-channel resource leak

**What goes wrong:** A `ChannelAccept{id}` or `ChannelReject{id}` arrives before the corresponding `ChannelOpen{id}` has been processed (reordering within a reliable stream is impossible, but the responder could send an accept for an id the requester already closed client-side). If the state machine does not handle this gracefully, it either panics or leaves a half-open channel entry that is never cleaned up, leaking memory.

**Warning sign:** `channel_map.len()` grows monotonically in a long-running test that opens and closes many channels; or a `unwrap()` on a `HashMap::get` for an id that was already removed.

**Prevention:** The channel state machine must handle `Accept`/`Reject` for unknown ids as no-ops (or a logged warning), not as panics. Rejected channels must be removed from the pending-open map immediately on receipt of `ChannelReject`, not only when the caller calls `close()`. Write a test that sends a `ChannelReject` for a channel the initiator already closed, and asserts no panic and no entry in the channel map.

**Owning phase:** Mux protocol implementation phase.

---

### Pitfall M-5: channel state must be re-established after cold reattach — not replayed

**What goes wrong:** The existing cold-reattach protocol (Phase 6 / `run_reattach_session`) replays `PtyData` frames byte-exactly from `last_acked_seq` on one reliable stream. Channels are logical constructs on top of streams. After reattach, the replayed byte stream does not replay `ChannelOpen`/`ChannelAccept` frames — those are gone. If the client tries to restore channels from the replay, it will misparse `PtyData` frames as channel frames or open duplicate channels. If it does not, the server-side channel state is orphaned.

**Warning sign:** After a reattach, scrollback sync or agent-forward channels never respond; or the client and server have mismatched channel-id maps.

**Prevention:** On cold reattach, all channel state is reset. The client must re-open any persistent channels (e.g. scrollback sync) after `ReattachOk` is received, not during replay. The server must close all orphaned channel state when the session transitions to `Reconnecting`. Document this explicitly in the protocol spec: channels are ephemeral per-connection, not persistent across reattach. Write a test: open a scrollback channel, trigger orphan/reattach, verify the channel is re-opened and delivers scrollback from the correct offset.

**Owning phase:** Scrollback sync phase (first channel consumer).

---

### Pitfall M-6: per-channel flow-control window deadlock with scrollback sender

**What goes wrong:** Scrollback sync sends history lines over a reliable channel. The client's per-channel receive window fills up if the client cannot process lines fast enough (e.g. it is rendering or the user is at a fast-scroll boundary). The server's channel write blocks. If the write blocks inside the same `tokio::select!` arm that also handles PTY output and datagram ticks, the entire session pump stalls: PTY output backs up, the terminal model stops updating, and datagram ticks emit stale state.

**Warning sign:** Live session input latency spikes while scrollback is being sent; or the 16 ms diff tick starts missing.

**Prevention:** Scrollback channel writes must be driven from a separate tokio task, not inline in the main session pump loop. Use a bounded `mpsc::channel` between the pump and the scrollback task; if the scrollback channel's send buffer is full, drop the oldest lines rather than blocking the pump. Write a test that fills the scrollback channel to capacity and verifies PTY input latency does not spike.

**Owning phase:** Scrollback sync phase.

---

## Scrollback Sync

### Pitfall S-1: sending scrollback over datagrams violates the loss-tolerant channel assumption

**What goes wrong:** The datagram channel (RFC 9221) is explicitly loss-tolerant and latest-state-wins: the receiver always applies the newest datagram it has seen and discards older ones. Scrollback history is sequential and must not have gaps. If any scrollback line is lost in a datagram, the client receives a garbled history with missing lines and no way to detect or correct it (datagrams carry no sequence numbers in the nosh model).

**Warning sign:** Scrollback missing random lines on high-loss connections; or the client and server disagree on scrollback line count.

**Prevention:** Scrollback sync MUST go over a reliable QUIC stream (the channel multiplexing layer). This is noted as a constraint in the ROADMAP ("Scrollback must NOT go over datagrams") and is the primary motivation for building the mux layer first. Enforce this with a compile-time architectural constraint: the scrollback sender must only accept a `quinn::SendStream` (or a channel-abstracted wrapper over one), not a `quinn::Connection::send_datagram` path. Write a test that verifies scrollback lines arrive in order and without gaps under 20% simulated packet loss (possible with quinn's test utilities or a lossy UDP proxy).

**Owning phase:** Scrollback sync phase. Must be verified before any scrollback data is sent.

---

### Pitfall S-2: scrollback vs alt-screen confusion — history from the wrong buffer

**What goes wrong:** The terminal has two conceptual buffers: the primary buffer (normal scrollback-eligible content) and the alternate screen (full-screen TUI content — vim, htop — which has no scrollback by convention). If the server sends alt-screen content as scrollback, the client displays `vim`'s internal state as if it were shell output history, which is nonsensical and confusing. The current implementation has `echo_state.alt_screen` as a flag but no separate buffer; scrollback currently only collects primary-buffer lines pushed by `scroll_up()`. Once a real alt-screen is implemented (see Alt-Screen section), the scrollback accumulator must gate on `!alt_screen`.

**Warning sign:** Scrollback viewer shows vim/htop control sequences or the TUI's last-rendered state as if it were shell text.

**Prevention:** The scrollback accumulator (`scroll_up()` in `terminal.rs`) must check `self.echo_state.alt_screen` and suppress `scrollback.push_back()` when the alt screen is active. Add a test: run a sequence that activates alt screen, produces output that would scroll, deactivates alt screen, produces primary-buffer output that scrolls, and assert that only the primary-buffer lines appear in `scrollback`.

**Owning phase:** Alt-screen phase (must land before or alongside scrollback sync).

---

### Pitfall S-3: resize reflow — scrollback line widths do not match the current terminal width

**What goes wrong:** `TerminalState::resize()` (line 236) keeps scrollback lines as-is with their original column count ("Scrollback lines are kept as-is (original column count preserved)" per the comment). When the client resizes the terminal, the viewport reflows but scrollback lines retain the old width. Sending these to the client as a grid of `cols`-wide rows either truncates or pads them incorrectly in the scrollback viewer, misaligning content.

**Warning sign:** Scrollback content appears truncated or has extra trailing spaces after a resize; or a line that was 200 columns wide wraps incorrectly in an 80-column view.

**Prevention:** The scrollback sync protocol must include the original column width of each line in its metadata, or the client's scrollback renderer must handle variable-width lines explicitly. Do not assume scrollback lines match the current terminal width. Alternatively, defer scrollback reflow to the client: send raw scrollback content with per-line width metadata; let the client reflow. Add a test: write to a 200-column terminal, resize to 80 columns, and assert that scrollback lines carry the correct original width.

**Owning phase:** Scrollback sync phase.

---

### Pitfall S-4: unbounded scrollback memory — the 10,000-line cap is post-auth only

**What goes wrong:** `SCROLLBACK_LINE_CAP = 10_000` bounds the in-memory history. At 80 columns and 1 byte per cell character, that is roughly 800 KB per session. With many authenticated sessions (the server is multi-user by design), this multiplies: 100 sessions × 800 KB = 80 MB. Worse, the scrollback synced to the client over the channel consumes additional memory proportional to the un-acked send buffer on the QUIC stream. There is no cap on how much scrollback the client requests at once.

**Warning sign:** Server RSS grows proportionally to session count; OOM kill on a server with many concurrent users.

**Prevention:** (a) The `SCROLLBACK_LINE_CAP` is already a reasonable per-session cap. (b) The scrollback sync protocol must support paging/backpressure: the client requests N lines at a time via CREDIT frames (channel flow control), not the entire history in one shot. (c) The per-channel send buffer on the server side must be bounded; if the client is not consuming, the server must not buffer more than a few hundred lines in-memory pending ack. This is part of the Pitfall M-6 pump-isolation requirement. Log the per-session scrollback line count as a metric so operators can detect runaway growth.

**Owning phase:** Scrollback sync phase and mux flow-control phase.

---

### Pitfall S-5: consistency between synced scrollback and the live datagram grid

**What goes wrong:** The client has two sources of content: the reliable scrollback sync channel (historical lines) and the live datagram grid (current viewport). There is no synchronisation barrier between them. The datagram grid epoch can advance while a scrollback sync is in progress, leaving the client with scrollback that stops at line N and a live viewport that starts at row M where M < N (overlap) or M > N (gap), depending on timing.

**Warning sign:** The client displays duplicate lines or a gap between scrollback and the live viewport; this is intermittent and depends on how long scrollback sync takes.

**Prevention:** The scrollback sync message must include the epoch and grid state at which the snapshot was taken. The client must apply the historical snapshot up to (but not including) that epoch, then resume from live datagrams. This is the same class of problem as the CR-01 snapshot-at-send-time fix already in `server.rs` (the `epoch_snapshots` VecDeque). Design the scrollback sync handshake to include an `epoch_at_snapshot` field; the client gates its transition from scrollback-replay to live-grid on receiving a datagram with `epoch >= epoch_at_snapshot`.

**Owning phase:** Scrollback sync phase.

---

## Alt-Screen and Unicode

### Pitfall A-1: a half-built alt-screen buffer is worse than the current no-op

**What goes wrong:** `terminal.rs` line 505 currently handles `?1049h`/`?1049l` by setting `self.echo_state.alt_screen = enable` — no buffer swap, no save, no restore, no clear-on-enter. Full-screen TUI apps (vim, htop, Claude Code) assume that on `?1049h`: the primary-buffer content is saved, the alt screen is cleared to blank, and cursor position is reset. On `?1049l` the primary content is restored. If the implementation saves the grid in `echo_state.alt_screen = true` but does not clear it, the alt-screen grid inherits the primary-buffer content and TUIs render on top of residual shell text. If it clears on entry but does not restore on exit, the shell prompt disappears after quitting vim. Either half-state is worse than the current no-op (which at least produces consistently wrong behaviour that users recognise).

**Warning sign:** Vim opens but shows shell text bleeding through; or the shell prompt is gone after `:q`; or `echo_state.alt_screen` is `true` but `grid` still contains primary-buffer content.

**Prevention:** The alt-screen implementation is atomic: it must implement all three operations together before shipping — (a) save primary grid + cursor on `?1049h`, (b) clear alt grid to blank on `?1049h`, (c) restore primary grid + cursor on `?1049l`. The 999.5 ROADMAP entry says "investigation-first — reproduce on a Linux client↔server before fixing." Follow this exactly: do not start coding until you can reproduce the garbled rendering on Linux, so you know which path the bug actually takes. The mandatory test: run `vim --noplugin -c q` through a full server PTY, capture the datagram stream, and assert that the primary-buffer content before vim started is fully restored in the grid after vim exits.

**Owning phase:** 999.5 (full-screen TUI rendering correctness phase).

---

### Pitfall A-2: wide-char column drift — single-width cell written at a wide-char position

**What goes wrong:** CJK characters and emoji occupy two terminal columns. The server's `print_char()` (line 306) writes one cell per call and advances `cursor.col` by 1. If a wide character occupies columns 4 and 5, but `print_char()` writes it at column 4 and advances to column 5, then the character at column 5 is overwritten by the next character, causing column drift. All subsequent characters are off by one column.

**Warning sign:** htop renders misaligned bars; vim's status line has columns shifted by one; a box-drawing character renders as two overlapping glyphs.

**Prevention:** `print_char()` must be width-aware: for a `char` with `unicode_width::UnicodeWidthChar::width() == Some(2)`, write the character at `col`, write a placeholder (space or a wide-char continuation marker) at `col + 1`, and advance `cursor.col` by 2. Clamp at the right edge (if `col + 2 > cols`, wrap). Add a unit test: advance `\u{4e2d}` (a CJK wide char, width 2) and assert `cursor.col` is 2 after the write, and that `cell(0, 1)` is a placeholder.

**Owning phase:** 999.5 (unicode cell-width audit).

---

### Pitfall A-3: grapheme cluster and ZWJ sequences — one user-perceived glyph, multiple scalars

**What goes wrong:** An emoji like a family emoji (`👨‍👩‍👦`, U+1F468 ZWJ U+1F469 ZWJ U+1F466) is a sequence of Unicode scalars joined by zero-width joiners. `vte` calls `print()` once per scalar value. If `print_char()` advances the cursor after each scalar, the ZWJ scalars each occupy a cell, and the rendered glyph is fragmented across 3–5 cells instead of 2 (the display width of the base emoji).

**Warning sign:** Emoji rendered as multiple disconnected glyphs; or the cursor position diverges from what a reference terminal shows for the same content.

**Prevention:** The predictor already handles this correctly via `EpochReset` for combining marks and ZWJ (`classify_printable` returns `EpochReset` for `UnicodeWidthChar::width` returning `Some(0)` or `None`). The server-side terminal model must apply the same rule: if `UnicodeWidthChar::width(c)` returns `Some(0)`, the character is a combining mark or ZWJ — do NOT advance the cursor; either attach it to the previous cell or ignore it (the scope fence in `D-12-02b` currently ignores it via the implicit default `print()` → `print_char()` → `cursor.col += 1`, which is wrong). Add a unit test: advance ZWJ sequence bytes and assert the cursor does not advance past the base glyph's width.

**Owning phase:** 999.5 (unicode cell-width audit).

---

### Pitfall A-4: alt-screen resize — saved primary buffer has different dimensions

**What goes wrong:** The user resizes the terminal while vim (alt screen) is open. `TerminalState::resize()` resizes `grid` (the active screen) but not the saved primary buffer. When vim exits (`?1049l`), the implementation restores the old primary buffer — which is now the wrong size. The restored buffer is either truncated (rows/cols cut off) or padded with blank rows (if the terminal grew), creating a mismatched viewport.

**Warning sign:** After resizing while in vim, the shell prompt is rendered at the wrong position after exit; or content from the primary buffer bleeds into the new rows.

**Prevention:** When `resize()` is called and alt screen is active, both buffers must be resized — the active alt-screen grid AND the saved primary grid. The saved primary grid's resize follows the same logic as the main `resize()` (truncate or pad). Add a test: activate alt screen, resize from 80×24 to 80×30, deactivate alt screen, and assert primary grid dimensions are 80×30 with correctly placed content.

**Owning phase:** 999.5 (alt-screen implementation phase).

---

### Pitfall A-5: predictor predicts inside cursor-addressing apps — corrupts the screen

**What goes wrong:** The predictor is designed for shell readline prompts: it predicts character echo at a fixed cursor position. Full-screen TUI apps (vim, htop) use cursor-addressing sequences (`CSI H`, `CSI A/B/C/D`) to position text anywhere on screen. If the predictor is still active during alt-screen mode, it will predict characters at positions that the app is already managing, producing corrupted overlays (e.g., a speculative 'j' appears at row 5, col 22 in the middle of vim's buffer).

**Warning sign:** Characters appear at wrong positions in vim or htop; the predictor's overlay is visible on top of a TUI app's content.

**Prevention:** The predictor already has an `EpochReset` for escape sequences and cursor motion (see `classify_input`: any `\x1b` sequence not matching a known motion key → `EpochReset`). However, this fires per-keystroke; it does not globally disable prediction when the server signals alt-screen mode. The server already sends `?1049h`/`?1049l` via the reliable stream (as PTY data). The client must observe `echo_state.alt_screen` from the server's datagram or a dedicated signal and call `predictor.reset()` (or disable prediction entirely) on transition to alt screen. The existing `EpochReset` is not sufficient alone because the user may not type anything for a full `vte` rendering cycle. Add a test: send `?1049h` through the session, verify `predictor.pending` is empty and `should_display()` is suppressed.

**Owning phase:** 999.5 (client-side predictor integration with alt-screen state).

---

### Pitfall A-6: cursor save/restore (DECSC/DECRC) not implemented — interacts with alt-screen

**What goes wrong:** `?1049h` is specified to save the cursor position as part of the enter-alt-screen operation (equivalent to `ESC 7` DECSC before the switch). `?1049l` restores it (equivalent to `ESC 8` DECRC after the switch). If the implementation saves the primary buffer but not the cursor, the cursor after `?1049l` is at whatever position it was left at when the alt buffer exited, not where it was before vim opened. The shell prompt renders at row 0 instead of the last prompt position.

**Warning sign:** Cursor is at (0, 0) after exiting vim; shell prompt appears at the top of the screen instead of the expected position.

**Prevention:** The saved primary state must include cursor position. Implement this as a struct: `saved_primary: Option<(Vec<Vec<Cell>>, CursorPos)>`. On `?1049h`: save `(grid.clone(), cursor)`. On `?1049l`: restore both. Add a test: position cursor at (12, 40), activate alt screen, position cursor at (0, 0), deactivate alt screen, assert cursor is at (12, 40).

**Owning phase:** 999.5 (alt-screen implementation phase). Must be done together with Pitfall A-1 (atomic implementation).

---

## Repaint Pacing

### Pitfall R-1: the 999.4 infinite-spin — recomputing fresh_runs during burst drain

This is the exact failure that caused the 999.4 burst implementation to be reverted. It is documented in ROADMAP 999.6 and the ROADMAP 999.4 plan entry.

**What goes wrong:** `build_state_diff()` in `server.rs` (line 294) computes `fresh_runs = compute_diff_runs(&cells, last_acked_snapshot)` on every call. During a burst (multiple datagrams per tick), `last_acked_snapshot` does NOT advance between burst iterations — epoch-acks arrive in a different `select!` arm that does not run while the burst arm is synchronously looping. Re-merging `fresh_runs` into `all_runs` every iteration refills the deferred queue faster than it drains (deferred is prepended to fresh_runs on every call). The `pending_deferred` queue never empties. The result is a `loop` that spins, calling `build_state_diff` forever: `mutual_auth_inprocess_happy_path` hangs, and the session is effectively dead.

**Warning sign:** Integration test `mutual_auth_inprocess_happy_path` (or any live-session test) hangs indefinitely. CPU pegged at 100% on the server session task. `pending_deferred.len()` is non-zero and not decreasing.

**Prevention (from 999.6 mandatory gates):** When `pending_deferred` is non-empty (drain mode), do NOT call `compute_diff_runs` again — use only the existing `pending_deferred` contents as `all_runs`. Only call `compute_diff_runs` on the first datagram of the burst (when `pending_deferred` is empty). The mandatory regression test is named in the ROADMAP: `burst_drains_when_grid_differs_from_acked_baseline`. This test must: set up a non-empty grid vs an empty `last_acked_snapshot`, call `build_state_diff` in a simulated burst loop, and assert that `pending_deferred.len()` is 0 after at most N iterations (N = ceil(grid cells / MTU runs)). The test must FAIL before the fix and PASS after.

**Owning phase:** 999.6 (repaint pacing phase). This test must be the first thing written.

---

### Pitfall R-2: the 999.4 noecho-epoch security regression — per-datagram epoch increment

This is the second failure that caused the 999.4 revert. It is documented in the ROADMAP 999.6 entry and directly interacts with the predictor's structural noecho suppression invariant.

**What goes wrong:** The 999.4 burst implementation incremented `current_epoch` once per datagram emitted during the burst (normal: one epoch per tick). On the client, receiving a datagram with `new_epoch > last_known_epoch` causes `cull()` to run and potentially advance `confirmed_epoch`. During a `read -s` / `stty -echo` window, the server never echoes the typed characters, so `cull()` should always find a mismatch and `confirmed_epoch` should never advance. However, the per-datagram epoch increment changed the cadence: the burst sent N datagrams with N distinct epochs before the `read -s` had a chance to suppress echo. The client's `confirmed_epoch` advanced N times during the password-entry window. The literal secret characters remained suppressed (the tentative mechanism still hid them), but the confirmed-state proxy moved — which violates the invariant tested by `noecho_read_dash_s_zero_predicted_chars`.

**Warning sign:** `noecho_read_dash_s_zero_predicted_chars` integration test fails. Characters typed during `read -s` are not visible (good), but `predictor.confirmed_epoch()` has advanced past `0` (bad — means the proxy moved). On a live session: briefly visible "ghost" predictions during sudo/ssh password entry.

**Prevention (from 999.6 mandatory gates):** All datagrams in one burst share a single epoch (one epoch per tick, not per datagram). Assign `current_epoch` once at the start of the burst tick, and stamp all burst datagrams with the same epoch. The client sees repeated datagrams with the same epoch — which is fine; `cull()` already handles `epoch >= epoch_required` (not `==`) and the client's `decode_epoch_ack` path takes `max(last_acked, acked)`. The mandatory test `noecho_read_dash_s_zero_predicted_chars` MUST pass with the burst code active — it is the primary regression gate. Run it in CI as a required check, not an `#[ignore]`-gated test.

**Owning phase:** 999.6 (repaint pacing phase). Both this and R-1 must be solved together before any burst code ships.

---

### Pitfall R-3: QUIC datagram send-buffer pressure and congestion — bursting too hard

**What goes wrong:** `conn.send_datagram()` on quinn 0.11 succeeds synchronously if the send buffer has space. The 999.6 ROADMAP entry notes the confirmed mechanism: "There is no QUIC flow-control ceiling (datagrams are not ack-gated; send buffer is 1 MiB). The only limiter is the one-datagram-per-tick policy." Removing that limiter without a congestion budget means the burst can inject 1 MiB of datagrams into the QUIC send buffer in one tick, overwhelming the congestion window. QUIC will drop or delay the excess. At 150 ms RTT (the live test scenario), injecting more datagrams than the congestion window allows simply causes them to be queued, not delivered faster — the burst becomes indistinguishable from the current drip in terms of delivery time, while consuming more memory.

**Warning sign:** `datagram_send_buffer_space()` (the per-tick budget gate mentioned in the 999.6 ROADMAP) is not used; the burst loop runs until `pending_deferred` is empty regardless of network capacity. Alternatively: `send_datagram` returns `SendDatagramError::TooLarge` (impossible if `encode_datagram` uses `max_datagram_size`) but the buffer fills silently.

**Prevention:** Use `conn.datagram_send_buffer_space()` (public in quinn 0.11.9 as documented in the 999.6 ROADMAP) as the per-tick budget gate. Send burst datagrams only while `datagram_send_buffer_space() > mtu`. Do not burst more than `min(pending_deferred.len(), floor(buffer_space / mtu))` datagrams per tick. This also provides automatic fall-back to single-datagram behaviour on a congested path. If `datagram_send_buffer_space()` is unavailable or returns an unexpected type in the actual API, use a fixed conservative burst limit (e.g. 8 datagrams per tick) as a fallback.

**Owning phase:** 999.6 (repaint pacing phase).

---

### Pitfall R-4: direction artefact — top-down vs bottom-up paint order within a burst

**What goes wrong:** `compute_diff_runs()` (line 212) walks `current` row by row from top (row 0) to bottom. For a full-screen repaint, the first datagram of the burst contains the top rows; subsequent datagrams contain the bottom rows. The client applies datagrams in arrival order. The visual effect is a top-down repaint wave. The converse happens for content that scrolled: older deferred runs (which were cursor-proximate) go first, and the bottom of the screen arrives before the top — a bottom-up artefact. This was observed in the 999.4 live test ("vim startup paints top-down and a pasted multi-line block paints bottom-up in visible waves at 150 ms RTT").

**Warning sign:** A full-screen app like vim appears to "wipe in" from the top or bottom rather than appearing atomically in ~1 RTT.

**Prevention:** The direction artefact is inherent to the sequential cell-walk scan order and is not a bug — it is what happens when a repaint takes more than one MTU. Bursting multiple datagrams reduces the number of ticks over which the repaint drips, collapsing the artefact from "N ticks × RTT" to "1 tick × RTT". At 150 ms RTT and 80×24 terminal, a full repaint is ~1920 cells. With a typical 1200-byte MTU and ~10 cells per run, one datagram covers ~120 cells; 16 datagrams per burst covers the full screen in 1 tick. The artefact is acceptable if the total delivery time collapses to 1 RTT. Do not attempt to reorder runs within the burst (e.g. interleaving top/bottom) — it complicates the deferred queue without measurable benefit.

**Owning phase:** 999.6 (repaint pacing phase). Document the residual artefact in the release notes as a known behaviour, not a bug.

---

## Security-Critical Interactions

### Pitfall SEC-1: noecho suppression is structural — anything that advances confirmed_epoch breaks it

The predictor's noecho suppression (PREDICT-04) is not an explicit flag. It falls out of the epoch mechanism: when the server never echoes a character (`stty -echo` / `read -s`), `cull()` always finds a mismatch, `confirmed_epoch` never advances, and all predictions remain tentative (hidden). The invariant is proven by `noecho_suppression` (unit test) and `noecho_read_dash_s_zero_predicted_chars` (integration test).

Any change that causes `confirmed_epoch` to advance during a noecho window — regardless of whether literal secret characters are displayed — breaks this invariant. Demonstrated failure modes:
- 999.4 burst: per-datagram epoch increment → confirmed_epoch advances during `read -s` (the 999.4 revert).
- Any change that sends a datagram whose epoch the client will advance past, even for non-secret cells, when a `read -s` is in progress.

**Prevention:**
- `noecho_read_dash_s_zero_predicted_chars` is the mandatory gate. It must be run as a required (non-`#[ignore]`) test before any datagram-timing or epoch-cadence change ships.
- Never increment `current_epoch` more than once per tick. All datagrams in a burst share one epoch (R-2 above).
- Never send epoch-advancing datagrams on a separate timer or background task that runs concurrently with the main tick loop.
- When adding any new code path that calls `conn.send_datagram`, audit whether it can fire during a `read -s` window and whether the datagram it sends will cause the client to advance `confirmed_epoch`.

---

### Pitfall SEC-2: unbounded server memory from scrollback + OSC accumulation (post-auth OOM)

Two independent post-auth memory exhaustion vectors:

**Scrollback:** 10,000 lines × 80 columns × (per-cell overhead) per session. With many sessions, RSS grows proportionally. The per-session cap is reasonable; the risk is multi-session accumulation. The scrollback sync channel adds a second buffer (the in-flight send buffer on the QUIC stream). Both must be bounded.

**OSC accumulation (999.7):** `vte` with the `std` feature accumulates OSC bytes in an unbounded `Vec<u8>` across `advance()` calls until the terminator arrives. `OSC_52_MAX_BYTES` and `MAX_TITLE_BYTES` caps live in `osc_dispatch`, but vte has already allocated the full unbounded buffer before `osc_dispatch` runs. A `ESC]52;c;<100MB>` sequence allocates 100 MB in the server process before any application-level cap fires. This is tracked as Phase 999.7 and is explicitly deferred from M5 — but the risk is real for any exposed server. The mitigation (an OSC-length pre-filter in `TerminalState::advance`) must not be accidentally blocked by M5 work.

**Prevention:**
- Keep the `SCROLLBACK_LINE_CAP` constant. Do not raise it without a measured reason.
- Bound the scrollback sync send buffer at the channel layer (Pitfall S-4 / M-6 above).
- Do not defer 999.7 past M5's security pass. If M5 ships without the OSC pre-filter, document the residual risk explicitly in `docs/999.7-SECURITY.md`.
- Add a `tracing::warn!` when `scrollback.len()` approaches `SCROLLBACK_LINE_CAP` for operator observability.

---

### Pitfall SEC-3: postcard discriminant invariant as a protocol security boundary

The `Message` enum's discriminant ordering is a security boundary as well as a compatibility boundary. If a variant is inserted before `ReattachErr` (discriminant 7), old clients will decode the new variant as `ReattachErr`. This means a legitimate `ChannelOpen` message could be decoded as "reattach rejected" by an old client — not a security issue in itself, but a protocol correctness failure that could be exploited if the new variant carries sensitive data that is silently discarded instead of acted on. Conversely, an `ReattachErr` decoded as a `ChannelOpen` by an old server could open an unintended channel.

**Prevention:** The discriminant stability test (Pitfall M-1) also covers this case. Run it in CI. Never insert variants before existing ones.

---

## Looks Done But Isn't — Sign-off Checklist

This checklist is the sign-off criteria for M5. Each item has a specific test or observable that confirms it is genuinely complete, not superficially done.

### Channel Multiplexing

- [ ] `message_discriminant_order_is_stable` test passes — encodes every `Message` variant and asserts its discriminant byte matches the hardcoded expected value. New variants appended after `TerminalControl` (9) must be added to this test.
- [ ] Channel-id namespace partitioning test passes — client-open and server-open fired simultaneously produce non-colliding ids, session survives.
- [ ] `ChannelAccept`/`ChannelReject` for unknown id is a no-op, not a panic — verified by a test that sends an accept for a channel the requester already closed.
- [ ] After cold reattach, scrollback channel is re-opened from scratch and delivers correct content — not replayed from the byte-stream replay.
- [ ] Flow-control: scrollback channel filled to its window limit; PTY input latency measured and confirmed < 5 ms during the backpressure episode.

### Scrollback Sync

- [ ] Scrollback content is sent only over a reliable QUIC stream (channel), never over datagrams — enforced by a type-level constraint (the sender accepts only `SendStream`, not `Connection`).
- [ ] Scrollback is suppressed while `echo_state.alt_screen` is true — unit test: activate alt screen, force scroll, assert `scrollback.len()` is unchanged.
- [ ] Scrollback lines include original column-width metadata — client can render a 200-column scrollback line in an 80-column window without truncation artefacts.
- [ ] `epoch_at_snapshot` field present in scrollback sync handshake — client transitions from scrollback-replay to live-grid cleanly, with no duplicate or missing lines at the boundary.
- [ ] Scrollback paging works — client requests 100 lines at a time; server does not send the next 100 until a CREDIT frame is received; verified under simulated slow client.

### Alt-Screen and Unicode

- [ ] `?1049h` clears the alt-screen grid to blank and saves the primary grid + cursor — verified by: write to primary, `?1049h`, write to alt, `?1049l`, assert primary content restored and cursor at saved position.
- [ ] `?1049l` restores primary grid content exactly — bit-for-bit match against the saved state.
- [ ] Resize while alt-screen is active resizes both the alt grid and the saved primary grid — verified by resize test (Pitfall A-4).
- [ ] Wide-char (CJK, width 2) advances cursor by 2 and writes a placeholder at `col + 1` — unit test with `\u{4e2d}`.
- [ ] ZWJ / combining marks do not advance the cursor — unit test with a ZWJ emoji sequence.
- [ ] Predictor is suppressed or reset when `echo_state.alt_screen` transitions to `true` — unit test: send `?1049h` via PTY bytes, assert `predictor.pending` is empty and no predictions are displayed.
- [ ] After `:q` from vim, the shell prompt is visible and at the correct position (live human validation required, not automatable).

### Repaint Pacing

- [ ] `burst_drains_when_grid_differs_from_acked_baseline` test passes — RED before fix, GREEN after. Verifies that `pending_deferred.len()` reaches 0 in a finite number of iterations with a non-empty grid vs an empty acked baseline.
- [ ] `noecho_read_dash_s_zero_predicted_chars` passes with burst code active — mandatory security gate. Must be a required non-`#[ignore]` test in CI.
- [ ] `current_epoch` increments exactly once per tick regardless of burst size — unit test: call the burst loop with N=8 datagrams, assert `current_epoch` incremented by 1.
- [ ] `datagram_send_buffer_space()` is used as the per-tick budget gate — verified by code review; no burst loop that ignores the buffer space.
- [ ] A full 80×24 screen repaint is delivered in ≤ 2 RTT at 150 ms RTT — live test with `time vim --noplugin -c q` measuring time from the keystroke to the first fully-rendered frame.

### Security and Memory

- [ ] `noecho_read_dash_s_zero_predicted_chars` passes — non-negotiable gate for any epoch or datagram timing change.
- [ ] Server RSS under 100 sessions does not exceed expected bound (100 × scrollback cap × per-cell size) — load test or calculation.
- [ ] OSC 999.7 mitigation is in place OR `docs/999.7-SECURITY.md` is updated to reflect deferred status with current scope.
- [ ] `postcard` discriminant test added to CI — any future variant addition that breaks discriminant order fails CI immediately.

---

## Sources

- `.planning/ROADMAP.md` — 999.4, 999.5, 999.6, 999.7 phase entries (exact bug descriptions, reverts, mandatory gates). HIGH confidence — these are first-party documented production failures.
- `crates/nosh-server/src/server.rs` — `build_state_diff`, `compute_diff_runs`, `run_session`, `run_reattach_session`, `EPOCH_SNAPSHOT_CAP`. HIGH confidence.
- `crates/nosh-server/src/terminal.rs` — `TerminalState`, `EchoState`, `SCROLLBACK_LINE_CAP`, `csi_dispatch` (`?1049` no-op at line 505), `print_char` (width-agnostic at line 306), `scroll_up`. HIGH confidence.
- `crates/nosh-client/src/predictor.rs` — `PredictionOverlay`, noecho suppression invariant, `confirmed_epoch`, `cull()`, `classify_input`, `is_tentative`. HIGH confidence.
- `crates/nosh-proto/src/messages.rs` — `Message` enum, discriminant-order comments (lines 56–62, 154–160), append-only invariant. HIGH confidence.
- `.planning/PROJECT.md` — milestone context, feature scope, security invariants. HIGH confidence.
- `CLAUDE.md` — security invariants section, discriminant-order and noecho decisions. HIGH confidence.
