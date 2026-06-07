# Pitfalls Research

**Domain:** QUIC-based roaming remote shell (Rust) — v1.2 M4 Predictive Echo + Daily-Driver Hardening
**Researched:** 2026-06-01
**Confidence:** HIGH (Mosh USENIX paper + GitHub issues; QUIC RFCs + quinn docs; tokio spawn_blocking docs; nosh codebase v1.1 inspection)

---

## CRITICAL PITFALLS

### Pitfall 1: Predicting in Cursor-Addressing Apps — The vim/less/htop Trap

**What goes wrong:**
The prediction engine echoes the typed character at the current cursor position on the client-side terminal model, then waits for server confirmation. In a line-discipline shell prompt this is almost always correct. In cursor-addressing apps (vim, less, htop, emacs, any curses app), the "current cursor position" is wherever a cursor-move escape sequence last landed — and that position changes faster than round-trip confirmation arrives. The predicted echo lands in the wrong cell, producing a corrupt screen that flickers and then corrects one RTT later. This is WORSE than no prediction: the screen looks broken on every keystroke.

**Why it happens:**
CSI A/B/C/D (cursor movement), CSI H (cursor position), ED/EL (erase) sequences all move or invalidate the cursor position without producing visible characters. If the prediction engine trusts cursor position from the last-seen confirmed state while the server has moved the cursor with escape sequences that have not yet arrived at the client, every prediction will be displaced.

Mosh solves this by running a FULL terminal emulator model on both sides (the server sends terminal state diffs, not raw byte streams); naive implementations that predict against the raw stream fail immediately on any curses app.

**How to avoid:**
- Do NOT attempt to predict while the client's terminal model has unconfirmed cursor-move or erase sequences outstanding. Track an "is-in-cursor-addressing-app" flag: any non-echoed control sequence in the pending-confirmation window → disable prediction for that epoch.
- Alternatively (Mosh's approach): run the terminal model on the client, accept state diffs from the server, and only echo when the client model agrees the cursor is at a stable position and the character at that position is the predicted one.
- The conservative fallback is mandatory: if any prediction from an epoch is not confirmed within one RTT, reset epoch and go dark (show no predicted characters, wait for confirmed state). This gives vim/less/htop correct behaviour at the cost of no prediction.
- Write an adversarial test: open a vim session, type `iHello`, confirm zero corrupt cells appear in the client terminal model during the typing.

**Warning signs:**
Prediction is attempted after any CSI sequence; prediction epoch never resets on arrow-key press; the prediction engine treats the remote terminal as a simple line-editor even in alternate-screen mode.

**Phase to address:**
Predictive echo phase (M4) — this is the design gate; the entire prediction architecture must be built around epoch-reset-on-cursor-move from the start.

---

### Pitfall 2: Wide Characters (CJK, Emoji) Corrupt the Predicted Cursor Column

**What goes wrong:**
CJK characters and emoji occupy two columns in most terminal emulators. The prediction engine inserts a predicted character at column N and advances the cursor model by 1. The remote shell, having received a wide character, advances by 2. The client-side cursor model is now 1 column ahead of reality. Every subsequent prediction lands one cell to the left of where it should be. The mismatch accumulates until the server sends a confirmed state that resets the model. In the meantime the display shows characters written on top of each other.

**Why it happens:**
Unicode column-width is not trivially derived from the code point — it depends on `wcwidth()` semantics (East Asian Width property), combining characters (zero-width), variation selectors, and emoji sequences. Implementations that assume `char → 1 column` produce wrong predictions for any CJK user.

Mosh explicitly documents this as a known gap in its canonical-mode editing: "there is no easy solution to this problem."

**How to avoid:**
- Use a proper Unicode width library (`unicode-width` crate in Rust) to compute column advance for every predicted character.
- For characters where `unicode_width::UnicodeWidthChar` returns `None` or `2` (wide), either skip prediction entirely for that character or emit a tentative two-column prediction and reset the epoch immediately so the server can correct.
- Combining characters (zero-width), variation selectors (VS15/VS16), and multi-codepoint emoji sequences (ZWJ, flag sequences) should all trigger epoch reset (no prediction for that input unit) unless the terminal model explicitly tracks them.
- Write a test: type `你好` (CJK wide chars) into a predicted shell and verify cursor column is reported correctly after each character.

**Warning signs:**
Predicted cursor column advances by 1 for every codepoint regardless of width; no `unicode-width` dependency in the prediction subsystem; tests use only ASCII.

**Phase to address:**
Predictive echo phase (M4) — the width logic must be part of the initial prediction commit, not a follow-up.

---

### Pitfall 3: Paste / Bulk Input Floods the Prediction Engine

**What goes wrong:**
User pastes 200 characters. The prediction engine eagerly predicts all 200 characters one at a time, inserting predicted cells into the client terminal model. The server processes the paste differently (shell may run the text as a command, readline may scroll, bracketed paste mode may wrap it). The mismatch between predicted state and actual state is large; the correction visible to the user is a jarring full-screen repaint 1 RTT later.

A second failure mode: during the paste, datagrams carrying the predicted state arrive at the server and the client simultaneously, but the client updates its model faster than the server sends back confirmations. The client's epoch counter races ahead; when the first confirmed response arrives it may confirm a stale epoch, resetting prediction for an epoch that is already 50 characters stale.

**Why it happens:**
Prediction is designed for single-keystroke interactive input. Paste is a bulk operation. Mosh added explicit bracketed paste support (commit c6bf3a2) specifically because the prediction engine's behaviour on paste was visually worse than no prediction.

**How to avoid:**
- Detect bracketed paste mode (CSI ?2004h/l from the server): when the server has told the client it is in bracketed paste mode, suppress ALL prediction for the duration of the paste (between `\x1b[200~` and `\x1b[201~` in the input stream).
- Detect non-bracketed bulk input heuristically: if more than N bytes arrive in the same tokio `select!` batch (e.g. > 4 bytes in one read), skip prediction for that batch and send as raw PTY data.
- Emit a conservative epoch reset whenever more than one keystroke is queued.

**Warning signs:**
Prediction runs on every byte of input without a bulk-input fast-path; bracketed paste mode not tracked in the terminal state model; no test for paste-mode prediction suppression.

**Phase to address:**
Predictive echo phase (M4) — bracket paste mode detection is a required feature of the initial implementation, not an optional improvement.

---

### Pitfall 4: Epoch / Confirmation Desync After Datagram Loss

**What goes wrong:**
The server sends a terminal state-sync datagram confirming epoch N, predictions 0..K. The datagram is lost (QUIC datagrams are unreliable). The client never receives the confirmation. The prediction engine keeps predictions from epoch N tentative indefinitely, eventually expiring them after a timeout and resetting the epoch. Meanwhile the server, having seen epoch N confirmed, starts sending diffs assuming those cells are accepted. The client and server's models diverge: the client shows no prediction, the server believes confirmed predictions were applied. The session will eventually self-correct when the next confirmed datagram lands, but during the divergence window the screen is corrupted.

A subtler variant: the client's epoch counter is incremented (due to a control character) between when a server datagram was sent and when it arrives. The server's confirmation carries epoch N but the client is now in epoch N+1. The confirmation is silently discarded. The N+1 predictions are never confirmed; they eventually expire. Each frame the user sees an underlined prediction appear, flicker, and disappear.

**Why it happens:**
QUIC datagrams are RFC 9221 best-effort. The prediction confirmation must handle loss gracefully. The naive approach is to confirm exactly one epoch/sequence per datagram — if that datagram is lost, the confirmation is simply never received.

Mosh's SSP sends entire terminal state objects (not diffs of predictions), so the confirmation is implicit in the state rather than a separate acknowledgment message. A diff-based design must handle explicit confirmation loss.

**How to avoid:**
- Design the datagram payload to carry the FULL current terminal state (or enough of a diff that any single received datagram is self-consistent), not a delta that requires every prior datagram. "Latest-state-wins": each datagram is idempotent — receiving datagram N makes datagram N-1 irrelevant.
- Confirmation of predictions should be implicit: when the client receives a server state that matches the predicted cell at position (row, col), that prediction is considered confirmed, regardless of epoch numbering. The epoch is a grouping hint, not a strict ack sequence.
- The client must coalesce consecutive dropped confirmations rather than failing after one miss: wait at least 2 RTTs before resetting epoch on an unconfirmed prediction.
- Write a test: simulate 30% datagram loss during a typing session; confirm no permanent screen corruption (may flicker, but must self-correct within 2 RTTs after loss stops).

**Warning signs:**
Each datagram carries only a delta that requires all prior datagrams to interpret; confirmation is a separate ack message (not implicit in state); epoch resets on first missed confirmation rather than after a timeout.

**Phase to address:**
Predictive echo phase (M4) — the datagram state format must be designed for idempotency before any prediction code is written.

---

### Pitfall 5: Datagram MTU Limits — Terminal State Objects Too Large

**What goes wrong:**
The terminal state sync datagram exceeds `connection.max_datagram_size()`. Quinn returns `SendDatagramError::TooLarge`. The terminal state update is dropped. The prediction engine's state diverges from the server. On Windows (quinn_udp), the OS returns `WSAEMSGSIZE` for oversized UDP sends, which quinn surfaces as the same error but which also manifests as the existing v1.1 warning log that has been carried as tech debt.

**Why it happens:**
A full 80x24 terminal cell grid is 1920 cells. Even a compact binary encoding (cell = char + style flags) at 4 bytes/cell = 7.68 KB — well over the ~1200-byte QUIC datagram limit. Even a sparse diff can exceed the limit on a large terminal refresh (initial screen draw, vim `:e` file reload, `clear` + `cat big_file`).

`max_datagram_size()` fluctuates during the connection lifetime as DPLPMTUD probes the path. The minimum guaranteed value is ~1200 bytes.

**How to avoid:**
- Never send the full terminal state grid as one datagram. Send only the **changed cells** since the last confirmed server state, capped at `max_datagram_size() - overhead` bytes.
- Always check `connection.max_datagram_size()` before each `send_datagram()` call. If the diff exceeds the limit: (a) send the largest subset that fits, with sequence context so the receiver can apply it; or (b) fall back to sending a partial state snapshot (prioritize current cursor context) and let the full state arrive via the reliable stream sync on the next reattach.
- For the `WSAEMSGSIZE` warning: it means the datagram was sized above the path MTU before DPLPMTUD had finished probing. Fix by capping outgoing datagrams at 1200 bytes during the first 10 datagrams (before DPLPMTUD converges) and then tracking `max_datagram_size()` dynamically.
- Add a hard assertion in tests: `assert!(payload.len() <= 1100)` (reserve 100 bytes for QUIC framing overhead).

**Warning signs:**
Datagram payload serializes the full terminal grid regardless of size; `max_datagram_size()` is not called before `send_datagram()`; the `WSAEMSGSIZE` warning is suppressed rather than fixed; no test for datagram size compliance.

**Phase to address:**
Predictive echo phase (M4) — datagram size must be enforced in the first implementation of state sync. The `WSAEMSGSIZE` fix is bundled in the daily-driver hardening sub-phase.

---

### Pitfall 6: PTY Reader Zombie Race — `spawn_blocking` + `abort()` Cannot Interrupt a Blocked `read()`

**What goes wrong:**
The current server code in `run_session` spawns a `spawn_blocking` task that calls `reader.read(&mut buf)` in a loop. When a session is orphaned or cleaned up, the code calls `output_reader.abort()`. But `abort()` on a `spawn_blocking` task has no effect on a task that is already executing — it can only cancel tasks that have not yet started. The blocking thread remains alive indefinitely, holding the PTY master reader fd open and consuming a thread from tokio's blocking pool.

If the shell exits, the PTY master fd becomes EOF and the `read()` returns 0, unblocking the thread naturally. But while the shell is alive (orphaned session), the `spawn_blocking` thread is permanently blocked on `read()`. Under the default idle_timeout=0, this is every orphaned session — one permanently-stuck blocking thread per orphan.

Two failure cascades: (1) tokio's blocking thread pool (`max_blocking_threads`, default 512) fills up over time with blocked PTY readers, new `spawn_blocking` calls start returning errors or hanging; (2) the PTY master fd is held open by the zombie reader thread even after the slot is evicted from the registry, preventing the MasterPty from closing, preventing SIGHUP from reaching the shell, and creating a genuine resource leak.

**Why it happens:**
tokio's documentation explicitly states: "Tasks spawned using `spawn_blocking` cannot be aborted because they are not async. If you call abort on a spawn_blocking task, it will not have any effect once the task has started running." Blocking I/O (read on a PTY master fd) has no async cancellation point. The only way to unblock it is to close the fd it is reading from — which is what dropping `MasterPty` would do, but `MasterPty` is held by the session slot.

**How to avoid:**
The fix is to use a separate signaling fd (a self-pipe or a non-blocking eventfd/pipe pair) to interrupt the blocking read. The pattern:
1. Create a `(signal_read_fd, signal_write_fd)` pipe (or `eventfd` on Linux) alongside the PTY master fd.
2. In the blocking reader thread: use `select()` or `poll()` on both the PTY master fd and the signal fd. When either is readable, check which: if PTY data, forward it; if signal fd, break out of the loop.
3. To interrupt the reader: write one byte to `signal_write_fd` from async code. The blocked `read()` / `select()` wakes up and exits cleanly.
4. The PTY master fd can then be closed by dropping `MasterPty`, which is now safe.

Alternatively: use OS-level non-blocking mode on the PTY master fd combined with tokio's `AsyncFd` to poll the PTY fd from async code directly (requires `O_NONBLOCK` on the PTY master; `portable-pty` may need to expose this). This avoids the blocking thread entirely.

A simpler stopgap that bounds the damage without fully fixing it: after orphaning a session, close the master reader fd clone used by the output pump (not the master fd in the slot, but the clone returned by `try_clone_reader()`). This causes the blocking `read()` to return `Err(EIO)` or `Ok(0)`, exiting the loop. The slot's `MasterPty` still holds the master fd open (so no SIGHUP), but the blocking thread exits. Then on reattach, `slot.clone_pty_reader()` gets a fresh reader clone.

**Warning signs:**
`output_reader.abort()` is called on a `JoinHandle` from `spawn_blocking` and expected to interrupt a live read; no signal mechanism to wake the blocking thread; blocking thread count grows monotonically with orphaned sessions; PTY master fd is not closed on reattach (old reader clone still live).

**Phase to address:**
Daily-driver hardening sub-phase — this is the latent tech debt bug identified in the v1.1 audit. Must be the first thing addressed in M4 before any load testing.

---

### Pitfall 7: Interaction Between Datagram State Sync and Reliable-Stream Reattach Replay

**What goes wrong:**
The existing reattach protocol (ROAM-02) replays reliable-stream chunks from the `SequencedOutputBuffer` to restore the client's view after reconnect. When datagram state sync is added in M4, the server maintains two parallel views of terminal state: the reliable-stream sequence (byte-exact replay buffer) and the datagram state (lossy terminal cell grid). On reattach, the client replays the reliable stream, which gives it the byte sequence but not necessarily the rendered terminal state (the sequence may include escape sequences that the client's new terminal session does not render identically).

If the client also receives a datagram state before the reliable-stream replay is complete, it may apply the datagram state to a partially-replayed terminal, causing a visual discontinuity: cells from the datagram (server's current state) overwrite cells from the replay (server's historical state), producing a torn view.

**Why it happens:**
The reliable stream and datagrams are independent channels. After reattach, the reliable stream replay is sequential (one PtyData frame at a time); datagrams are continuous (server keeps sending state updates during replay). The race window is: server sends datagram at T=100ms, replay completes at T=150ms, client applies datagram to partial replay state.

**How to avoid:**
- During reattach replay, suppress datagram state application until replay is complete. The client should ignore incoming datagrams between `ReattachOk` and the end of replayed `PtyData` frames (or until the client explicitly signals "replay complete").
- Send a `ResumeComplete` frame (or repurpose `Ack` with a special flag) from client to server after all replayed frames are applied; the server then starts sending fresh datagrams.
- Alternatively, after reattach the server sends one terminal state datagram AFTER the replay is complete (as the `ResumeComplete` trigger), giving the client a full authoritative state.

**Warning signs:**
Server sends datagrams immediately after `ReattachOk` without waiting for replay completion; client applies datagram state during replay; no `ResumeComplete` handshake in the reattach protocol.

**Phase to address:**
Predictive echo phase (M4), specifically the sub-phase that integrates datagram state sync with existing reattach.

---

### Pitfall 8: TOFU Pin Forgery Window — First Connection to a New Server Is Unprotected

**What goes wrong:**
nosh uses TOFU for the server host key: on first connect, the server's SPKI is stored in `known_hosts` and trusted on all subsequent connections. If an attacker can intercept the very first connection (before the pin is stored), they can present a forged host key and the client will trust it. All subsequent connections use the forged pin. The session is now proxied through the attacker indefinitely.

This is the canonical TOFU weakness — it does not protect against attackers present at first contact. SSH has the same weakness. The security doc must name it explicitly so operators know whether to pre-distribute host keys out-of-band.

**Why it happens:**
TOFU accepts anything on first contact by design. nosh's `known_hosts` implementation stores the server SPKI on first use and pins it thereafter (correct behaviour). The gap is the first contact window.

**How to avoid:**
- The security doc must describe the TOFU threat model honestly: the system is secure against MITM on all connections AFTER the first, assuming the first contact was to the genuine server.
- Provide a mechanism to pre-populate `known_hosts` (e.g. `nosh-keyscan` or manual copy of the server's host public key) before first connection for high-security deployments.
- The client must display the server fingerprint on first connection and require explicit user confirmation before storing it (SSH-style `The authenticity of host '...' can't be established. [...] Are you sure you want to continue connecting (yes/no)?`). Silently auto-accepting is an anti-pattern.
- For the threat model doc: categorize TOFU under "accepted residual risk with mitigation available (pre-distribution)" rather than "mitigated."

**Warning signs:**
First connection silently stores the host key without user confirmation; no `nosh-keyscan` equivalent; security doc omits the TOFU first-contact window.

**Phase to address:**
Security design pass phase — document the threat explicitly; UX of first-connection prompt belongs to a subsequent QoL phase.

---

### Pitfall 9: Predicted Echo as a Timing Side-Channel (Information Leak)

**What goes wrong:**
The prediction engine immediately echoes predicted characters to the screen before receiving server confirmation. An attacker who can observe the client terminal output (e.g. screen-recording malware, physical shoulder surfing, or a compromised terminal multiplexer) can learn what the user is typing in real-time from the predicted echo, even if the reliable stream to the server is encrypted. For password prompts (which suppress echo on the server), the client must suppress prediction — if it does not, passwords are leaked visually at the client.

**Why it happens:**
The prediction engine runs on the client before any data reaches the server. Passwords typed into a `noecho` tty (stty -echo) will NOT be echoed back by the server in the terminal state diffs — but a naive prediction engine will still echo them locally, since it does not know the server is suppressing echo.

**How to avoid:**
- Track echo state from the server's terminal model. When the server's diff indicates `stty -echo` mode (no character echo), the prediction engine MUST suppress local echo for typed characters. This is visible in the terminal state as the TTY echo flag or, more practically, as the absence of echoed characters in the server state diffs.
- Conservatively: if the last server-confirmed state does not echo the previous typed character within one RTT, disable prediction for the current epoch. This naturally suppresses prediction during password prompts.
- This is Mosh's behaviour: "Mosh does not make predictions that would echo back keystrokes in contexts where the remote is not echoing."
- Write a test: start a `read -s` (noecho) prompt; type characters; confirm the client terminal model shows no predicted characters for those keystrokes.

**Warning signs:**
Prediction always runs regardless of server echo state; no test for password prompt echo suppression; prediction epoch is not reset when server echo is disabled.

**Phase to address:**
Predictive echo phase (M4) — this is a security requirement of the prediction feature itself, not a separate hardening step. Must be in the initial design.

---

### Pitfall 10: Security Design Doc Omits the Datagram Injection / Replay Surface

**What goes wrong:**
The security threat model covers the reliable-stream protocol (auth, reattach token, session hijacking) thoroughly but treats datagram session traffic as equivalent in security to reliable streams. QUIC datagrams ARE authenticated and encrypted by TLS 1.3 (same connection), so injection from outside the connection is not possible. However, two specific threats must be explicitly documented:

1. **Replay within a session**: QUIC provides replay protection per-connection via packet number space. A datagram sent in session epoch N cannot be replayed as epoch N+1 by an on-path attacker (QUIC's crypto handles this). The doc should state this explicitly to close the question.

2. **Stale state application**: A delayed datagram (QUIC may deliver old datagrams, and there is no strict ordering guarantee between datagrams and streams) carrying an old terminal state diff may be applied after a newer diff, moving the terminal model backwards. This is a correctness bug, not a crypto issue, but the security doc should note that datagram-carried state must carry a monotonic sequence number so the client can discard stale updates.

**How to avoid:**
- The datagram state payload must include a monotonic sequence number (or timestamp). The client discards any datagram whose sequence is not greater than the last applied datagram.
- The security doc should explicitly address: (a) QUIC's TLS 1.3 provides datagram authentication (no external injection); (b) QUIC per-packet replay protection applies to datagrams; (c) application-layer monotonic sequencing handles within-connection stale delivery.
- Write a test: send two datagrams with seq=10 then seq=5; confirm the client applies only seq=10 (seq=5 is discarded as stale).

**Warning signs:**
Datagram state carries no monotonic sequence number; security doc does not address datagram replay/staleness; datagrams treated as if ordering is guaranteed.

**Phase to address:**
Both predictive echo phase (implement monotonic seq) and security design pass phase (document the analysis).

---

### Pitfall 11: Security Design Doc Omits Privilege Model Gap (Server Runs as the Connecting User)

**What goes wrong:**
The nosh server runs as the single authenticated user — there is no privilege drop, no separate daemon process, no privilege separation (as SSH's `sshd` achieves with `privsep`). The security doc must name this clearly:

- The server process has full access to the user's files, environment, and processes — compromising the nosh server binary or exploiting a bug in the session handler is equivalent to full user compromise (not root, but the full user account).
- There is no MAC/SELinux/AppArmor sandboxing on the server process in the default configuration.
- The server does NOT run as root and cannot acquire root, so privilege escalation to root requires an additional step (sudo bug, kernel exploit, etc.).

The risk is not that this is wrong — it is the correct design for a single-account shell tool — but that the threat model doc must state it explicitly so operators can audit appropriately.

**How to avoid:**
The security doc must include a "Privilege Model" section that:
1. States the server runs as the authenticated user (no privilege drop, no daemon, single-account model).
2. Contrasts this with sshd's privsep model (which nosh intentionally does not implement).
3. Notes that exploiting a bug in the nosh server gives the attacker the same access as the authenticated user.
4. Notes that the pre-auth connection handler (before `handle_connection` dispatches to `run_session`) runs with the same user permissions as the authenticated session — there is no privsep layer separating pre-auth from post-auth code.
5. Lists the security mitigations in place: pre-auth DoS cap, auth timeout, env sanitization, TLS mutual auth.

**Warning signs:**
Security doc says "no privilege escalation" without explaining the privilege model; threat model does not address the absence of privsep; no "privilege model" section at all.

**Phase to address:**
Security design pass phase — this is a documentation gap, not a code change.

---

### Pitfall 12: Windows Cross-Compile CI Gate — False-Green from Build-Only Check

**What goes wrong:**
The current CI gate compiles the Windows client cross-target (`x86_64-pc-windows-gnu` or `-msvc`) but does not run any tests on a real Windows runner. The build passes because the Rust type system catches most cross-platform errors at compile time. But runtime Windows-specific behaviour (codepage, VT processing, resize events, socket errors including `WSAEMSGSIZE`) is not tested. The CI gate gives false confidence: "it builds, so it works on Windows." Real bugs that only manifest at runtime on Windows remain undetected.

**Why it happens:**
Running a Windows job in CI requires a `windows-latest` GitHub Actions runner. Cross-compiling from Linux is cheaper and faster. Developers set up cross-compilation as the CI gate and never wire up an actual Windows job because they cannot test it locally.

The v1.1 audit noted: "Windows cross-compile CI gate exists but has never run (no git remote configured)."

**How to avoid:**
- Add a `windows-latest` GitHub Actions job that runs `cargo test` on the Windows runner — not just `cargo build`. At minimum, run the unit tests that do not require a real network connection (`cargo test -- --skip integration`).
- The cross-compile job on Linux is kept as a fast smoke check. The Windows native job is the functional gate.
- Pin the Windows toolchain version in `rust-toolchain.toml` so runner drift does not silently change the build environment.
- Add the `MSVC` or `GNU` target explicitly in `rust-toolchain.toml` or the workflow file, not as an ad-hoc `rustup target add`.
- Verify the git remote is configured and the CI workflow file is present and syntactically valid before treating this as complete.

**Warning signs:**
CI only runs cross-compile from Linux; no `windows-latest` runner job; no `rust-toolchain.toml` pinning the toolchain; git remote not configured (the current known state).

**Phase to address:**
Daily-driver hardening sub-phase — wire this early so subsequent M4 work is validated on Windows from the start.

---

## Technical Debt Patterns

| Shortcut | Immediate Benefit | Long-term Cost | When Acceptable |
|----------|-------------------|----------------|-----------------|
| Predict on all input without epoch reset on cursor-move | Simpler prediction engine | Corrupted screen in vim/less/htop; worse than no prediction | Never; epoch-reset-on-cursor-move is the minimum viable design |
| Skip Unicode width check in prediction | Simpler code | CJK cursor drift; accumulating prediction errors for all CJK users | Never for CJK-bearing products; acceptable for ASCII-only spike |
| Predict on paste (no bulk input detection) | Simpler code | Jarring screen flicker on paste; prediction causes visible regression | Never; paste suppression must be in the initial design |
| Full terminal grid in each datagram | Simplest state sync | Always exceeds MTU (>7 KB); mandatory `TooLarge` errors | Never; must use diffs with size cap |
| `output_reader.abort()` to clean up spawn_blocking PTY reader | One-line cleanup | Abort has no effect; zombie thread per orphan; blocking pool exhaustion | Never in production; spike use only |
| Build-only CI gate for Windows | Cheaper CI | False confidence; runtime Windows bugs slip through | Only until a real Windows runner job is added (short-term acceptable) |
| Security doc omits TOFU first-contact gap | Simpler doc | Operators don't know to pre-distribute host keys | Never; TOFU gaps must be named in the security doc |
| No echo-state tracking in prediction (predict on noecho prompts) | Simpler prediction | Passwords leaked visually at client | Never; this is a security requirement |
| Datagram state carries no monotonic sequence number | Simpler format | Stale datagrams corrupt terminal state; no defence against delayed delivery | Never; monotonic seq is required for correctness |

---

## Integration Gotchas

| Integration | Common Mistake | Correct Approach |
|-------------|----------------|------------------|
| vte `Perform` trait for terminal state | Treating control sequences as opaque bytes in prediction | Parse with vte; track cursor position, scroll regions, alternate screen mode, and echo state to gate prediction |
| portable-pty reader + spawn_blocking | Calling `abort()` to interrupt blocked read | Use a self-pipe / eventfd signal fd so the blocking thread wakes up on demand; close the reader clone on orphan |
| QUIC datagrams + terminal state | Sending full terminal grid per datagram | Send only changed cells; cap at `max_datagram_size() - 100`; check before every send |
| Reattach replay + datagram sync | Server sends datagrams during replay window | Client ignores datagrams until replay is complete; server waits for `ResumeComplete` signal |
| Prediction + noecho (password) prompts | Prediction runs regardless of TTY echo state | Track server echo state; suppress prediction when server echo is disabled |
| Windows CI gate | Cross-compile only from Linux | Add `windows-latest` native runner job with `cargo test`; pin toolchain in `rust-toolchain.toml` |
| Mosh-style prediction + fish/zsh autosuggestions | Prediction inserts before autosuggestion rather than overwriting it | Epoch-reset on any input that changes cursor context; let server correction handle the rest |
| Token rotation on reattach send failure | Rotate token before confirming send → client never receives new token | Mint candidate, send, then commit (W1 pattern — already implemented in v1.1, preserve on any refactor) |

---

## Performance Traps

| Trap | Symptoms | Prevention | When It Breaks |
|------|----------|------------|----------------|
| Datagram sent once per PTY output chunk | One datagram per `PtyData` frame even for small chunks | Coalesce PTY output chunks into one datagram per tick (e.g. drain `out_rx` channel, build one diff, send one datagram per frame interval ≈ 16 ms) | Under high-throughput output (large file cat) |
| Datagram encode/decode on every frame regardless of change | Wasted work on quiet sessions | Track dirty cells; only encode when something changed | Always on sessions where nothing is happening |
| Blocking pool exhaustion from zombie PTY readers | New `spawn_blocking` hangs; server unresponsive | Fix PTY reader interrupt (Pitfall 6) | At ~512 orphaned sessions (tokio default blocking pool limit) |
| vte parsing on both client and server for state sync | Double parse of every escape sequence | The server-side vte parse is already required (for the state diff); the client parse is for echo-state tracking; share the same parse tree if possible | With large escape sequence bursts (full-screen redraws) |
| Wide char detection on every keystroke | microseconds per check but called O(1) per key | Acceptable cost; `unicode_width` is a pure lookup table | Not a real problem; mention for completeness |

---

## Security Mistakes

| Mistake | Risk | Prevention |
|---------|------|------------|
| Predict on `noecho` (password) prompts | Passwords displayed in plaintext at the client terminal | Track server echo state; suppress prediction when echo is disabled (Pitfall 9) |
| Security doc omits TOFU first-contact vulnerability | Operators don't pre-distribute host keys; MITM on first connect is undetected | Name the TOFU gap explicitly with mitigation (Pitfall 8) |
| Security doc treats datagram channel as equivalent to reliable stream without analysis | Stale-delivery and "is QUIC replay protection sufficient?" left unanswered for readers | Document QUIC TLS 1.3 datagram authentication + per-packet replay protection + application-layer monotonic seq (Pitfall 10) |
| Reattach token leaked via failed-send recovery (W1 bug pattern) | Client holds old token; server rotated → session permanently un-reattachable after one failed send | Mint candidate, send, THEN commit — preserve the v1.1 W1 fix across any refactor of the reattach path |
| Datagram state sync bypasses reattach two-factor auth | An attacker who corrupts a datagram mid-session gets... nothing extra (QUIC auth applies); document this explicitly | Note in security doc that datagrams share the TLS 1.3 session — same auth as streams |
| Security doc omits privilege model (no privsep) | Operators assume separation between pre-auth and post-auth code | Explicit "Privilege Model" section describing single-account, no-privsep design (Pitfall 11) |
| Prediction epoch state logged with token context | Token bytes appear in prediction debug logs | Prediction state logs must not include reattach token; log only epoch number and fingerprint |

---

## UX Pitfalls

| Pitfall | User Impact | Better Approach |
|---------|-------------|-----------------|
| Prediction visible in full-screen apps (vim, less) | Screen corruption on every keystroke; worse than no prediction | Epoch reset on any cursor-move or alternate-screen sequence; go dark until confirmed |
| No underline on tentative predictions | User cannot distinguish predicted text from confirmed text | Dim/underline predicted cells (Mosh behaviour); remove styling on confirmation |
| Prediction immediately displayed with no delay threshold | On low-latency local network, prediction makes screen flicker visually | Use adaptive threshold: only display predictions when RTT > ~40 ms (Mosh `--predict=adaptive` mode) |
| Full-screen repaint flicker on paste | Paste followed by a full-screen correction 1 RTT later | Suppress prediction on paste (Pitfall 3); use bracketed paste detection |
| No connection-loss visual indication during datagram-only window | User doesn't know if their input is being received when reliable stream is gone but datagrams still flow | Connection-loss notification must fire when reliable stream is lost even if datagrams continue; reliable stream is the auth-checked channel |
| WSAEMSGSIZE warning in log every session on Windows | Log noise; operators file bug reports about "errors" that aren't errors | Fix the MTU sizing (Pitfall 5) so the warning never fires; don't suppress it |

---

## "Looks Done But Isn't" Checklist

- [ ] **Prediction in vim:** Open vim, type `iHello<Esc>`, confirm no corrupt cells appear in the client terminal model at any point during typing (prediction must be suppressed in cursor-addressing mode).
- [ ] **Wide char cursor column:** Type `你好` into a predicted shell; verify cursor column is reported correctly (2 columns per character, not 1).
- [ ] **Paste suppression:** Enable bracketed paste mode, paste 50 characters, confirm no predicted cells appear between `\x1b[200~` and `\x1b[201~`.
- [ ] **Datagram size:** Assert every outgoing datagram payload is `<= max_datagram_size() - 100` bytes; add this assertion in test/debug builds.
- [ ] **WSAEMSGSIZE resolved:** No `WSAEMSGSIZE`/`TooLarge` log line appears during a normal session on Windows.
- [ ] **PTY reader thread exits on orphan:** After orphaning a session, verify the `spawn_blocking` reader task exits within 1 s (the signal pipe woke it up); verify blocking thread count does not grow with orphan count.
- [ ] **Password prompt no echo:** Start `read -s` in the shell; type characters; confirm client terminal model shows NO predicted characters.
- [ ] **Datagram staleness rejected:** Send two datagrams in reverse order (seq=10 then seq=5); confirm client applies only seq=10.
- [ ] **Reattach during datagram sync:** Connect, trigger datagram state, disconnect, reattach; confirm replay completes before any post-reattach datagram is applied.
- [ ] **Windows CI runs:** A `windows-latest` GitHub Actions job runs `cargo test` (not just `cargo build`) and passes.
- [ ] **Security doc covers:** TOFU first-contact gap (with mitigation), privilege model (no privsep), datagram replay analysis, echo-suppression for passwords, reattach two-factor design.

---

## Recovery Strategies

| Pitfall | Recovery Cost | Recovery Steps |
|---------|---------------|----------------|
| Prediction in cursor-addressing apps ships corrupted | HIGH (UX regression, requires protocol change) | Add epoch-reset-on-cursor-move; requires prediction format revision; client-only change |
| Wide char prediction cursor drift ships | MEDIUM (affects CJK users, hard to test without CJK environment) | Add `unicode-width` column advance; client-only change; deploy new client binary |
| PTY reader zombie race fills blocking pool | MEDIUM (server degradation, not crash) | Implement self-pipe interrupt; requires server restart to clear blocked threads |
| Password echo leak (noecho prediction) ships | HIGH (security incident for password users) | Disable prediction entirely as emergency hotfix; then implement echo-state tracking |
| WSAEMSGSIZE / datagram MTU bug ships | LOW (warning log, not functional breakage) | Fix outgoing datagram size cap; Windows-only change |
| Windows CI false-green masks runtime bugs | MEDIUM (bugs discovered in production) | Add Windows runner job; run existing tests; fix any failures found |
| Security doc omits TOFU gap | MEDIUM (operator misunderstanding) | Update doc; no code change needed |
| Token rotation W1 pattern broken during refactor | HIGH (sessions permanently un-reattachable after one failed send) | Revert refactor; restore mint-send-commit order |

---

## Pitfall-to-Phase Mapping

| Pitfall | Prevention Phase | Verification |
|---------|------------------|--------------|
| Prediction in cursor-addressing apps (vim/less) | M4 Predictive echo — design gate | Adversarial test: vim session with prediction active; zero corrupt cells |
| Wide char cursor drift | M4 Predictive echo — initial implementation | Typed CJK; cursor column correct after each character |
| Paste / bulk input flood | M4 Predictive echo — initial implementation | Bracketed paste; zero predicted cells during paste |
| Epoch/confirmation desync after datagram loss | M4 Predictive echo — datagram format design | 30% loss simulation; self-corrects within 2 RTTs |
| Datagram MTU / WSAEMSGSIZE | M4 Predictive echo + daily-driver hardening | Datagram size assertion; no WSAEMSGSIZE on Windows |
| PTY reader zombie race | M4 Daily-driver hardening — first task | Thread count stable under orphan load; reader exits within 1s of orphan |
| Datagram sync / reattach race | M4 Predictive echo — reattach integration | Reattach during datagram session; no torn view |
| TOFU first-contact gap (doc) | M4 Security design pass | Security doc names TOFU gap + mitigation |
| Predicted echo as info leak (noecho) | M4 Predictive echo — initial implementation | `read -s` shows no predicted characters |
| Datagram injection/replay (doc) | M4 Security design pass | Security doc covers QUIC TLS auth + monotonic seq |
| Privilege model gap (doc) | M4 Security design pass | Security doc has "Privilege Model" section |
| Windows CI false-green | M4 Daily-driver hardening | `windows-latest` CI job runs and passes `cargo test` |

---

## Sources

- Mosh USENIX paper (Keith Winstein & Hari Balakrishnan, 2012): epoch-based prediction, conservative mode, wide character gap — https://mosh.org/mosh-paper.pdf
- Mosh GitHub issue #932: predictive echo + fish autosuggestions interaction (text pushed right instead of overwritten) — https://github.com/mobile-shell/mosh/issues/932
- Mosh GitHub issue #6: misdisplayed prediction with Enter at bottom of screen (prediction at scrolling boundary) — https://github.com/mobile-shell/mosh/issues/6
- Mosh bracketed paste implementation (commit c6bf3a2) — https://github.com/mobile-shell/mosh/commit/c6bf3a2025e86b34512a995ea8b1e45d7586860f
- Mosh GitHub issue #427: bracketed paste background — https://github.com/mobile-shell/mosh/issues/427
- tokio `spawn_blocking` abort semantics (cannot abort once started): https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html
- tokio discussion #6570: Aborting a Task with spawn_blocking — https://github.com/tokio-rs/tokio/discussions/6570
- quinn issue #1572: UDP packet size and MTU fragmentation — https://github.com/quinn-rs/quinn/issues/1572
- Marten Seemann: IP fragmentation and DPLPMTUD — https://seemann.io/posts/2025-02-19---ip-fragmentation/
- RFC 9221: Unreliable Datagram Extension to QUIC (datagrams are unreliable, not replayed) — https://datatracker.ietf.org/doc/rfc9221/
- RFC 9001: QUIC-TLS (TLS 1.3 provides per-packet authentication including datagrams)
- QUIC security review (Springer 2022): datagram injection and replay surface analysis — https://link.springer.com/article/10.1007/s10207-022-00630-6
- SSH agent explained (Smallstep): agent attack surface, no key export — https://smallstep.com/blog/ssh-agent-explained/
- TOFU weakness (Wikipedia/HN): first-contact MITM window — https://news.ycombinator.com/item?id=41848404
- nosh codebase v1.1: session.rs (PTY spawn, spawn_blocking reader), server.rs (output_reader.abort() pattern), registry.rs (W1 mint-send-commit token rotation)
- nosh PROJECT.md v1.2: PTY reader-zombie race identified as latent tech debt requiring `/gsd:debug` pass
- dvtm PR #69: self-pipe trick for SIGWINCH/SIGCHLD in PTY-using programs — https://github.com/martanne/dvtm/pull/69

---
*Pitfalls research for: QUIC-based roaming remote shell (nosh), v1.2 M4 Predictive Echo + Daily-Driver Hardening*
*Researched: 2026-06-01*
