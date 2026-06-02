---
phase: 16-qol-feature-pack-windows-ci-gate
verified: 2026-06-02T00:00:00Z
status: human_needed
score: 5/5 must-haves verified (code); 2 items require human sign-off
overrides_applied: 0
re_verification:
  previous_status: none
  previous_score: n/a
human_verification:
  - test: "Push the branch to GitHub and confirm the CI workflow runs green"
    expected: "Both jobs green; the build-windows step log shows `cargo build --locked --target x86_64-pc-windows-msvc -p nosh-client` succeeding (not skipped) — the not-false-green check for HARDEN-02"
    why_human: "D-16-04b — origin/main is stale with unpushed commits; gh/push not available from the sandbox. CI green-run is not machine-verifiable here."
  - test: "From a real client session, run a shell command that writes the clipboard via OSC 52 (e.g. `printf '\\033]52;c;%s\\007' $(echo hi | base64)`) on a terminal that supports OSC 52 (iTerm2/kitty/wezterm)"
    expected: "The text 'hi' appears in the LOCAL clipboard on the client machine; an OSC 52 read/query (`...;c;?`) does nothing (never honored)"
    why_human: "Requires a live session + a real OSC-52-capable terminal emulator applying the re-emitted sequence; not observable by static analysis."
  - test: "Set a remote title via OSC 0/2 (e.g. `printf '\\033]2;remote-host\\007'`) during a live session"
    expected: "The local terminal tab title reflects 'remote-host'; under `--status` the title instead shows `nosh: <rtt>ms` (forwarded title suppressed)"
    why_human: "Visual terminal-title behavior + --status precedence require a live emulator."
  - test: "Sever the network for >5 s during a live session, then restore it"
    expected: "A row-0 reverse-video banner appears reading `nosh: reconnecting — last contact Ns ago. Press ~. to disconnect.` with N incrementing live each second; the banner clears automatically when datagram traffic resumes"
    why_human: "Live datagram-silence timing + visual overlay rendering + live counter increment require a running session."
---

# Phase 16: QoL Feature Pack + Windows CI Gate Verification Report

**Phase Goal:** Day-to-day ergonomics land (connection-loss banner, OSC 52 clipboard passthrough, terminal title propagation, predict-mode flag) and the Windows CI gate actually runs on every push.
**Verified:** 2026-06-02
**Status:** human_needed
**Re-verification:** No — initial verification

## Goal Achievement

### Observable Truths (ROADMAP Success Criteria)

| # | Truth | Status | Evidence |
|---|-------|--------|----------|
| 1 | Datagram-silence >5 s → row-0 overlay with live elapsed counter + "Press ~. to disconnect"; clears on resume | ✓ VERIFIED (code) | `screen.rs:91-139` live `ConnectionLossOverlay` (active/last_contact/cols), `cell_at` returns None unless `active && row==0`, REVERSE banner advertising `~.`; `main.rs:716` `sleep_until(last_datagram_time + 5s)` silence arm sets `active=true` and renders (`:874-890`); `main.rs:894` `loss_tick` 1s interval arm re-renders live while active; datagram arm clears `active=false` and resets `last_datagram_time` (`:786-789`); overlay applied through compositor in `render_with_predictor` (`:397-404`). Tests `connection_loss_overlay_inactive_returns_none`/`_active_renders_row0` present. Live timing/visual = human. |
| 2 | OSC 52 clipboard write → local clipboard; read never honored | ✓ VERIFIED (code) | Server: `terminal.rs:707` read-gate `if data == b"?" { return; }` BEFORE store at `:715`; cap `OSC_52_MAX_BYTES=65536` (`:56`,`:714`); drain `take_osc52` (`:399`). Registry `drain_terminal_control` (`:521`) + tests incl. `..._osc52_read_form_yields_none`. Server forwards over RELIABLE `send` in both loops (`server.rs:625-650`, `:1132-1158`), never `send_datagram`. Client re-emits OSC 52 to stdout out-of-band (`main.rs:744-755`). Proto round-trip test passes. Local-clipboard application = human. |
| 3 | OSC 0/2 title not stripped; local tab reflects remote context | ✓ VERIFIED (code) | Server `terminal.rs:687-697` stores title (cap `MAX_TITLE_BYTES=1024`), `take_title` drain (`:412`); forwarded as `TerminalControlPayload::Title` over reliable stream (`server.rs:626-636`). Client re-emits `\x1b]0;{title}\x07` to stdout when `!status` (`main.rs:756-765`). Visual tab behavior = human. |
| 4 | `ci.yml` build-windows job on windows-latest builds nosh-client for x86_64-pc-windows-msvc every push; not false-green | ✓ VERIFIED (code) / ⏳ human green-run | `.github/workflows/ci.yml` has `linux` (cargo build + test) and `build-windows` (windows-latest, `cargo build --locked --target x86_64-pc-windows-msvc -p nosh-client` — `cargo build` not `cargo check`). `windows-cross.yml` deleted (only `ci.yml` present). Green Actions run = human (D-16-04b). |
| 5 | WSAEMSGSIZE quinn_udp warning suppressed with rationale + upstream issue in code comment | ✓ VERIFIED | `main.rs:415-425` `#[cfg(target_os = "windows")]` adds `quinn_udp=error` directive; comment cites WSAEMSGSIZE rationale + `quinn-rs/quinn#2041`; explicitly scoped to `quinn_udp` not `quinn`. |

**Score:** 5/5 truths verified in code. 2 (SC #4 green-run, SC #1/#2/#3 live terminal behavior) require human sign-off — both explicitly designated human-verification items in the plans (D-16-04b) and the verification method.

### Goal extras (QOL-04 predict flag)

| Item | Status | Evidence |
|------|--------|----------|
| `--predict <adaptive\|always\|never>` flag wired, default adaptive | ✓ VERIFIED | `main.rs:326` `predict: PredictDisplayMode` (default adaptive); threaded through `fresh_session`/`reattach_session`/`run_pump` (`:513,:528,:589,:641,:682` → `PredictionOverlay::new(predict_mode, ...)`). |
| `--status` surfaces SRTT in title | ✓ VERIFIED | `main.rs:334` `status: bool`; datagram arm emits `\x1b]0;nosh: {rtt_ms}ms\x07` from `conn.rtt()` when `status` (`:797,:848-852`); forwarded title suppressed under `--status`. |
| `~.` escape state machine | ✓ VERIFIED | `main.rs:117-145` newline-`~`-`.` recognition; tests `line_start_tilde_dot_quits_no_bytes`, `tilde_tilde_forwards_one_literal_tilde`, `mid_line_tilde_dot_is_literal`. |

### Required Artifacts

| Artifact | Expected | Status | Details |
|----------|----------|--------|---------|
| `crates/nosh-proto/src/messages.rs` | TerminalControl variant + payload | ✓ VERIFIED | Variant at `:174` appended AFTER `Ack` (`:149`); `TerminalControlPayload {Clipboard, Title}` at `:182`; `variant_name` arm `:229`; no payload-byte leak (test). Round-trip tests pass. |
| `crates/nosh-server/src/terminal.rs` | read-gate + caps + drains | ✓ VERIFIED | Caps `:56,:63`; read-gate `:707` before store; `take_osc52`/`take_title` `:399,:412`; adversarial bound tests updated to new caps. |
| `crates/nosh-server/src/registry.rs` | drain accessor | ✓ VERIFIED | `drain_terminal_control` `:521` calls `take_title()/take_osc52()` under the terminal_state lock; 3 tests incl. read-form→None. |
| `crates/nosh-server/src/server.rs` | drain+forward both loops | ✓ VERIFIED | `:625-650` (run_session) and `:1132-1158` (run_reattach_session) over reliable `send`; datagram path carries only StateDiff. |
| `crates/nosh-server/Cargo.toml` | vte std re-enabled | ✓ VERIFIED | `:41` `vte = { version = "0.15" }` (no `default-features=false`); CR-03 re-mitigation comment present. |
| `crates/nosh-client/src/screen.rs` | live overlay | ✓ VERIFIED | Stateful `ConnectionLossOverlay`; removed from overlays Vec (`:184 overlays: vec![]`); applied in `render_with_predictor`. |
| `crates/nosh-client/src/main.rs` | client wiring | ✓ VERIFIED | `--status`, silence timer, loss_tick, OSC re-emit arm, RTT title, WSAEMSGSIZE filter all present. |
| `.github/workflows/ci.yml` | Linux + windows-latest MSVC | ✓ VERIFIED | Both jobs present; cargo build (not check); windows-cross.yml deleted. |

### Key Link Verification

| From | To | Via | Status |
|------|----|----|--------|
| terminal.rs osc_dispatch | osc52_pending / title | read-gate + cap before store | ✓ WIRED (`:707` gate before `:715` store) |
| server.rs both loops | Message::TerminalControl | drain accessor → write_message over reliable send | ✓ WIRED (`:625,:1132`) |
| client reliable-stream arm | stdout | TerminalControl → OSC 52/0/2 write_all (out-of-band) | ✓ WIRED (`main.rs:744-766`) |
| client silence arm | loss_overlay.active | sleep_until(last_datagram + 5s) | ✓ WIRED (`:716,:874`) |
| client loss_tick arm | live re-render | interval(1s) tick while active | ✓ WIRED (`:695,:894`) |
| client datagram arm | terminal title | if status emit OSC 0/2 with conn.rtt() | ✓ WIRED (`:797,:848`) |

### Behavioral Spot-Checks

| Behavior | Command | Result | Status |
|----------|---------|--------|--------|
| nosh-client compiles (Windows host) | `cargo build -p nosh-client --locked` | Finished, clean | ✓ PASS |
| nosh-client clippy clean | `cargo clippy -p nosh-client --bins --lib` | Finished, no warnings | ✓ PASS |
| proto TerminalControl round-trips | `cargo test -p nosh-proto --locked terminal_control` | 2 passed | ✓ PASS |
| nosh-server / full-workspace test compile (Windows host) | `cargo test -p nosh-client --locked` | nosh-server fails (nix/unistd/signal_shutdown/master_raw_fd) | ? SKIP — by design (Linux-only server, CLAUDE.md); NOT a Phase 16 regression. Linux CI job is the authoritative gate. |

### Probe Execution

No project probe scripts (`scripts/*/tests/probe-*.sh`) found; phase is not a migration/probe phase. N/A.

### Requirements Coverage

| Requirement | Source Plan | Status | Evidence |
|-------------|-------------|--------|----------|
| QOL-01 (loss overlay) | 16-02 | ✓ SATISFIED (code) | SC #1 above |
| QOL-02 (OSC 52 clipboard, write-only) | 16-01, 16-02 | ✓ SATISFIED (code) | SC #2 above |
| QOL-03 (OSC 0/2 title) | 16-01, 16-02 | ✓ SATISFIED (code) | SC #3 above |
| QOL-04 (--status SRTT) | 16-02 | ✓ SATISFIED | goal-extras table |
| HARDEN-02 (Windows CI gate) | 16-03 | ✓ SATISFIED (authored) / ⏳ human green-run | SC #4 above |
| HARDEN-03 (WSAEMSGSIZE suppression) | 16-02 | ✓ SATISFIED | SC #5 above |

No orphaned requirements: REQUIREMENTS.md maps exactly QOL-01..04 + HARDEN-02/03 to Phase 16; all claimed across the three plans.

### Anti-Patterns Found

No blocker anti-patterns. The `if data == b"?"` early-return in osc_dispatch and the best-effort `let _ = stdout.write_all(...)` on out-of-band control sequences are intentional and documented (control sequences, not display state). No unreferenced TBD/FIXME/XXX debt markers in the Phase-16 files. The `\x1b]52` / `\x1b]0` raw writes are deliberate out-of-band re-emit (D-16-01), not stubs.

### Human Verification Required

1. **CI green-run (HARDEN-02, D-16-04b)** — push to GitHub; confirm both `linux` and `build-windows` jobs green and the windows build step actually compiled (not skipped).
2. **OSC 52 clipboard** — live session on an OSC-52-capable terminal; write form lands in local clipboard, read form (`?`) does nothing.
3. **Terminal title** — live OSC 0/2 reflects in local tab; under `--status` the RTT title takes precedence.
4. **Connection-loss banner** — sever network >5 s; row-0 reverse banner with live-incrementing counter appears and clears on resume.

### Gaps Summary

No code gaps. All five ROADMAP success criteria are satisfied in the codebase, verified by tracing requirement → source → test (not by trusting summaries): the proto variant is correctly append-only after Ack; the server read-gate precedes the store and the caps bound both OSC 52 and title; both server loops forward over the reliable stream (never datagrams); the client re-emits out-of-band, drives the overlay via a >5 s silence timer with a live 1s tick, suppresses the forwarded title under `--status`, and emits the RTT title; and the Windows CI gate is a real `cargo build` (not `cargo check`) on windows-latest with `windows-cross.yml` retired. `nosh-client` builds and clippy-passes clean on this Windows host; the `nosh-server` build failure on Windows is the documented Linux-only design (nix/`#[cfg(unix)]`), NOT a Phase 16 regression. Status is `human_needed` (not `passed`) solely because the CI green-run and the live terminal behaviors are inherently human-verification items — exactly as the plans designated (D-16-04b) and the verification method directed.

---

_Verified: 2026-06-02_
_Verifier: Claude (gsd-verifier)_
