# Phase 3: PTY Session Core - Context

**Gathered:** 2026-05-29
**Status:** Ready for planning

<domain>
## Phase Boundary

An authenticated QUIC connection (from Phase 2) spawns a real PTY login shell on Linux. Keystrokes flow client→server→PTY stdin; shell output flows PTY→server→client; window resize (SIGWINCH) propagates with coalescing; signals (Ctrl-C) reach the foreground process group; the client terminal is in raw mode and restored on any exit; environment is sanitized on shell open; the remote exit code propagates to the client process; the connection closes cleanly with a structured reason; a server-side session struct holds state for future M3 reattach (reattach itself NOT implemented); session events are traced. Replaces the Phase 2 echo loops. Covers SESS-01..11. No predictive echo (M4), no scrollback/forwarding/multiplexing (M5).

</domain>

<decisions>
## Implementation Decisions

### Shell I/O stream layout
- **D-01:** A single bidirectional QUIC stream carries the whole session. Everything — `SessionOpen`, PTY data both directions, `Resize`, `SessionClose` — is framed via the existing `nosh-proto` postcard `Message` codec (extend the `Message` enum with the needed variants). Reuses Phase 1 framing, preserves ordering, and is the clean base for M5's channel model. No raw-bytes side channel this milestone.
- **D-02:** Datagrams are NOT used for shell I/O this milestone (predictive-echo state-sync is M4). The datagram path from Phase 1 stays exercised by tests but carries no session traffic yet.

### Shell selection & login semantics
- **D-03:** The server looks up the authenticated user's shell from `/etc/passwd` and spawns it as a **login shell** (argv[0] prefixed with `-`, e.g. `-bash`), so the user's login profile runs — faithful to an SSH session. (The authenticated user identity comes from Phase 2's auth; for the spike the server runs the shell as the account it runs under — privilege-dropping/setuid is out of scope, note it.)
- **D-04:** PTY allocated via `portable-pty` `native_pty_system()` (already the ConPTY backend on Windows, keeping the M6 seam clean). Initial `PtySize` set from the client's reported rows×cols before spawn.

### Client env / locale forwarding
- **D-05:** At session open the client forwards `TERM`, the window size, and its **whitelisted** locale vars (`LANG`, `LC_*`, `TZ`) so the remote session matches the local terminal's locale/encoding. SSH `SendEnv`-style.
- **D-06:** Env sanitization (SESS-07, locked) is applied at shell spawn: the server builds the child env from a **whitelist** (`TERM`, `LANG`/`LC_*`, `TZ`, plus the shell-provided `HOME`/`USER`/`SHELL`/`PATH` baseline) and NEVER inherits or accepts client-supplied `LD_*`, `DYLD_*`, `BASH_ENV`, `ENV`, `IFS`, `SHELLOPTS`, `PYTHONPATH`, `NODE_OPTIONS`. `SSH_AUTH_SOCK` is never set from client input (agent forwarding is M5, dedicated channel). Implement as deny-by-default: only whitelisted client vars pass.

### Disconnect behavior
- **D-07:** On client disconnect (no reattach this milestone): send the shell `SIGHUP`, `wait()`/reap the child (via `spawn_blocking`), and free the PTY — no zombies, no orphans (satisfies the success criterion). The session struct exists but its process is not kept running.
- **D-08:** Server-side session struct (SESS-10): a discrete type holding `session_id` (UUID), the SSH identity (from Phase 2 auth), the PTY handle, shell PID, and `idle_since`. It is the structural seam M3 reattach attaches to; do NOT inline the whole session into the connection handler. Reattach is NOT implemented.

### Exit & close (locked criteria, recorded for clarity)
- **D-09:** Remote shell exit status is captured via `Child::wait()` (on `spawn_blocking`) and sent as `SessionClose { exit_code, reason }` (the `Message` variant already stubbed in Phase 1) before the connection closes; the client process exits with that code (SESS-08). Use QUIC application error codes for the connection close with a structured reason (shell exited / auth failed / server shutdown) (SESS-09).

### Client raw mode & resize
- **D-10:** Client puts its local terminal into raw mode and restores it via an RAII guard that fires on normal exit, panic, AND abrupt network loss (SESS-03). Ctrl-C etc. pass through as bytes; the server PTY line discipline delivers signals to the foreground group (SESS-06) — no special signal handling beyond raw mode + PTY.
- **D-11:** Client catches `SIGWINCH`, debounces/coalesces (~30–50 ms), and sends a `Resize` message; server calls `resize()` on the `MasterPty` (SESS-05).

### Claude's Discretion
- Exact `Message` enum variants/shape for `SessionOpen`/`PtyData`/`Resize` and the framing details.
- Bridging blocking `portable-pty` reader/writer to tokio (`spawn_blocking` vs `AsyncFd`) — research says `spawn_blocking` is fine for the spike.
- Output read-chunk size and reliance on QUIC stream flow control for backpressure (no custom per-channel windows this milestone — that's M5).
- Exact resize debounce interval within ~30–50 ms; tracing span field details (SESS-11: session_id, peer_addr, username).
- Whether `--shell` override exists (default is the passwd login shell, D-03).
- How the integration test drives an interactive shell headlessly (e.g. spawn server+client in-process, write `echo` / `exit 42`, assert output and exit code; a PTY-aware assertion for `stty size`).

</decisions>

<specifics>
## Specific Ideas

- Reuse, don't reinvent: the `SessionClose` Message variant already exists from Phase 1 — extend the same enum for the rest. The Phase 2 echo loops in `handle_connection` are what get replaced by the session pump.
- Keep the M6 (Windows/ConPTY) and M3 (reattach) seams clean: PTY behind `portable-pty`'s `native_pty_system()`, session as a discrete struct — but do NOT build either feature now.
- "spike done" bar (from research FEATURES.md): vim/htop render correctly, resize reflows, Ctrl-C interrupts sleep, `exit 42` → client exits 42, SIGKILL'd client leaves terminal usable + no zombie children, env has TERM/LC_*/TZ but not LD_PRELOAD/BASH_ENV/SSH_AUTH_SOCK.

</specifics>

<canonical_refs>
## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### Phase scope & prior decisions
- `.planning/PROJECT.md` — scope, security invariants (env sanitization, no SSH_AUTH_SOCK via env)
- `.planning/REQUIREMENTS.md` — SESS-01..11 acceptance criteria
- `.planning/phases/01-quic-transport-skeleton/01-CONTEXT.md` — codec/Message + transport decisions
- `.planning/phases/02-ssh-key-mutual-auth/02-CONTEXT.md` — auth/identity available to the session

### Existing code to modify
- `crates/nosh-server/src/server.rs` — `handle_connection` (replace `stream_echo_loop`/`datagram_echo_loop` with the PTY session pump; session struct lives here/in a new module)
- `crates/nosh-client/src/client.rs` — replace the echo round-trips with the interactive session loop (raw mode, stdin→stream, stream→stdout, SIGWINCH, exit-code propagation)
- `crates/nosh-proto/src/messages.rs` + `codec.rs` — extend the `Message` enum (SessionOpen/PtyData/Resize; SessionClose already present)

### Research (stack & gotchas)
- `.planning/research/STACK.md` — `portable-pty` 0.9.0 API (`native_pty_system`/`openpty`/`spawn_command`/`resize`/reader+writer), tokio bridging
- `.planning/research/PITFALLS.md` — PTY lifecycle (reap child or zombies; RAII raw-mode restore on panic/abrupt loss; SIGWINCH coalescing; `Child::wait()` on spawn_blocking); env-injection deny-by-default whitelist; never forward SSH_AUTH_SOCK
- `.planning/research/FEATURES.md` §1 & "spike done" — table-stakes session features and the done bar

No project-external specs beyond the above.

</canonical_refs>

<code_context>
## Existing Code Insights

### Reusable Assets
- `nosh-proto` `Message` enum + postcard codec (u32-BE framing) — extend with session variants; `SessionClose { exit_code, reason }` already defined.
- `nosh-server::handle_connection` — already gives an authenticated `quinn::Connection` after the Phase 2 handshake + the pre-auth permit released; the PTY session replaces the echo pumps here.
- `nosh-client` connect path — authenticated connection established; replace echo round-trips with the session loop.
- `tracing` instrumentation and `anyhow` error handling patterns established.

### Established Patterns
- Auth completes inside the TLS handshake (Phase 2); by the time `handle_connection` has a live `conn`, the peer is authenticated and the SSH identity is known — feed it into the session struct (D-08).
- Single bidi stream + Message codec mirrors how Phase 1 already frames messages.

### Integration Points
- `portable-pty` is a new dependency for `nosh-server`.
- Client gains terminal raw-mode handling (e.g. a termios RAII guard) and SIGWINCH handling — new client-side surface.

</code_context>

<deferred>
## Deferred Ideas

- Predictive local echo / datagram terminal state-sync — M4.
- Native scrollback, channel multiplexing, port/agent forwarding, OSC 52, file transfer — M5.
- Cold reattach / session persistence across disconnect — M3 (the session struct is the seam; behavior not built).
- Privilege drop / setuid to the authenticated user, multi-user server — out of scope for the spike (note the single-account limitation).
- Windows/ConPTY — M6 (kept tractable via `native_pty_system()`).
- Per-channel flow-control windows — M5; rely on QUIC stream flow control now.

</deferred>

---

*Phase: 03-pty-session-core*
*Context gathered: 2026-05-29*
