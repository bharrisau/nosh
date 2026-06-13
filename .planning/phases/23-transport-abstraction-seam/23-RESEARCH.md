# Phase 23: Transport Abstraction Seam - Research

**Researched:** 13/06/2026
**Domain:** Rust trait abstraction over quinn connection/stream types; async-fn-in-trait object safety
**Confidence:** HIGH — based on direct source reads of the production codebase and verified trait design reasoning

---

<user_constraints>
## User Constraints (from CONTEXT.md)

### Locked Decisions

- **D-01:** Introduce three traits — `NoshTransport` (wraps `send_datagram`, `datagram_send_buffer_space`, `max_datagram_size`, `accept_bi`, `open_bi`, `remote_address`, `close`), `NoshSendStream`, `NoshRecvStream`. Thin I/O boundary only; everything above it (codec, terminal model, registry, predictor, scrollback buffer) is untouched.
- **D-02:** `ChannelEvent::Stream` changes to carry `Box<dyn NoshSendStream>` + `Box<dyn NoshRecvStream>` (ROADMAP SC#4). Dynamic dispatch is accepted — the per-call overhead is irrelevant next to network I/O, and it keeps `run_channel_task` / `run_scrollback_sender_task` transport-agnostic without monomorphising the whole pump.
- **D-03:** `run_session`, `run_reattach_session`, `send_burst`, `build_state_diff`, `handle_connection`, `run_channel_task`, `run_scrollback_sender_task` all become generic over `NoshTransport` (or take boxed trait objects) rather than concrete `quinn::Connection` / `quinn::SendStream` / `quinn::RecvStream`.
- **D-04:** The Quinn concrete wrapper (`QuinnConnection: NoshTransport`, plus stream wrappers) is a **pure pass-through** — no added logic, no behavioural change. This is the SC#3 acceptance bar.
- **D-05:** `cargo test --workspace` passes with **zero test modifications**. If a test needs changing, that is evidence the refactor changed behaviour — stop and reconsider.

### Claude's Discretion

- **Trait location:** Recommend the traits live in `nosh-proto` (`nosh-proto/src/transport.rs` or similar) — the ARCHITECTURE researcher read the actual source and recommended this; it avoids spawning a new crate for a thin trait. Planner decides final module path.
- **Async-trait mechanism:** Planner's call; prefer native async-fn-in-trait if the toolchain supports it cleanly, else `async-trait`.

### Deferred Ideas (OUT OF SCOPE)

None — this phase is deliberately scoped to the seam only. WebTransport implementation is Phase 24.
</user_constraints>

<phase_requirements>
## Phase Requirements

| ID | Description | Research Support |
|----|-------------|------------------|
| WT-01 | A `NoshTransport` / `NoshSendStream` / `NoshRecvStream` abstraction lets the session pump (live session, cold reattach, channels, scrollback) run over either native QUIC or WebTransport with no behavioural change — all existing tests pass unchanged against the Quinn wrapper | Trait design, method surface, object-safety mechanism, pass-through wrapper pattern all addressed in this research |
</phase_requirements>

---

## Summary

Phase 23 is a pure refactoring phase: introduce a transport abstraction trait (`NoshTransport` / `NoshSendStream` / `NoshRecvStream`) in `nosh-proto`, wrap every `quinn::Connection`, `quinn::SendStream`, and `quinn::RecvStream` usage behind that trait, and verify nothing changed by requiring `cargo test --workspace` to pass with zero test modifications.

The codebase currently has no transport trait at all. Every transport-facing call is against concrete quinn types. The surface area is well-bounded: the connection-level methods called in production code are exactly `send_datagram`, `datagram_send_buffer_space`, `max_datagram_size`, `accept_bi`, `open_bi`, `remote_address`, `close`, and `read_datagram`. The stream-level methods are `write_all`/`flush` (via `AsyncWrite`), `read_exact`/`read` (via `AsyncRead`), `finish`, `stopped`, `reset` (send-only), and `stop` (recv-only).

The single load-bearing design decision is the async-trait mechanism. D-02 requires `Box<dyn NoshSendStream>` and `Box<dyn NoshRecvStream>` in `ChannelEvent::Stream` — this mandates that both traits are object-safe. Native `async fn` in traits (AFIT, stable since Rust 1.75) desugars to `-> impl Future`, which is NOT object-safe. The recommended approach is the `async-trait` crate (version 0.1.89), which rewrites async methods to return `Pin<Box<dyn Future + Send>>` and is object-safe by construction. The alternative is hand-writing `Pin<Box<dyn Future<Output=...> + Send + '_>>` return types, which achieves the same goal without the macro but is significantly more verbose and error-prone.

The Quinn concrete wrappers are trivial: `quinn::SendStream` already implements `AsyncWrite + Unpin` and `quinn::RecvStream` implements `AsyncRead + Unpin`, so `write_all`, `flush`, `read_exact`, and `read` all delegate to the underlying tokio I/O trait implementations. The extra stream methods (`finish`, `stopped`, `reset`, `stop`) are direct pass-throughs. The `NoshTransport` wrapper for `quinn::Connection` delegates every method 1:1 — there is no impedance mismatch.

**Primary recommendation:** Place traits in a new `crates/nosh-proto/src/transport_trait.rs` file, use `async-trait 0.1.89` for object-safe async methods, implement `QuinnTransport` / `QuinnSendStream` / `QuinnRecvStream` wrappers in `nosh-server/src/quinn_transport.rs` and `nosh-client/src/quinn_transport.rs`.

---

## Architectural Responsibility Map

| Capability | Primary Tier | Secondary Tier | Rationale |
|------------|-------------|----------------|-----------|
| Trait definition (`NoshTransport`, `NoshSendStream`, `NoshRecvStream`) | `nosh-proto` | — | Both `nosh-server` and `nosh-client` depend on `nosh-proto`; the trait must live in a shared crate to avoid a circular dependency |
| Quinn concrete wrappers | `nosh-server`, `nosh-client` | — | Each crate wraps its own `quinn::Connection` usage; no cross-crate sharing of the wrapper type is needed |
| Session pump refactoring (`run_session`, `run_reattach_session`, `handle_connection`, `send_burst`, `build_state_diff`) | `nosh-server` | — | All five functions live in `nosh-server/src/server.rs`; no client-side counterpart to session pump |
| Channel task refactoring (`run_channel_task`, `run_scrollback_sender_task`, `run_channel_task_inner`, `run_echo_loop`, client drain tasks) | `nosh-server`, `nosh-client` | — | Server and client each have their own `channel.rs`; both carry concrete `quinn::SendStream`/`RecvStream` types that must be abstracted |
| `ChannelEvent::Stream` variant type change | `nosh-server` | — | `ChannelEvent` is defined and used only in `nosh-server`; client channel types in `nosh-client/src/channel.rs` use concrete types too but are function parameters, not enum variants |
| Varint reader (`read_varint_u32`) | `nosh-server` | — | Lives in `nosh-server/src/channel.rs`; currently takes `&mut quinn::RecvStream` — must be updated to accept the trait object |
| Client session helpers (`open_session`, `open_channel`, `send_reattach`, etc.) | `nosh-client` | — | These are test-support / library functions in `nosh-client/src/client.rs` that accept concrete `quinn::Connection`/`SendStream`/`RecvStream` — must be updated |

---

## Standard Stack

### Core

| Library | Version | Purpose | Why Standard |
|---------|---------|---------|--------------|
| `async-trait` | 0.1.89 | Object-safe async methods in trait definitions | The standard solution for object-safe async traits; rewrites `async fn` to `-> Pin<Box<dyn Future + Send>>`; used by the entire tokio/axum ecosystem; 0.1.89 confirmed from `cargo search` |
| `quinn` | 0.11.9 (existing) | Concrete `Connection`, `SendStream`, `RecvStream` being wrapped | Already in workspace; the wrapper impls delegate to it directly |
| `tokio` | 1.x (existing) | `AsyncWrite`, `AsyncRead` traits (via `tokio::io`) | Already in workspace; `quinn::SendStream` impls `AsyncWrite`, `quinn::RecvStream` impls `AsyncRead` |

### Supporting

No new supporting libraries are needed for Phase 23. The entire phase uses existing workspace dependencies.

### Alternatives Considered

| Instead of | Could Use | Tradeoff |
|------------|-----------|----------|
| `async-trait` crate | Hand-written `Pin<Box<dyn Future<Output=...> + Send + '_>>` return types | Achieves object safety without a new dep; produces identical code at the call site; but verbose and error-prone; maintenance burden outweighs the dep-count savings |
| `async-trait` crate | Native AFIT (`async fn` in trait, stable since 1.75) | Ergonomic, zero heap allocation per call; BUT not object-safe — `-> impl Future` cannot be used with `Box<dyn NoshSendStream>` (D-02 locks this) |
| `async-trait` crate | `dynosaur` 0.3.0 or `trait-variant` 0.1.2 | Both provide dyn compatibility for AFIT; `dynosaur` works by generating a vtable at the call site; but these are niche crates (low downloads) vs `async-trait`'s ubiquity; not worth the added uncertainty for a thin I/O trait |

**Installation:**
```bash
# Add to workspace Cargo.toml [workspace.dependencies]:
async-trait = "0.1.89"

# Add to crates/nosh-proto/Cargo.toml:
async-trait = { workspace = true }

# Add to crates/nosh-server/Cargo.toml (for wrappers):
async-trait = { workspace = true }

# Add to crates/nosh-client/Cargo.toml (for wrappers):
async-trait = { workspace = true }
```

---

## Package Legitimacy Audit

| Package | Registry | Age | Downloads | Source Repo | slopcheck | Disposition |
|---------|----------|-----|-----------|-------------|-----------|-------------|
| `async-trait` | crates.io | ~6 yrs (2019) | Hundreds of millions | github.com/dtolnay/async-trait | [OK — dtolnay, core Rust ecosystem author] | Approved |

`async-trait` is authored by David Tolnay (dtolnay), who also authors `serde`, `anyhow`, `thiserror`, and `syn` — all already in this workspace. The crate is a foundational tokio-ecosystem dependency. No slopcheck concerns.

**Packages removed due to slopcheck [SLOP] verdict:** none
**Packages flagged as suspicious [SUS]:** none

---

## Architecture Patterns

### System Architecture Diagram

```
                          nosh-proto
                    ┌─────────────────────┐
                    │  transport_trait.rs  │
                    │                      │
                    │  trait NoshTransport │◄──────────────────────────┐
                    │  trait NoshSendStream│                           │
                    │  trait NoshRecvStream│                           │
                    └─────────────────────┘                           │
                              ▲ ▲                              implements
                              │ │                                      │
             depends on       │ │                        ┌─────────────────────────┐
    ┌────────────────────────┘ └──────────────────┐     │  nosh-server             │
    │                                             │     │  quinn_transport.rs      │
    │  nosh-server/src/                          │     │  QuinnTransport           │
    │  server.rs                                 │     │  QuinnSendStream          │
    │    handle_connection(Box<dyn NoshTransport>)│     │  QuinnRecvStream          │
    │    run_session<T: NoshTransport>(...)       │     └─────────────────────────┘
    │    run_reattach_session<T: ...>(...)        │
    │    send_burst(&dyn NoshTransport, ...)      │     ┌─────────────────────────┐
    │    build_state_diff(...)  [unchanged]       │     │  nosh-client             │
    │                                             │     │  quinn_transport.rs      │
    │  channel.rs                                 │     │  QuinnTransport           │
    │    ChannelEvent::Stream(                    │     │  QuinnSendStream          │
    │      Box<dyn NoshSendStream>,               │     │  QuinnRecvStream          │
    │      Box<dyn NoshRecvStream>                │     └─────────────────────────┘
    │    )                                        │
    │    run_channel_task(...)                    │
    │    run_scrollback_sender_task(              │
    │      ch_send: &mut dyn NoshSendStream,      │
    │      ch_recv: &mut dyn NoshRecvStream, ...) │
    │    read_varint_u32(                         │
    │      recv: &mut dyn NoshRecvStream)         │
    └────────────────────────────────────────────┘

Data flow (session pump → transport trait → quinn):
  run_session
    → conn.send_datagram()      [NoshTransport::send_datagram]
    → conn.read_datagram()      [NoshTransport::read_datagram]
    → conn.max_datagram_size()  [NoshTransport::max_datagram_size]
    → conn.accept_bi()          [NoshTransport::accept_bi → Box<dyn NoshSendStream + NoshRecvStream>]
    → write_message(&mut send)  [AsyncWrite on Box<dyn NoshSendStream>]
    → read_message(&mut recv)   [AsyncRead on Box<dyn NoshRecvStream>]
    → send.finish()             [NoshSendStream::finish]
    → send.stopped()            [NoshSendStream::stopped]
```

### Recommended Project Structure

```
crates/
  nosh-proto/
    src/
      transport_trait.rs   # NEW — NoshTransport, NoshSendStream, NoshRecvStream traits
      lib.rs               # MODIFIED — pub mod transport_trait; re-export traits
      transport.rs         # UNCHANGED — quinn TransportConfig builder
  nosh-server/
    src/
      quinn_transport.rs   # NEW — QuinnTransport, QuinnSendStream, QuinnRecvStream impls
      server.rs            # MODIFIED — generics over NoshTransport
      channel.rs           # MODIFIED — ChannelEvent::Stream types, read_varint_u32
  nosh-client/
    src/
      quinn_transport.rs   # NEW — same wrapper types as server (or a shared internal crate helper)
      client.rs            # MODIFIED — generic/boxed stream helpers
      channel.rs           # MODIFIED — concrete stream types → trait objects
```

### Pattern 1: `NoshTransport` Trait (connection level)

**What:** Thin wrapper over connection-level operations. All methods that touch the connection object move behind this trait.

**When to use:** Everywhere `&quinn::Connection` is passed as a function parameter or captured by a session task.

**Example:**

```rust
// Source: crates/nosh-proto/src/transport_trait.rs (NEW)
use bytes::Bytes;
use std::net::SocketAddr;

#[async_trait::async_trait]
pub trait NoshTransport: Send + Sync + 'static {
    /// Send an unreliable datagram (synchronous — mirrors quinn::Connection::send_datagram).
    fn send_datagram(&self, data: Bytes) -> Result<(), SendDatagramError>;
    /// Bytes available in the datagram send buffer (synchronous).
    fn datagram_send_buffer_space(&self) -> usize;
    /// Maximum datagram payload size on the current path.
    fn max_datagram_size(&self) -> Option<usize>;
    /// Receive the next incoming datagram.
    async fn read_datagram(&self) -> anyhow::Result<Bytes>;
    /// Accept the next inbound bidirectional stream.
    async fn accept_bi(&self) -> anyhow::Result<(Box<dyn NoshSendStream>, Box<dyn NoshRecvStream>)>;
    /// Open a new outbound bidirectional stream.
    async fn open_bi(&self) -> anyhow::Result<(Box<dyn NoshSendStream>, Box<dyn NoshRecvStream>)>;
    /// Peer socket address (for logging / rate limiting).
    fn remote_address(&self) -> SocketAddr;
    /// Close the connection with an error code and reason bytes.
    fn close(&self, code: u32, reason: &[u8]);
}
```

**Key design notes:**
- `send_datagram` and `datagram_send_buffer_space` are SYNCHRONOUS (matching quinn's API — no `.await`). `send_burst` calls both inside a non-async loop and must not block.
- `read_datagram` is async (quinn's `read_datagram()` is `.await`-ed in the session select! loop).
- `max_datagram_size` returns `Option<usize>` — `None` means datagrams not negotiated (matching quinn's API exactly).
- `open_bi` hides the wtransport double-await inside the wrapper: the trait's signature is single-await, and the wtransport impl calls `conn.open_bi().await?.await` internally.

### Pattern 2: `NoshSendStream` and `NoshRecvStream` Traits (stream level)

**What:** Thin wrappers over the stream I/O operations. Both traits must be object-safe (D-02).

**Example:**

```rust
// Source: crates/nosh-proto/src/transport_trait.rs (NEW)
#[async_trait::async_trait]
pub trait NoshSendStream: Send + 'static {
    async fn write_all(&mut self, data: &[u8]) -> anyhow::Result<()>;
    async fn flush(&mut self) -> anyhow::Result<()>;
    async fn finish(&mut self) -> anyhow::Result<()>;
    async fn stopped(&mut self) -> anyhow::Result<()>;
    fn reset(&mut self, code: u32);
}

#[async_trait::async_trait]
pub trait NoshRecvStream: Send + 'static {
    async fn read_exact(&mut self, buf: &mut [u8]) -> anyhow::Result<()>;
    async fn read(&mut self, buf: &mut [u8]) -> anyhow::Result<Option<usize>>;
    fn stop(&mut self, code: u32);
}
```

**Key design notes:**
- `write_all` and `flush` together cover `codec::write_message`'s `AsyncWrite` usage.
- `read_exact` and `read` cover `codec::read_message`'s `AsyncRead` usage plus the channel drain loops.
- `finish`, `stopped`, `reset` (send-only) and `stop` (recv-only) are confirmed necessary from the source read — they appear in `channel.rs` and `server.rs` half-close sequences.
- `reset` and `stop` are SYNCHRONOUS in quinn — keep them synchronous here.

**Codec integration concern:** `nosh_proto::codec::write_message` and `read_message` are currently generic over `AsyncWrite + Unpin` and `AsyncRead + Unpin`. `Box<dyn NoshSendStream>` does not directly implement `AsyncWrite`. Two options:
  - Option A (preferred): add `write_message_boxed(stream: &mut dyn NoshSendStream, msg: &Message)` helper to `nosh-proto` that delegates to the trait's own `write_all`/`flush` — avoids touching the existing `write_message` generic.
  - Option B: implement `AsyncWrite` for `Box<dyn NoshSendStream>` via a newtype/blanket impl — more ergonomic but requires `Pin` threading into the trait, which makes the trait less clean.
  Option A is simpler and avoids changing the existing codec API.

### Pattern 3: Quinn Wrapper Impls

**What:** Pure pass-through structs wrapping quinn types. No logic — only forwarding.

**Example (server-side):**

```rust
// Source: crates/nosh-server/src/quinn_transport.rs (NEW)
use nosh_proto::transport_trait::{NoshTransport, NoshSendStream, NoshRecvStream};

pub struct QuinnTransport(quinn::Connection);

#[async_trait::async_trait]
impl NoshTransport for QuinnTransport {
    fn send_datagram(&self, data: bytes::Bytes) -> Result<(), SendDatagramError> {
        self.0.send_datagram(data)
    }
    fn datagram_send_buffer_space(&self) -> usize {
        self.0.datagram_send_buffer_space()
    }
    fn max_datagram_size(&self) -> Option<usize> {
        self.0.max_datagram_size()
    }
    async fn read_datagram(&self) -> anyhow::Result<bytes::Bytes> {
        Ok(self.0.read_datagram().await?)
    }
    async fn accept_bi(&self) -> anyhow::Result<(Box<dyn NoshSendStream>, Box<dyn NoshRecvStream>)> {
        let (s, r) = self.0.accept_bi().await?;
        Ok((Box::new(QuinnSendStream(s)), Box::new(QuinnRecvStream(r))))
    }
    async fn open_bi(&self) -> anyhow::Result<(Box<dyn NoshSendStream>, Box<dyn NoshRecvStream>)> {
        let (s, r) = self.0.open_bi().await?;
        Ok((Box::new(QuinnSendStream(s)), Box::new(QuinnRecvStream(r))))
    }
    fn remote_address(&self) -> std::net::SocketAddr { self.0.remote_address() }
    fn close(&self, code: u32, reason: &[u8]) { self.0.close(code.into(), reason) }
}
```

**Accept loop implication:** `handle_connection` currently takes `quinn::Incoming`. After the refactor, it converts the resolved `quinn::Connection` into `Box<dyn NoshTransport>` (via `QuinnTransport`) immediately after auth completes — before calling `run_session` or `run_reattach_session`. The accept loop itself (`run_accept_loop`) is unchanged: it still takes a `quinn::Endpoint`.

### Pattern 4: Generics vs Boxing in the Session Pump

D-02 specifies `Box<dyn NoshSendStream + NoshRecvStream>` in `ChannelEvent::Stream`. D-03 specifies the session pump functions become "generic over `NoshTransport` OR take boxed trait objects".

The recommended split:

| Function | Mechanism | Reason |
|----------|-----------|--------|
| `handle_connection` | takes `Box<dyn NoshTransport>` | Called from accept loop; dispatch happens once per connection; boxing is clean |
| `run_session` | generic `T: NoshTransport` (or `&dyn NoshTransport`) | Called exactly once per connection after `handle_connection`; monomorphisation is acceptable; avoids double-boxing |
| `run_reattach_session` | same as `run_session` | Same reasoning |
| `send_burst` | takes `&dyn NoshTransport` | Already takes a ref to the connection; trait object ref avoids cloning |
| `build_state_diff` | unchanged | No connection type; pure computation on slot data |
| `run_channel_task` | takes boxed stream refs via `ChannelEvent::Stream` | Task receives stream via `ChannelEvent::Stream(Box<dyn ...>, Box<dyn ...>)` — already boxed |
| `run_scrollback_sender_task` | parameter types become `&mut dyn NoshSendStream` / `&mut dyn NoshRecvStream` | Called with the unboxed stream pair from the `ChannelEvent::Stream` match arm |
| `read_varint_u32` | parameter becomes `&mut dyn NoshRecvStream` | Only reads a varint prefix; simple change |
| Client session helpers | parameters become `&dyn NoshTransport` / `&mut dyn NoshSendStream` / `&mut dyn NoshRecvStream` | These are test-support helpers; boxing at the call site is acceptable |

### Pattern 5: `ChannelEvent::Stream` Type Change

**What:** The enum variant that binds a new QUIC bidi stream to a channel task changes from concrete quinn types to boxed trait objects.

**Before:**
```rust
pub enum ChannelEvent {
    Stream(quinn::SendStream, quinn::RecvStream),
    Credit(u64),
    Close,
}
```

**After:**
```rust
pub enum ChannelEvent {
    Stream(Box<dyn NoshSendStream>, Box<dyn NoshRecvStream>),
    Credit(u64),
    Close,
}
```

**Cascade:** Every match arm on `ChannelEvent::Stream` in `server.rs` and `channel.rs` binds `(s, r)` where `s: Box<dyn NoshSendStream>` and `r: Box<dyn NoshRecvStream>`. The match arms already use `mut` bindings so unboxing is not needed at the match site — `*s` / `*r` dereferences give `&mut dyn NoshSendStream` / `&mut dyn NoshRecvStream` for calls to `run_scrollback_sender_task` and `run_channel_task_inner`.

### Anti-Patterns to Avoid

- **Moving auth logic into the trait:** `extract_peer_identity` calls `conn.peer_identity()` and `conn.handshake_data()`, which are quinn-specific and not part of `NoshTransport`. These calls must remain in `handle_connection` BEFORE the `QuinnTransport` box is constructed — they belong to the quinn-specific pre-abstraction setup. Do not add `peer_identity()` or `handshake_data()` to `NoshTransport`.
- **Adding `read_datagram` to the session select arm before the trait exists:** The `read_datagram` arm in `run_session`'s `select!` will need to call `conn.read_datagram().await` on the trait object. Ensure the trait's async method returns the correct type (`anyhow::Result<Bytes>` or `Result<Bytes, TransportError>`) — a mismatch here causes an error on an otherwise working refactor.
- **Implementing `AsyncWrite` + `AsyncRead` on the trait objects and threading them into the existing generic codec:** The existing `write_message<W: AsyncWrite + Unpin>` and `read_message<R: AsyncRead + Unpin>` functions cannot accept `Box<dyn NoshSendStream>` without a blanket impl. Add `write_message_trait` / `read_message_trait` helpers that use the trait's own methods rather than retro-fitting `AsyncWrite` impl on a trait object.
- **Modifying any test:** D-05 is absolute — zero test modifications. If a test fails, the refactor changed behaviour. The pass-through wrapper must preserve semantics exactly.
- **Removing the `#[cfg(any(test, feature = "test-support"))]` echo-loop gates:** These affect `ChannelEvent::Stream` dispatch paths. The abstract stream types must flow through these gates unchanged; the gates are based on *which channel type* is accepted, not on stream type.

---

## Don't Hand-Roll

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| Object-safe async methods | Manual `Pin<Box<dyn Future<...>>>` return type | `async-trait` macro | Hand-written boxing is correct but error-prone; async-trait generates the same code reliably; dtolnay authorship means correctness is battle-tested |
| AFIT dyn compatibility | `dynosaur` / `trait-variant` | `async-trait` | Neither is as widely adopted; adds uncertainty for a thin I/O trait |

**Key insight:** The `async-trait` crate generates exactly the code you would write by hand but without the opportunity for transcription errors in `Pin<Box<dyn Future<Output = Result<...>> + Send + '_>>` signatures.

---

## Common Pitfalls

### Pitfall 1: AFIT is not object-safe — using `async fn` directly in the trait

**What goes wrong:** `async fn write_all(&mut self, ...)` in a trait desugars to `fn write_all(...) -> impl Future<...>`. `impl Future` is an opaque type — the vtable cannot hold an opaque return type. `Box<dyn NoshSendStream>` fails to compile with "the trait `NoshSendStream` is not dyn compatible".

**Why it happens:** D-02 requires `Box<dyn NoshSendStream>` in `ChannelEvent::Stream`. Rust 1.75+ stabilised AFIT but did NOT stabilise dyn compatibility for AFIT. Dyn compatibility for AFIT is tracked in rust-lang/rust but is not stable as of nightly 1.97 without `#![feature(dyn_compatible_trait_objects)]` which does not exist yet.

**How to avoid:** Use `#[async_trait]` from the `async-trait` crate on the trait definition and all `impl` blocks. The macro rewrites `async fn` to `fn -> Pin<Box<dyn Future + Send>>` which is object-safe.

**Warning signs:** Compiler error: "the trait `NoshSendStream` is not dyn compatible" or "return type cannot be made into an object".

### Pitfall 2: `send_datagram` and `datagram_send_buffer_space` must remain synchronous

**What goes wrong:** Wrapping `send_datagram` as an `async fn` in the trait causes `send_burst` (which calls it in a tight loop, no `.await`) to require restructuring as an async function and to stop being usable in the non-async session select! branches.

**Why it happens:** `quinn::Connection::send_datagram` is synchronous — it places the datagram in the QUIC send buffer immediately. `send_burst`'s entire design relies on calling it synchronously in a loop without yielding.

**How to avoid:** Keep `send_datagram` and `datagram_send_buffer_space` as synchronous (`fn`, not `async fn`) in `NoshTransport`. The `#[async_trait]` attribute does not prevent non-async methods in the trait.

**Warning signs:** `send_burst` becomes `async fn` or gains `.await` on the datagram send — that's a design regression.

### Pitfall 3: `max_datagram_size` returns `Option<usize>`, not `usize`

**What goes wrong:** Changing the return type to `usize` causes the callers (both `run_session` and `run_reattach_session` match arms) to break — they currently use `match conn.max_datagram_size() { Some(c) if c >= MIN_CAP => c, _ => continue }`.

**Why it happens:** When datagrams are not negotiated, `max_datagram_size()` returns `None`. The session pump correctly skips that tick. Flattening to `usize` (returning 0 for None) would silently enter `send_burst` with `cap < MIN_CAP`, triggering the `MIN_CAP` guard and always returning None from `build_state_diff`.

**How to avoid:** Keep the return type `Option<usize>` in `NoshTransport`.

### Pitfall 4: `quinn::Connection::close` takes `quinn::VarInt`, not `u32`

**What goes wrong:** The `NoshTransport::close(code: u32, reason: &[u8])` signature maps `u32` to `quinn::VarInt`. In the wrapper, `self.0.close(code.into(), reason)` works because `quinn::VarInt: From<u32>`. No issue — but the planner must remember to include `.into()` in the wrapper.

**How to avoid:** Use `code.into()` in the Quinn wrapper's `close` implementation.

### Pitfall 5: `extract_peer_identity` uses `conn.peer_identity()` — NOT part of the trait

**What goes wrong:** `extract_peer_identity` calls `conn.peer_identity()` and `conn.handshake_data()` on the `quinn::Connection` to extract the TLS client certificate. Both methods are quinn-specific and do not belong in `NoshTransport` (WebTransport does not use them; inner auth is different).

**Why it happens:** `handle_connection` currently calls `extract_peer_identity(&conn)` BEFORE dispatching to `run_session`. After the refactor, this call must happen BEFORE wrapping the connection in `QuinnTransport`. The refactored `handle_connection` flow is: (1) resolve incoming, (2) extract identity from `quinn::Connection`, (3) wrap in `Box::new(QuinnTransport(conn))`, (4) dispatch to `run_session(transport, ...)`.

**How to avoid:** Do not add auth methods to `NoshTransport`. `extract_peer_identity` stays a standalone function operating on `&quinn::Connection` directly, called before the boxing step.

### Pitfall 6: `read_varint_u32` currently takes `&mut quinn::RecvStream`

**What goes wrong:** `read_varint_u32` in `nosh-server/src/channel.rs` is called from the `accept_bi` arm in both `run_session` and `run_reattach_session`. After the trait change, `accept_bi` returns `Box<dyn NoshRecvStream>`. The function signature must change to `async fn read_varint_u32(recv: &mut dyn NoshRecvStream)`.

**How to avoid:** Update `read_varint_u32` as part of the `channel.rs` changes. The function body only calls `recv.read_exact(&mut buf)` so the body works unchanged after the parameter type change.

### Pitfall 7: Client channel.rs functions take concrete stream types — must be updated

**What goes wrong:** `nosh-client/src/channel.rs` has `run_channel_drain_task(ch_recv: quinn::RecvStream, ch_send: quinn::SendStream, ...)` and the scrollback drain counterpart. These must be updated to accept boxed trait objects. The existing integration tests in `crates/nosh-client/tests/` construct channels via `client::open_channel(conn, ...)` which returns `(quinn::SendStream, quinn::RecvStream)` — after the refactor this must return `(Box<dyn NoshSendStream>, Box<dyn NoshRecvStream>)`.

**How to avoid:** The test change rule (D-05: zero test modifications) means the integration tests must still compile. The client helper functions in `client.rs` (test support surface) must return `Box<dyn NoshSendStream>` / `Box<dyn NoshRecvStream>`, and the tests must accept `Box<dyn ...>` in return — but if the tests currently call `send.write_all(...)` directly on the returned `quinn::SendStream`, those calls must still work via the trait. Verify that `Box<dyn NoshSendStream>` exposes `write_all` correctly before assuming tests pass.

**Warning signs:** D-05 violation — a test that previously compiled fails after the refactor. That is the signal to check whether the test was relying on a quinn-specific API not on the trait.

---

## Code Examples

### Full trait surface with `async-trait`

```rust
// Source: crates/nosh-proto/src/transport_trait.rs (NEW — authoring reference)
use async_trait::async_trait;
use bytes::Bytes;
use std::net::SocketAddr;

/// Errors that can occur on datagram send.
/// Re-exports quinn's error type for the Quinn wrapper; WtransportSendStream maps its own errors.
#[derive(Debug, thiserror::Error)]
pub enum SendDatagramError {
    #[error("datagram too large for current path MTU")]
    TooLarge,
    #[error("peer does not support datagrams")]
    UnsupportedByPeer,
    #[error("datagrams disabled on this connection")]
    Disabled,
    #[error("connection lost: {0}")]
    ConnectionLost(String),
}

#[async_trait]
pub trait NoshTransport: Send + Sync + 'static {
    fn send_datagram(&self, data: Bytes) -> Result<(), SendDatagramError>;
    fn datagram_send_buffer_space(&self) -> usize;
    fn max_datagram_size(&self) -> Option<usize>;
    async fn read_datagram(&self) -> anyhow::Result<Bytes>;
    async fn accept_bi(&self) -> anyhow::Result<(Box<dyn NoshSendStream>, Box<dyn NoshRecvStream>)>;
    async fn open_bi(&self) -> anyhow::Result<(Box<dyn NoshSendStream>, Box<dyn NoshRecvStream>)>;
    fn remote_address(&self) -> SocketAddr;
    fn close(&self, code: u32, reason: &[u8]);
}

#[async_trait]
pub trait NoshSendStream: Send + 'static {
    async fn write_all(&mut self, data: &[u8]) -> anyhow::Result<()>;
    async fn flush(&mut self) -> anyhow::Result<()>;
    async fn finish(&mut self) -> anyhow::Result<()>;
    async fn stopped(&mut self) -> anyhow::Result<()>;
    fn reset(&mut self, code: u32);
}

#[async_trait]
pub trait NoshRecvStream: Send + 'static {
    async fn read_exact(&mut self, buf: &mut [u8]) -> anyhow::Result<()>;
    async fn read(&mut self, buf: &mut [u8]) -> anyhow::Result<Option<usize>>;
    fn stop(&mut self, code: u32);
}
```

### Codec helper functions for trait objects

```rust
// Source: crates/nosh-proto/src/transport_trait.rs (NEW) — addendum to above
// These parallel write_message / read_message but work with the trait types.

/// Write a Message via NoshSendStream (parallel to codec::write_message).
pub async fn write_message_ns(
    stream: &mut dyn NoshSendStream,
    msg: &crate::Message,
) -> Result<(), crate::ProtoError> {
    let frame = crate::codec::encode(msg)?;
    stream.write_all(&frame).await.map_err(|e| {
        crate::ProtoError::Io(std::io::Error::new(std::io::ErrorKind::BrokenPipe, e))
    })?;
    stream.flush().await.map_err(|e| {
        crate::ProtoError::Io(std::io::Error::new(std::io::ErrorKind::BrokenPipe, e))
    })?;
    Ok(())
}

/// Read a Message via NoshRecvStream (parallel to codec::read_message).
pub async fn read_message_ns(
    stream: &mut dyn NoshRecvStream,
) -> Result<crate::Message, crate::ProtoError> {
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await.map_err(|e| {
        crate::ProtoError::Io(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, e))
    })?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > crate::codec::MAX_FRAME_LEN {
        return Err(crate::ProtoError::FrameTooLarge(len));
    }
    let mut body = vec![0u8; len];
    stream.read_exact(&mut body).await.map_err(|e| {
        crate::ProtoError::Io(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, e))
    })?;
    crate::codec::decode(&body)
}
```

### Quinn `SendStream` wrapper

```rust
// Source: crates/nosh-server/src/quinn_transport.rs (NEW)
pub struct QuinnSendStream(pub quinn::SendStream);

#[async_trait::async_trait]
impl NoshSendStream for QuinnSendStream {
    async fn write_all(&mut self, data: &[u8]) -> anyhow::Result<()> {
        use tokio::io::AsyncWriteExt;
        self.0.write_all(data).await?;
        Ok(())
    }
    async fn flush(&mut self) -> anyhow::Result<()> {
        use tokio::io::AsyncWriteExt;
        self.0.flush().await?;
        Ok(())
    }
    async fn finish(&mut self) -> anyhow::Result<()> {
        self.0.finish();  // quinn 0.11: finish() is synchronous, returns ()
        Ok(())
    }
    async fn stopped(&mut self) -> anyhow::Result<()> {
        self.0.stopped().await?;
        Ok(())
    }
    fn reset(&mut self, code: u32) {
        let _ = self.0.reset(code.into());
    }
}
```

**Note on `quinn::SendStream::finish()` API:** In quinn 0.11.x, `finish()` is synchronous (`fn finish(&mut self)`) — it marks the stream as finished but does NOT await acknowledgement. `stopped()` is the async half-close confirmation. The wrapper's `async fn finish()` therefore wraps a sync call (fine — the async-trait machinery will just poll it once and return Ready immediately).

### Migration detection after connection boxing

```rust
// Illustrates the pattern for run_session after refactor
// The conn variable is now &dyn NoshTransport

// Before:
let cur = conn.remote_address();

// After (unchanged call, just through the trait):
let cur = conn.remote_address();
```

No change at the call site — `remote_address()` is on the trait.

---

## Exact Method Surface (Source-Verified)

This table documents every transport-level method call found by reading the production source files directly, grouped by where they appear.

### `nosh-server/src/server.rs` — connection methods

| Method | Call site | Notes |
|--------|-----------|-------|
| `conn.send_datagram(payload)` | `send_burst` | Synchronous; `SendDatagramError` variants: `TooLarge`, `UnsupportedByPeer`, `Disabled`, `ConnectionLost` |
| `conn.datagram_send_buffer_space()` | `send_burst` while-loop guard | Synchronous |
| `conn.max_datagram_size()` | `run_session` and `run_reattach_session` diff tick | Returns `Option<usize>` |
| `conn.read_datagram()` | `run_session` and `run_reattach_session` select! arm | Async |
| `conn.accept_bi()` | `handle_connection` (first stream); both session loops (secondary channels) | Async |
| `conn.remote_address()` | `handle_connection` (peer logging); migration detection poll | Synchronous |
| `conn.close(code, reason)` | `handle_connection` (3 sites); `run_session` (3 sites); `run_reattach_session` | Synchronous |
| `conn.handshake_data()` | `handle_connection` (ALPN log) — NOT on trait | Quinn-specific, pre-boxing |
| `conn.peer_identity()` | `extract_peer_identity` — NOT on trait | Quinn-specific, pre-boxing |

### `nosh-server/src/channel.rs` — stream methods

| Method | Call site | Notes |
|--------|-----------|-------|
| `ch_send.finish()` | `run_channel_task` half-close; `run_scrollback_sender_task` (3 sites) | Synchronous in quinn 0.11 |
| `ch_send.stopped()` | Same sites, bounded by timeout | Async |
| `ch_send.write_all(data)` | `run_scrollback_sender_task` (encoded page write) | Via `AsyncWrite` |
| `ch_recv.read_exact(&mut buf)` | `read_varint_u32` | Via `AsyncRead` |
| `ch_recv.read(&mut buf)` | `run_channel_task_inner` (production stub), `run_echo_loop` | Returns `Ok(Some(n))` / `Ok(None)` / `Err` |
| `ch_send.reset(0u32.into())` | `run_session` accept_bi arm (unknown channel) | Synchronous |
| `ch_recv.stop(0u32.into())` | Same arm | Synchronous, returns `Result` (ignored) |

### `nosh-client/src/client.rs` — connection and stream methods

| Method | Call site |
|--------|-----------|
| `conn.open_bi()` | `stream_echo_roundtrip`, `reattach_collect`, `open_session`, `open_channel` |
| `conn.send_datagram(payload)` | `datagram_roundtrip` |
| `conn.read_datagram()` | `datagram_roundtrip` |
| `conn.max_datagram_size()` | `datagram_roundtrip` |
| `send.write_all(payload)` | `stream_echo_roundtrip` |
| `send.finish()` | `stream_echo_roundtrip` |
| `send.write_message` | `open_session_with_token`, `send_reattach`, `send_ack`, `run_session_collect` |
| `recv.read_message` | `await_reattach_reply`, `collect_until_close`, `run_session_collect` |

### `nosh-client/src/channel.rs` — stream methods

| Method | Call site |
|--------|-----------|
| `ch_recv: quinn::RecvStream` (drain task) | `run_channel_drain_task`, `run_scrollback_drain_task` |
| `ch_send: quinn::SendStream` (drain task) | Same |
| `ch_send.finish()` | Both drain tasks |
| `ch_send.stopped()` | Both drain tasks |

---

## State of the Art

| Old Approach | Current Approach | When Changed | Impact |
|--------------|------------------|--------------|--------|
| `async fn` in traits required `async-trait` macro | Native AFIT stable since Rust 1.75 (Dec 2023) | Rust 1.75 | Ergonomic improvement for static dispatch; dyn dispatch still needs async-trait |
| `quinn::SendStream::finish()` was async | `finish()` is synchronous in quinn 0.11 | quinn 0.11 | Wrapper's `async fn finish()` wraps a sync call — fine |

**Deprecated / outdated:**
- `async-trait` is NOT deprecated — it remains the standard for object-safe async traits and is maintained by dtolnay.
- Native AFIT without async-trait is only appropriate when dyn dispatch is not needed. Since D-02 mandates `Box<dyn NoshSendStream>`, async-trait remains mandatory here.

---

## Assumptions Log

| # | Claim | Section | Risk if Wrong |
|---|-------|---------|---------------|
| A1 | `quinn::SendStream::finish()` is synchronous in quinn 0.11.9 (i.e. `fn finish(&mut self) -> ()` not `async fn`) | Code Examples | If it is still async, the wrapper's `async fn finish()` body needs `self.0.finish().await` — minor fix, no design impact |
| A2 | The installed nightly toolchain (1.97.0-nightly) does not have stable dyn-compatible AFIT — `async fn` in trait with `Box<dyn T>` still requires `async-trait` | Standard Stack | If nightly 1.97 has this feature stabilised, native AFIT would work; the team could switch from `async-trait` to native AFIT — but `async-trait` remains valid regardless |

---

## Open Questions

1. **`quinn::SendStream::finish()` exact API in 0.11.9**
   - What we know: ARCHITECTURE.md and the session pump code call `ch_send.finish()` with no `.await` in many places; the code assigns `let _ = ch_send.finish()` suggesting it returns `Result<(), _>` or `()`.
   - What's unclear: Whether `finish()` is `fn finish(&mut self)` or `fn finish(&mut self) -> Result<()>`. The wrapper's `async fn finish()` should propagate the result.
   - Recommendation: The planner should include a task to check `cargo doc --open quinn::SendStream` for the exact signature, and wire the wrapper accordingly (either `.map_err(Into::into)?` or `Ok(())`).

2. **`nosh-client` test helpers return `quinn::SendStream` / `quinn::RecvStream` directly — will zero-test-modification hold?**
   - What we know: D-05 is absolute. The tests in `crates/nosh-client/tests/` call helpers like `open_channel(conn, ...)` that currently return `(quinn::SendStream, quinn::RecvStream)`. After the refactor these return `(Box<dyn NoshSendStream>, Box<dyn NoshRecvStream>)`.
   - What's unclear: Whether the test bodies call quinn-specific methods (e.g. `send.reset(...)`) that are NOT on the trait, which would force a test modification.
   - Recommendation: The planner should include a task to audit each test file for quinn-specific method calls on the returned stream pair before finalising the plan. If found, those calls must be moved into the trait or the test is a false failure.

---

## Environment Availability

No external dependencies beyond the existing workspace are needed at runtime. `async-trait` is a compile-time proc-macro crate only.

| Dependency | Required By | Available | Version | Fallback |
|------------|------------|-----------|---------|----------|
| `cargo` | Build system | ✓ | nightly 1.97.0 | — |
| `async-trait` | Trait definitions | NEW — add to Cargo.toml | 0.1.89 | Hand-written Pin<Box<dyn Future>> (not recommended) |

---

## Validation Architecture

`nyquist_validation` is `false` in `.planning/config.json` — this section is omitted per the skip condition.

---

## Security Domain

This phase introduces no new attack surface. It is a pure code refactor. Existing security invariants:

- The pre-auth DoS cap (`Semaphore`) remains in `run_accept_loop` — unchanged by this refactor.
- `extract_peer_identity` runs BEFORE the `QuinnTransport` box is constructed — the auth invariant is preserved.
- No environment variables are touched; `SSH_AUTH_SOCK` forwarding rule unchanged.

No new ASVS categories apply to a pure trait-introduction refactor.

---

## Sources

### Primary (HIGH confidence — direct source reads)

- `/home/bharris/github.com/bharrisau/nosh/crates/nosh-server/src/server.rs` — all connection-method call sites: `send_datagram`, `datagram_send_buffer_space`, `max_datagram_size`, `read_datagram`, `accept_bi`, `remote_address`, `close`; `extract_peer_identity` pattern (pre-boxing requirement)
- `/home/bharris/github.com/bharrisau/nosh/crates/nosh-server/src/channel.rs` — `ChannelEvent::Stream(quinn::SendStream, quinn::RecvStream)` (exact current type); `finish`, `stopped`, `reset`, `stop`, `read_exact`, `read`, `write_all` method calls; `read_varint_u32` signature
- `/home/bharris/github.com/bharrisau/nosh/crates/nosh-client/src/client.rs` — `open_bi`, `send_datagram`, `read_datagram`, `max_datagram_size` calls; stream helper function signatures
- `/home/bharris/github.com/bharrisau/nosh/crates/nosh-client/src/channel.rs` — client drain tasks, stream parameter types
- `/home/bharris/github.com/bharrisau/nosh/crates/nosh-proto/src/codec.rs` — `write_message<W: AsyncWrite + Unpin>` and `read_message<R: AsyncRead + Unpin>` (why new `_ns` helpers are needed)
- `/home/bharris/github.com/bharrisau/nosh/crates/nosh-proto/src/lib.rs` — module structure; existing `transport.rs` file name (chosen name `transport_trait.rs` avoids collision)
- `/home/bharris/github.com/bharrisau/nosh/Cargo.toml` — workspace MSRV 1.74, existing deps, nightly toolchain confirmed
- `.planning/phases/23-transport-abstraction-seam/23-CONTEXT.md` — locked decisions D-01 through D-05
- `.planning/research/ARCHITECTURE.md` — original trait design sketch, component responsibility map

### Secondary (MEDIUM confidence — toolchain knowledge + cargo search)

- `cargo search async-trait` — version 0.1.89 confirmed on crates.io
- Rust Reference / RFC 3185 — AFIT is NOT object-safe as of stable Rust; dyn-compatible AFIT is not stabilised on nightly 1.97 — object-safety requirement drives the `async-trait` recommendation
- `cargo search dynosaur` — version 0.3.0 confirmed; considered and rejected as niche alternative

---

## Metadata

**Confidence breakdown:**
- Exact method surface: HIGH — sourced by direct source-file reads
- Async-trait mechanism choice: HIGH — AFIT object-safety limitation is documented Rust language behaviour; `async-trait` is the standard mitigation
- Quinn wrapper pattern: HIGH — trivial delegation; no novel design
- Codec helper approach: MEDIUM — the recommendation (add `write_message_ns`/`read_message_ns`) is one of two valid options; the planner may choose differently

**Research date:** 13/06/2026
**Valid until:** This research is based on source code, not external APIs — it remains valid until the source files it references are modified. External: async-trait 0.1.89 is stable and widely used; no near-term breaking changes expected (30 days).
