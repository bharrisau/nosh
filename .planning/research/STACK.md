# Stack Research

**Domain:** QUIC-based roaming remote shell (Rust) — v1.1 M3 Roaming + Windows Client
**Researched:** 2026-05-30
**Confidence:** HIGH (all crate versions and APIs verified against docs.rs live data)

---

## Existing Validated Stack (v1.0 — DO NOT RE-RESEARCH)

| Technology | Pinned Version | Role |
|------------|---------------|------|
| `quinn` | 0.11.9 | QUIC transport |
| `rustls` | 0.23.x | TLS 1.3 (via quinn) |
| `tokio` | 1.52.x | Async runtime |
| `portable-pty` | 0.9.0 | PTY (Linux) |
| `ssh-key` | 0.6.7 | Key parsing / authorized_keys / known_hosts |
| `ssh-agent-client-rs` | 1.1.x | Agent signing (Linux) |
| `ed25519-dalek` | 2.2.0 | Ed25519 material |
| `vte` | 0.15.0 | VT parser |
| `rcgen` | 0.14.x | Ephemeral self-signed certs |
| `crossterm` | 0.28.1 | Client terminal raw mode + event reading |
| `postcard` + `serde` | 1.x / 1.x | Frame serialization |
| `bytes`, `tracing`, `anyhow`, `thiserror`, `clap` | — | Shared utilities |
| `uuid` | 1.x (v4) | Session IDs (server) |
| `nix` | 0.29 | Signal handling (server, Linux) |
| `dirs` | 5.x | Platform path resolution |
| `x509-parser` | 0.18 | SPKI extraction from TLS certs |

---

## v1.1 Stack Additions and Changes

### 1. QUIC Connection Migration (quinn 0.11.9)

**No new crate required.** Migration is already implemented inside quinn 0.11.9. The findings below are the authoritative API surface.

#### How migration works in quinn 0.11.9

**NAT rebinding (passive — zero code change needed):** When the client's source UDP 4-tuple changes due to NAT rebind, the server receives packets from the new address. With `ServerConfig::migration(true)` (the DEFAULT), quinn-proto automatically runs PATH_CHALLENGE/PATH_RESPONSE on the new path. If path validation succeeds, the connection migrates. No application-level call is needed on either side. `Connection::remote_address()` will reflect the new address after migration. This is the behavior that handles Wi-Fi→cellular IP change at the OS/NAT level.

**Deliberate interface switch (active — client must call `Endpoint::rebind`):** When the client deliberately switches to a new network interface (e.g., binding a new local UDP socket), the application must call `Endpoint::rebind(socket: UdpSocket) -> Result<()>` or `Endpoint::rebind_abstract(socket: Arc<dyn AsyncUdpSocket>) -> Result<()>`. These methods replace the underlying UDP socket live across all active connections, sending a `ConnectionEvent::Rebind` to each connection driver. The QUIC layer then probes the new path with PATH_CHALLENGE.

**Warning from docs:** `Endpoint::rebind` — "Incoming connections and connections to servers unreachable from the new address will be lost." This is expected: the intent is exactly to change the local socket when the network interface changes.

#### Exact method signatures (verified, quinn 0.11.9)

```rust
// On Endpoint
pub fn rebind(&self, socket: UdpSocket) -> Result<()>
pub fn rebind_abstract(&self, socket: Arc<dyn AsyncUdpSocket>) -> Result<()>
pub fn local_addr(&self) -> Result<SocketAddr>

// On ServerConfig
pub fn migration(&mut self, value: bool) -> &mut ServerConfig  // default: true

// On Connection (complete method list — no migrate/set_path/network_path exist)
pub fn remote_address(&self) -> SocketAddr   // updates after migration
pub fn local_ip(&self) -> Option<IpAddr>     // local side; may be None on some platforms
pub fn rtt(&self) -> Duration
pub fn stats(&self) -> ConnectionStats       // includes PathStats (rtt, cwnd, lost_packets, current_mtu)
pub fn stable_id(&self) -> usize             // stable for connection lifetime; use as session map key
// ... open_bi, accept_bi, open_uni, accept_uni, send_datagram, read_datagram, close, etc.
```

**Methods that do NOT exist in 0.11.9:**
- `Connection::migrate()` — does not exist
- `Connection::set_path()` — does not exist
- `Connection::network_path()` — does not exist
- `Connection::path()` — does not exist

#### TransportConfig knobs affecting migration quality

```rust
let mut tc = quinn::TransportConfig::default();
// Keep-alive prevents idle timeout during quiescent network change
tc.keep_alive_interval(Some(Duration::from_secs(15)));
// Generous idle timeout gives cold-reconnect window (Mosh-style persistence)
tc.max_idle_timeout(Some(Duration::from_secs(300).try_into().unwrap()));
// MTU discovery adapts to new path characteristics after migration
tc.mtu_discovery_config(Some(MtuDiscoveryConfig::default()));
```

#### Connection ID management

`EndpointConfig::cid_generator()` allows custom CID generation (e.g., for load balancers). The default `HashedConnectionIdGenerator` is appropriate for nosh. `Connection::stable_id()` returns a usize that is stable for the lifetime of the connection and is the correct handle for the server-side orphaned-session registry key.

**Confidence: HIGH** — verified from docs.rs/quinn 0.11.9 Connection method enumeration, Endpoint source, quinn-proto connection/mod.rs path migration logic.

---

### 2. Windows Client — Terminal I/O

**`crossterm` already in the tree at 0.28.1. Upgrade to 0.29.0 is recommended.**

#### crossterm 0.28.1 → 0.29.0 upgrade

0.29.0 (released April 5, 2025) adds OSC52 clipboard support (useful for M5), keyboard enhancement flag queries, and rustix 1.0. No breaking API changes for the existing raw-mode + event loop usage. Ratatui and gitui have both bumped to 0.29.0. Recommend bumping crossterm to 0.29.0 in the workspace.

#### Windows MSVC support

crossterm 0.29.0 explicitly lists `x86_64-pc-windows-msvc` and `i686-pc-windows-msvc` as supported targets. `enable_raw_mode()` works on Windows 10+ via VT mode; falls back to WinAPI on older systems. Windows 10 is the minimum realistic nosh client target.

#### Async event reading (tokio EventStream)

Enable the `event-stream` feature to get `crossterm::event::EventStream`, which implements `futures::Stream<Item = Result<Event>>` and works with tokio select loops.

```toml
# nosh-client/Cargo.toml
crossterm = { version = "0.29", features = ["events", "event-stream"] }
futures = "0.3"  # for StreamExt
```

```rust
use crossterm::event::{EventStream, Event};
use futures::StreamExt;

let mut event_stream = EventStream::new();
loop {
    tokio::select! {
        Some(Ok(event)) = event_stream.next() => { /* handle */ }
        // ... other nosh futures
    }
}
```

The existing Linux client already uses crossterm; the same code compiles and runs on Windows MSVC without changes.

**Critical: do NOT enable the `use-dev-tty` feature.** It is Unix-only and breaks the build in combination with `event-stream` (crossterm issue #935). The default Windows path in crossterm does not need this flag.

#### Does quinn/tokio/ring build on `x86_64-pc-windows-msvc`?

Yes. `ring` 0.17.14 ships precompiled assembly objects for `x86_64-windows` in the crates.io package — no NASM assembler required. The only prerequisite is "Build Tools for Visual Studio 2022" with the "Desktop development with C++" workload. `tokio` and `quinn` are pure Rust and build on all MSVC targets. `quinn` uses `socket2` for UDP, which has full Windows support.

**What correctly does NOT build on Windows (excluded from v1.1 Windows client scope):**
- `nosh-server` — has `portable-pty` (Linux PTY) and `nix` (Unix-only signal/user features)
- `ssh-agent-client-rs` — Unix socket client; deferred on Windows (Pageant uses named pipes, different protocol)

**Confidence: HIGH** — crossterm 0.29.0 Windows targets verified on docs.rs; ring BUILDING.md precompiled-object claim verified; quinn/tokio known-pure-Rust.

---

### 3. On-Disk OpenSSH Key Signing (Windows Client)

**No new crate required.** `ssh-key 0.6.7` already in the tree has the complete API.

#### Loading a private key from disk

```rust
use ssh_key::PrivateKey;
use std::path::Path;

// Requires: ssh-key features "std" (already enabled in the workspace)
let key = PrivateKey::read_openssh_file(Path::new("/home/user/.ssh/id_ed25519"))?;
```

#### Passphrase-encrypted keys

```rust
// Requires: ssh-key feature "encryption" (not currently enabled — must add for Windows path)
if key.is_encrypted() {
    let passphrase = /* prompt user */;
    let key = key.decrypt(passphrase.as_bytes())?;
}
```

The `encryption` feature pulls in `bcrypt-pbkdf`, AES-256-CTR, and ChaCha20Poly1305 — all pure Rust, no native deps, builds on Windows MSVC cleanly. This feature is NOT needed on Linux (where signing goes through ssh-agent), so gate it via `cfg(target_os = "windows")` or a `file-key` Cargo feature to avoid inflating the Linux server binary.

#### Ed25519 signing without ssh-agent

`PrivateKey` implements `signature::Signer` (from the `signature` crate, a transitive dep). For wiring into `rustls::sign::SigningKey`, extract the raw Ed25519 key material and delegate to `ed25519-dalek`:

```rust
// Extract dalek SigningKey from ssh_key::PrivateKey
let ed_kp = private_key.key_data().ed25519()
    .ok_or(anyhow::anyhow!("not an Ed25519 key"))?;
// ed_kp.private is the 32-byte seed (+ public key in expanded form)
// ed25519_dalek::SigningKey::from_bytes(&seed_bytes) gives a dalek key
// Then implement rustls::sign::Signer::sign(message) via dalek_key.sign(message)
```

Signing path: `read_openssh_file → decrypt (if encrypted) → extract Ed25519 bytes → ed25519-dalek::SigningKey → rustls::sign::Signer`. Zero agent round trips. The `WindowsFileSigningKey` struct implementing `rustls::sign::SigningKey` lives in `nosh-auth`, gated on `cfg(windows)` or a `file-key` feature.

**Required feature addition (Windows target only):**

```toml
# In nosh-auth/Cargo.toml or nosh-client/Cargo.toml, gated:
[target.'cfg(target_os = "windows")'.dependencies]
ssh-key = { version = "0.6", default-features = false,
            features = ["ed25519", "std", "alloc", "encryption"] }
```

**Confidence: HIGH** — `PrivateKey::read_openssh_file`, `decrypt`, `is_encrypted`, key_data API all verified from docs.rs/ssh-key 0.6.7; encryption feature deps verified from Cargo.toml source.

---

### 4. Session Persistence and Reattach Tokens

**No new crates required.** The existing tree already has everything needed.

#### Reattach token generation

`uuid` 1.x with `v4` feature (already in `nosh-server/Cargo.toml`) generates tokens via `Uuid::new_v4()`, which calls `getrandom` internally (CSPRNG-backed OS call). UUID v4 provides 122 bits of entropy — sufficient for an unguessable reattach token. Token is bound to the SSH identity at creation and checked against the re-authenticating peer at reattach time.

If 32 bytes of entropy are preferred, `getrandom` is already transitive in the lockfile (0.2.17, 0.3.4, and 0.4.2 all present via ring/ssh-key). `getrandom::fill(&mut [0u8; 32])` — no direct dep addition needed.

#### Sequence-numbered resume buffer (ET BackedReader pattern)

Application logic over existing quinn streams — no new crate. Pattern:

- Server maintains a `VecDeque<(seq: u64, Bytes)>` per orphaned session, bounded by a max-bytes cap (e.g., 1 MiB)
- Each outbound frame carries an incrementing `seq` field in the nosh-proto envelope (postcard + serde already serializes this)
- On reattach, client sends `ReattachRequest { session_token, last_acked_seq: u64 }` over a new authenticated connection
- Server verifies the token matches the claiming identity, then replays `seq > last_acked_seq` frames on the new stream

#### Session identity threading (v1.0 seam)

After handshake: `connection.peer_identity()` returns `Option<Box<dyn Any + Send>>`. For quinn's rustls backend, downcast to `Vec<CertificateDer>`. Extract SPKI with `x509-parser` (already in `nosh-auth`). Parse to `ssh_key::PublicKey`. Store in `Session.identity`. All existing crates; no new deps.

`Connection::stable_id()` (usize, stable for connection lifetime) is the correct live-connection key for the session map; replaced by the reattach token on disconnect.

#### Per-identity session cap

`HashMap<ssh_key::Fingerprint, VecDeque<OrphanedSession>>` in the server, bounded by count or total buffer bytes. `ssh_key::PublicKey::fingerprint(HashAlg::Sha256)` provides the identity hash. No new crates.

**Confidence: HIGH** — uuid v4 → getrandom path verified in lockfile; postcard/serde already used for proto frames; Connection::stable_id and peer_identity verified from docs.rs.

---

## Summary of Cargo.toml Changes

### Workspace `Cargo.toml`

```toml
[workspace.dependencies]
# Add futures for crossterm EventStream (tokio-based async event reading)
futures = "0.3"
# Bump crossterm from 0.28.1 to 0.29.0
crossterm = { version = "0.29", features = ["events"] }
```

### `nosh-client/Cargo.toml`

```toml
# Add event-stream feature (was missing in 0.28.1 entry)
crossterm = { workspace = true, features = ["event-stream"] }
futures = { workspace = true }

# Windows-only: on-disk key signing with passphrase support
[target.'cfg(target_os = "windows")'.dependencies]
ssh-key = { version = "0.6", default-features = false,
            features = ["ed25519", "std", "alloc", "encryption"] }
# Remove ssh-agent-client-rs from Windows target (Unix socket only)
```

### `nosh-server/Cargo.toml`

No new dependencies for roaming or session persistence. `uuid` v4 already present.

### `nosh-auth/Cargo.toml`

No new crates. Add `WindowsFileSigningKey` implementation gated on `cfg(windows)` using existing `ssh-key` and `ed25519-dalek`.

---

## Alternatives Considered

| Category | Recommended | Alternative | Why Not |
|----------|-------------|-------------|---------|
| Reattach token | `uuid` v4 (already in tree) | `rand::random::<[u8; 32]>()` | uuid already present, formats cleanly, 122-bit entropy sufficient |
| Reattach token | `uuid` v4 | `getrandom` directly | Both work; uuid more ergonomic for serialization |
| Resume buffer | Hand-rolled VecDeque | A dedicated replay crate | No such crate exists at the right abstraction; the pattern is ~50 lines |
| Windows terminal | `crossterm` 0.29 | `windows-rs` console API directly | crossterm abstracts WinAPI/ANSI duality; already in tree; no reason to drop |
| Windows key signing | `ssh-key` + `ed25519-dalek` (in-process) | `ssh-agent-client-rs` on Windows | Pageant uses named pipes, not Unix sockets; ssh-agent-client-rs is Unix-only |
| crossterm version | 0.29.0 | Stay at 0.28.1 | 0.29.0 adds OSC52 (useful M5) and rustix 1.0; upgrade cost is minimal |
| Crypto backend | Keep `rustls-ring` | Switch to `aws-lc-rs` | aws-lc-rs needs CMake on Windows; ring 0.17.14 has precompiled x86_64 objects |

---

## What NOT to Add

| Avoid | Why | Use Instead |
|-------|-----|-------------|
| Any dedicated "session resume" crate | None at the right abstraction; BackedReader is ~50 lines of VecDeque logic | Hand-roll on postcard + serde |
| `russh` for Windows key signing | Full SSH protocol implementation; pulls large transitive deps | `ssh-key` + `ed25519-dalek` (already in tree) |
| `crossterm` `use-dev-tty` feature | Unix-only; breaks build combined with `event-stream` (issue #935) | Omit this feature flag |
| `getrandom` as a direct dependency | Already transitive via ring and uuid; adding a direct dep risks version skew | Use `Uuid::new_v4()` for tokens |
| `tokio-rustls` | Not needed; quinn handles TLS/QUIC internally | quinn only |
| `ed25519-dalek` 3.0.0-pre.* | Pre-release API; may change | `ed25519-dalek` 2.2.0 (stable, already in tree) |

---

## Version Compatibility (v1.1 additions only)

| Package | Compatible With | Notes |
|---------|-----------------|-------|
| `crossterm` 0.29.0 | `tokio` 1.44+ | EventStream works with tokio 1.44+ (verified dev dep constraint); no breaking changes from 0.28.1 |
| `crossterm` 0.29.0 | `x86_64-pc-windows-msvc` | Explicit supported target in docs.rs 0.29.0 |
| `ring` 0.17.14 | `x86_64-pc-windows-msvc` | Precompiled asm objects in crates.io package; no NASM needed |
| `quinn` 0.11.9 | `x86_64-pc-windows-msvc` | Pure Rust + socket2; confirmed buildable |
| `ssh-key` 0.6.7 `encryption` feature | `x86_64-pc-windows-msvc` | Pure Rust (bcrypt-pbkdf, AES-CTR); builds on all targets |

---

## Sources

- https://docs.rs/quinn/0.11.9/quinn/struct.Connection.html — complete method list verified; no migrate/set_path/network_path confirmed absent
- https://docs.rs/quinn/0.11.9/quinn/struct.Endpoint.html — rebind / rebind_abstract signatures verified
- https://docs.rs/quinn/0.11.9/quinn/struct.ServerConfig.html — migration() method signature and default=true verified
- https://docs.rs/quinn/0.11.9/quinn/struct.TransportConfig.html — keep_alive_interval, max_idle_timeout, mtu_discovery_config verified
- https://docs.rs/quinn/0.11.9/quinn/struct.ConnectionStats.html — PathStats fields (rtt, cwnd, lost_packets, current_mtu) verified
- https://docs.rs/quinn/0.11.9/quinn/struct.EndpointConfig.html — cid_generator; stable_id on Connection verified
- https://github.com/quinn-rs/quinn/blob/main/quinn/src/endpoint.rs — rebind_abstract implementation; ConnectionEvent::Rebind broadcast
- https://github.com/quinn-rs/quinn/blob/main/quinn-proto/src/connection/mod.rs — path/prev_path/path_counter fields; PATH_CHALLENGE/PATH_RESPONSE logic
- https://docs.rs/ssh-key/0.6.7/ssh_key/private/struct.PrivateKey.html — read_openssh_file, is_encrypted, decrypt, sign API verified
- https://github.com/RustCrypto/SSH/blob/master/ssh-key/Cargo.toml — encryption feature deps (bcrypt-pbkdf, AES-CTR, ChaCha20Poly1305) verified
- https://docs.rs/crossterm/0.29.0/crossterm/index.html — features (event-stream, events, windows), x86_64-pc-windows-msvc target support verified
- https://docs.rs/crossterm/latest/crossterm/event/struct.EventStream.html — event-stream feature flag, tokio compatibility, Windows targets verified
- https://github.com/crossterm-rs/crossterm/releases — 0.29.0 released April 5 2025 confirmed latest
- https://github.com/crossterm-rs/crossterm/issues/935 — use-dev-tty + event-stream incompatibility documented
- https://github.com/briansmith/ring/blob/main/BUILDING.md — x86_64-pc-windows-msvc precompiled asm objects (no NASM from crates.io) verified
- https://docs.rs/getrandom/latest/getrandom/ — getrandom 0.4.2; fill() API; already transitive via ring/uuid in lockfile
- Cargo.lock (nosh workspace, direct inspection) — uuid 1.23.1 v4 in nosh-server; crossterm 0.28.1 in nosh-client; ring 0.17.14; getrandom 0.2.17/0.3.4/0.4.2 all transitive

---
*Stack research for: nosh QUIC remote shell — v1.1 M3 Roaming + Windows Client*
*Researched: 2026-05-30*
