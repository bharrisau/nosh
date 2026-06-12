---
status: issues_found
phase: 22-scrollback-sync
depth: standard
files_reviewed: 8
reviewed_by: 3 parallel gsd-code-reviewer passes (proto / server / client), diff-scoped
critical: 1
warning: 6
info: 6
total: 13
---

# Phase 22 — Scrollback Sync — Code Review

Consolidated from three subsystem reviews (`22-REVIEW-proto.md`, `22-REVIEW-server.md`,
`22-REVIEW-client.md`). Files were reviewed diff-scoped (`git diff bf67b1c..HEAD`) because
the full source files exceed a single reviewer's context window.

## Blockers

### CR-C-01 — `epoch_at_snapshot: 0` makes scrollback non-functional
`crates/nosh-client/src/main.rs` (~1790) — Entering `ScrollbackView::Active` on Shift-PageUp
seeds `epoch_at_snapshot = 0`. The epoch gate (~1507) exits Active when `diff.epoch >= epoch_at_snapshot`;
since live `diff.epoch` is always >= 1, the gate fires on the very next datagram and scrollback
exits before the first `ScrollbackPage` arrives. Scrollback enters and immediately exits on every
keypress. Fix: seed `epoch_at_snapshot` from the current applied epoch at entry (e.g.
`screen.last_applied_epoch()`), and add/adjust a test that uses the real initial value (existing
tests use `active_view(10, 100)` and miss this).

## Warnings

### WR-P-01 — S-5 epoch/scrollback TOCTOU (read epoch inside the mutex)
`crates/nosh-server/src/channel.rs` (~297-299) — `epoch_src.load(Acquire)` and
`slot.with_terminal_state(...)` are separate ops with no `.await` between, but on a multi-threaded
executor a PTY task can push scrollback lines between the load and the mutex acquisition, so the page
can contain content newer than `epoch_at_snapshot` → duplicate at the scrollback/live seam (the exact
S-5 condition). Fix: read the epoch counter inside the `terminal_state` lock so epoch + scrollback are
one atomic acquisition.

### WR-S-01 — `try_send(ChannelEvent::Close)` can silently drop the close signal
`crates/nosh-server/src/server.rs` (~scrollback spawn) — If the channel's 64-slot event queue is full,
`try_send(Close)` drops it and the scrollback task stays alive holding `slot_clone` until connection
teardown. Same class as the already-fixed `Stream` event (WR-01). Fix: use `send(...).await` for Close.

### WR-S-02 — inner credit-wait loop can block indefinitely
`crates/nosh-server/src/channel.rs` — If a `ScrollbackPage` exceeds `INITIAL_CREDIT` (256 KiB) and the
client never grants more credit, the inner wait blocks forever. Add a bounded timeout (e.g. 30 s).

### WR-C-01 — credit fallback of 0 understates consumed bytes
`crates/nosh-client/src/channel.rs` (~252) — On `codec::encode()` failure the credit fallback is `0`,
which starves the server's flow-control window. Use a conservative non-zero count (e.g. `MAX_FRAME_LEN + 4`).

### WR-C-02 — Active entered even when the scrollback channel is unavailable
`crates/nosh-client/src/main.rs` (~1787-1808) — If the channel open failed/was rejected, Shift-PageUp
still transitions to Active and sends requests that silently drop. Add a `scrollback_available` guard.

### WR-P-02 — `ChannelType` discriminants unguarded by a stability test
`crates/nosh-proto/src/codec.rs` — The discriminant-stability test covers `Message` but not
`ChannelType` (Scrollback=1, PortForward=2, AgentForward=3). Add `channel_type_discriminant_order_is_stable`.

## Info

- IN-P-01 — `ScrollbackLine.width` carries post-resize width, not original live width (S-3 metadata claim false after shrink resize). Pre-existing in the `Vec<Cell>` representation.
- IN-P-02 — `cells` doc says trailing blanks "may be omitted" but all cells are always sent; fix the doc.
- IN-S-01 — Stale comment/log at server.rs (~1246/1253) "until Phase 22-02 wires the scrollback handler" — Phase 22 is complete; update.
- IN-S-02 — `run_reattach_session` drops scrollback control-stream frames via `Ok(_) => {}`; add an explicit debug-log arm to match `run_session`.
- IN-C-01 — `scrollback_ctrl_tx.clone()` at main.rs (~1375) where the original is never used; move instead of clone.
- IN-C-02 — CSI `pending` accumulator is an unbounded `Vec` with an "at most 8 bytes" doc claim; add a `debug_assert!`.
