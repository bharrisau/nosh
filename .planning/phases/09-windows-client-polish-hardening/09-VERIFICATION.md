---
phase: 09-windows-client-polish-hardening
verified: 2026-05-30T00:00:00Z
status: human_needed
score: 6/6 must-haves verified (Linux-verifiable portions); 3 require Windows-host runtime confirm
re_verification:
  previous_status: none
  note: initial verification
human_verification:
  - test: "Windows host: run vim/less in a nosh session; arrows, PageUp/PageDown navigate; bare Ctrl-C interrupts the remote command (does NOT exit nosh-client with code 130). After quitting nosh, console echo + line editing are restored."
    expected: "VT input mode active; special keys arrive as ANSI; Ctrl-C forwarded as 0x03; console restored on exit (incl. error/panic paths)."
    why_human: "Requires a native Windows console; cannot compile for Windows here (no mingw). Win32 console-mode behavior is runtime-observable only."
  - test: "Windows host: confirm the `~.` escape disconnects and `~~` sends a literal tilde when typed at a real raw-mode Windows console (after pressing Enter, i.e. via \\r)."
    expected: "`~.` quits locally without sending bytes to the server; `~~` sends one `~`."
    why_human: "Escape LOGIC is fully verified on Linux (unit tests incl. \\r case). End-to-end behavior in the Windows console raw-input path is runtime-only."
  - test: "Windows host: `cargo build` on Windows emits no unused-import warning for std::path::PathBuf in nosh-auth/src/signer.rs."
    expected: "No warning on the Windows build."
    why_human: "Warning-absence on the Windows target cannot be checked from this Linux host."
---

# Phase 9: Windows Client Polish & Hardening Verification Report

**Phase Goal:** The nosh client is usable day-to-day from a native Windows console (real-key TUI input works) and degrades gracefully, with better server observability — closing the gaps from the 2026-05-30 live Windows→Linux validation.

**Verified:** 2026-05-30
**Status:** human_needed
**Re-verification:** No — initial verification

## Goal Achievement

### Observable Truths (the 6 ROADMAP Success Criteria)

| # | Truth | Status | Evidence |
|---|-------|--------|----------|
| 1 | Windows console set to VT INPUT mode (VT_INPUT on; PROCESSED/LINE/ECHO cleared); stdout VT_PROCESSING on; both original modes saved + restored on exit | ✓ VERIFIED (static) / human_needed (runtime) | client.rs:327-385 sets `ENABLE_VIRTUAL_TERMINAL_INPUT` and clears `ENABLE_PROCESSED_INPUT \| ENABLE_LINE_INPUT \| ENABLE_ECHO_INPUT` on stdin (line 352-353); stdout gets `ENABLE_VIRTUAL_TERMINAL_PROCESSING` (line 376). Both originals stored in struct (300-303). Drop (392-423) restores both BEFORE `disable_raw_mode`, with WR-01 guard (`!= INVALID_HANDLE_VALUE && != 0`) at 415/418. Named windows-sys constants used throughout — no magic numbers in code. `#[cfg(not(windows))]` → `Ok(Self {})`: no Unix symbol leak. Compiles via cfg but Windows runtime not buildable here. |
| 2 (logic) | ssh-style `~.` escape: line-start `~`; `~.`→quit; `~~`→literal `~`; `~`+other→both forwarded; fed ONLY local stdin (server output cannot trigger disconnect); CR-01 fix (`\r` triggers line-start) | ✓ VERIFIED | main.rs:110-162 — all 3 transition sites use `matches!(byte, b'\n' \| b'\r')` (lines 120, 144, 154): **CR-01 FIX CONFIRMED**. Security (T-09-01): run_pump (main.rs:632-637) writes server `PtyData` directly to stdout; `escape.process()` fed only `stdin_buf` (663); `~.` returns UserQuit forwarding nothing (664-667). EscapeState persisted across reads (619) → partial-escape across read boundaries handled. 9 unit tests pass incl. 2 CR regression tests driving escape via `\r`. |
| 3 | authorized_keys warn+skip (sshd behavior); mixed file loads exactly the Ed25519 key; fail-closed; no key material logged | ✓ VERIFIED | keys.rs:114-146 — `match from_openssh_line` Ok→push / Err→`tracing::warn` + implicit continue (loop). Accepted set is strict subset of `from_openssh_line` acceptance (unchanged) → fail-closed; cannot admit wrong/unauthorized key. IN-03 cap present (132-134, 64-char). Only `key_type` token + error logged, never key material (D-07). 3 tests pass (mixed→1, all-bad→0 Ok, fail-closed subset). |
| 4 | Client connect() timeout (~10s default, `--connect-timeout`); clear error; reattach/reconnect not regressed | ✓ VERIFIED | client.rs:187-206 wraps `connecting` future in `tokio::time::timeout(connect_timeout, ...)`; error names addr:port + timeout. main.rs:313-314 `--connect-timeout` default 10; main.rs:419,451 threaded to the single connect call site in the supervisor loop (covers fresh + reattach; reattach reuses conn via `open_bi`, not a new connect). All connect call sites updated (tests use 30s). Workspace tests green. |
| 5 | Server INFO log on actual remote_address change; loop not broken; no spin | ✓ VERIFIED | server.rs:401-451 — `last_seen_addr` baseline (405), 500ms interval w/ `MissedTickBehavior::Skip` (406-407, no spin); migration_poll arm logs INFO `session_id` + `old→new` ONLY when `cur != last_seen_addr` (442-450); arm does not break loop; other arms (shell exit, PTY out, client frames) untouched. |
| 6 | Windows-only unused PathBuf import gated `#[cfg(unix)]` | ✓ VERIFIED (static) / human_needed (Windows warning-absence) | signer.rs:18-19 `#[cfg(unix)] use std::path::PathBuf;`. PathBuf used only by `#[cfg(unix)]` AgentSigner (45, 54). Unix build warning-free. Windows warning-absence needs Windows host. |

**Score:** 6/6 must-haves verified for all Linux-verifiable portions. Items 1, 6, and the runtime of item 2 carry non-blocking Windows-host human confirmation.

### Required Artifacts

| Artifact | Expected | Status | Details |
|----------|----------|--------|---------|
| `crates/nosh-client/src/client.rs` | RawModeGuard Windows VT save/set/restore + timeout-wrapped connect | ✓ VERIFIED | 297-424 (guard), 187-220 (connect timeout). Substantive, cfg-gated, wired. |
| `crates/nosh-client/src/main.rs` | EscapeState machine wired into run_pump stdin; `--connect-timeout` | ✓ VERIFIED | 77-163 (machine), 619/663-667 (wiring), 313-314/419/451 (flag). |
| `crates/nosh-client/Cargo.toml` | windows-sys target dep | ✓ VERIFIED | 41-46 `[target.'cfg(windows)'.dependencies]` windows-sys workspace = true; workspace pin 0.59 w/ Win32_System_Console + Win32_Foundation (Cargo.toml:30). |
| `crates/nosh-auth/src/keys.rs` | warn+skip parser | ✓ VERIFIED | 114-146, IN-03 cap. |
| `crates/nosh-auth/src/signer.rs` | cfg(unix)-gated PathBuf | ✓ VERIFIED | 18-19. |
| `crates/nosh-server/src/server.rs` | remote_address change detect + INFO log | ✓ VERIFIED | 401-451. |

### Key Link Verification

| From | To | Via | Status | Details |
|------|----|----|--------|---------|
| RawModeGuard::enable | windows-sys SetConsoleMode | #[cfg(windows)] after enable_raw_mode | ✓ WIRED | client.rs:355,378 |
| run_pump stdin arm | EscapeState::process | filter stdin before send_input | ✓ WIRED | main.rs:663 |
| main.rs Args | client::connect timeout | --connect-timeout threaded | ✓ WIRED | main.rs:419,451 |
| keys.rs load_authorized_keys | tracing::warn | Err arm logs + continues | ✓ WIRED | keys.rs:137 |
| run_session loop | tracing::info migration | poll remote_address vs last_seen | ✓ WIRED | server.rs:440-450 |

### Behavioral Spot-Checks

| Behavior | Command | Result | Status |
|----------|---------|--------|--------|
| Escape state machine (incl. CR-01) | `cargo test --bin nosh-client escape` | 9 passed; 0 failed (incl. 2 `\r` regression tests) | ✓ PASS |
| authorized_keys warn+skip | `cargo test -p nosh-auth authorized_keys` + `fail_closed` | 3 + 1 passed | ✓ PASS |
| Full workspace (regression) | `cargo test --workspace` | 91 passed; 0 failed; 3 ignored | ✓ PASS |
| Migration test (de-flake check) | included in workspace run | `migration_survives_path_change ... ok` (4.56s) | ✓ PASS (not re-flaked) |
| Clean build (Linux) | `cargo build --workspace` | no warnings, no errors | ✓ PASS |

### Anti-Patterns Found

| File | Line | Pattern | Severity | Impact |
|------|------|---------|----------|--------|
| (none) | — | No TODO/FIXME/XXX/HACK/TBD/unimplemented! in any of the 5 modified source files | — | Clean |

### Gaps Summary

No gaps. All six ROADMAP success criteria are implemented and verified against the actual code:

- The CR-01 BLOCKER (escape only triggered on `\n`, unreachable in raw mode) is genuinely fixed — all three EscapeState transition sites use `matches!(byte, b'\n' | b'\r')`, and two new regression tests drive the escape via `\r` (carriage_return_resets_to_line_start_enabling_escape, carriage_return_mid_line_tilde_dot_is_literal). Both pass.
- The escape security property (T-09-01) holds by code trace: server PtyData is written to stdout and never enters EscapeState; only local stdin feeds the machine; `~.` quits without forwarding bytes.
- authorized_keys warn+skip is fail-closed (accepted set is a strict subset of from_openssh_line acceptance), with IN-03 log-hygiene cap and no key-material logging.
- connect timeout (default 10s, `--connect-timeout`) wraps establishment only; the single supervisor-loop call site covers both fresh and reattach; reattach reuses the connection via open_bi.
- server migration logging fires INFO only on an actual address change, never breaks the loop, 500ms Skip cadence (no spin).
- signer.rs PathBuf import is `#[cfg(unix)]`-gated.

Three items carry **non-blocking** Windows-host runtime confirmation (consistent with Phase 8): Windows VT console behavior (item 1), the end-to-end Windows escape behavior (runtime of item 2), and the Windows-build warning-absence for PathBuf (item 6). These cannot be exercised from this Linux host (no mingw). All logic is fully verified on Linux. This is the expected terminal state — **human_needed, not gaps_found**.

---

_Verified: 2026-05-30_
_Verifier: Claude (gsd-verifier, opus, adversarial pass)_
