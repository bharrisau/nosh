---
phase: 16-qol-feature-pack-windows-ci-gate
reviewed: 2026-06-02T00:00:00Z
depth: deep
files_reviewed: 7
files_reviewed_list:
  - crates/nosh-proto/src/messages.rs
  - crates/nosh-server/src/terminal.rs
  - crates/nosh-server/src/registry.rs
  - crates/nosh-server/src/server.rs
  - crates/nosh-client/src/screen.rs
  - crates/nosh-client/src/main.rs
  - .github/workflows/ci.yml
findings:
  critical: 1
  warning: 3
  info: 2
  total: 6
status: findings
---

# Phase 16: Code Review Report

**Reviewed:** 2026-06-02T00:00:00Z
**Depth:** deep
**Files Reviewed:** 7
**Status:** findings

## Summary

Phase 16 adds three features: OSC 52 / OSC 0/2 passthrough over the reliable stream
(TerminalControl proto variant + server-side drain + client re-emit), the
ConnectionLossOverlay silence detection UI, and a native Windows MSVC CI gate.

The OSC 52 security boundary (read-gate, byte-cap) is correctly implemented.
The proto serialization ordering (append-only discriminant discipline) is correctly
maintained. The CI gate structure is sound.

Three distinct defects were found:

1. A **BLOCKER** hot-spin: after the silence timer fires once, `silence_sleep` is
   recreated each loop iteration at a deadline already in the past, causing it to
   resolve immediately on every subsequent iteration. This floods the render path
   (continuous full repaints) and pegs one CPU core while the connection is lost.

2. A **WARNING** OSC injection risk: the OSC 52 `selection` and `data` byte vectors
   are forwarded to the local terminal without sanitizing ESC (`\x1b`) or BEL (`\x07`)
   bytes. A malicious server can embed a premature BEL terminator in `selection` to
   inject arbitrary OSC sequences to the client's local terminal.

3. A **WARNING** about `loss_tick` missing `MissedTickBehavior::Skip`, causing
   burst re-renders if the loop was busy (e.g. heavy keystroke traffic) while the
   overlay was active.

Two info-level findings: the Linux CI job runs no clippy check, and the
`take_title` drain semantics permanently clear `TerminalState::title`, meaning
after forwarding, a second query of `title()` returns `None` (by design, but
silently inconsistent with `osc52_pending`'s read-vs-drain split).

---

## Critical Issues

### CR-01: `silence_sleep` hot-spin after overlay activates

**File:** `crates/nosh-client/src/main.rs:716`

**Issue:** `silence_sleep` is recreated at the top of every loop iteration as
`sleep_until(last_datagram_time + Duration::from_secs(5))`. When the silence
arm fires (line 874), it sets `loss_overlay.active = true` but does NOT update
`last_datagram_time`. On every subsequent loop iteration the new `silence_sleep`
is immediately resolved (its deadline is already 5 s in the past). Without a
`biased` select, tokio randomly picks among ready arms on each iteration, but
`silence_sleep` is always ready, so it wins roughly half the time in competition
with `loss_tick.tick()`.

Concrete effect: while the connection is lost, the loop spins at ~CPU-max speed,
calling `render_with_predictor` continuously, pegging a CPU core, and writing
redundant renders to stdout (flickering). The `loss_tick` 1 s guard is supposed
to rate-limit the elapsed-counter re-renders, but `silence_sleep` bypasses it.

**Fix:** Add a guard on the `silence_sleep` arm that skips it when the overlay is
already active, mirroring the `loss_tick` guard pattern:

```rust
// In the loop body, just before tokio::select!:
let silence_sleep = tokio::time::sleep_until(last_datagram_time + Duration::from_secs(5));

tokio::select! {
    // ...
    _ = silence_sleep, if !loss_overlay.active => {
        loss_overlay.active = true;
        loss_overlay.last_contact = last_datagram_time.into_std();
        // ... render ...
    }
    // ...
}
```

With `if !loss_overlay.active`, once the overlay is active the silence arm
becomes `pending()` and only `loss_tick.tick()` drives the 1 s re-renders.

---

## Warnings

### WR-01: OSC 52 `selection` / `data` not sanitized before terminal re-emission

**File:** `crates/nosh-client/src/main.rs:750-752`

**Issue:** The client re-emits OSC 52 as:

```rust
let sel = String::from_utf8_lossy(&selection);
let b64 = String::from_utf8_lossy(&data);
let osc52 = format!("\x1b]52;{sel};{b64}\x07");
```

Neither `selection` nor `data` is checked for embedded BEL (`\x07`) or ESC
(`\x1b`) bytes before interpolation. The OSC sequence is terminated by the first
BEL in the output. If `selection` contains a literal `\x07`, the sequence closes
early after `\x1b]52;<selection-bytes-before-\x07>`, and everything after the
embedded BEL up to the closing `\x07` is emitted as raw bytes to the terminal,
potentially containing arbitrary escape sequences.

Because the server's `osc_dispatch` does not strip control bytes from `selection`
or `data` before storing, a process running on the server that emits crafted OSC 52
sequences could inject arbitrary terminal control sequences into the client's local
terminal (ESC-injection attack on the local terminal emulator). This is distinct from
the clipboard leak the existing read-gate prevents; it is an injection into the
client's local terminal output stream.

The `data` field is nominally base64 and unlikely to contain control bytes in
practice, but `selection` can contain arbitrary bytes as passed by the PTY
application (the field is often just `c` but has no protocol-enforced charset).

**Fix — strip or reject control bytes in the server `osc_dispatch` before storing:**

```rust
// In terminal.rs osc_dispatch, b"52" arm, before storing:
let selection = params.get(1).copied().unwrap_or(b"c");
// Reject selection containing control bytes (ESC, BEL).
if selection.iter().any(|&b| b == b'\x1b' || b == b'\x07') {
    return; // drop malformed OSC 52
}
```

Or equivalently, sanitize on the client before re-emitting (strip control bytes
from `sel` and `b64` after `from_utf8_lossy`):

```rust
let sel: String = String::from_utf8_lossy(&selection)
    .chars()
    .filter(|&c| c != '\x07' && c != '\x1b')
    .collect();
let b64: String = String::from_utf8_lossy(&data)
    .chars()
    .filter(|&c| c != '\x07' && c != '\x1b')
    .collect();
```

The server-side fix is preferred because it prevents the malformed payload from
ever entering the TerminalControl wire message.

### WR-02: `loss_tick` interval missing `MissedTickBehavior::Skip`

**File:** `crates/nosh-client/src/main.rs:695`

**Issue:** `loss_tick` is created as:

```rust
let mut loss_tick = tokio::time::interval(Duration::from_secs(1));
```

`tokio::time::interval` defaults to `MissedTickBehavior::Burst`, which causes
all missed ticks to fire back-to-back if the event loop was delayed (e.g., a
slow `render_with_predictor` call, or heavy keystroke traffic). When the overlay
is active and the loop was busy, a burst of rapid re-renders can occur until the
missed ticks drain, causing visual flicker. The `ack_interval` at line 666
correctly uses `Skip`. The `loss_tick` should use the same policy.

**Fix:**

```rust
let mut loss_tick = tokio::time::interval(Duration::from_secs(1));
loss_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
```

### WR-03: `loss_tick` fires an unconsumed first tick immediately at startup

**File:** `crates/nosh-client/src/main.rs:695`

**Issue:** `tokio::time::interval` fires its first tick IMMEDIATELY on the first
`.tick()` call (documented tokio behavior). The arm has a guard
`if loss_overlay.active` so it is suppressed while the overlay is inactive,
but on the loop iteration where `silence_sleep` fires and sets
`loss_overlay.active = true`, the next iteration's `loss_tick.tick()` arm fires
immediately (the pending first tick fires), causing a second render just
milliseconds after the silence arm's render — a spurious double repaint at
activation time.

This is minor but unnecessary. The fix is to call `loss_tick.tick().await` once
before the main loop to consume the immediate first tick, or (preferably) use
`tokio::time::interval_at` starting 1 s in the future:

```rust
let mut loss_tick = tokio::time::interval_at(
    tokio::time::Instant::now() + Duration::from_secs(1),
    Duration::from_secs(1),
);
loss_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
```

---

## Info

### IN-01: Linux CI job does not run `cargo clippy`

**File:** `.github/workflows/ci.yml:13-20`

**Issue:** The `linux` job runs `cargo build --locked` and `cargo test --locked`
but no `cargo clippy` step. Clippy catches correctness issues (e.g., inadvertent
captures in `tokio::select!` arms, `unused_must_use` on `Result` futures) that
`cargo build` does not flag. Given that this codebase uses `#[allow(clippy::too_many_arguments)]`
at two sites in `main.rs`, suggesting clippy is expected to be run at some point.

**Fix:** Add a clippy step to the linux job:

```yaml
- name: Clippy
  run: cargo clippy --locked -- -D warnings
```

### IN-02: `take_title` permanently clears `TerminalState::title`; `title()` and drain split inconsistent

**File:** `crates/nosh-server/src/terminal.rs:412-414`, `crates/nosh-server/src/registry.rs:521-524`

**Issue:** `take_title()` calls `self.title.take()` which clears the title field.
This means after `drain_terminal_control()` drains a title, `terminal_state.title()`
returns `None` — the server's own terminal model loses the current title. This is
intentional for the drain-once forwarding semantics, but it has a subtle consequence:
a future call to `TerminalState::title()` (e.g. for diff extraction in a later
phase) will see `None` even if the title was set by the remote program. A subsequent
OSC 0/2 from the remote program will restore it, but there is a window after drain
where the server-side model is inconsistent.

By contrast, `osc52_pending` is designed as a transient write-detect field (it's
never used for grid state), so draining it has no semantic loss. The title field is
potentially used by future phases for state-model reads.

**Fix (optional/deferred):** Keep a separate `last_title: Option<String>` field
that is updated by `osc_dispatch` (always set) but not cleared by `take_title()`.
`take_title()` drains a `title_pending: Option<String>` forward-only field.
This separates the read-model state from the forwarding queue — the same pattern
`osc52_pending` already uses, but explicitly.

This is not a correctness bug in Phase 16's scope (no code reads `title()` after
`drain_terminal_control()`), but it is an architectural inconsistency that will
create a subtle bug in a future phase that queries the title for diff extraction
or display purposes.

---

_Reviewed: 2026-06-02_
_Reviewer: Claude (gsd-code-reviewer)_
_Depth: deep_
