# Phase 3: PTY Session Core — Research

**Researched:** 2026-05-29
**Confidence:** HIGH (synthesizes the committed `.planning/research/{STACK,PITFALLS,FEATURES,ARCHITECTURE}.md` against the live Phase 1/2 code)

This phase replaces the Phase 2 echo loops in `nosh-server::handle_connection` and the
client echo round-trips with a real PTY login-shell session, framed over a single bidi
QUIC stream using the existing `nosh-proto` postcard `Message` codec. Covers SESS-01..11.

---

## Key questions answered

### Q1. How is the session framed on the wire?

A single bidirectional QUIC stream (`open_bi` on client / `accept_bi` on server). Every
session frame — `SessionOpen`, `PtyData` (both directions), `Resize`, `SessionClose` — is a
`Message` enum variant encoded with the existing length-delimited postcard codec
(`nosh_proto::codec::{write_message, read_message}`). This reuses Phase 1 framing verbatim;
no raw byte side channel, no datagrams for shell I/O (D-01/D-02). `SessionClose` already
exists; we add `SessionOpen { term, cols, rows, env }`, `PtyData { data }`, `Resize { cols, rows }`.

**Important codec note:** Phase 1's `read_message`/`write_message` are generic over
`AsyncRead`/`AsyncWrite + Unpin`. `quinn::SendStream` implements `tokio::io::AsyncWrite` and
`quinn::RecvStream` implements `tokio::io::AsyncRead`, so the codec works directly on the QUIC
stream halves with no adapter. The Phase 1 echo path used `read_to_end`/`write_all` on whole
streams; the session instead loops `read_message`/`write_message` on a long-lived stream.

### Q2. PTY allocation and shell spawn (portable-pty 0.9.0)

```rust
let pty = native_pty_system();
let pair = pty.openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })?;
let mut cmd = CommandBuilder::new(&shell_path);
cmd.arg0(format!("-{}", shell_basename)); // login shell: argv[0] = "-bash"
cmd.env_clear();                          // deny-by-default
for (k, v) in whitelisted { cmd.env(k, v); }
let child = pair.slave.spawn_command(cmd)?;
let reader = pair.master.try_clone_reader()?; // Box<dyn Read + Send>
let writer = pair.master.take_writer()?;      // Box<dyn Write + Send>
```

- `CommandBuilder::arg0(..)` sets argv[0] — this is how we make it a *login* shell
  (argv[0] prefixed with `-`), faithful to SSH (D-03). Verified present in portable-pty 0.9.0.
- `CommandBuilder::env_clear()` gives deny-by-default; we then add only whitelisted vars
  plus the baseline `HOME/USER/SHELL/PATH/LOGNAME` derived from the passwd entry (D-06).
- portable-pty calls `setsid()` on the slave on Linux, so the child gets its own controlling
  terminal and signals stay scoped (PITFALL: signal isolation — satisfied by the library).
- The reader/writer are **blocking** `std::io::{Read,Write}`. Bridge to tokio with
  `spawn_blocking` (research says this is fine for the spike). Output pump: a blocking thread
  reads chunks (e.g. 8 KiB) from `reader` into an `mpsc` channel the async task drains to
  `write_message(PtyData)`. Input pump: async task reads `PtyData` from the stream and writes
  to `writer` (wrap writer in a `spawn_blocking` or `Mutex<Box<dyn Write>>` write).

### Q3. Shell selection from /etc/passwd

Look up the current uid's passwd entry to get the login shell + HOME/USER. The simplest
dependency-light path on Linux is to read the relevant env / call `getpwuid`. To avoid an
unsafe `libc::getpwuid` FFI block, use the `SHELL`/`HOME`/`USER` of the server process as the
baseline AND fall back to parsing `/etc/passwd` for the uid via `nix` or a tiny reader. For
the spike (single-account, no privilege drop, D-03) reading `/etc/passwd` for the server's own
uid is sufficient and honest. Chosen approach: a small `passwd` lookup that reads `/etc/passwd`,
matches the numeric uid (from `nix::unistd::geteuid` or `libc::geteuid`), and returns
`{ name, shell, home }`; fall back to `$SHELL`/`/bin/sh`, `$HOME`, `$USER` if not found.
A `--shell` override flag on the server is allowed (D-03 Claude's discretion) and useful for tests.

### Q4. Disconnect / lifecycle (no zombies — SESS-10, PITFALL)

When the client stream closes or the connection drops:
1. Send the child `SIGHUP` (via the `Child` kill or by closing master + signalling the pid).
   `portable-pty`'s `Child` has `.kill()` (SIGKILL) and exposes `.process_id()`. To send SIGHUP
   specifically we capture the pid and `kill(pid, SIGHUP)` (libc/nix), then fall back to `.kill()`.
2. `wait()`/reap the child on `spawn_blocking` so the async runtime is never blocked, and so no
   zombie remains (PITFALL: "not calling wait() before dropping leaves zombies").
3. Drop the PTY pair (closes master/slave fds).
A `Session` struct owns these resources; its teardown path performs SIGHUP→reap→free.

### Q5. Exit code propagation (SESS-08/09)

The server runs `child.wait()` on `spawn_blocking`; the resulting `ExitStatus.exit_code()`
becomes `SessionClose { exit_code, reason }`, written to the stream **before** the connection
closes. The server then closes the QUIC connection with a structured application error code
(0 = clean) and reason string. The client, upon reading `SessionClose`, restores the terminal
and exits the process with that exit code (`std::process::exit(code)`).

### Q6. Client raw mode + RAII restore (SESS-03, PITFALL/UX)

Use `crossterm::terminal::{enable_raw_mode, disable_raw_mode}` (or a direct termios guard).
`crossterm` is the lightest well-maintained option and also gives us a portable resize hook.
The RAII guard stores nothing but calls `disable_raw_mode()` in `Drop`, so it fires on normal
return, on panic (unwinding runs Drop), and on the error path after abrupt network loss
(the guard is held in `main`/`run`, and any early return drops it). SIGKILL cannot run Drop —
that is the one case that is a documented human-verification item.

### Q7. SIGWINCH coalescing (SESS-05, PITFALL: resize storms)

Install a `SIGWINCH` handler (tokio `signal(SignalKind::window_change())`). On each signal,
record the latest size and (re)arm a ~40 ms debounce timer; when it fires, query the current
terminal size (`crossterm::terminal::size()`) and send one `Resize { cols, rows }`. Server
calls `pair.master.resize(PtySize { .. })`. Debounce coalesces a window-drag burst into a
trickle. Implemented with a `tokio::select!` loop: a `signal` branch resets a `sleep` deadline,
the `sleep` branch emits the resize.

### Q8. Signal passthrough (SESS-06)

No special handling: the client is in raw mode, so Ctrl-C is delivered to the server as the
byte `0x03` inside `PtyData`. The server writes those bytes to the PTY master; the PTY line
discipline (ISIG) translates `0x03` to SIGINT delivered to the shell's foreground process
group. This is automatic given a real PTY — we just must NOT swallow Ctrl-C on the client.

### Q9. Server-side session struct (SESS-10, M3 seam)

```rust
struct Session {
    session_id: Uuid,
    identity: NoshPublicKey,   // SSH identity from Phase 2 auth
    master: Box<dyn MasterPty + Send>,
    child_pid: Option<u32>,
    idle_since: Option<Instant>,
}
```
A discrete `session` module (not inlined in `handle_connection`). Reattach is NOT implemented;
`idle_since` and the struct shape are the seam M3 attaches to.

### Q10. tracing spans (SESS-11)

A `tracing::info_span!("session", %session_id, %peer_addr, username = %username)` entered for
the session lifetime; open/resize/close logged inside it. `username` comes from the passwd
lookup (the account the server runs as for this spike). `peer_addr` from `conn.remote_address()`.

---

## Headless testing strategy (the hard part — SESS-01..11)

Reuse the Phase 2 in-process harness (`tests/common/mod.rs`: ephemeral Ed25519 keys, temp
trust files, in-process server). Add session client helpers in `nosh-client::client` that
drive a session over the authenticated connection WITHOUT touching the real terminal (so tests
run headlessly):

- `open_session(conn, SessionOpen) -> (SendStream, RecvStream)` — opens the bidi stream and
  sends `SessionOpen`.
- A test-only driver that writes `PtyData` (command bytes), reads `PtyData`/`SessionClose`
  frames, and collects output until `SessionClose`.

Assertions per requirement:

| Req | Test |
|-----|------|
| SESS-01 | session runs `tty` / `test -t 0 && echo ISTTY`; output contains `ISTTY` (real PTY) |
| SESS-02 | send `echo hello-nosh\n`; collected output contains `hello-nosh` |
| SESS-04 | open with cols=132 rows=40, run `stty size`; output contains `40 132` |
| SESS-05 | after open, send `Resize{cols:100,rows:50}`, then `stty size`; output contains `50 100` |
| SESS-07 | SessionOpen.env carries `LD_PRELOAD=/evil.so`, `BASH_ENV=/x`, `SSH_AUTH_SOCK=/a`, `LC_ALL=C`, `TZ=UTC`; run `env`; assert output has `LC_ALL`/`TZ`/`TERM`, and does NOT contain `LD_PRELOAD`/`BASH_ENV`/`SSH_AUTH_SOCK`/`IFS`/`SHELLOPTS` (explicit security assertion) |
| SESS-08 | session runs `exit 42`; client surfaces exit code 42 via SessionClose |
| SESS-09 | normal session close → connection close_reason is the structured app close, no error |
| SESS-10 | disconnect mid-session → child reaped: poll `/proc/<pid>` gone or zombie-state check; assert pid no longer alive and not `Z` |
| SESS-11 | spans present (smoke: code path entered; tracing verified by inspection — non-asserting) |
| SESS-03 | raw-mode restore on SIGKILL: HUMAN verification (Drop cannot run on SIGKILL) |
| SESS-06 | Ctrl-C interrupts `sleep`: attempt headless (send `sleep 5\n` then `0x03`, expect prompt back fast). If flaky, HUMAN verification |

Shells differ; tests should prefer `/bin/sh` via a `--shell`-style override and skip
gracefully if no POSIX shell with `stty`/`tty` is on PATH (CI guard), mirroring the Phase 2
"skip if ssh-agent unavailable" pattern.

## Sources
- `.planning/research/STACK.md` — portable-pty 0.9.0 API, tokio bridging via spawn_blocking
- `.planning/research/PITFALLS.md` — FOOTGUN-1/2 (env), zombie reap, raw-mode RAII, SIGWINCH coalesce, exit-code propagation
- `.planning/research/FEATURES.md` — table-stakes session features + "spike done" bar
- Phase 1 codec (`crates/nosh-proto/src/codec.rs`) — generic over AsyncRead/AsyncWrite, works on quinn streams
- Phase 2 harness (`crates/nosh-client/tests/common/mod.rs`) — reused for session tests
</content>
</invoke>
