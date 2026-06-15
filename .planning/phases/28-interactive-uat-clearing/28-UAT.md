---
status: testing
phase: 28-interactive-uat-clearing
source: [19-HUMAN-UAT.md, 23-HUMAN-UAT.md, 999.3-VERIFICATION.md, 999.4-VERIFICATION.md, ROADMAP.md Phase 28 SC1-4]
started: "2026-06-14"
updated: "2026-06-14"
---

## Current Test
<!-- OVERWRITE each test - shows where we are -->

number: 1
name: vim alternate-screen round-trip (Windows client)
expected: |
  On the Windows nosh client against the Linux server, `vim --noplugin <file>`
  opens to a blank canvas (no shell text bleeding through the alternate screen);
  after `:q`, the primary buffer and cursor are exactly as they were before vim
  launched (clean alt-screen enter/exit, no residue).
awaiting: user response

## Tests

### 1. vim alternate-screen round-trip (Windows client)
expected: vim --noplugin opens to a blank canvas (no shell bleed-through); after :q the primary buffer + cursor are restored exactly. [TUI-01, SC#1; re-test on Windows]
result: [pending]

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
result: [pending]

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
issues: 0
pending: 12
skipped: 0
blocked: 0

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
