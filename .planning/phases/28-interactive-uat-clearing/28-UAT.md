---
status: testing
phase: 28-interactive-uat-clearing
source: [19-HUMAN-UAT.md, 23-HUMAN-UAT.md, 999.3-VERIFICATION.md, 999.4-VERIFICATION.md, ROADMAP.md Phase 28 SC1-4]
started: "2026-06-14"
updated: "2026-06-14"
---

## Current Test
<!-- OVERWRITE each test - shows where we are -->

[paused — blocker found on Test 1: reliable-stream framing desync → reconnect loop
(FrameTooLarge 0x6669673D). Tests 2-6 (Windows visual) and 12 (M7 reattach) are
blocked on the same path. Awaiting operator decision: diagnose+fix now vs continue
non-affected items (8 CI, 9 latency) first.]

## Tests

### 1. vim alternate-screen round-trip (Windows client)
expected: vim --noplugin opens to a blank canvas (no shell bleed-through); after :q the primary buffer + cursor are restored exactly. [TUI-01, SC#1; re-test on Windows]
result: issue
reported: "Tested vim - it isn't great. When I first open it I got a quick disconnect as nosh fell into 'reliable mode'. Looks like the screen is now too small when in VIM. After reconnect, recovered + vim usable."
severity: major

### 2. htop rendering vs reference terminal (Windows client)
expected: htop renders columns, bars, and CPU meters aligned/correct side-by-side against the same htop in a reference terminal — no garbled output, no missing spaces. [TUI-04, SC#1]
result: [pending]

### 3. Claude Code / full-screen TUI — no predictor overlay (Windows client)
expected: running Claude Code (or another full-screen TUI) over nosh shows no speculative-echo overlay flicker while typing inside the alternate screen; rendering is correct. [TUI-05, SC#1]
result: [pending]

### 4. tmux alt-screen + scrollback interaction (Windows client — operator-chosen 4th)
expected: tmux enters/exits its alternate screen cleanly; entering copy-mode / scrolling back shows correct history; on detach/exit the underlying shell screen is restored without residue. (Stresses alt-screen + scrollback together.) [SC#1 fourth scenario]
result: [pending]

### 5. CJK + emoji column accuracy (999.3 rendering pack)
expected: pasting `中文流语` at the prompt causes no column drift; a ZWJ emoji sequence advances the cursor correctly (renders as one cluster, cursor lands in the right column). [TUI-03 / 999.3]
result: [pending]

### 6. Mid-session Ctrl-L full-screen clear (999.3)
expected: type a few lines, then press Ctrl-L — the whole screen clears at once (cursor home, prior lines gone), NOT one line at a time. [999.3 human_needed item BUG-H]
result: [pending]

### 7. read -s noecho + Enter line-advance + predictive echo (Windows client) (999.4 D-02 / 999.3)
expected: at a `read -s` password prompt, typed secret characters are structurally suppressed (no echo, no predicted chars); pressing Enter advances the line (prompt drops a line) rather than stalling; fast-typing / holding a key (typematic) in vim and bracketed-paste of a multi-line block show no predictive-echo glitch over a real RTT. [999.4 D-02 + 999.3 typematic/read-s items]
result: issue
reported: "Tested the SECRET read: the first enter is better as it moves the cursor correctly. But the second one now places the cursor at position 0, not at the end of the prompt on the next line."
severity: major

### 8. CI green — build-windows + cargo audit
expected: the latest `main` CI run shows the `build-windows` job and the `cargo audit` job both green. [SC#1 support; orchestrator verifies via gh, operator confirms]
result: [pending]

### 9. PTY echo latency budget — p50 ≤ 5 ms on live hardware (23-HUMAN-UAT #1)
expected: on representative hardware under normal load, the predictive/echo PTY round-trip median (p50) stays at or under the 5 ms budget asserted by `channel_echo_roundtrip` (the prior flake was load-induced scheduler jitter, not a refactor regression). [23 deferred item]
result: [pending]

### 10. Mode A connect + blocking TOFU prompt (M7 / UAT-02)
expected: the Windows/Linux client connects to a nosh server bound to UDP/443 directly (no proxy, WebTransport Mode A). On first contact a BLOCKING TOFU prompt appears showing the server's SHA-256 hex fingerprint and requires an explicit typed `yes` to proceed; a non-`yes` answer aborts the connection (no silent accept). [SC#2, SC#3, D-04]
result: [pending]

### 11. Interactive shell + scrollback + predictive echo over Mode A (M7 / UAT-02)
expected: after TOFU accept, an interactive shell runs over the Mode A WebTransport tunnel; scrollback sync works; predictive local echo behaves as on native QUIC. [SC#2, D-04]
result: [pending]

### 12. Simulated network change → transparent reattach over Mode A (M7 / UAT-02)
expected: with the client on a different network, force a network change (Wi-Fi↔cellular / VPN toggle); the session resumes transparently — byte-exact replay of pre-change output, NO re-auth/TOFU re-prompt, shell continues where it left off. [SC#4, D-04]
result: [pending]

## Summary

total: 12
passed: 0
issues: 2
pending: 5
skipped: 0
blocked: 5

## Gaps

- truth: "Windows nosh client compiles (prerequisite for UAT-01 tests 1-7 and CI test 8)"
  status: fixed
  reason: "Operator hit E0599 building on Windows: keys.rs::record_known_host called OpenOptions::mode(0o600) unconditionally, but mode() is a Unix-only OpenOptionsExt method (the #[cfg(unix)] gated only the `use` import at keys.rs:206, not the call at :211). Introduced during v1.4 TOFU file-hardening; broke the Windows build and the build-windows CI job."
  severity: blocker
  test: 1
  root_cause: "Unconditional .mode(0o600) on fs::OpenOptions in record_known_host (keys.rs:211); Unix-only API not cfg-gated for Windows."
  fix: "cfg(unix)-gate the mode() call inside a block (keys.rs); on Windows the known_hosts file is created with default ACLs (documented limitation, windows-client-test.md §2; mirrors signer.rs not(unix) path). Verified: native cargo test -p nosh-auth green (25 passed). Windows cross-check pending operator rebuild."
  artifacts:
    - path: "crates/nosh-auth/src/keys.rs"
      issue: "Unconditional Unix-only .mode(0o600) broke Windows build"
  debug_session: ""

- truth: "Full-screen TUIs (vim etc.) render at the client's actual terminal size, including after a reconnect"
  status: failed
  reason: "User reported: vim renders into a too-small screen after a quick disconnect/reconnect on first open (nosh briefly 'fell into reliable mode' = transport hiccup → reconnect). Session recovered and vim was usable but sized too small."
  severity: major
  test: 1
  root_cause: "HYPOTHESIS (needs diagnosis): terminal size not re-applied to the server PTY after a reconnect, so the PTY stays at a stale/default size (likely 80x24) while the client window is larger. Possibly compounded by a Windows datagram (quinn_udp WSAEMSGSIZE/GSO) hiccup during vim's heavy initial repaint triggering the reconnect. Display flows exclusively via the datagram path (main.rs:459-462), so a degraded datagram path also degrades rendering."
  artifacts:
    - path: "crates/nosh-client/src/main.rs"
      issue: "resize not re-sent on reconnect? (resize poll vs reattach) — confirm"
  missing:
    - "Re-send current terminal size on reconnect/reattach (or verify the reattach path carries it)"
    - "Investigate Windows datagram degradation under heavy repaint (WSAEMSGSIZE/GSO)"
  debug_session: ""

- truth: "read -s: each Enter's predicted cursor lands at the next prompt's end column once confirmed, not stuck at column 0"
  status: failed
  reason: "User reported: first Enter moves the cursor correctly, but the SECOND Enter places the cursor at column 0 instead of at the end of the prompt on the next line."
  severity: major
  test: 7
  root_cause: "HYPOTHESIS (needs diagnosis): PredictEnter predicts cursor (row+1, col 0) and relies on sync_cursor_from_confirmed to snap to the real prompt column once the server confirms (predictor.rs:458-485). On the second consecutive Enter/epoch the re-sync does not fire (or confirmed cursor is read as col 0), so the predicted col-0 sticks."
  artifacts:
    - path: "crates/nosh-client/src/predictor.rs"
      issue: "PredictEnter col-0 prediction not re-synced to confirmed prompt column on 2nd epoch"
  missing:
    - "Adversarial repro: two consecutive read -s prompts; assert predicted cursor snaps to prompt-end col on the 2nd Enter"
  debug_session: ""

- truth: "Reliable-stream framing stays in sync; the client does not misread payload as a frame-length prefix, and reattach/replay does not desync the stream"
  status: failed
  reason: |
    User reported a garbled screen that clear/Ctrl-L/closing vim could NOT recover, with the prompt marker offset by several rows. CLIENT LOG (smoking gun):
      WARN nosh_client: reliable stream error, triggering reconnect: frame too large: 1718183741 bytes (max 16777216)
    1718183741 = 0x6669673D = ASCII "fig=" — terminal payload bytes read as a u32 BE frame-length prefix (codec.rs:67-71). The reliable-stream length-prefixed framing desynced; the bogus 1.7GB length exceeds MAX_FRAME_LEN (16 MiB) → error → reconnect → reattach+replay → desync again → reconnect LOOP.
    SERVER LOG corroboration: connection accepted; "reattach accepted"; "replay complete replaying_from_seq=10 chunks=9 truncated=false"; then "transport lost during reattach; re-orphaning"; new connection; session open term=xterm-256color cols=270 rows=72 (so size WAS sent correctly — 270x72); then "reattach rejected" (token consumed by the loop); "transport lost; orphaning session". A classic reconnect/reattach storm.
  severity: blocker
  test: 1
  root_cause: |
    HYPOTHESIS (needs diagnosis): the reliable-stream framing desyncs — most likely in the reattach-replay write path (server replays 9 PtyData chunks over the reliable stream on reattach; the desync appears right at "replay complete"). A frame written with a length prefix that doesn't match its body length, OR raw bytes written to the reliable stream without a length prefix, would shift every subsequent read so a later read interprets payload ("fig=") as a 4-byte length. Likely PLATFORM-AGNOSTIC (framing/replay), not the documented Windows datagram quirk. Display flows exclusively via the datagram path (main.rs:459-462), so the reliable-stream desync + reconnect loop is what corrupts the screen and prevents recovery. This also threatens the M7 reattach test (12), which exercises the same replay path over WebTransport.
  artifacts:
    - path: "crates/nosh-proto/src/codec.rs"
      issue: "Length-prefixed framing (u32 BE + body); desync read 0x6669673D as length"
    - path: "crates/nosh-server/src/server.rs"
      issue: "Reattach replay write path (replay complete chunks=9) — suspected source of framing desync"
  missing:
    - "Diagnose: audit reattach-replay write path for a frame-length/body mismatch or an unframed raw write to the reliable stream"
    - "Adversarial regression test: reattach with N replay chunks; assert the client reads every frame without a FrameTooLarge/desync"
    - "Confirm whether a fresh connect (no reconnect/reattach) renders cleanly — isolates replay-path vs general framing"
  debug_session: ""
  blocks_tests: [2, 3, 4, 5, 6, 12]  # Windows visual items + M7 reattach all ride the affected path
