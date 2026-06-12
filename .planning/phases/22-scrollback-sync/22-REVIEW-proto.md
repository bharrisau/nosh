---
phase: 22-scrollback-sync
reviewed: 2026-06-12T00:00:00Z
depth: standard
files_reviewed: 3
files_reviewed_list:
  - crates/nosh-proto/src/messages.rs
  - crates/nosh-proto/src/codec.rs
  - crates/nosh-server/src/terminal.rs
findings:
  critical: 0
  warning: 2
  info: 2
  total: 4
status: issues_found
---

# Phase 22: Code Review Report (Proto + Terminal)

**Reviewed:** 2026-06-12
**Depth:** standard
**Files Reviewed:** 3
**Status:** issues_found

## Summary

Reviewed the three Phase 22 scrollback-sync files via `git diff bf67b1c..HEAD`. The wire-type additions (`ScrollbackRequest`/`ScrollbackPage`/`ScrollbackCredit` at discriminants 15–17) are correctly appended after `ChannelClose=14` and the discriminant-stability test covers them. `SCROLLBACK_LINE_CAP` is unchanged at 10,000. The `!alt_screen` gate is present and correct on both `scroll_up()` and `resize()` paths. The `scrollback_lines()` accessor is bounds-safe for all adversarial inputs tested. No blcokers.

Two warnings: a TOCTOU race between the epoch atomic and the terminal-state mutex (S-5 constraint violated under concurrent diff tick), and a missing discriminant-stability assertion for `ChannelType`. Two info items: a doc/impl contract mismatch on `ScrollbackLine.cells` trailing-blank omission, and a `width` field inaccuracy after resize.

## Warnings

### WR-P-01: S-5 epoch/scrollback TOCTOU — epoch loaded outside terminal-state lock

**File:** `crates/nosh-server/src/channel.rs:297-299`
**Issue:** `epoch_at_snapshot` is loaded from `epoch_src` (`AtomicU64`, `Acquire`) and then the terminal state is read under a separate mutex via `with_terminal_state`. In tokio's multi-threaded executor these two operations are not atomic. A concurrent PTY input task can modify `terminal_state` (pushing new scrollback lines) between the `epoch_src.load` and the `mutex.lock()` inside `with_terminal_state`. The epoch_src is updated by the diff loop *after* `run_tick` — it reflects the last *diffed* epoch, not the current PTY write frontier. If a PTY write occurs after the last diff tick but before the scrollback snapshot, the captured lines may include content from after `epoch_at_snapshot`. The client waits for `live >= epoch_at_snapshot` before transitioning; a line present in both the scrollback page and the live datagram for that epoch will be rendered twice (duplicate at the boundary — exactly the condition S-5 is meant to prevent).

The code comment ("There is NO .await between this load and the closure") is correct for single-thread execution but does not protect against truly concurrent tokio worker threads.

**Fix:** Embed the epoch counter inside the terminal-state mutex so it can be read atomically with the scrollback snapshot in a single lock acquisition:

```rust
// In with_terminal_state closure:
let (raw_lines, total_available) = slot.with_terminal_state(|ts| {
    let epoch = ts.current_epoch(); // read from TerminalState, under the same lock
    (ts.scrollback_lines(from_line, count), epoch)
});
let ((raw_lines, total_available), epoch_at_snapshot) = /* destructure */;
```

Alternatively, store a snapshot-epoch field in `TerminalState` that is updated every time scrollback lines are appended (i.e., inside `scroll_up()`). That field is then read under the same lock as the scrollback lines, guaranteeing the epoch is never behind the lines returned.

The window is narrow (sub-microsecond on a quiescent terminal) but the protocol specification explicitly prohibits duplicates at this boundary (S-5).

---

### WR-P-02: `ChannelType` discriminant order not covered by stability test

**File:** `crates/nosh-proto/src/codec.rs:276-312` (test `message_discriminant_order_is_stable`)
**Issue:** The `message_discriminant_order_is_stable` test verifies `Message` enum discriminants (0–17) but there is no corresponding assertion for `ChannelType` enum discriminants. `ChannelType` is postcard-encoded as a nested enum inside `Message::ChannelOpen`. Phase 22 added `Scrollback` at position 1 (after `Echo=0`). If a future commit reorders or inserts a variant before `Scrollback`, the wire discriminant shifts silently — the test suite would not catch it until actual connection negotiation fails with a peer running the old code. `PortForward` and `AgentForward` are already declared and their discriminants (2, 3) are equally unguarded.

**Fix:** Add a discriminant-stability test for `ChannelType` analogous to the `Message` test:

```rust
#[test]
fn channel_type_discriminant_order_is_stable() {
    use crate::messages::ChannelType;
    use postcard::to_allocvec;
    let cases: &[(u8, ChannelType)] = &[
        (0, ChannelType::Echo),
        (1, ChannelType::Scrollback),
        (2, ChannelType::PortForward),
        (3, ChannelType::AgentForward),
    ];
    for (expected, ct) in cases {
        let encoded = to_allocvec(ct).expect("encode");
        assert_eq!(encoded[0], *expected,
            "ChannelType::{ct:?} must encode with discriminant {expected}");
    }
}
```

## Info

### IN-P-01: `ScrollbackLine.width` reflects post-resize column width, not original live width

**File:** `crates/nosh-server/src/channel.rs:308` / `crates/nosh-proto/src/messages.rs:42-43`
**Issue:** `ScrollbackLine.width` is documented as "Terminal column width when this line was live (S-3 original width metadata)." In practice it is computed as `row.len() as u16`, where `row` is the raw `Vec<Cell>` from the scrollback `VecDeque`. When `resize()` shrinks the terminal, grid rows are truncated to the new column width *before* being pushed to scrollback (lines 466-491 of terminal.rs). The pushed row therefore has length equal to the *new* column width, not the original. A line written at 220 columns then scrolled off during a shrink to 80 columns will carry `width=80`, not `width=220`. The client would believe the line was always 80 columns wide.

This is a pre-existing design limitation of the `Vec<Cell>` scrollback representation (no original-width metadata stored at push time), not introduced by Phase 22. Phase 22 surfaces it as a visible contract mismatch. No immediate behaviour bug in existing clients (the field is informational), but the doc claim is false.

**Fix:** Either update the doc comment to say "Terminal column width at time of serialisation (may differ from original live width after a terminal resize)" — which is at least accurate — or, for a proper S-3 implementation, store original width alongside each scrollback row (e.g., `VecDeque<(u16, Vec<Cell>)>`).

---

### IN-P-02: `ScrollbackLine.cells` doc claims trailing blanks "may be omitted"; implementation never omits them

**File:** `crates/nosh-proto/src/messages.rs:43`
**Issue:** The `cells` field doc says "Length ≤ `width` (trailing blank cells may be omitted)." The implementation in `channel.rs:308-318` converts all cells in the row without trimming trailing blanks, so `cells.len()` always equals `width`. A client that relies on the omission optimisation (e.g., allocating `width` cells then overwriting the first `cells.len()`) is correct, but a client that reads the doc and expects omission may be surprised. The doc creates a false expectation and any future refactor that actually omits trailing blanks would be a silent wire-format change if clients have already been deployed with the always-full assumption.

**Fix:** Change the doc to match the implementation: "Length equals `width`; all cells including trailing blanks are always included." If trailing-blank omission is desired as a future bandwidth optimisation, document it as explicitly not yet implemented and flag the field as append-only.

---

_Reviewed: 2026-06-12_
_Reviewer: Claude (gsd-code-reviewer)_
_Depth: standard_
