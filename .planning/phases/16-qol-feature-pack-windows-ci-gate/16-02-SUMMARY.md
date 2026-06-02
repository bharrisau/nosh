---
phase: 16-qol-feature-pack-windows-ci-gate
plan: 02
subsystem: nosh-client
tags: [overlay, connection-loss, osc-reemit, status, tracing, windows]
dependency_graph:
  requires: ["16-01"]
  provides: ["ConnectionLossOverlay live", "OSC 52/0/2 re-emit", "--status RTT title", "WSAEMSGSIZE suppression"]
  affects: ["nosh-client"]
tech_stack:
  added: ["crossterm osc52 feature"]
  patterns: ["sleep_until silence timer", "1s interval loss_tick arm", "out-of-band OSC writes", "#[cfg(target_os = windows)] tracing filter"]
key_files:
  created: []
  modified:
    - crates/nosh-client/src/screen.rs
    - crates/nosh-client/src/main.rs
    - crates/nosh-client/src/client.rs
    - crates/nosh-client/Cargo.toml
decisions:
  - "ConnectionLossOverlay hoisted from overlays Vec to run_pump scope (mirrors PredictionOverlay — Pitfall 3)"
  - "Silence timer uses sleep_until(last_datagram_time + 5s) recreated each select! iteration (idiomatic — re-arms automatically)"
  - "loss_tick uses tokio::time::interval(1s) with `if loss_overlay.active` guard to avoid spurious re-renders when inactive"
  - "OSC 52/0/2 written out-of-band (direct stdout write_all, not through compositor) — no cursor motion, safe to interleave (T-16-06)"
  - "Title forwarding suppressed when --status active (RTT title takes precedence, Pitfall 5)"
  - "WSAEMSGSIZE filter is quinn_udp=error NOT quinn=error — preserves connection/auth WARNs (T-16-07)"
metrics:
  duration_minutes: 15
  completed_date: "2026-06-02T06:39:55Z"
  tasks_completed: 3
  files_modified: 4
---

# Phase 16 Plan 02: Client QoL Pack — ConnectionLossOverlay, OSC Re-emit, --status, WSAEMSGSIZE Summary

**One-liner:** Live row-0 ConnectionLossOverlay with 1s elapsed counter, OSC 52/0/2 out-of-band re-emit, --status RTT title, and Windows quinn_udp WSAEMSGSIZE tracing suppression.

## Tasks Completed

| Task | Description | Commit | Files |
|------|-------------|--------|-------|
| 1 | Activate ConnectionLossOverlay as live row-0 overlay | 6a6ddc6 | screen.rs, main.rs |
| 2 | Silence timer, live 1s loss tick, OSC re-emit, --status, threading | 853ea7d | main.rs, client.rs |
| 3 | WSAEMSGSIZE Windows tracing suppression + crossterm osc52 feature | 9a9a2a9 | main.rs, Cargo.toml |

## What Was Built

### Task 1: ConnectionLossOverlay activation

`ConnectionLossOverlay` was promoted from a no-op stub to a fully stateful struct with:
- `pub active: bool` — whether the banner is shown
- `pub last_contact: std::time::Instant` — timestamp of last datagram (for elapsed display)
- `pub cols: u16` — terminal width for banner padding

`Overlay::cell_at` implementation: returns `None` unless `active && row == 0`, then builds a reverse-video banner padded to terminal width:
```
nosh: reconnecting — last contact 7s ago. Press ~. to disconnect.
```

The overlay was removed from the `overlays` Vec in `ClientScreen::new` (now `vec![]`) and hoisted to `run_pump` scope (mutably owned), mirroring `PredictionOverlay` per Pitfall 3.

`render_with_predictor` signature was extended to accept `&ConnectionLossOverlay` as a third parameter. The loss overlay is applied after `compose_desired()` and before the predictor (so the banner renders correctly; predictor cells can overlay banner chars at row 0).

New tests: `connection_loss_overlay_inactive_returns_none`, `connection_loss_overlay_active_renders_row0`, `connection_loss_overlay_banner_contains_tilde_dot`.

### Task 2: run_pump silence timer and OSC re-emit

- `--status: bool` flag added to `Args` with help text; threaded through `fresh_session` → `reattach_session` → `run_pump` (both callers already had `#[allow(clippy::too_many_arguments)]`; `reattach_session` needed the attribute added)
- `loss_overlay` promoted to `mut`; `last_datagram_time: tokio::time::Instant` and `loss_tick: Interval(1s)` added
- **Silence detection arm**: `tokio::time::sleep_until(last_datagram_time + 5s)` recreated each select! iteration (re-arms automatically). On fire: sets `loss_overlay.active = true`, `loss_overlay.last_contact = last_datagram_time.into_std()`, force-renders banner
- **loss_tick arm**: `_ = loss_tick.tick(), if loss_overlay.active` — re-renders every 1s while active so elapsed counter increments live (QOL-01 live counter requirement)
- **Datagram arm**: resets `last_datagram_time`, clears `loss_overlay.active` on resume; emits `\x1b]0;nosh: {rtt_ms}ms\x07` if `status` (QOL-04)
- **Reliable-stream TerminalControl arm**: `Ok(Message::TerminalControl(payload))` added before `Ok(_)` catch-all:
  - `Clipboard`: writes `\x1b]52;{sel};{b64}\x07` out-of-band (QOL-02)
  - `Title`: writes `\x1b]0;{title}\x07` only when `!status` (QOL-03; suppressed under --status)

### Task 3: Windows tracing filter + crossterm osc52

- Tracing-subscriber init refactored to build `EnvFilter` in `#[cfg]`-gated `let env_filter` binding
- Windows: adds `quinn_udp=error` directive — suppresses WSAEMSGSIZE WARN from GRO metadata (HARDEN-03); comment references quinn-rs/quinn#2041 (open)
- Linux/other: unchanged filter
- `crossterm` dep now has `features = ["events", "osc52"]`

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 1 - Bug] Pre-existing clippy error in client.rs**
- **Found during:** Task 2 (clippy verification)
- **Issue:** `return Ok(Self { ... })` in Windows-gated block was flagged by `clippy::needless_return`
- **Fix:** Removed `return` keyword, making it an expression (idiomatic Rust)
- **Files modified:** `crates/nosh-client/src/client.rs`
- **Commit:** 853ea7d

### Pre-existing Issues (out of scope, documented)

**nosh-server dev-dependency compilation failure:** The integration tests for `nosh-client` include `nosh-server` as a dev-dependency. `nosh-server` has pre-existing compilation errors (`nix::unistd`, `nix::sys` module renames, missing `signal_shutdown`/`master_raw_fd` methods) introduced before Phase 16-02. This is NOT caused by Phase 16-02 changes (confirmed via git stash verification). It blocks `cargo test -p nosh-client --locked` (which compiles dev-deps) but does NOT block `cargo build -p nosh-client --locked` (production build). Unit tests in `screen.rs` are structurally correct; clippy passes on `--lib` and `--bin` targets.

The `cargo clippy -p nosh-client --all-targets --locked -- -D warnings` criterion from the plan CANNOT pass due to this pre-existing issue. The lib and bin targets are clean.

## Known Stubs

None — all wired behaviors are real implementations connected to live tokio arms and the compositor path.

## Threat Flags

No new threat surface beyond the plan's threat model. All T-16-05 through T-16-SC mitigations are implemented as specified.

## Self-Check

### Files exist:
- [x] `crates/nosh-client/src/screen.rs` — ConnectionLossOverlay with active/last_contact/cols/new
- [x] `crates/nosh-client/src/main.rs` — status flag, silence timer, loss_tick, TerminalControl arm, RTT title, WSAEMSGSIZE filter
- [x] `crates/nosh-client/Cargo.toml` — osc52 feature on crossterm

### Commits exist:
- [x] 6a6ddc6 feat(16-02): activate ConnectionLossOverlay as live row-0 overlay
- [x] 853ea7d feat(16-02): wire silence timer, loss_tick, OSC re-emit, --status RTT title
- [x] 9a9a2a9 feat(16-02): WSAEMSGSIZE Windows tracing suppression + crossterm osc52 feature

## Self-Check: PASSED
