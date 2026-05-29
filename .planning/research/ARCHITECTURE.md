# Architecture Research

**Domain:** QUIC-based roaming remote shell (Rust) — M0–M2 architecture-validation spike
**Researched:** 2026-05-29
**Confidence:** HIGH (quinn/rustls/portable-pty APIs confirmed against current docs.rs; design patterns confirmed against official sources and quicshell prior art)

---

## System Overview

```
  CLIENT PROCESS                          SERVER PROCESS
  ┌──────────────────────────────┐        ┌──────────────────────────────────────────┐
  │  LocalTerminal               │        │  Listener (Endpoint::accept loop)        │
  │  (raw-mode stdin/stdout)     │        │  ┌────────────────────────────────────┐  │
  │         │  ▲                 │        │  │  Session                           │  │
  │         ▼  │                 │        │  │  ┌─────────────────────────────┐   │  │
  │  TerminalProxy               │        │  │  │  AuthLayer                  │   │  │
  │  ┌──────────────────────┐    │        │  │  │  (ServerCertVerifier +      │   │  │
  │  │  ShellStream (bidi)  │◄───┼──QUIC──┼──┼──┤   ClientCertVerifier impl)  │   │  │
  │  │  stdin → SendStream  │    │  443   │  │  └──────────┬──────────────────┘   │  │
  │  │  RecvStream → stdout │    │  UDP   │  │             │ post-auth            │  │
  │  ├──────────────────────┤    │        │  │  ┌──────────▼──────────────────┐   │  │
  │  │  ControlStream(bidi) │◄───┼──QUIC──┼──┼──┤  ShellSession               │   │  │
  │  │  resize / signals    │    │        │  │  │  ┌──────────────────────┐   │   │  │
  │  ├──────────────────────┤    │        │  │  │  │  PtyBridge           │   │   │  │
  │  │  Datagram recv       │◄───┼──QUIC──┼──┼──┼──┤  (portable-pty)      │   │   │  │
  │  │  (future state sync) │    │  dgram │  │  │  │  MasterPty           │   │   │  │
  │  └──────────────────────┘    │        │  │  │  │  ChildProcess (PTY)  │   │   │  │
  │  ┌──────────────────────┐    │        │  │  │  └──────────────────────┘   │   │  │
  │  │  AgentSigningKey     │    │        │  │  └─────────────────────────────┘   │  │
  │  │  (ssh-agent-client)  │    │        │  └────────────────────────────────────┘  │
  │  └──────────────────────┘    │        └──────────────────────────────────────────┘
  └──────────────────────────────┘
                │
          ssh-agent socket
          ($SSH_AUTH_SOCK)
```

---

## Component Boundaries

### 1. Transport Layer: `quinn::Endpoint` + `quinn::Connection`

**Responsibility:** UDP socket management, QUIC connection lifecycle, stream multiplexing, datagram delivery.

**What it owns:**
- Binding to UDP/443
- TLS 1.3 handshake (via rustls `ServerConfig` / `ClientConfig`)
- Opening and accepting `SendStream` / `RecvStream` / bidirectional stream pairs
- `send_datagram` / `read_datagram` — unreliable datagram frames (RFC 9221)
- Connection migration (QUIC connection IDs; hands off to M3 without any code change here)

**Does NOT own:** Auth logic, PTY lifecycle, session state, protocol framing.

**Key API (HIGH confidence — docs.rs quinn 0.11.8):**
- `Endpoint::server(config, addr)` — bind server
- `endpoint.accept().await` → `Incoming` → `.await` → `Connection`
- `conn.open_bi().await` → `(SendStream, RecvStream)`
- `conn.accept_bi().await` → `(SendStream, RecvStream)`
- `conn.send_datagram(bytes)` / `conn.read_datagram().await`

**Communicates with:** Auth layer (provides rustls verifier impls), Session layer (accepts Connection and hands it off).

---

### 2. Auth Layer: `nosh-auth` crate

**Responsibility:** Provide rustls-compatible certificate verifier implementations that authorize SSH keys instead of X.509 PKI chains.

**Two verifier roles:**

| Role | Trait | Side | Checks |
|------|-------|------|--------|
| Verify server host key | `rustls::client::danger::ServerCertVerifier` | Client | Key fingerprint vs `~/.ssh/known_hosts`; TOFU on first contact |
| Verify client public key | `rustls::server::danger::ClientCertVerifier` | Server | Key fingerprint vs `~/.ssh/authorized_keys` |

Both are passed into `rustls::ClientConfig::builder().dangerous().with_custom_certificate_verifier()` or `rustls::ServerConfig::builder().with_client_cert_verifier()` respectively. The verifiers receive the raw certificate bytes from the TLS handshake; for the self-signed-cert-pinning path, those bytes contain the SSH public key wrapped in a throwaway self-signed X.509. The verifier ignores the certificate validity period and chain and checks only the embedded public key against the trust file.

**Signer role (client-side):** A `rustls::sign::SigningKey` + `Signer` implementation that delegates `sign()` calls to `ssh-agent-client-rs`, routing the TLS `CertificateVerify` signature through the agent. `rustls::sign::SigningKey::choose_scheme()` returns `Some(Box<AgentSigner>)` for any scheme the agent supports; `AgentSigner::sign()` calls `agent.sign(key, data)`. Note: `sign()` is synchronous in rustls's current trait design — use a blocking call or `tokio::task::block_in_place` to call the (synchronous) ssh-agent-client-rs API from async context.

**Does NOT own:** Connection lifecycle, PTY, session persistence.

**Communicates with:** Transport layer (provides verifier objects to quinn's rustls config), ssh-agent socket.

---

### 3. Protocol Framing: `nosh-proto` crate

**Responsibility:** Typed message definitions, encoding/decoding for the control stream and shell stream. This is the shared protocol crate both `nosh-client` and `nosh-server` depend on.

**Scope for M0–M2:**
- `ShellData` — raw byte payload (stdin/stdout) on the shell bidirectional stream
- `ControlMsg` — typed enum: `Resize { cols, rows }`, `Signal { signum }`, `EnvVar { key, value }`, `ShellOpen { env_vars, initial_size }`, `Ack`
- Stream-type identifiers / ALPN string (`"nosh/1"`)

**Seam for M4:** Add `DatagramMsg` — the state-sync object (terminal diff + echo prediction). Datagram frames are already available at the quinn layer; the protocol crate just needs a new message type and codec. No changes needed in the transport or session layers.

**Seam for M5:** Add `ChannelOpen` / `ChannelAccept` / `ChannelReject` variants to `ControlMsg`, adopting the quicshell control-first model: negotiate channel kind on stream 0 (the control stream) before binding to a dedicated bidi stream.

**Does NOT own:** I/O, connection state, PTY.

---

### 4. Session Layer: `ShellSession` struct (in `nosh-server`)

**Responsibility:** Owns the lifecycle of one authenticated shell session end-to-end.

**Owns for M0–M2:**
- A `quinn::Connection` reference (Arc)
- A `PtyBridge` (the portable-pty wrapper)
- An `env::SanitizedEnv` — the whitelist-filtered environment passed to the shell
- Two async tasks: `pty_to_net` (reads PTY stdout, writes to QUIC SendStream) and `net_to_pty` (reads QUIC RecvStream, writes to PTY stdin)
- A `control_task` loop: reads ControlMsg from the control bidi stream, dispatches resize/signal actions

**Seam for M3 (roaming / cold-reattach):** Wrap `ShellSession` in a `SessionStore` (a `HashMap<SessionToken, Arc<Mutex<ShellSession>>>`) keyed on an opaque reattach token bound to the client's SSH key fingerprint. When the connection drops, `ShellSession` stays alive in the store (with a TTY still attached to the shell process). On reconnect, the client sends its token in the first `ControlMsg`; the server looks it up, replaces the connection reference in the existing `ShellSession`, and the two pump tasks resume on the new streams. This pattern requires almost no change to the spike code — just wrapping the session in a store and adding the reattach token path in the server accept loop.

**Does NOT own:** QUIC mechanics, auth, PTY abstraction internals.

---

### 5. PTY Abstraction: `PtyBridge` (in `nosh-server`, backed by `portable-pty`)

**Responsibility:** Isolate all PTY/process mechanics behind a narrow interface so the Linux fork-based Unix PTY can be swapped for ConPTY (Windows, M6) without touching the session layer.

**Interface (spike stage):**
```rust
pub struct PtyBridge {
    master: Box<dyn MasterPty + Send>,
    child:  Box<dyn portable_pty::Child + Send>,
}

impl PtyBridge {
    pub fn spawn(cmd: CommandBuilder, size: PtySize) -> Result<Self>;
    pub fn reader(&self) -> Box<dyn std::io::Read + Send>;   // try_clone_reader
    pub fn writer(&self) -> Box<dyn std::io::Write + Send>;  // take_writer
    pub fn resize(&self, size: PtySize) -> Result<()>;
    pub fn kill(&self) -> Result<()>;
}
```

MasterPty's `try_clone_reader()` returns a blocking `Box<dyn Read + Send>`. Wrap it with `tokio::io::AsyncReadExt` via `tokio::task::spawn_blocking` or `tokio::io::unix::AsyncFd` for zero-copy async reads. The `take_writer()` is similarly blocking; use `spawn_blocking` or a dedicated write thread.

**Seam for M6 (Windows):** portable-pty's `native_pty_system()` already returns `WinPtySystem` on Windows (ConPTY), so the `PtyBridge::spawn` factory function is the only place any platform-conditional logic lives.

---

### 6. Environment Sanitization: `nosh::env` module (in `nosh-server`)

**Responsibility:** Strip dangerous client-supplied environment variables before exec, enforce the whitelist.

**Rules (baked in at M2, not configurable):**
- Strip: `LD_*`, `DYLD_*`, `BASH_ENV`, `ENV`, `IFS`, `SHELLOPTS`, `PYTHONPATH`, `NODE_OPTIONS`, and any var starting with `LD_` or `DYLD_`
- Whitelist: `TERM`, `LANG`, `LC_*`, `TZ`, `COLORTERM`, `DISPLAY`
- Never pass `SSH_AUTH_SOCK` via environment (agent forwarding uses a dedicated channel in M5)

---

### 7. Client Terminal Proxy: `TerminalProxy` (in `nosh-client`)

**Responsibility:** Put the local terminal into raw mode, wire raw stdin to the QUIC send path, wire the QUIC recv path to raw stdout, and handle resize events (SIGWINCH → ControlMsg::Resize).

**For M0–M2:** Single bidi stream for shell I/O, one bidi stream for control messages. No predictive echo yet — all output arrives over the reliable stream.

---

## Data Flow

### Keystroke Path (client → server → PTY)

```
User keypress
  → LocalTerminal stdin (raw mode)
  → TerminalProxy reads bytes
  → ShellStream SendStream.write_all(bytes)    [QUIC reliable stream]
  → [network]
  → server ShellStream RecvStream.read(bytes)
  → PtyBridge writer().write_all(bytes)        [PTY master write = slave stdin]
  → Shell process stdin
```

### Output Path (PTY → server → client → terminal)

```
Shell process stdout/stderr
  → PTY slave output
  → PtyBridge reader().read(bytes)             [blocking; wrapped in spawn_blocking]
  → server ShellStream SendStream.write_all(bytes)   [QUIC reliable stream]
  → [network]
  → client ShellStream RecvStream.read(bytes)
  → LocalTerminal stdout.write_all(bytes)
```

### Resize Path

```
SIGWINCH (client terminal resized)
  → client TerminalProxy catches signal / detects new terminal size
  → ControlMsg::Resize { cols, rows } serialized → ControlStream SendStream
  → [network]
  → server control_task reads ControlMsg::Resize
  → PtyBridge.resize(PtySize { rows, cols, .. })
  → TIOCSWINSZ ioctl → SIGWINCH to shell process
```

### Stream / Datagram Assignment

| Channel | QUIC primitive | Reliability | Rationale |
|---------|---------------|-------------|-----------|
| Shell I/O (stdin/stdout) | Bidi stream | Reliable, ordered | Shell output is stateful; ordering matters for M0–M2 |
| Control (resize, signals) | Bidi stream | Reliable, ordered | Must not be lost or reordered |
| State-sync object (M4) | Datagram | Unreliable, unordered | Latest-wins terminal diffs; loss-tolerant by design |
| Agent forwarding (M5) | Bidi stream (one per request) | Reliable | Agent protocol is request-response |
| Port forwarding (M5) | Bidi stream (one per forward) | Reliable | TCP semantics |

**Spike decision:** All shell I/O rides the reliable bidi stream for M0–M2. This is correct and intentional — predictive echo (M4) does NOT change how the reliable stream works; it adds a parallel datagram path carrying speculative state. The reliable stream remains the ground truth. This means zero refactoring when M4 arrives: just add the datagram read/write tasks alongside the existing stream tasks.

---

## Workspace / Crate Layout

```
nosh/                           ← workspace root
├── Cargo.toml                  ← workspace manifest
├── crates/
│   ├── nosh-proto/             ← shared: message types, codec, ALPN constant
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── messages.rs     ← ControlMsg, ShellData (later DatagramMsg)
│   │       └── codec.rs        ← length-prefixed framing (e.g. bincode or postcard)
│   │
│   ├── nosh-auth/              ← shared: SSH-key verifiers, agent signing key
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── verifier.rs     ← SshKeyServerVerifier, SshKeyClientVerifier
│   │       ├── agent_signer.rs ← AgentSigningKey + AgentSigner impls
│   │       ├── known_hosts.rs  ← parse/check/TOFU ~/.ssh/known_hosts
│   │       └── authorized_keys.rs ← parse/check ~/.ssh/authorized_keys
│   │
│   ├── nosh-server/            ← server binary (noshd)
│   │   └── src/
│   │       ├── main.rs         ← Endpoint setup, accept loop
│   │       ├── session.rs      ← ShellSession, session_store stub (for M3)
│   │       ├── pty_bridge.rs   ← PtyBridge wrapping portable-pty
│   │       └── env.rs          ← environment variable sanitization
│   │
│   └── nosh-client/            ← client binary (nosh)
│       └── src/
│           ├── main.rs         ← Endpoint setup, connect, drive session
│           ├── terminal.rs     ← TerminalProxy, raw mode, SIGWINCH handling
│           └── session.rs      ← ClientSession (stream wiring)
```

### Rationale

- **`nosh-proto` as shared crate:** Both binaries must agree on message encoding. Keeping it isolated makes future protocol changes reviewable in one place and prevents import cycles.
- **`nosh-auth` as shared crate:** Both client and server use the same key-parsing logic (only the verifier *direction* differs). Sharing avoids duplicating `known_hosts` / `authorized_keys` parsing.
- **`nosh-server` and `nosh-client` as separate binaries:** Different binaries simplify deployment (ship only what you need), prevent accidental mixing of server-only logic (PTY spawning, env sanitization) into the client, and mirror how Mosh/ET ship.
- **`pty_bridge.rs` inside `nosh-server`:** The PTY abstraction is server-only. Keeping it here (not in its own crate) avoids over-engineering the spike; extract to `nosh-pty` crate at M6 if the ConPTY swap needs more isolation.

---

## Patterns to Follow

### Pattern 1: Auth-Before-Session (Transport-Layer Auth + App-Layer Rejection)

**What:** Auth happens inside the TLS handshake via custom rustls verifiers. If the client key is not in `authorized_keys`, `ClientCertVerifier::verify_client_cert()` returns `Err(...)`, which causes the TLS handshake to fail. No QUIC connection is established. No session code runs.

**Why:** Reject at the earliest possible point. Unauthenticated connections never reach the session layer, so there is no "authenticated but not yet checked" window.

**Seam:** The `verify_client_cert` implementation receives the raw certificate DER. For the self-signed-cert-pinning path it extracts the public key bytes and checks against `authorized_keys`. For the RFC 7250 RPK path (preferred, once confirmed available in rustls), the bytes are the raw `SubjectPublicKeyInfo` directly.

### Pattern 2: Session as an Owned Async Task Tree

**What:** `ShellSession::run(conn, pty)` spawns three `tokio::spawn` tasks:
1. `net_to_pty` — reads QUIC `RecvStream`, writes to PTY writer
2. `pty_to_net` — reads PTY reader (blocking, in `spawn_blocking`), writes to QUIC `SendStream`
3. `control_loop` — reads QUIC control `RecvStream`, dispatches `ControlMsg` variants

All three tasks hold an `Arc<ShellSession>` (or just their required handles). A `tokio::select!` in the main accept loop awaits all three; any one completing (or erroring) triggers cleanup of the others.

**Why:** Mirrors the natural concurrency: PTY output and network input are independent event sources. Tokio tasks map cleanly onto them with no shared mutable state.

### Pattern 3: Blocking PTY I/O via spawn_blocking

**What:** `portable_pty::MasterPty::try_clone_reader()` returns `Box<dyn std::io::Read + Send>` — a blocking handle. Wrap in `tokio::task::spawn_blocking` per read chunk, or keep a dedicated OS thread that blocks on the PTY and sends chunks through a `tokio::sync::mpsc::channel`.

**Why:** tokio's async executor must not block. PTY file descriptors are not epoll-friendly on all Linux kernels for all sizes. The `spawn_blocking` bridge is the safe, idiomatic pattern.

**Alternative:** `tokio::io::unix::AsyncFd` wrapping the raw PTY fd — lower overhead, but requires careful handling of `EAGAIN` on the PTY. Valid for M2+; `spawn_blocking` is simpler for the spike.

---

## Anti-Patterns to Avoid

### Anti-Pattern 1: Application-Layer Auth Handshake

**What people do:** Establish the QUIC connection, then send a login message on a stream.

**Why it's wrong for nosh:** SSH-key auth via the TLS handshake means auth is cryptographically bound to the transport. An app-layer handshake after connection is a second step that can be bypassed or confused; it also adds a round trip. The TLS `CertificateVerify` already proves key possession — use it.

**Exception:** The quicshell HELLO/FINISH model is acceptable if RFC 7250 raw-public-key support turns out to be unavailable in the chosen rustls version (see Pitfalls). In that case, use self-signed-cert-pinning at the TLS layer (still transport-layer auth) and no additional app-layer handshake.

### Anti-Pattern 2: Muxing PTY Output and Control on the Same Stream

**What people do:** Multiplex resize acknowledgements and stdout bytes in-band with escape sequences (SSH's approach).

**Why it's wrong:** Escape-sequence parsing in the mux layer is a complexity and correctness sink. QUIC streams are cheap — open a dedicated bidi stream for the control channel. This is what the quicshell control-first model does, and what M5 channel multiplexing will formalize.

### Anti-Pattern 3: Storing the Session by Connection ID

**What people do:** Use the QUIC connection ID or remote address as the session lookup key.

**Why it's wrong:** Connection IDs rotate (QUIC's privacy feature) and the remote address changes on migration. For M3 reattach, the key must be bound to the SSH identity (key fingerprint), not the transport layer. Use an opaque `SessionToken: [u8; 32]` derived from the client's public key fingerprint, generated at session open and stored in `SessionStore`.

### Anti-Pattern 4: Global PTY System Construction

**What people do:** Call `portable_pty::native_pty_system()` in main or at crate init.

**Why it's wrong:** This makes testing hard and the Windows swap invisible. Call it inside `PtyBridge::spawn` so it's a single call site, and the whole pty-creation path is in one function.

---

## Build Order (Component Dependencies)

Each component must exist before what follows can compile or be meaningfully tested.

```
Step 1: nosh-proto
  → No dependencies on other nosh crates
  → Deliverable: ControlMsg, ShellData types; length-prefixed codec; ALPN constant

Step 2: nosh-auth
  → Depends on nosh-proto (for ALPN string only)
  → Deliverable: SshKeyClientVerifier, SshKeyServerVerifier, AgentSigningKey
  → Test independently: parse known_hosts / authorized_keys test fixtures

Step 3: Transport skeleton (nosh-server + nosh-client, no auth yet)
  → Depends on nosh-proto
  → Deliverable: quinn Endpoint up/down, echo bytes over bidi stream, datagram round-trip
  → This is M0: proves QUIC + stream + datagram coexist

Step 4: Wire auth into transport (nosh-auth into nosh-server + nosh-client)
  → Depends on steps 2 + 3
  → Deliverable: mutual SSH-key auth over TLS handshake, connection rejected for unknown keys
  → This is M1

Step 5: PtyBridge + env sanitization (nosh-server)
  → Depends on step 3 (connection handle available)
  → Deliverable: server spawns PTY, interactive shell reachable, env stripped
  → env sanitization is a pure function — test before PTY integration

Step 6: Wire PTY into QUIC session (ShellSession tasks)
  → Depends on steps 4 + 5
  → Deliverable: live interactive shell over QUIC with SSH-key auth
  → This is M2

Step 7: Terminal resize (client SIGWINCH → ControlMsg → PTY resize)
  → Depends on step 6 (control stream exists)
  → Deliverable: resize propagates to shell, no storm on drag
  → Part of M2
```

---

## Seams to Leave for Deferred Milestones

These are not features to build in the spike — they are design decisions in the spike code that avoid a rewrite later.

| Deferred Milestone | Seam to Leave Now | Where |
|-------------------|-------------------|-------|
| M3: Cold reattach | `SessionStore: HashMap<SessionToken, Arc<Mutex<ShellSession>>>` stub; `ShellSession` holds a `replaceable_conn` field (initially just the one conn, later swappable on reattach) | `nosh-server/session.rs` |
| M3: Roaming | No seam needed — QUIC connection migration is transparent at the quinn layer; ShellSession's pump tasks just keep working | N/A |
| M4: Datagram state-sync | Add `DatagramMsg` variant to `nosh-proto`; add `datagram_task` in `ShellSession` running alongside the existing stream tasks | `nosh-proto/messages.rs`, `nosh-server/session.rs` |
| M5: Channel multiplexing | `ControlMsg` already has a bidi control stream; add `ChannelOpen/Accept/Reject` variants; `ShellSession` grows a `channel_map: HashMap<u32, ChannelHandle>` | `nosh-proto/messages.rs`, `nosh-server/session.rs` |
| M6: Windows ConPTY | `PtyBridge::spawn` calls `portable_pty::native_pty_system()` — on Windows this returns the ConPTY backend with no other changes | `nosh-server/pty_bridge.rs` |
| M7: WebTransport | Requires a separate `nosh-webtransport` crate wrapping `wtransport`; `nosh-proto` message types reuse unchanged | new crate |

**Principle:** The spike should feel slightly underbuilt relative to the full design. Stubs and `todo!()` at the seams are correct; premature abstractions are not.

---

## Integration Points

### External: `ssh-agent` (Unix domain socket)

| Component | Integration | Notes |
|-----------|-------------|-------|
| `nosh-auth::AgentSigningKey` | `ssh-agent-client-rs` (synchronous API) | Call from `sign()` inside `spawn_blocking`; agent socket path from `SSH_AUTH_SOCK` env var at *client startup*, never forwarded to server |

### External: `~/.ssh/known_hosts`

| Component | Integration | Notes |
|-----------|-------------|-------|
| `nosh-auth::known_hosts` | `ssh-key` crate for key parsing; custom file parser for known_hosts format | TOFU: on first unknown host, write fingerprint and accept; on mismatch, hard reject (no `StrictHostKeyChecking=no` mode) |

### External: `~/.ssh/authorized_keys`

| Component | Integration | Notes |
|-----------|-------------|-------|
| `nosh-auth::authorized_keys` | `ssh-key` crate for key parsing | Server reads at connection time (no caching for the spike — acceptable at M0–M2 scale) |

### Internal Boundaries

| Boundary | Communication | Notes |
|----------|---------------|-------|
| `nosh-auth` → `quinn` | rustls `ServerConfig` / `ClientConfig` objects | Verifiers are constructed once at startup; rustls holds Arc references |
| `ShellSession` → `PtyBridge` | Direct method calls + mpsc channels for the async-blocking bridge | No shared state beyond the Arc wrapping the session |
| `nosh-client` → `TerminalProxy` | `tokio::sync::mpsc` for resize events from SIGWINCH handler to control task | Signal handler must be minimal; send to channel, handle in async task |

---

## Sources

- [quinn 0.11.8 docs — Connection API (streams, datagrams)](https://docs.rs/quinn/0.11.8/quinn/struct.Connection.html) — HIGH confidence
- [quinn 0.11.8 docs — Endpoint::accept](https://docs.rs/quinn/0.11.8/quinn/struct.Endpoint.html) — HIGH confidence
- [rustls — ClientCertVerifier trait](https://docs.rs/rustls/latest/rustls/server/danger/trait.ClientCertVerifier.html) — HIGH confidence
- [rustls — howto: custom SigningKey for HSM/remote key](https://docs.rs/rustls/latest/rustls/manual/_03_howto/index.html) — HIGH confidence
- [Quinn certificate configuration guide](https://quinn-rs.github.io/quinn/quinn/certificate.html) — HIGH confidence
- [portable-pty — MasterPty trait](https://docs.rs/portable-pty/latest/portable_pty/trait.MasterPty.html) — HIGH confidence
- [quicshell spec.md — control-first channel model, OPEN/ACCEPT/REJECT](https://github.com/haukened/quicshell) — HIGH confidence
- [Eternal Terminal — BackedReader/BackedWriter architecture](https://eternalterminal.dev/howitworks/) — MEDIUM confidence (used for session-persistence seam design only)

---

*Architecture research for: nosh QUIC remote shell (M0–M2 spike)*
*Researched: 2026-05-29*
