# Pitfalls Research

**Domain:** QUIC-based roaming remote shell (Rust) — v1.1 M3 Roaming + Windows Client
**Researched:** 2026-05-30
**Confidence:** HIGH (quinn/rustls official docs; RFC 9000; ssh-key crate docs; Windows terminal research; QUIC path validation research)

---

## CRITICAL PITFALLS

### Pitfall 1: `ServerConfig::migration` Not Set — Migration Silently Disabled on the Server

**What goes wrong:**
The client IP changes (Wi-Fi→cellular, NAT rebind), quinn sends updated packets from the new address, but the server drops them. The session stalls, then times out. `quinn`'s `ServerConfig::migration` defaults to `true`, but if anyone explicitly set it to `false` for an earlier test or security audit, the server silently refuses all client address changes.

**Why it happens:**
Migration is a server-side gate: `ServerConfig::migration(bool)`. Even if the client correctly uses a fresh connection ID on the new path (as RFC 9000 §9.3 requires), the server will ignore address changes when migration is disabled. The failure looks like a network drop, not a config error.

**How to avoid:**
- Confirm `ServerConfig::migration(true)` is set explicitly, not just relying on the default.
- Add an integration test: establish a connection on loopback `127.0.0.1`, then force the client to rebind to `127.0.0.2` (or use a dual-interface test) and assert the connection survives.
- In headless CI, simulate migration by changing the client socket binding mid-session; verify no `ConnectionError::ConnectionClosed` results.

**Warning signs:**
Session drops immediately after a network interface switch; `ConnectionError::TimedOut` rather than continued operation; migration test passes on loopback but fails on real interfaces.

**Phase to address:**
Phase 1 (migration) — first thing to verify before any other migration work.

---

### Pitfall 2: Path Validation Anti-Amplification Stall After Migration

**What goes wrong:**
After the client migrates to a new IP, the server is subject to the anti-amplification limit on the new path: it may send at most 3× the bytes it has received from that address until path validation completes (RFC 9000 §9.4). In a shell session with large server output (e.g. `cat bigfile`), the server stalls mid-stream waiting for the client to send enough data to raise the amplification limit, even though the connection is alive. This manifests as an output pause of 1–2 RTTs after every migration.

**Why it happens:**
RFC 9000 mandates this limit to prevent amplification attacks. On migration, the client's new address is initially unvalidated. The server sends PATH_CHALLENGE and waits for PATH_RESPONSE before fully lifting the limit. Any large burst of server output during this validation window is throttled.

**How to avoid:**
- Send a small PING or control frame from the client immediately after detecting that migration has occurred (to quickly advance the amplification budget for the server).
- In the migration test, pipe a large payload from server→client and assert no gap longer than 2 RTTs appears in the output stream after a simulated path change.
- Do not attempt to pre-validate an alternate path before the migration (multipath) — that is a separate QUIC extension not in scope for v1.1.

**Warning signs:**
Terminal output pauses for ~100–500 ms after a network change; `cat`/`tail -f` pauses then resumes; no connection error, just a delay.

**Phase to address:**
Phase 1 (migration) — validate with an output-heavy stream test around migration events.

---

### Pitfall 3: Connection ID Linkability — Not Rotating CIDs on Migration

**What goes wrong:**
The client migrates to a new IP but keeps using the same connection ID. An eavesdropper on both the old and new network paths (e.g. a corporate VPN and a cellular carrier) can correlate the two paths to the same user session. RFC 9000 §9.5 explicitly requires that a migrating endpoint use a new connection ID to prevent linkability.

**Why it happens:**
quinn manages CID rotation automatically if the endpoint has been supplied with enough CIDs via `NEW_CONNECTION_ID` frames. The failure mode is: the server runs out of CIDs to issue (because `EndpointConfig` sets a low `cid_generator` limit or no fresh CIDs are issued), so the client is forced to reuse the current CID. The functional behavior is correct but the privacy property is broken.

**How to avoid:**
- Ensure the server issues new CIDs proactively via quinn's built-in mechanism. The default `EndpointConfig` generates new CIDs automatically.
- After a migration, verify (in tests with QUIC event logging enabled) that the new-path packets use a CID different from those on the old path.
- Enable quinn's `qlog` feature during integration testing and inspect `connection_id_updated` events around path changes.

**Warning signs:**
QUIC qlog shows no `new_connection_id` events around migration; the same CID appears in packets on both the old and new local address.

**Phase to address:**
Phase 1 (migration) — validate via qlog inspection, not just functional correctness.

---

### Pitfall 4: Keep-Alive and Migration Interact — Session Drops Immediately After IP Change if Keep-Alive Is on the Wrong Side or Misconfigured

**What goes wrong:**
After migration, the old path is dead. If the server is sending keep-alive PINGs and the client is not, the server continues PINGing the old address (it may not have received the path-change notification yet). The client, now on a new address, gets no PING ACKs. Both sides' idle timers start counting. If the session idle timeout is shorter than the path-validation round trip, the session drops before migration completes.

**Why it happens:**
Keep-alive is configured on the **client** per the v1.0 design. However, there is a gap: during the migration window, the server may not immediately begin sending on the new path. If the client has migrated but the keep-alive interval fires before path validation is complete (and the PING goes to the new path but gets no ACK because path is still validating), the client may prematurely declare timeout.

**How to avoid:**
- Keep the client-side `keep_alive_interval` at 15 s and `max_idle_timeout` at 300 s (as established in v1.0), with the idle timeout large enough to survive multi-second path validation.
- Do not reduce idle timeout for "faster failure detection" during migration testing — this is the most common footgun.
- Set `max_idle_timeout` on both sides to at least 60 s during migration tests; reduce only after migration is proven stable.

**Warning signs:**
Session drops with `ConnectionError::TimedOut` within 1–5 s of an IP change; reducing keep-alive interval "fixes" it (a lie — you've just made keep-alive fire more often than the timeout).

**Phase to address:**
Phase 1 (migration) — verify idle timeout survives a 5–10 s simulated path change window.

---

### Pitfall 5: Orphaned Session Memory Growth Without a Cap — Per-Identity Limit Is the Safety Valve

**What goes wrong:**
Server-side sessions persist after client disconnect (Mosh-style). Without a per-identity cap, a user who connects and disconnects repeatedly (or a client that crashes in a loop) accumulates orphaned sessions. Each holds a live PTY, a shell process, and buffered output. Memory grows without bound. At 10 sessions of ~5 MB each, that is 50 MB per user; at 100 sessions it is a denial-of-service against the server.

**Why it happens:**
The idle timeout defaults to 0 (disabled) per the v1.1 design decision. This is correct for the UX goal but means no automatic session eviction. Without an explicit cap, every disconnect becomes a permanent resource leak until the shell inside exits.

**How to avoid:**
- Implement the per-identity cap before enabling session persistence. The cap should be configurable (default 5 sessions per identity); any new connection that would exceed the cap must either reject the reattach, evict the oldest orphan, or be explicitly documented.
- Store orphaned sessions in a `HashMap<SshIdentityFingerprint, VecDeque<OrphanedSession>>` with a max-len guard on insert.
- Log a warning whenever the cap is hit; do not silently evict.

**Warning signs:**
Server memory grows monotonically under stress; `ps aux` shows many shell processes per SSH fingerprint; no limit in the orphan map data structure.

**Phase to address:**
Phase 2 (session persistence) — the cap must exist before first orphan is stored, not added later.

---

### Pitfall 6: Zombie Shell Processes When PTY Outlives the QUIC Connection

**What goes wrong:**
The server's QUIC connection closes (clean or abrupt). The server stores the orphaned session (PTY + shell PID). If the shell eventually exits but the server never calls `Child::wait()` (or equivalent), the process enters zombie state: the OS keeps the exit-status entry in the process table. At scale, a server accumulates thousands of zombie entries.

**Why it happens:**
`portable-pty`'s `Child` trait requires explicit `wait()` or `try_wait()` calls; there is no auto-reaping. The orphaned session struct holds the `Child` handle. If the `OrphanedSession` is dropped without calling `wait()`, the zombie persists until the server process exits.

**How to avoid:**
- Run a background task (e.g. `tokio::spawn`) that periodically calls `child.try_wait()` on all orphaned sessions and removes entries where the shell has exited.
- In `OrphanedSession`'s `Drop` impl, send SIGHUP to the shell if the shell is still running and the session is being evicted.
- Write a test: create an orphaned session, let the shell exit, assert that `ps aux | grep Z` shows no zombie within 5 s.

**Warning signs:**
`ps aux | grep defunct` shows entries accumulating over time; shell exit is not logged by the server even though the PTY master fd is readable.

**Phase to address:**
Phase 2 (session persistence) — implement the reaper task immediately alongside the orphan store.

---

### Pitfall 7: SIGHUP Sent to Shell on Client Disconnect — Session Terminates Instead of Persisting

**What goes wrong:**
The client QUIC connection closes. The server closes the PTY master fd (e.g. by dropping the `MasterPty` handle). The kernel detects no open master fd for the PTY and delivers SIGHUP to the shell (session leader). The shell exits. The session does not persist — the user's long-running job is killed.

**Why it happens:**
SIGHUP is sent by the kernel to the PTY session leader when the controlling terminal is "hung up" — which is exactly what happens when the last open master fd is closed. This is the correct Unix behavior for a session that ends, but the wrong behavior for a session that should persist.

**How to avoid:**
- Do not close the `MasterPty` handle when the client disconnects. Instead, move the `MasterPty` (and the `Child`) into the `OrphanedSession` struct. Keep the master fd open.
- The server process must hold the master fd open for the lifetime of the orphaned session. This is what prevents SIGHUP.
- Verify by running a shell that prints a message on SIGHUP (`trap 'echo got SIGHUP' HUP`), disconnecting the client, and asserting the message does not appear.

**Warning signs:**
Shell exits immediately when the client disconnects; log shows "session terminated" at client disconnect time rather than at shell exit time.

**Phase to address:**
Phase 2 (session persistence) — this is the core correctness requirement; get it right before any reconnect work.

---

### Pitfall 8: Cold Reattach Token Bound to Transport Layer, Not SSH Identity — Session Hijacking Risk

**What goes wrong:**
The reattach token is issued to a connection (e.g. derived from the QUIC connection ID or a random nonce stored per-connection) rather than bound to the authenticated SSH identity. An attacker who learns the token (e.g. from a server log, a side channel, or a compromised network path) can reattach to the session without possessing the SSH private key.

**Why it happens:**
The natural implementation is: "on disconnect, store the session with a random token; on reattach, check the token." The token check replaces auth. This is wrong for nosh — the token must be a secondary check; the primary check is that the reconnecting client can prove it holds the same SSH identity.

**How to avoid:**
- Reattach authorization is a **two-factor check**: (1) the new QUIC connection must complete the same mutual SSH-key TLS handshake as the original connection — the same `authorized_keys` check runs; (2) the `ReattachRequest` control message must carry the session token AND the SSH identity fingerprint from step 1 must match the fingerprint stored in the orphaned session.
- The session token prevents reattaching to the wrong session (session selector), but the SSH handshake prevents theft.
- If either check fails, close the connection with an error; do not reveal whether a session with that token exists (oracle leak).

**Warning signs:**
Reattach is implemented without re-running the TLS mutual auth; a test that reattaches with a correct token but a different key succeeds.

**Phase to address:**
Phase 3 (cold reattach) — the identity check must be in the design from the start; it cannot be retrofitted without changing the protocol.

---

### Pitfall 9: Sequence-Number Resync Delivers Duplicate or Missing Output on Reattach

**What goes wrong:**
The client reconnects and sends a `ReattachRequest{last_seen_sequence: N}`. The server resumes from sequence N+1. However:
- If the server's "sent up to sequence M" counter is not the same as "acknowledged by client up to sequence N" (because the sequence space is not continuously tracked), the server may re-send output the client already displayed, or skip output the client never received.
- If sequence numbers are 32-bit and the session generates enough output between connections, wrap-around causes the comparison to be incorrect.

**Why it happens:**
Sequence numbers in a reattach protocol are easy to get wrong because they cover the boundary between two different QUIC connections — reliable delivery within a connection is handled by QUIC streams, but the cross-connection gap must be handled by the application layer.

**How to avoid:**
- Use a monotonically increasing, never-resetting u64 sequence number for server→client output. Assign sequence numbers per-byte or per-message at the application layer (not QUIC stream offset, which resets per connection).
- The server maintains a ring buffer of the last N bytes of output (for scrollback on reconnect). On reattach, it replays from `last_seen_sequence + 1` up to the current tail.
- On initial connection, `last_seen_sequence = 0` (no replay). On reattach, the client supplies its last acknowledged sequence.
- Write a test: disconnect mid-stream during a large output run; reconnect; verify the combined output on the client matches the full server-side output with no gaps or duplicates.

**Warning signs:**
Duplicate lines appear after reconnect; output from before disconnect is missing; scrollback is inconsistent between connect and reconnect paths.

**Phase to address:**
Phase 3 (cold reattach) — sequence numbering must be designed before any reconnect message is defined.

---

### Pitfall 10: Reattach Race — Two Clients Claim the Same Session

**What goes wrong:**
A client disconnects but the QUIC connection is not yet fully dead (still in the idle-timeout window). A second client (or the same client reconnecting quickly) sends a `ReattachRequest` for the same session. Both connections are briefly active; the old connection's event loop is still reading from the PTY master fd. The shell receives garbled input from two readers; the new client gets incomplete output.

**Why it happens:**
The original connection is in a zombie state: the application layer has not yet received `ConnectionError` (the idle timeout hasn't fired). The server's orphan store doesn't yet have the session (it is still attached to the old connection). The new reattach request arrives before the original disconnect is fully processed.

**How to avoid:**
- The server must atomically transition a session from "connected" to "orphaned" to "reconnected." Use a state machine with explicit states: `Active(connection_id)`, `Orphaned(token, fingerprint)`, `Reconnecting(token, fingerprint, new_connection_id)`.
- Only transition to `Reconnecting` if the session is in `Orphaned` state. If `Active`, send a `ReattachConflict` error.
- On the original connection's final close (however it arrives), the state transition from `Active` to `Orphaned` must run exactly once.
- A session in `Active` state is not reattachable — the reattach request must wait until the session becomes `Orphaned` or fail fast.

**Warning signs:**
Two simultaneous connections produce garbled shell output; reattach succeeds when the original client is still connected; no state machine around session lifecycle.

**Phase to address:**
Phase 3 (cold reattach) — the state machine is the protocol; implement it before any reconnect logic.

---

### Pitfall 11: `Session.identity` Fingerprint Captured Before Handshake Completes

**What goes wrong:**
The `Session.identity` field is populated from the peer cert during connection setup, but the code path that reads `connection.peer_identity()` or the equivalent is called before the TLS handshake completes. The result is `None` or the previous connection's identity (if the connection object is reused). Reattach authorization then either panics, silently succeeds with no identity check, or compares against stale data.

**Why it happens:**
`quinn`'s `Connecting::await` completes when the QUIC handshake finishes, but the peer certificate is not available until `connection.peer_identity()` is called after `Connecting` resolves. If the auth code calls this method too early (on a `Connecting` future, not a fully established `Connection`), it gets nothing or garbage.

**How to avoid:**
- Extract `Session.identity` exactly once, immediately after `connecting.await?` resolves to a `Connection`, before any application data is processed.
- Encapsulate the extraction in a `NoshSession::from_authenticated_connection(conn: Connection) -> Result<NoshSession>` constructor that extracts and validates identity, returning `Err` if absent.
- Write a test that asserts `session.identity` equals the fingerprint of the key that was presented in the handshake (not just that it is non-empty).
- Never allow a `Session` struct to exist without a populated identity field — use a type-level guarantee (the `identity` field is not `Option<T>`, it is `T`, populated at construction).

**Warning signs:**
`session.identity` is `Option<_>` and code does `unwrap_or_default()` or `unwrap_or(EMPTY)`; identity is populated in a separate `init()` method called after construction.

**Phase to address:**
Phase 4 (identity threading) — this unblocks the rest of v1.1; address in the first phase before migration or persistence work.

---

### Pitfall 12: Windows On-Disk Private Key — Key Material Remaining in Process Memory After Use

**What goes wrong:**
The Windows client reads the private key from disk with `PrivateKey::read_openssh_file()`, uses it to sign the TLS handshake, and then drops the `PrivateKey` struct. However, due to Rust's memory model (no guarantee of memory scrubbing on drop by default), the raw key bytes may remain in process memory and be visible in a crash dump, core dump, or via process memory inspection.

**Why it happens:**
`ssh-key`'s `PrivateKey` struct uses `Zeroizing<Vec<u8>>` for key material internally, which scrubs memory on drop. However, the decrypted key bytes may have been copied into intermediate buffers (e.g. during passphrase decryption, DER encoding, or signature algorithm lookup) that are not zeroized. This is a best-effort mitigation, not a guarantee.

**How to avoid:**
- Sign only inside a narrow scope: load, sign, drop immediately. Do not hold the `PrivateKey` across async await points.
- Use `let key = ...; let sig = key.sign(...)?;` — the key is dropped at end of the block, not after the connection's lifetime.
- For passphrase-protected keys, use `PrivateKey::decrypt()` inside the same narrow scope.
- Explicitly document that Windows key-file signing is a temporary exception to the "never handle the private key directly" invariant, with a code comment pointing to the M5/M6 Windows agent integration path.
- Do not log, trace, or serialize the private key at any level.

**Warning signs:**
`PrivateKey` stored in a struct field that lives for the connection duration; passphrase or key bytes appear in `tracing` spans.

**Phase to address:**
Phase 5 (Windows client) — this design discipline must be baked in before first implementation; not a refactor.

---

### Pitfall 13: Windows File Permission Check for Private Key Uses `std::fs::Permissions` — ACLs Not Checked

**What goes wrong:**
The Windows client loads the private key from disk and optionally warns if permissions are too open. Using `std::fs::metadata().permissions().readonly()` only checks `FILE_ATTRIBUTE_READONLY` — it does not evaluate Windows ACLs. A key file readable by other users on the system (via ACL) passes the check and no warning is issued.

**Why it happens:**
`std::fs::Permissions` on Windows wraps `FILE_ATTRIBUTE_READONLY` only. The Rust standard library explicitly documents that it does not read ACLs. OpenSSH for Windows (`Win32-OpenSSH`) uses `GetNamedSecurityInfo` + `GetSecurityDescriptorDacl` to verify that only the key owner has access. Rust's `std::fs` cannot replicate this.

**How to avoid:**
- For v1.1, emit a warning at startup if the key file is world-readable by `std::fs` check, but clearly document that the ACL check is not implemented and the user should manually verify.
- Do not claim the permission check is comprehensive; treat it as best-effort.
- Optionally use `windows-acl` crate or raw Win32 API (via `windows` crate) for a real ACL check — but this is optional scope for v1.1.
- Document this gap as a known limitation in the Windows client.

**Warning signs:**
Code uses `fs::metadata().permissions().readonly()` and claims it has validated key security; no documentation of the ACL limitation.

**Phase to address:**
Phase 5 (Windows client) — at minimum emit the best-effort warning and document the gap.

---

### Pitfall 14: Windows Client Resize Events — WINDOW_BUFFER_SIZE_RECORD vs. SIGWINCH

**What goes wrong:**
The Windows client needs to detect terminal resize and send a resize message to the server. On Linux, this is `SIGWINCH`. On Windows, there is no `SIGWINCH` — resize events arrive as `WINDOW_BUFFER_SIZE_RECORD` records in `ReadConsoleInputW`, or in VT mode are not delivered at all. A Windows client that waits for `SIGWINCH` (or the `crossterm` Unix path) never sends resize messages; the server PTY stays at its initial size.

**Why it happens:**
`crossterm` handles resize events on both platforms, but the mechanism is fundamentally different. On Windows, resize events can be confused with scroll events (`WINDOW_BUFFER_SIZE_RECORD` is emitted for both). Additionally, `crossterm::terminal::size()` in some Windows Terminal versions returns the buffer size (including scrollback) rather than the viewport size when the terminal is not in VT mode.

**How to avoid:**
- Use `crossterm::event::EventStream` to poll resize events — it abstracts the platform difference. On Windows, this polls `ReadConsoleInputW` internally.
- After any resize event, double-check the dimensions with `crossterm::terminal::size()` and send the authoritative size, not the event's delta.
- Test resize behavior specifically inside Windows Terminal (not just in cmd.exe or PowerShell): start a session, resize the window, verify the server PTY reflects the new size within 100 ms.
- Do not implement a SIGWINCH listener on Windows — it does not exist.

**Warning signs:**
Resize events are never sent from the Windows client; `vim` or `less` does not reflowing content on terminal resize; resize works on Linux client but not Windows.

**Phase to address:**
Phase 5 (Windows client) — write a resize integration test that is Windows-only.

---

### Pitfall 15: Windows Client CRLF / Codepage — Raw Mode Breaks or Output Is Garbled

**What goes wrong:**
The Windows client enters raw mode (disables `ENABLE_PROCESSED_INPUT` and `ENABLE_LINE_INPUT`). The server sends VT/ANSI escape sequences for the remote terminal. Windows Terminal handles these natively, but cmd.exe and older PowerShell hosts do not enable `ENABLE_VIRTUAL_TERMINAL_PROCESSING` by default. Result: escape sequences are printed verbatim rather than interpreted, and the terminal output is unreadable garbage.

**Why it happens:**
`ENABLE_VIRTUAL_TERMINAL_PROCESSING` must be explicitly set on the Windows console output handle before escape sequences are interpreted. `crossterm` sets this flag when entering raw mode, but only for the console it controls — not for inherited console handles. Additionally, the default Windows codepage is not UTF-8 (it is typically CP1252 or similar); non-ASCII characters in the remote shell output are misinterpreted.

**How to avoid:**
- Call `crossterm::terminal::enable_raw_mode()` — it handles `ENABLE_VIRTUAL_TERMINAL_PROCESSING` and `ENABLE_PROCESSED_OUTPUT` flags.
- At startup, check the current output codepage via `GetConsoleOutputCP()` (accessible via the `windows` crate) and emit a warning if it is not 65001 (UTF-8). Ideally, set it via `SetConsoleOutputCP(65001)` or instruct the user.
- Test the client in both Windows Terminal and a legacy cmd.exe window to verify both work (or at least fail gracefully).

**Warning signs:**
Escape sequences printed as literal `^[[H^[[2J` strings; non-ASCII characters replaced with `?` or `â€`; output is correct in Windows Terminal but garbled in cmd.exe.

**Phase to address:**
Phase 5 (Windows client) — test in both Windows Terminal and a legacy host.

---

## Technical Debt Patterns

| Shortcut | Immediate Benefit | Long-term Cost | When Acceptable |
|----------|-------------------|----------------|-----------------|
| Skip per-identity session cap for now | Simpler orphan store | Unbounded memory growth under crash/reconnect abuse | Never; cap must exist before first orphan is stored |
| Session state machine as ad-hoc `Option<Connection>` flags | Simpler code | Reattach race condition; two clients one session | Never; use explicit state enum |
| Hold `PrivateKey` for the lifetime of the connection | Simpler key-loading code | Key material in memory longer than needed; visible in dumps | Never; narrow-scope load-sign-drop |
| Check file permissions with `std::fs` on Windows | Cross-platform code path | ACLs not checked; false security assurance | Acceptable for v1.1 with explicit documentation |
| `crossterm` resize polling without double-check of `terminal::size()` | Simpler event loop | Stale size sent to server on fast consecutive resizes | Only if resize accuracy is not critical |
| Reattach token without re-running SSH auth | Simpler reconnect flow | Session hijacking if token is leaked | Never; auth runs on every connection |
| Store orphaned sessions in a global `Mutex<HashMap>` | Trivial to implement | Contention at scale; hard to integrate per-identity cap | Only if sessions per server is very small (< 10) |

---

## Integration Gotchas

| Integration | Common Mistake | Correct Approach |
|-------------|----------------|------------------|
| quinn migration | Not setting `ServerConfig::migration(true)` explicitly | Assert migration is enabled in server config constructor; add integration test |
| QUIC path validation | Assuming anti-amplification is transparent | Factor in 1–2 RTT stall after migration in output-heavy test assertions |
| Session orphan store | Inserting session before verifying identity is threaded | Use `NoshSession::from_authenticated_connection()` constructor that panics if identity absent |
| Cold reattach auth | Checking token only, not re-running SSH handshake | Two-factor check: SSH handshake + token + identity fingerprint match |
| Windows key loading | Holding `PrivateKey` in a `Connection` struct | Narrow scope: load in signing function, drop before first `await` |
| Windows resize | Using `SIGWINCH` handler on Windows | Use `crossterm::event::EventStream` which abstracts platform |
| Windows raw mode | Assuming `enable_raw_mode()` handles VT processing | Test in both Windows Terminal and cmd.exe; VT processing may be off in legacy hosts |
| Zombie orphan cleanup | No periodic `try_wait()` on orphaned child processes | Background reaper task polls all orphaned sessions |
| Sequence resync | Using QUIC stream offsets as sequence numbers | Use application-layer u64 monotonic counter; QUIC offsets reset per connection |

---

## Performance Traps

| Trap | Symptoms | Prevention | When It Breaks |
|------|----------|------------|----------------|
| PATH_CHALLENGE flood attack per Seemann 2023 | Server memory grows unbounded if attacker sends many PATH_CHALLENGEs | quinn 0.11.x limits queued PATH_RESPONSE frames to 256 per connection — verify the version in use includes this fix | Under deliberate attack; quinn >= 0.10.4 has the fix |
| Orphaned session ring buffer unbounded growth | Server memory grows proportional to shell output × session count | Cap ring buffer at a fixed size (e.g. 64 KB per session); this bounds the reattach replay size | When many sessions are orphaned and shells produce high output |
| Resize coalescing not applied on Windows | Resize flood on window drag on Windows too | Apply same 30–50 ms debounce as Linux path | Any window drag on Windows client |
| Cold reattach replays entire ring buffer on reconnect | Reconnect latency grows with buffer size | Only replay unacknowledged bytes (from `last_seen_sequence`), not the full buffer | If ring buffer is large and reconnect is frequent |

---

## Security Mistakes

| Mistake | Risk | Prevention |
|---------|------|------------|
| Reattach token replaces SSH auth (not supplements it) | Token theft = session hijacking without key possession | SSH handshake runs on every connection; token is a session selector only |
| Session token logged or included in error messages | Token exposure via log aggregators | Never log the token; use session IDs (non-secret) for logging |
| Oracle: reveal whether session token exists on mismatch | Attacker enumerates valid tokens | Return same error for "bad token" and "bad identity" |
| Windows private key in memory beyond signing scope | Key visible in dumps/memory probes | Narrow scope; `Zeroizing` types in `ssh-key` help but are not a guarantee |
| Windows key file world-readable (ACL gap) | Any user on the system can read the private key | Warn if `FILE_ATTRIBUTE_READONLY` is not set; document ACL limitation |
| CID reuse across migration paths | Correlates user across networks | Rely on quinn's automatic CID rotation; verify with qlog |
| Orphaned session not bound to SSH identity | Any authenticated user can reattach to any orphan | Identity fingerprint stored in `OrphanedSession`; checked on reattach |

---

## UX Pitfalls

| Pitfall | User Impact | Better Approach |
|---------|-------------|-----------------|
| No resize event sent on cold reattach | Remote editor/pager wrong size after reconnect | Send a resize message as part of the `ReattachRequest` or immediately after reattach completes |
| Reattach latency hides behind "connecting..." | User unsure if reconnect is working or hung | Emit a brief client-side status message: "Reconnecting..." then "Session resumed." |
| Windows codepage warning on every startup | Annoys users who already have UTF-8 set | Only warn once; cache the check; suppress if codepage is already 65001 |
| Orphan eviction kills a running job silently | User loses work | Log a message when an orphan is evicted due to the per-identity cap; make the cap configurable |
| Session token visible in process arguments | Leaks token to other local users via `ps` | Pass token via control message on the QUIC stream, not as a command-line argument |

---

## "Looks Done But Isn't" Checklist

- [ ] **Migration enabled:** Assert `ServerConfig::migration(true)` is set; run a test that migrates the client IP and verifies the session survives.
- [ ] **CID rotation on migration:** Inspect QUIC qlog; verify new CIDs are used on the new path.
- [ ] **Anti-amplification stall test:** Run a large server→client output stream through a simulated migration; assert no pause longer than 3 RTTs.
- [ ] **Session persists through disconnect:** Disconnect client mid-session; verify server-side shell is still running; verify no SIGHUP delivered.
- [ ] **Per-identity cap enforced:** Create more sessions than the cap for one identity; verify new connections are rejected or oldest orphan is evicted.
- [ ] **Zombie reaper running:** Let multiple orphaned shells exit; within 10 s, verify `ps aux | grep defunct` shows no zombies.
- [ ] **Reattach requires SSH re-auth:** Reattach with a valid token but a different key; verify the connection is rejected.
- [ ] **Reattach with correct identity and token succeeds:** Full happy-path reconnect test; verify output is replayed correctly from `last_seen_sequence`.
- [ ] **Sequence resync: no duplicates, no gaps:** Disconnect during large output; reconnect; diff combined output against full server-side log.
- [ ] **Identity threaded:** After handshake, log `session.identity`; assert it equals the expected SSH fingerprint.
- [ ] **Windows key-file narrow scope:** Confirm `PrivateKey` is not stored in any long-lived struct; code review the signing path.
- [ ] **Windows resize works:** In Windows Terminal, resize the window during a running session; verify the server PTY reflects the new size.
- [ ] **Windows VT processing:** Run the client in cmd.exe; verify escape sequences are interpreted, not printed literally.

---

## Recovery Strategies

| Pitfall | Recovery Cost | Recovery Steps |
|---------|---------------|----------------|
| `ServerConfig::migration` off — sessions drop on IP change | LOW | Add `.migration(true)` to server config; re-run migration integration test |
| Orphan SIGHUP kills shell on disconnect | HIGH (protocol redesign) | Restructure session ownership: `MasterPty` must be moved to orphan struct, not dropped |
| Reattach token without SSH re-auth shipped | HIGH (security incident) | Add SSH handshake check before honoring token; audit any sessions that reattached during the window |
| Sequence number wrap-around causes output corruption | MEDIUM | Upgrade u32 to u64; bump protocol version; add wrap detection test |
| Windows key file held too long — key in memory dump | MEDIUM | Refactor signing path to narrow scope; key-dropping is a code change only |
| Zombie accumulation — reaper not implemented | MEDIUM | Add `try_wait()` background task; force-kill orphans above a threshold |
| Windows VT processing not enabled — garbled output | LOW | `crossterm::terminal::enable_raw_mode()` sets the flag; verify it is called before first byte of output |

---

## Pitfall-to-Phase Mapping

| Pitfall | Prevention Phase | Verification |
|---------|------------------|--------------|
| `ServerConfig::migration` not set | Phase 1 (migration) | Integration test: IP change during active session; session must survive |
| Anti-amplification stall after migration | Phase 1 (migration) | Large-output stream test through migration event |
| CID linkability across migration | Phase 1 (migration) | qlog inspection: new CIDs on new path |
| Keep-alive/idle timeout misconfig during migration | Phase 1 (migration) | 5–10 s simulated path change; no `TimedOut` |
| Orphaned session memory growth (no cap) | Phase 2 (session persistence) | Stress test: many disconnects; memory stays bounded |
| Zombie shell processes | Phase 2 (session persistence) | Orphan then exit shell; `ps aux | grep defunct` = empty |
| SIGHUP kills shell on disconnect | Phase 2 (session persistence) | Disconnect client; shell still running after 5 s |
| Reattach token without SSH re-auth | Phase 3 (cold reattach) | Negative test: correct token, wrong key → rejected |
| Sequence number resync (duplicates/gaps) | Phase 3 (cold reattach) | Diff combined output against full server log |
| Reattach race (two clients one session) | Phase 3 (cold reattach) | Concurrent reattach test; only one must succeed |
| `Session.identity` not threaded | Phase 4 (identity threading) | Assert `session.identity` matches presented key fingerprint |
| Windows key material in memory | Phase 5 (Windows client) | Code review + test: `PrivateKey` not in long-lived structs |
| Windows file permission ACL gap | Phase 5 (Windows client) | Document limitation; best-effort `readonly()` warning |
| Windows resize (no SIGWINCH) | Phase 5 (Windows client) | Windows-only resize integration test |
| Windows VT processing off in legacy hosts | Phase 5 (Windows client) | Test in cmd.exe; escape sequences interpreted not printed |

---

## Sources

- Quinn `ServerConfig` docs — `migration()` method and default (enabled): https://docs.rs/quinn/latest/quinn/struct.ServerConfig.html
- Quinn `TransportConfig` docs — `keep_alive_interval`, `max_idle_timeout` defaults: https://docs.rs/quinn/latest/quinn/struct.TransportConfig.html
- RFC 9000 §9.4 — Anti-amplification limit on new paths (3× bytes received): https://www.rfc-editor.org/rfc/rfc9000.html
- RFC 9000 §9.5 — CID rotation requirement on migration for privacy: https://www.rfc-editor.org/rfc/rfc9000.html
- Marten Seemann — PATH_CHALLENGE flood attack, 256-frame cap fix: https://seemann.io/posts/2023-12-18---exploiting-quics-path-validation/
- ssh-key `PrivateKey` docs — `Zeroizing` types, `read_openssh_file`, signing: https://docs.rs/ssh-key/latest/ssh_key/private/struct.PrivateKey.html
- Windows OpenSSH private key permissions — ACL vs FILE_ATTRIBUTE_READONLY: https://github.com/PowerShell/Win32-OpenSSH/wiki/Security-protection-of-various-files-in-Win32-OpenSSH
- Rust `std::fs::Permissions` docs — ACLs not read: https://doc.rust-lang.org/std/fs/struct.Permissions.html
- Windows Terminal — no VT encoding for resize events; WINDOW_BUFFER_SIZE_RECORD quirks: https://github.com/microsoft/terminal/issues/394
- crossterm resize events on Windows — `WINDOW_BUFFER_SIZE_RECORD` vs SIGWINCH: https://github.com/crossterm-rs/crossterm/issues/165
- crossterm window size on Windows Terminal returns buffer not viewport: https://github.com/crossterm-rs/crossterm/issues/1021
- QUIC multipath — simultaneous both-endpoint migration via CID distinction: https://quicwg.org/multipath/draft-ietf-quic-multipath.html
- INIT.md §9 (session persistence, cold reattach design) — authoritative source for sequence-number reattach model
- INIT.md §5 (security invariants) — env sanitization, no SSH_AUTH_SOCK in env, privilege-escalation footguns

---
*Pitfalls research for: QUIC-based roaming remote shell (nosh), v1.1 M3 Roaming + Windows Client*
*Researched: 2026-05-30*
