---
phase: 09-windows-client-polish-hardening
reviewed: 2026-05-30T00:00:00Z
depth: standard
files_reviewed: 7
files_reviewed_list:
  - crates/nosh-client/src/client.rs
  - crates/nosh-client/src/main.rs
  - crates/nosh-auth/src/keys.rs
  - crates/nosh-auth/src/signer.rs
  - crates/nosh-server/src/server.rs
  - crates/nosh-client/Cargo.toml
  - Cargo.toml
findings:
  critical: 1
  warning: 2
  info: 3
  total: 6
status: issues_found
---

# Phase 9: Code Review Report

**Reviewed:** 2026-05-30
**Depth:** standard
**Files Reviewed:** 7
**Status:** issues_found

## Summary

Phase 9 adds Windows VT console-input mode, an ssh-style `~.` escape state machine, authorized_keys warn+skip, a client connect timeout, server connection-migration logging, and a gated Windows-only import. The security-isolation goal for the escape machine (T-09-01: server cannot inject `~.` to force a local disconnect) is correctly met — server `PtyData` never enters `EscapeState`. The authorized_keys warn+skip is fail-closed (T-09-04 holds). The Windows console handle setup is logically correct for the flag selection and restoration order.

One blocker-level bug was found: the escape state machine uses LF (`\n`, 0x0A) as the line-start trigger, but a raw-mode terminal delivers CR (`\r`, 0x0D) when the user presses Enter. This makes `~.` unreachable after any Enter keypress in normal interactive use. Two warnings and three info items follow.

## Structural Findings (fallow)

No structural pre-pass was provided for this phase.

## Narrative Findings (AI reviewer)

## Critical Issues

### CR-01: Escape state machine uses `\n` as line-start trigger, but raw mode sends `\r` from Enter

**File:** `crates/nosh-client/src/main.rs:115,138,147`

**Issue:** The `EscapeState` machine transitions to `LineStart` only when it sees byte `b'\n'` (LF, 0x0A). In a raw-mode terminal on both Unix and Windows, pressing Enter delivers `\r` (CR, 0x0D) — the ICRNL/ONLCR mappings that convert CR to LF are disabled when `crossterm::terminal::enable_raw_mode()` is called. As a result, after any Enter keypress, the machine transitions from `LineStart` to `MidLine` (via the `else → MidLine` branch for non-`\n` bytes) and never returns to `LineStart` again through normal interactive input. The escape sequence `~.` is therefore inaccessible in practice after the first Enter keypress; it only works at the very beginning of a session before any command is typed.

OpenSSH's implementation (`clientloop.c`) tracks `last_was_cr`, setting it to 1 after either `\r` or `\n`, and recognises the escape character only when `last_was_cr` is set. The nosh machine is modelled on this design but omits the `\r` case.

The tests (`newline_resets_to_line_start_enabling_escape`) pass `b"\n~."` to demonstrate the mechanism, which is correct for LF but does not cover the CR case that a user actually sends.

**Severity:** BLOCKER — the primary user-controlled disconnect mechanism (`~.`) is non-functional after typing any command and pressing Enter in an interactive session. A user stuck on a hung remote shell has no software escape.

**Fix:** Treat `\r` as an additional line-start trigger alongside `\n`. Match OpenSSH: transition to `LineStart` on `\r` or `\n` in every branch. Specifically, replace `byte == b'\n'` with `matches!(byte, b'\n' | b'\r')` at the three state-transition sites:

```rust
// In EscapeState::LineStart arm (line ~115):
*self = if matches!(byte, b'\n' | b'\r') {
    EscapeState::LineStart
} else {
    EscapeState::MidLine
};

// In EscapeState::SeenTilde arm for ~<other> (line ~138):
*self = if matches!(byte, b'\n' | b'\r') {
    EscapeState::LineStart
} else {
    EscapeState::MidLine
};

// In EscapeState::MidLine arm (line ~147):
if matches!(byte, b'\n' | b'\r') {
    *self = EscapeState::LineStart;
}
```

Add a companion test to the existing `escape_tests` module:

```rust
#[test]
fn carriage_return_resets_to_line_start_enabling_escape() {
    let mut s = EscapeState::new();
    let (_, _) = run(&mut s, b"x");      // → MidLine
    let (fwd, quit) = run(&mut s, b"\r~.");
    assert!(quit, "~. after \\r must quit");
    assert_eq!(fwd, b"\r", "\\r before ~. must be forwarded");
}
```

---

## Warnings

### WR-01: `RawModeGuard::drop` calls `SetConsoleMode` on unvalidated handles

**File:** `crates/nosh-client/src/client.rs:398-402`

**Issue:** In `Drop`, `GetStdHandle(STD_INPUT_HANDLE)` and `GetStdHandle(STD_OUTPUT_HANDLE)` are called without checking the returned values before passing them to `SetConsoleMode`. If the process detaches from its console between `enable()` and the `Drop` (unusual but possible in some hosting environments), `GetStdHandle` can return `INVALID_HANDLE_VALUE` (-1 as `isize`). The subsequent `SetConsoleMode(INVALID_HANDLE_VALUE, ...)` call will fail with `ERROR_INVALID_HANDLE`, which is silently ignored — so the terminal restoration does not crash. However, `crossterm::terminal::disable_raw_mode()` then also runs against a detached console and may log its own error. The "best effort in Drop" comment correctly documents the intent, but the code is more permissive than documented.

More critically, `GetStdHandle` can also return NULL (0) when a process has no attached standard handles (e.g., created with `DETACHED_PROCESS`). In the `enable()` path this is caught correctly (NULL passes the `INVALID_HANDLE_VALUE` check, `GetConsoleMode(NULL, ...)` fails, and `enable()` returns `Err`). But if that failure path is reached, `Ok(Self {...})` is never returned, so `Drop` is never called for that guard instance — the invariant holds. The residual concern is documentation accuracy and the subtle reliance on `GetConsoleMode` failing for NULL to avoid the guard being constructed in a bad state.

**Fix:** Add explicit NULL checks in `Drop` to match the intent, and document the invariant:

```rust
#[cfg(windows)]
{
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::System::Console::{
        GetStdHandle, SetConsoleMode, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    };
    let stdin_handle = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
    let stdout_handle = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
    // Only restore if we have plausibly valid handles; ignore errors regardless.
    if stdin_handle != INVALID_HANDLE_VALUE && !stdin_handle.is_null() {
        let _ = unsafe { SetConsoleMode(stdin_handle, self.orig_stdin_mode) };
    }
    if stdout_handle != INVALID_HANDLE_VALUE && !stdout_handle.is_null() {
        let _ = unsafe { SetConsoleMode(stdout_handle, self.orig_stdout_mode) };
    }
}
```

Note: `windows_sys::Win32::Foundation::HANDLE` is `isize`; `is_null()` is not available on `isize` directly — use `stdin_handle == 0` for the NULL check instead.

---

### WR-02: Comment in `RawModeGuard::enable` shows identical numeric values for unrelated flags on different handles

**File:** `crates/nosh-client/src/client.rs:319-322`

**Issue:** The comment block listing Windows console flag values shows:

```
// ENABLE_ECHO_INPUT              0x0004 — cleared by crossterm already
// ENABLE_VIRTUAL_TERMINAL_PROCESSING 0x0004 — stdout handle flag (different handle)
```

Both `ENABLE_ECHO_INPUT` and `ENABLE_VIRTUAL_TERMINAL_PROCESSING` are listed as `0x0004`. The values are correct for their respective handles (stdin vs stdout), and the parenthetical `(different handle)` is present. However, a reviewer reading this table in isolation will see the same hex value for two flags and question the accuracy, potentially leading to an incorrect edit that applies `ENABLE_VIRTUAL_TERMINAL_PROCESSING` as a stdin flag (which would conflict with `ENABLE_ECHO_INPUT`). A more defensive future reviewer might corrupt the flag logic while "fixing" what looks like a typo.

**Fix:** Rewrite the comment to make the handle distinction structurally unambiguous, grouping stdin and stdout flags separately:

```
// Stdin handle flags:
//   ENABLE_PROCESSED_INPUT         0x0001 — must be CLEARED (Ctrl-C → 0x03)
//   ENABLE_LINE_INPUT              0x0002 — cleared by crossterm already
//   ENABLE_ECHO_INPUT              0x0004 — cleared by crossterm already
//   ENABLE_VIRTUAL_TERMINAL_INPUT  0x0200 — must be SET (ANSI escape sequences)
//
// Stdout handle flags (different handle; numeric values are independent):
//   ENABLE_VIRTUAL_TERMINAL_PROCESSING 0x0004 — must be SET (render ANSI from server)
```

---

## Info

### IN-01: `EscapeState` resets to `LineStart` on every reconnect, losing mid-line context

**File:** `crates/nosh-client/src/main.rs:584`

**Issue:** `EscapeState::new()` (which initialises to `LineStart`) is called at the top of `run_pump`. Since `run_pump` is called fresh on each reconnect (from both `fresh_session` and `reattach_session`), the escape state is reset after every transport drop. If the user was mid-line before the connection dropped, the first character they type after reconnect will be processed as if it were at line-start. In practice this means a `~` typed immediately after reconnect would enter `SeenTilde` state rather than being forwarded literally. OpenSSH also resets per logical session, so this matches prior art, but it is undocumented and potentially surprising.

The severity is minor because: (a) this is consistent with OpenSSH's behavior, (b) given CR-01, the machine is almost always at `LineStart` already (since `\r` does not advance past `LineStart`), and (c) after a transport drop, the user's context is also disrupted.

**Fix:** Document the design intent. Once CR-01 is fixed, consider whether to preserve `EscapeState` across reconnects by moving it to the outer reconnect loop scope (before `loop {`), passing it by mutable reference into `run_pump`. This would match the intuition that "the session is the same; only the transport changed." If the current per-reconnect-reset behavior is intentional, add a comment explaining the decision.

---

### IN-02: Migration poll timer fires immediately on first tick

**File:** `crates/nosh-server/src/server.rs:406`

**Issue:** `tokio::time::interval` delivers its first tick immediately when the `select!` loop starts. The first `migration_poll.tick()` arm fires at session open, reads `conn.remote_address()`, and compares it against `last_seen_addr` (which was set to the same value two lines earlier). Since they are equal, no log is emitted and no state changes — the tick is a pure no-op. `MissedTickBehavior::Skip` is set correctly, preventing any spin risk. The only cost is one extra async wakeup and one call to `conn.remote_address()` at session start.

**Fix:** Not strictly necessary. If the spurious initial tick is a concern, use `tokio::time::interval_at(tokio::time::Instant::now() + Duration::from_millis(500), Duration::from_millis(500))` to delay the first tick by one interval period. This is a minor efficiency improvement.

---

### IN-03: `authorized_keys` warn log's `key_type` field could contain the entire line for malformed inputs without whitespace

**File:** `crates/nosh-auth/src/keys.rs:128`

**Issue:** The warn log in `load_authorized_keys` extracts `key_type` via `line.split_whitespace().next().unwrap_or("<empty>")`. For a well-formed but unsupported key line (e.g., `ssh-rsa AAAA...`), this gives `"ssh-rsa"` — benign and short. For a malformed line without any whitespace (e.g., a line that is one long base64 blob), `split_whitespace().next()` returns the entire line as `key_type`. This could log a multi-kilobyte string as a single structured field. The full key material (public key bytes) could appear in the log in base64 form, which is a minor information-disclosure concern (the key is public, not secret) and a log-volume concern.

The D-07 invariant ("log only key-type and reason, never the full line or key material") is technically violated for this edge case because a malformed line with no whitespace produces a `key_type` that equals the full line content.

**Fix:** Cap the `key_type` field to a reasonable length before logging:

```rust
let key_type_raw = line.split_whitespace().next().unwrap_or("<empty>");
let key_type = if key_type_raw.len() > 64 {
    "<malformed-no-whitespace>"
} else {
    key_type_raw
};
```

---

_Reviewed: 2026-05-30_
_Reviewer: Claude (gsd-code-reviewer)_
_Depth: standard_
