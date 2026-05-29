# Architecture Research

**Domain:** QUIC-based roaming remote shell (Rust) — v1.1 M3 Roaming + Windows Client
**Researched:** 2026-05-30
**Confidence:** HIGH (every claim grounded in actual crate source; file:line citations throughout)

---

## System Overview

The v1.1 architecture extends the v1.0 structure by inserting a `SessionRegistry`
between the accept loop and the session lifecycle, threading `identity` through the
auth to session boundary, splitting migration (transparent) from cold reattach (explicit
protocol), and abstracting the signing path so a Windows on-disk key can substitute
for the Linux ssh-agent.

```
  CLIENT PROCESS (Linux OR Windows)                SERVER PROCESS (Linux)
  ┌──────────────────────────────────┐              ┌─────────────────────────────────────────────────┐
  │  TerminalDriver                  │              │  Endpoint (accept loop)                         │
  │  (raw mode: crossterm / Win API) │              │  ┌───────────────────────────────────────────┐  │
  │      stdin / stdout              │              │  │  connection handler task                  │  │
  │          │  ▲                    │              │  │  1. TLS handshake → auth → extract        │  │
  │          ▼  │                    │              │  │     identity (NoshPublicKey fingerprint)   │  │
  │  Session pump                    │              │  │  2. accept_bi → read first Message        │  │
  │  ┌───────────────────────────┐   │              │  │  3a. SessionOpen   → registry.open        │  │
  │  │  bidi stream (nosh/0)     │◄──┼──QUIC UDP/443┤  │  3b. Reattach      → registry.reattach   │  │
  │  │  SessionOpen / PtyData /  │   │              │  └──────────────┬────────────────────────────┘  │
  │  │  Resize / SessionClose /  │   │              │                 │                               │
  │  │  Reattach (new v1.1)      │   │              │  ┌──────────────▼────────────────────────────┐  │
  │  └───────────────────────────┘   │              │  │  SessionRegistry                          │  │
  │  ┌───────────────────────────┐   │              │  │  HashMap<SessionToken, Arc<SessionSlot>>  │  │
  │  │  SigningBackend (trait)   │   │              │  │  per-identity cap + idle GC               │  │
  │  │  Linux: AgentSigner       │   │              │  └──────────────┬────────────────────────────┘  │
  │  │  Windows: FileSigner      │   │              │                 │                               │
  │  └───────────────────────────┘   │              │  ┌──────────────▼────────────────────────────┐  │
  └──────────────────────────────────┘              │  │  SessionSlot  (Arc<Mutex<…>>)             │  │
              │                                     │  │  identity: NoshPublicKey                  │  │
        ssh-agent (Linux)                           │  │  session: Session  (pty + child + seam)   │  │
        OR on-disk key (Windows)                    │  │  conn: Option<quinn::Connection>          │  │
                                                    │  │  output_buf: SequencedOutputBuffer        │  │
                                                    │  │  idle_since: Option<Instant>              │  │
                                                    │  └───────────────────────────────────────────┘  │
                                                    └─────────────────────────────────────────────────┘
```

### What changes from v1.0

v1.0 `run_session` (in `crates/nosh-server/src/server.rs:185`) allocates a `Session`,
spawns pump tasks, and drops everything on disconnect. There is no persistent store.
The QUIC `Connection` and the `Session` struct live together and die together.

v1.1 inserts a `SessionRegistry` between the accept loop and the per-connection pump.
The `Connection` can be replaced; the `Session` (PTY + child) is independent of any
particular connection.

---

## Component Boundaries

### 1. `SessionRegistry` (new, `nosh-server/src/registry.rs`)

**Responsibility:** Owns the authoritative map of live and orphaned sessions.

**State:**
```rust
pub struct SessionRegistry {
    slots: Mutex<HashMap<SessionToken, Arc<SessionSlot>>>,
    // per-identity cap: prevents unbounded memory when idle_timeout = 0
    per_identity: Mutex<HashMap<[u8; 32], usize>>,
    max_per_identity: usize,
}
```

**Key operations:**
- `open(identity, conn, session) -> SessionToken` — creates a new slot; increments per-identity counter; returns the opaque token sent to the client in `SessionOpened`.
- `reattach(token, identity, new_conn) -> Result<Arc<SessionSlot>>` — looks up the token, verifies `slot.identity == identity` (authorization bound to SSH identity), returns the slot so the connection handler can rebind I/O.
- `mark_idle(token)` — called when a connection drops without a `SessionClose`; sets `idle_since`. Sessions with `idle_timeout > 0` are reaped by a background GC task.
- `remove(token)` — called when the shell exits; decrements per-identity counter.

**Where it lives:** A single `Arc<SessionRegistry>` is constructed in `crates/nosh-server/src/server.rs::run_accept_loop` (currently at line 101) and cloned into each spawned connection task.

**Per-identity cap:** `max_per_identity` (default `4`, configurable via `--max-sessions-per-identity`) is the memory bound. When `idle_timeout == 0` (default disabled), orphaned sessions would accumulate indefinitely without this cap; the cap forces eviction of the oldest idle slot when the limit is hit.

---

### 2. `SessionSlot` (new, `nosh-server/src/registry.rs`)

**Responsibility:** Decouples the QUIC `Connection` from the `Session` lifetime.

```rust
pub struct SessionSlot {
    pub identity: NoshPublicKey,            // bound at creation; used for reattach auth
    pub token: SessionToken,                // opaque [u8; 32], sent to client
    session: Mutex<Session>,               // the actual PTY + child (from session.rs)
    conn: Mutex<Option<quinn::Connection>>, // replaceable on reattach
    output_buf: Mutex<SequencedOutputBuffer>,
    pub idle_since: Mutex<Option<Instant>>,
}
```

**Owns:**
- The `Session` struct from `crates/nosh-server/src/session.rs:115` — the `master` PTY, `child`, `child_pid`, `idle_since` seam that already exists.
- The replaceable `Connection` handle.
- The `SequencedOutputBuffer` for reattach replay (see section 4 below).

**Does NOT own:** QUIC transport mechanics, auth logic.

---

### 3. Identity Threading (modified, `nosh-server/src/server.rs` + `nosh-auth/src/keys.rs`)

**The v1.0 seam:** `crates/nosh-server/src/session.rs:119` contains:
```rust
/// The authenticated SSH identity (Phase 2). `None` for this spike: the
/// connection handler does not yet surface the peer cert key (noted M3 seam).
pub identity: Option<NoshPublicKey>,
```

**What changes in v1.1:** After `incoming.await` resolves (handshake complete), extract the peer cert SPKI from the `quinn::Connection`. The peer's DER cert bytes are available via `conn.peer_identity()`, which returns `Option<Box<dyn Any>>` that downcasts to `Vec<CertificateDer<'static>>` under the rustls-ring backend.

```rust
// Inside handle_connection, after auth, before run_session:
let peer_key: NoshPublicKey = {
    let raw_certs = conn
        .peer_identity()
        .and_then(|id| id.downcast::<Vec<CertificateDer<'static>>>().ok())
        .context("peer identity unavailable after handshake")?;
    let cert = raw_certs.first().context("no peer cert")?;
    let spki = nosh_auth::keys::extract_spki_from_cert(cert)
        .context("extract peer SPKI")?;
    nosh_auth::keys::nosh_key_from_spki(&spki)
        .context("peer cert is not Ed25519")?
};
```

This `peer_key` is passed into `session::open(…, Some(peer_key.clone()))` at `crates/nosh-server/src/server.rs:215` (already accepts `Option<NoshPublicKey>` per `crates/nosh-server/src/session.rs:207`) and stored in the `SessionSlot.identity`.

**Required addition:** Expose `nosh_key_from_spki` (a small wrapper around the existing `parse_ed25519_from_spki` in `crates/nosh-auth/src/verifier.rs:218`) as a public function from `crates/nosh-auth/src/keys.rs`.

---

### 4. Reattach Protocol — `SequencedOutputBuffer` + new `nosh-proto` messages

**New message variants** (additions to `Message` enum in `crates/nosh-proto/src/messages.rs`):

```rust
/// Sent server → client immediately after SessionOpen succeeds. The client
/// stores this token and uses it on any subsequent cold-reconnect attempt.
SessionOpened {
    session_token: [u8; 32],
    /// Server's current output sequence number (0 for a fresh session).
    last_seq: u64,
},

/// First message the client sends on a NEW connection when reattaching.
/// Authorization is bound to the SSH identity proven by the TLS handshake
/// that completed before this message arrives.
Reattach {
    session_token: [u8; 32],
    /// Last server output sequence number the client received. Server will
    /// replay any buffered output with seq > last_acked_seq.
    last_acked_seq: u64,
},

/// Sent server → client when a Reattach is accepted.
ReattachOk {
    replaying_from_seq: u64,
},

/// Sent server → client when a Reattach is rejected.
ReattachErr {
    reason: String,
},
```

**`SequencedOutputBuffer`:** A small ring-buffer inside `SessionSlot` that tags each outgoing `PtyData` chunk with a monotonically increasing `u64` sequence number and retains up to a configurable number of bytes (default 64 KiB) for replay on reattach.

```rust
pub struct SequencedOutputBuffer {
    next_seq: u64,
    ring: VecDeque<(u64, Bytes)>,
    total_bytes: usize,
    max_bytes: usize,  // default 64 KiB
}
```

The buffer is written as a side-effect on every outgoing `PtyData` chunk so it is always current when needed for replay.

**Reattach flow (server):**

```
new QUIC connection arrives
  → accept_bi → read first Message
  → if Message::Reattach { token, last_acked_seq }:
      peer_key = extract identity from conn (same path as new-session)
      registry.reattach(token, peer_key) → Ok(slot) or Err
      if Ok:
        reply ReattachOk { replaying_from_seq }
        replay slot.output_buf.since(last_acked_seq) → PtyData frames on new stream
        rebind slot.conn = new conn
        resume normal pump
      if Err:
        reply ReattachErr { reason }
        close connection
  → if Message::SessionOpen { … }:
      (normal new-session path, unchanged)
```

**1-RTT guarantee:** The `Reattach` message arrives on the first stream of the new connection. The server validates and replies `ReattachOk` on the same stream. This is 1 RTT from the moment the QUIC handshake completes (which is itself TLS 1.3 1-RTT). Total client-perceived latency from first packet to usable session: 2 RTTs. 0-RTT stays deferred per the project decision recorded in `.planning/PROJECT.md`.

---

### 5. Migration Path (zero-code change)

QUIC connection migration (IP/path change on the same connection) is handled entirely by quinn. The pump tasks in `crates/nosh-server/src/server.rs:264` (the `tokio::select!` loop) hold handles that go through the `quinn::Connection` reference. That reference's stream operations continue working transparently when the peer's address changes via QUIC connection IDs. No changes are needed in the session layer.

**What does require attention:** The `transport_config` in `crates/nosh-proto/src/transport.rs:28` does not explicitly set a migration flag. Quinn 0.11 allows migration by default; add an explicit `transport.migration(true)` call for self-documentation and to guard against a future default change.

**Validation approach (headless):** Spawn client and server, call `endpoint.rebind(new_socket)` on the client's quinn `Endpoint` to simulate a network switch, and verify the session stream continues delivering output without any reconnect handshake.

---

### 6. Platform Abstraction: `SigningBackend` (`RawEd25519Signer` trait, `nosh-auth`)

v1.0 client always uses `ClientIdentity::from_agent` (see `crates/nosh-client/src/client.rs:39`), which requires `SSH_AUTH_SOCK` via a Unix domain socket. On Windows, `SSH_AUTH_SOCK` may be absent or backed by a named pipe; the v1.1 Windows client slice defers agent integration and reads an on-disk key file instead.

**The abstraction point:** `RawEd25519Signer` (already a trait in `crates/nosh-auth/src/signer.rs:26`) is the right boundary. `AgentSigner` implements it for Linux. Add `FileSigner` for Windows (and Linux fallback):

```rust
// crates/nosh-auth/src/signer.rs (addition)

/// An Ed25519 signer that loads the private key from an OpenSSH key file.
/// Used for the Windows client where ssh-agent is deferred (v1.1 exception).
/// SECURITY: holds the private key in memory. Use zeroize::ZeroizeOnDrop.
pub struct FileSigner {
    key: ed25519_dalek::SigningKey,
    key32: [u8; 32],
}

impl FileSigner {
    pub fn from_openssh_file(path: &Path) -> anyhow::Result<Self> {
        // Reuses nosh_auth::load_host_key (crates/nosh-auth/src/keys.rs:104)
        let private = load_host_key(path)?;
        InProcessEd25519Signer::from_ssh_private(&private).map(|s| Self {
            key: s.key,
            key32: s.public_key32(),
        })
    }
}

impl RawEd25519Signer for FileSigner { … }
```

**Client CLI change** (`crates/nosh-client/src/main.rs`): add `--identity-file <path>`. When present, build `ClientIdentity::from_signer(Arc::new(FileSigner::from_openssh_file(path)?))`. The existing `ClientIdentity::from_signer` at `crates/nosh-client/src/client.rs:27` already accepts any `Arc<dyn RawEd25519Signer>`; no structural change to the connection setup path.

**`crossterm` compatibility:** crossterm 0.28 (in `crates/nosh-client/Cargo.toml`) handles Windows console API internally. `RawModeGuard` at `crates/nosh-client/src/client.rs:208` is already cross-platform.

**SIGWINCH on Windows:** `tokio::signal::unix::SignalKind::window_change()` is Unix-only. In `crates/nosh-client/src/main.rs`, the SIGWINCH registration at line 103 must be gated `#[cfg(unix)]`. On Windows, terminal resize events arrive via `crossterm::event::Event::Resize`; add a `#[cfg(windows)]` branch using `crossterm::event::EventStream` or poll via a dedicated task. The resize debounce and `send_resize` call are shared across platforms.

**`cfg` boundary summary:** No `#[cfg]` forks needed in nosh-proto, nosh-auth (FileSigner is pure Rust), or in the server. The only platform gates are:
- `nosh-client/src/main.rs`: SIGWINCH handler (`#[cfg(unix)]`) + Windows resize polling (`#[cfg(windows)]`).
- `nosh-client/src/main.rs`: `SSH_AUTH_SOCK` lookup (can stay as `std::env::var_os` gated on non-Windows or simply fallback to `--identity-file`).

---

## Data Flow Changes for v1.1

### New-Session Flow (with identity threading)

```
Client → server: QUIC handshake (TLS CertificateVerify via AgentSigner or FileSigner)
  → handshake complete
  → server: conn.peer_identity() → extract NoshPublicKey  [NEW v1.1]
  → accept_bi → read SessionOpen { term, cols, rows, env }
  → registry.open(identity, conn, session)               [NEW v1.1]
      → session::open(passwd, …, Some(identity))         [identity threaded]
      → SessionToken generated ([u8; 32], random)
      → slot inserted in registry
  → write SessionOpened { session_token, last_seq: 0 }   [NEW v1.1]
  → enter pump loop (unchanged from v1.0)
  → on each PtyData written: push to output_buf          [NEW v1.1]
```

### Cold-Reattach Flow (1-RTT after handshake)

```
Client: connection lost (suspend/crash/network change)
  → client holds: server addr, session_token, last_acked_seq
  → new QUIC connection → handshake (same SSH key proves identity)
  → open_bi → write Reattach { session_token, last_acked_seq }

Server (new connection handler task):
  → TLS handshake → extract peer_key
  → read first Message → Reattach { token, last_acked_seq }
  → registry.reattach(token, peer_key)
      token not found                    → ReattachErr, close
      slot.identity != peer_key          → ReattachErr, close  [auth]
      shell already exited               → ReattachErr, close
      OK:
        rebind slot.conn = new conn
        slot.idle_since = None
  → write ReattachOk { replaying_from_seq }               [1 RTT from handshake]
  → replay output_buf.since(last_acked_seq) as PtyData
  → resume pump loop (PtyData, Resize, SessionClose)
```

### Migration Flow (transparent, no protocol change)

```
Client: IP/interface change (Wi-Fi → cellular)
  → QUIC PATH_CHALLENGE / PATH_RESPONSE by quinn internally
  → connection IDs rotate; quinn updates remote peer address
  → pump tasks (select! loop at server.rs:264): no change
  → session: no change; Session + MasterPty + child keep running
```

### Output Sequencing

```
PTY reader (spawn_blocking) → bytes
  → out_rx.recv() in async pump task
  → output_buf.push(bytes.clone()) → assigns seq N, archives in ring
  → write_message(&mut send, &Message::PtyData { data: bytes })
  → (on reattach: replay PtyData from seq last_acked+1)
```

---

## Modified vs New Components

| Component | v1.0 State | v1.1 Change | File |
|-----------|-----------|-------------|------|
| `SessionRegistry` | Absent | New; keyed by `SessionToken`; per-identity cap | `crates/nosh-server/src/registry.rs` (new) |
| `SessionSlot` | Absent | New; wraps `Session` + replaceable conn + output buffer | `crates/nosh-server/src/registry.rs` (new) |
| `SequencedOutputBuffer` | Absent | New; ring buffer for reattach replay | `crates/nosh-server/src/registry.rs` (new) |
| `Session.identity` | `None` always (session.rs:119 seam) | Set from peer cert SPKI after handshake | `crates/nosh-server/src/session.rs:119` |
| `handle_connection` | Directly calls `run_session` (server.rs:181) | Extracts identity; dispatches new vs reattach | `crates/nosh-server/src/server.rs:145` |
| `run_session` | Owns all lifecycle | Moves pump logic into `SessionSlot` or delegates | `crates/nosh-server/src/server.rs:185` |
| `run_accept_loop` | Creates endpoint + semaphore | Also creates `Arc<SessionRegistry>` | `crates/nosh-server/src/server.rs:101` |
| `Message` enum | 4 variants | +4 variants: `SessionOpened`, `Reattach`, `ReattachOk`, `ReattachErr` | `crates/nosh-proto/src/messages.rs` |
| `FileSigner` | Absent | New `RawEd25519Signer` impl for on-disk key | `crates/nosh-auth/src/signer.rs` |
| `ClientIdentity::from_file` | Absent | New constructor wrapping `FileSigner` | `crates/nosh-client/src/client.rs` |
| `nosh-client` `main.rs` | `--identity` selects agent key; SIGWINCH Unix-only (implicit) | `--identity-file`; `#[cfg(unix)]` gate on SIGWINCH | `crates/nosh-client/src/main.rs` |
| `transport_config` | No explicit migration flag | Add `transport.migration(true)` | `crates/nosh-proto/src/transport.rs` |
| `nosh_key_from_spki` | Private `parse_ed25519_from_spki` in verifier.rs | Expose as pub fn from keys.rs | `crates/nosh-auth/src/keys.rs` |

---

## Patterns to Follow

### Pattern 1: Auth-Before-Session, Identity-Threading After

The TLS handshake is the only auth gate (unchanged). The new addition: after the handshake, extract `NoshPublicKey` from `conn.peer_identity()` and immediately bind it to the `SessionSlot`. On reattach, compare `slot.identity == peer_key` before returning the slot. Both auth and reattach authorization are cryptographically bound to the TLS handshake with no extra round trip.

### Pattern 2: Connection-Decoupled Session Ownership

```rust
// SessionSlot owns both the session and a replaceable connection handle.
// The Session (PTY + child) outlives any particular QUIC connection.
pub struct SessionSlot {
    session: Mutex<Session>,
    conn: Mutex<Option<quinn::Connection>>,
    // ...
}

// On new connection (rebind):
*slot.conn.lock().unwrap() = Some(new_conn);
*slot.idle_since.lock().unwrap() = None;

// On disconnect (no shell exit — orphan):
*slot.conn.lock().unwrap() = None;
*slot.idle_since.lock().unwrap() = Some(Instant::now());
```

The pump tasks (`out_rx.recv`, `recv.read_message`, `wait_task`) must be stopped when the connection drops and restarted on reattach. Model: pack pump handles into a `JoinSet<()>` or a set of `AbortHandle`s in the slot. On reattach, abort the old pump (already dead since its streams died with the connection), install the new `conn`, spawn fresh pump tasks.

### Pattern 3: Sequence-Numbered Output for Exactly-Once Replay

```rust
// In the server pump loop, after receiving from out_rx:
let seq = output_buf.push(chunk.clone());  // side-effect: archives with seq
write_message(&mut send, &Message::PtyData { data: chunk }).await?;

// On reattach, replay:
for (seq, bytes) in output_buf.since(last_acked_seq) {
    write_message(&mut send, &Message::PtyData { data: bytes }).await?;
}
```

The ring is bounded; entries evicted by total byte count. If the ring overflows (session orphaned for many minutes), reattach succeeds but replays only what the ring holds. The client may have missed bytes, which is acceptable — this matches SSH reconnect behavior and is better than no reattach.

---

## Anti-Patterns to Avoid

### Anti-Pattern 1: Reattach Keyed on Connection ID or Remote Address

**What it looks like:** `HashMap<ConnectionId, Arc<SessionSlot>>`

**Why wrong:** QUIC connection IDs rotate for privacy; the remote address changes on migration and always changes on cold reconnect. The only stable, unforgeable identifier is the SSH public key proven by the TLS handshake. The correct key is an opaque `[u8; 32]` token issued to a specific identity, with `slot.identity == peer_key` verified on every reattach.

### Anti-Pattern 2: Holding the Registry Mutex Across I/O

**What it looks like:** `let _guard = registry.slots.lock(); …do reattach I/O…`

**Why wrong:** The mutex on `registry.slots` should only be held for the O(1) HashMap lookup. Actual reattach I/O (replay, stream rebind) happens after the lock is released, with the `Arc<SessionSlot>` in hand. The `SessionSlot` uses its own fine-grained `Mutex`es.

### Anti-Pattern 3: Sharing `Session` Across Tasks Without a Mutex Wrapper

**What it looks like:** `Arc<Session>` without wrapping, relying on `Session`'s existing internals.

**Why wrong:** `Session.master: Box<dyn MasterPty + Send>` is `Send` but not `Sync`. Wrap in `Mutex<Session>` in `SessionSlot`; the `resize` method (already `&self`) uses `MasterPty::resize(&self, …)` so only the child-management path needs `&mut self`.

### Anti-Pattern 4: Private Key in Logs or Environment

**What it looks like:** Passing `FileSigner`'s key bytes through `std::env`, tracing them, or storing them in a `String`.

**Why wrong:** `FileSigner` is a documented v1.1 exception to the "never handle private keys" invariant. It must hold the key only in the struct field, never log it, and must be zeroized on drop. Add `zeroize::ZeroizeOnDrop` to the key field.

### Anti-Pattern 5: Conflating Migration and Cold Reattach

**What it looks like:** Adding a reattach handshake on every IP change "just in case."

**Why wrong:** Migration (same connection, new path) is transparent — the pump tasks keep running without any notification from the session layer. Cold reattach (new connection, orphaned session) requires the `Message::Reattach` protocol. Mixing them adds latency to migration (which should be zero overhead by design) and complexity to reattach. Keep them structurally separate: migration = nothing in session code; reattach = dispatch on first message of the new connection.

---

## Build Order

```
Step 1: nosh-proto — add SessionOpened, Reattach, ReattachOk, ReattachErr variants
        to crates/nosh-proto/src/messages.rs
        (no new dependencies; codec handles them automatically via serde/postcard)

Step 2: nosh-auth — add FileSigner to crates/nosh-auth/src/signer.rs;
        expose nosh_key_from_spki in crates/nosh-auth/src/keys.rs
        (pure Rust; depends only on ed25519-dalek and ssh-key, already in scope)

Step 3: SessionRegistry + SessionSlot + SequencedOutputBuffer
        in crates/nosh-server/src/registry.rs (new file)
        — depends on nosh-proto (Message, SessionToken)
        — depends on nosh-server::session::Session (exists)
        — unit-testable independently: open/reattach/eviction with mock sessions

Step 4: Identity threading
        Modify crates/nosh-server/src/server.rs::handle_connection to extract peer_key
        via conn.peer_identity() and pass it to session::open and registry::open
        — depends on Step 2 (nosh_key_from_spki), Step 3 (registry)

Step 5: Server reattach dispatch
        Modify handle_connection to read first Message and branch on
        SessionOpen vs Reattach; implement replay path
        — depends on Steps 1, 3, 4

Step 6: nosh-client — add FileSigner path and --identity-file flag;
        store session_token from SessionOpened; send Reattach on reconnect;
        gate SIGWINCH on #[cfg(unix)]
        — depends on Step 1 (new proto messages), Step 2 (FileSigner)

Step 7: Migration headless test
        quinn endpoint.rebind() test verifying the session stream continues
        — depends on Step 5 (server running with registry)

Step 8: Windows cross-compilation check
        cargo check --target x86_64-pc-windows-gnu
        catches cfg-gated compilation errors early; no Windows runner required
```

---

## Integration Points

### `quinn::Connection::peer_identity()`

| Component | Integration | Notes |
|-----------|-------------|-------|
| `crates/nosh-server/src/server.rs::handle_connection` | `conn.peer_identity()` downcast to `Vec<CertificateDer<'static>>` | Stable in quinn 0.11.x under the rustls-ring backend. The downcast type matches the rustls-backed quinn path. HIGH confidence. |

### `SequencedOutputBuffer` and pump tasks

| Boundary | Communication | Notes |
|----------|---------------|-------|
| PTY reader task → output buffer | Sequence assignment happens in the async pump after receiving from the existing `out_rx` mpsc channel (server.rs:272) | The existing channel already exists; just add the `output_buf.push` call before the `write_message` call. |

### Windows client and `FileSigner`

| Boundary | Communication | Notes |
|----------|---------------|-------|
| `main.rs` CLI → `ClientIdentity` | `ClientIdentity::from_signer(Arc::new(FileSigner::from_openssh_file(path)?))` | `ClientIdentity::from_signer` already exists at `crates/nosh-client/src/client.rs:27`. No structural change to the connection setup path. |

### Windows terminal resize

| Boundary | Communication | Notes |
|----------|---------------|-------|
| `RawModeGuard` | `crossterm::terminal::enable_raw_mode()` | crossterm 0.28 handles Windows Console API. `RawModeGuard` at `crates/nosh-client/src/client.rs:208` is cross-platform with no changes. |
| SIGWINCH handler | `tokio::signal::unix::SignalKind::window_change()` | Unix-only API. Gate with `#[cfg(unix)]`. On Windows, use `crossterm::event::EventStream` to receive `Event::Resize`. The resize debounce and `send_resize` call are shared. |

---

## Workspace Structure for v1.1

```
nosh/                                       ← workspace root (unchanged)
├── crates/
│   ├── nosh-proto/src/
│   │   └── messages.rs                     ← ADD: SessionOpened, Reattach, ReattachOk, ReattachErr
│   │
│   ├── nosh-auth/src/
│   │   ├── signer.rs                       ← ADD: FileSigner
│   │   └── keys.rs                         ← ADD: pub nosh_key_from_spki()
│   │
│   ├── nosh-server/src/
│   │   ├── registry.rs                     ← NEW: SessionRegistry, SessionSlot, SequencedOutputBuffer
│   │   ├── server.rs                       ← MODIFY: identity extraction, registry integration,
│   │   │                                              reattach dispatch in handle_connection
│   │   └── session.rs                      ← MODIFY: Session.identity always set (no longer None)
│   │
│   └── nosh-client/src/
│       ├── client.rs                       ← ADD: ClientIdentity::from_file; store/resend token
│       └── main.rs                         ← ADD: --identity-file; #[cfg(unix)] SIGWINCH gate;
│                                                      Windows resize via crossterm EventStream
```

---

## Sources

- `crates/nosh-server/src/session.rs:115` — `Session` struct with `identity: Option<NoshPublicKey>` seam (line 119) and `idle_since: Option<Instant>` seam (line 128). Verified in codebase.
- `crates/nosh-server/src/server.rs:101` — `run_accept_loop`; `crates/nosh-server/src/server.rs:145` — `handle_connection`; `crates/nosh-server/src/server.rs:185` — `run_session`. Verified in codebase.
- `crates/nosh-proto/src/messages.rs` — `Message` enum (4 variants: SessionOpen, PtyData, Resize, SessionClose). Verified in codebase.
- `crates/nosh-auth/src/signer.rs:26` — `RawEd25519Signer` trait; `AgentSigner` and `InProcessEd25519Signer` impls. Verified in codebase.
- `crates/nosh-auth/src/keys.rs:172` — `extract_spki_from_cert`. Verified in codebase.
- `crates/nosh-auth/src/verifier.rs:218` — `parse_ed25519_from_spki` (private; to be exposed). Verified in codebase.
- `crates/nosh-client/src/client.rs:27` — `ClientIdentity::from_signer` accepting any `Arc<dyn RawEd25519Signer>`. Verified in codebase.
- `crates/nosh-client/src/client.rs:208` — `RawModeGuard` using crossterm. Verified in codebase.
- `crates/nosh-client/src/main.rs:103` — SIGWINCH handler (Unix-only, currently ungated). Verified in codebase.
- `crates/nosh-proto/src/transport.rs:28` — `transport_config` (no explicit migration flag). Verified in codebase.
- [quinn 0.11 Connection::peer_identity](https://docs.rs/quinn/0.11.9/quinn/struct.Connection.html#method.peer_identity) — HIGH confidence.
- `.planning/milestones/v1.0-research/ARCHITECTURE.md` — v1.0 seam inventory and component map. HIGH confidence.
- `.planning/PROJECT.md` — 0-RTT deferred decision, per-identity cap rationale, Windows client scope. HIGH confidence.

---

*Architecture research for: nosh v1.1 M3 Roaming + Windows Client*
*Researched: 2026-05-30*
