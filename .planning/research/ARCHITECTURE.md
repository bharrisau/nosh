# Architecture: nosh v1.4 (M7) WebTransport + Security Hardening Integration

**Domain:** Integration research for WebTransport reverse-proxy mode and inner SSH-key auth into an existing QUIC remote shell
**Researched:** 2026-06-13
**Confidence:** HIGH — based on reading actual source files and verified wtransport 0.7.x API documentation

---

## Summary

v1.4 adds WebTransport-over-HTTP/3 reverse-proxy support, an inner application-level SSH-key handshake, migration handover behind a QUIC-terminating proxy, and deferred security hardening items. The existing architecture is sound and the new features slot in at a well-defined seam.

The critical finding is that **no transport trait currently exists in the codebase** — all transport-facing code uses `quinn::Connection`, `quinn::SendStream`, `quinn::RecvStream`, and `quinn::Incoming` by concrete type. Introducing a transport abstraction trait is the first load-bearing step: it lets `run_session`, `run_reattach_session`, `send_burst`, the channel task API (`ChannelEvent`), and the client session pump run identically over either transport mode without duplication.

The second critical finding: `wtransport` 0.7.x exposes `with_custom_tls(TlsServerConfig / TlsClientConfig)` on its builder, which means **nosh's existing `rustls::ServerConfig` and `rustls::ClientConfig` constructions from `nosh-auth` can be reused verbatim** for the outer TLS layer. The outer TLS cert is still the host key / client key self-signed cert — but now the QUIC-terminating proxy will terminate it, so the outer TLS auth provides transport security only. The inner SSH-key handshake (a new application-level protocol on the first control stream) is what provides real mutual authentication end-to-end.

---

## Existing Architecture (Seams to Reuse)

### Workspace layout

```
crates/
  nosh-proto/          (wire types, codec, datagram, ALPN)
    src/messages.rs    (Message enum — append-only, postcard-stable discriminants)
    src/datagram.rs    (StateDiff, encode_datagram, decode_epoch_ack)
    src/codec.rs       (length-delimited postcard framing)
    src/transport.rs   (quinn TransportConfig builder — shared by both ends)
  nosh-auth/           (SSH-key verifiers, signer, SPKI pinning, cert mint)
    src/verifier.rs    (HostKeyVerifier, AuthorizedKeysVerifier — custom rustls traits)
    src/signer.rs      (RawEd25519Signer, AgentSigner, AgentSigningKey)
    src/keys.rs        (NoshPublicKey, SPKI extraction, known_hosts, authorized_keys)
  nosh-server/
    src/server.rs      (build_server_config, run_accept_loop, handle_connection,
                        run_session, run_reattach_session, build_state_diff, send_burst)
    src/channel.rs     (ChannelEvent, run_channel_task, run_scrollback_sender_task)
    src/registry.rs    (SequencedOutputBuffer, SessionRegistry, SessionSlot)
    src/session.rs     (Session, env sanitization, PTY spawn)
    src/terminal.rs    (TerminalState, scrollback, alt-screen model)
  nosh-client/
    src/client.rs      (build_client_config, connect, ClientIdentity)
    src/channel.rs     (client-side channel open/accept logic)
    src/screen.rs      (ClientScreen, apply, emit_diff)
    src/predictor.rs   (PredictionOverlay, epoch tracking)
```

### What every session currently uses (concrete types, no trait)

```
quinn::Connection     → send_datagram, datagram_send_buffer_space, accept_bi, open_bi,
                        max_datagram_size, remote_address, close, handshake_data
quinn::SendStream     → write_message (via nosh_proto::codec::write_message)
quinn::RecvStream     → read_message (via nosh_proto::codec::read_message)
quinn::Incoming       → incoming.await (resolves the TLS handshake)
channel::ChannelEvent → Stream(quinn::SendStream, quinn::RecvStream)
```

`ChannelEvent::Stream` carries concrete quinn stream types. `run_channel_task` and `run_scrollback_sender_task` also use quinn stream types directly. This is the entire surface area that must be abstracted.

### The reattach machinery (unchanged by this milestone)

`SequencedOutputBuffer` in `registry.rs` is a sequenced ring buffer of raw PTY bytes. On cold reattach, `run_reattach_session` replays buffered chunks from the buffer over the primary stream. The buffer is keyed by SSH identity (`NoshPublicKey`) and session ID. This is fully transport-agnostic: it does not care whether the underlying connection is native QUIC or WebTransport, as long as it has a reliable bidi stream to write on.

### The channel mux layer (the migration-handover hook)

After cold reattach completes (server sends `ResumeComplete`, replays PTY data, client acknowledges the sequence catch-up), the client re-opens secondary channels (`ChannelOpen` on the control stream, then a new QUIC bidi stream prefixed with a channel-id varint). This re-open path is the **migration-handover hook** for the proxy topology: after a new WebTransport session is established and the inner re-auth completes, the client re-opens channels exactly as it does after a cold reattach. No new protocol is needed.

---

## New Components and Where They Slot In

### 1. Transport abstraction trait (NEW — nosh-proto or nosh-server)

There is no transport trait today. Introducing one is the prerequisite for everything else. The trait must expose the operations that `run_session`, `run_reattach_session`, `build_state_diff`, `send_burst`, and the channel layer actually use.

```rust
// Proposed location: crates/nosh-proto/src/transport_trait.rs
// (re-exported from nosh-proto since both server and client need it)

pub trait NoshTransport: Send + Sync + 'static {
    /// Send an unreliable datagram. Non-blocking (mirrors quinn::Connection::send_datagram).
    fn send_datagram(&self, data: bytes::Bytes) -> Result<(), SendDatagramError>;
    /// How many bytes are available in the datagram send buffer.
    fn datagram_send_buffer_space(&self) -> usize;
    /// Maximum datagram payload size on the current path.
    fn max_datagram_size(&self) -> usize;
    /// Accept the next inbound bidirectional stream.
    async fn accept_bi(&self) -> Result<(Box<dyn NoshSendStream>, Box<dyn NoshRecvStream>), TransportError>;
    /// Open a new outbound bidirectional stream.
    async fn open_bi(&self) -> Result<(Box<dyn NoshSendStream>, Box<dyn NoshRecvStream>), TransportError>;
    /// Peer socket address (for logging / rate limiting).
    fn remote_address(&self) -> std::net::SocketAddr;
    /// Close the connection with an error code and reason bytes.
    fn close(&self, code: u32, reason: &[u8]);
    /// Wait until the connection is fully closed.
    async fn closed(&self) -> TransportError;
}

pub trait NoshSendStream: Send + 'static {
    async fn write_all(&mut self, data: &[u8]) -> anyhow::Result<()>;
    async fn finish(&mut self) -> anyhow::Result<()>;
}

pub trait NoshRecvStream: Send + 'static {
    async fn read_exact(&mut self, buf: &mut [u8]) -> anyhow::Result<()>;
    async fn read(&mut self, buf: &mut [u8]) -> anyhow::Result<usize>;
}
```

`quinn::Connection` implements `NoshTransport`. `wtransport::Connection` implements `NoshTransport` (using its `send_datagram`, `receive_datagram`, `accept_bi`, `open_bi` methods — the API is a near-1:1 match). `quinn::SendStream`/`RecvStream` and `wtransport::SendStream`/`RecvStream` both implement `NoshSendStream`/`NoshRecvStream` via thin wrappers.

`nosh_proto::codec::write_message` and `read_message` are already generic over `AsyncWrite` and `AsyncRead` respectively — they need to accept `Box<dyn NoshSendStream>` / `Box<dyn NoshRecvStream>` wrappers, or the trait can simply expose `write_message`/`read_message` at the stream level directly.

`ChannelEvent::Stream` must be updated from `(quinn::SendStream, quinn::RecvStream)` to `(Box<dyn NoshSendStream>, Box<dyn NoshRecvStream>)` — or defined as generic `<S: NoshSendStream, R: NoshRecvStream>`. Both `run_channel_task` and `run_scrollback_sender_task` then become generic over the stream types.

**What does NOT change:** The codec (postcard-framed `Message` enum), the datagram wire format (`StateDiff`, `encode_datagram`, `decode_datagram`), `SequencedOutputBuffer`, `SessionRegistry`, `TerminalState`, `PredictionOverlay`, or any terminal/session logic. The transport trait wraps the connection and stream I/O layer only.

**Placement decision:** Put the trait in `nosh-proto` so both `nosh-server` and `nosh-client` can depend on it without a circular dependency. A new file `crates/nosh-proto/src/transport_trait.rs`, re-exported from `nosh-proto::lib.rs`.

### 2. WebTransport transport implementations (NEW — nosh-server and nosh-client)

Two new `impl NoshTransport` blocks:

- **Server side:** `WtransportServerConnection(wtransport::Connection)` wrapping `wtransport::connection::Connection`. Lives in a new file `crates/nosh-server/src/wt_transport.rs` (or `crates/nosh-server/src/transport/mod.rs` alongside a `quinn_transport.rs`).
- **Client side:** `WtransportClientConnection(wtransport::Connection)` in a new file `crates/nosh-client/src/wt_transport.rs`.

`wtransport`'s `Connection` API (verified from docs.rs 0.7.1):
- `send_datagram<D: AsRef<[u8]>>(&self, payload: D) -> Result<(), SendDatagramError>` — matches quinn exactly
- `receive_datagram(&self) -> Result<Datagram, ConnectionError>` — matches quinn's read_datagram direction
- `open_bi(&self) -> Result<OpeningBiStream, ConnectionError>` — note: requires a second `.await` on `OpeningBiStream`
- `accept_bi(&self) -> Result<(SendStream, RecvStream), ConnectionError>` — direct tuple

The double-await on `open_bi` is the only API difference from quinn. The `NoshTransport::open_bi` implementation for the wtransport backend simply does both awaits internally before returning the stream pair.

**No new crate is needed.** `wtransport` is added as a workspace dependency and used in `nosh-server` and `nosh-client` behind a `feature = "webtransport"` Cargo feature flag (server and client both). Native QUIC mode remains the default; the feature activates the wtransport code paths.

### 3. WebTransport accept loop (MODIFIED — nosh-server/src/server.rs)

`run_accept_loop` currently takes a `quinn::Endpoint`. For WebTransport mode, a parallel function `run_wt_accept_loop` takes a `wtransport::Endpoint<Server>`. Both feed into `handle_connection`, which is refactored to take `Box<dyn NoshTransport>` instead of `quinn::Connection`.

`build_server_config` in `server.rs` is UNCHANGED — it produces a `quinn::ServerConfig`. For WebTransport mode, a new function `build_wt_server_config` in `wt_transport.rs` calls `wtransport::ServerConfig::builder().with_bind_address(addr).with_custom_tls(rustls_cfg).build()`, passing the SAME `rustls::ServerConfig` that `build_server_config` constructs (reusing `nosh_auth::AuthorizedKeysVerifier` and `nosh_auth::NoshServerCertResolver`). The outer TLS mutual auth thus works identically for both modes — but in WebTransport mode the outer auth is terminated at the proxy, making the inner handshake (below) mandatory.

### 4. Inner SSH-key handshake (NEW — nosh-proto + nosh-server + nosh-client)

This is the most significant design element of v1.4.

**Why it is needed:** Behind an HTTP/3 reverse proxy, the proxy terminates QUIC and TLS. The outer TLS handshake (which currently carries the mutual SSH-key auth) ends at the proxy. The nosh server sees a WebTransport session that the proxy has already authenticated with the server's TLS cert — but the server has no way to verify the client's SSH key, and the client has no way to verify the server's host key, because both verifications happened in the TLS layer the proxy terminated.

**Design — application-level challenge-response on the control stream:**

The inner handshake runs immediately after the WebTransport session is established, before any `SessionOpen` or `Reattach` frame is processed. It is an application-level mutual challenge-response using SSH key signing:

```
Step 1 — Server challenges client:
  Server generates 32 random challenge bytes (CSPRNG).
  Server sends: InnerAuthChallenge { server_nonce: [u8; 32], server_spki: Vec<u8> }
  (server_spki is the server's SSH public key in SPKI/DER form — the client
   uses this to verify the host key against known_hosts / TOFU)

Step 2 — Client verifies server and responds:
  Client extracts the server's public key from server_spki.
  Client checks server_spki against known_hosts (same HostKeyVerifier logic,
  called directly rather than via rustls — this is the inner TOFU / key-pin check).
  Client generates 32 random challenge bytes (CSPRNG).
  Client signs: SHA-256("nosh-inner-v1" || server_nonce || client_nonce || server_spki)
    using its SSH key (via RawEd25519Signer — agent or file).
  Client sends: InnerAuthResponse {
    client_nonce: [u8; 32],
    client_spki: Vec<u8>,
    client_sig: [u8; 64],
  }

Step 3 — Server verifies client and completes:
  Server verifies client_sig over (server_nonce, client_nonce, server_spki) using
  the public key extracted from client_spki.
  Server checks client_spki against authorized_keys (same AuthorizedKeysVerifier
  logic, called directly on the NoshPublicKey).
  Server generates its own signature over:
    SHA-256("nosh-inner-v1-server" || server_nonce || client_nonce || client_spki)
  Server sends: InnerAuthComplete { server_sig: [u8; 64] }

Step 4 — Client verifies server signature:
  Client verifies server_sig using server_spki. If this succeeds, mutual auth
  is complete and the session proceeds.
```

The signature covers both nonces to prevent replay across sessions (the server nonce is CSPRNG-fresh per connection; the client nonce prevents the server from replaying a previous client response). The message tag `"nosh-inner-v1"` prevents cross-protocol confusion attacks.

This handshake reuses:
- `nosh_auth::RawEd25519Signer` / `AgentSigner` — unchanged; client just calls `signer.sign(challenge_bytes)` instead of routing through rustls `CertificateVerify`
- `nosh_auth::keys::lookup_known_host` / `record_known_host` — unchanged; client calls them directly for server TOFU
- `nosh_auth::keys::extract_spki_from_bytes` (or a new peer of `extract_spki_from_cert`) — small addition to `nosh-auth/src/keys.rs`
- `nosh_auth::AuthorizedKeysVerifier`'s authorized-key lookup logic — extracted to a standalone `check_authorized(spki, authorized)` function in `keys.rs` that both the TLS verifier and the inner handshake can call

**New `Message` variants (append-only, after current last discriminant 17):**

```rust
// Append after ScrollbackCredit (discriminant 17):
InnerAuthChallenge { server_nonce: [u8; 32], server_spki: Vec<u8> },      // 18
InnerAuthResponse  { client_nonce: [u8; 32], client_spki: Vec<u8>,
                     client_sig: [u8; 64] },                              // 19
InnerAuthComplete  { server_sig: [u8; 64] },                              // 20
InnerAuthFail,     // FIELDLESS — no-oracle invariant; same reason as ReattachErr  // 21
```

`InnerAuthFail` is fieldless like `ReattachErr`: it reveals nothing about why the handshake failed (wrong key vs unknown key vs bad signature all map to the same response). Callers log only the identity fingerprint.

**New module: `nosh-server/src/inner_auth.rs` and `nosh-client/src/inner_auth.rs`**

Server side: `run_inner_auth_server(transport: &dyn NoshTransport, control_send: &mut dyn NoshSendStream, control_recv: &mut dyn NoshRecvStream, authorized: &[NoshPublicKey], host_signer: &dyn RawEd25519Signer) -> Result<NoshPublicKey, InnerAuthError>`

Client side: `run_inner_auth_client(transport: &dyn NoshTransport, control_send: &mut dyn NoshSendStream, control_recv: &mut dyn NoshRecvStream, identity: &ClientIdentity, known_hosts: &Path, host: &str) -> Result<(), InnerAuthError>`

In native QUIC mode, `handle_connection` skips `run_inner_auth_*` entirely — the TLS handshake already did mutual auth and `extract_peer_identity` reads the identity from the cert. In WebTransport mode, `handle_connection_wt` calls `run_inner_auth_server` immediately after the session is established, before reading `SessionOpen` or `Reattach`. The returned `NoshPublicKey` feeds into the same `registry` and `run_session` path.

### 5. Migration handover (REUSE — no new protocol)

In native QUIC mode, roaming is handled by `server_config.migration(true)` — the QUIC connection ID continues across IP changes with no application-layer involvement.

In WebTransport mode, the proxy terminates QUIC, so transport-layer migration is unavailable. The client detects connection loss (write error on any stream, or datagram timeout) and initiates a **new WebTransport session**. This maps cleanly to the existing 1-RTT cold-reattach path:

```
1. Client detects WebTransport session loss.
2. Client opens new WebTransport session to the same URL.
3. Inner SSH-key handshake runs (steps 1–4 above). This re-authenticates both sides.
4. Client sends Reattach { token, last_acked_seq } on the new control stream.
5. Server looks up the session in SessionRegistry by the verified identity.
6. Server sends ReattachOk { new_token, replaying_from_seq, truncated }.
7. Server replays from SequencedOutputBuffer (PTY output the client missed).
8. Client sends ResumeComplete (or its equivalent Ack chain).
9. Client re-opens secondary channels (Scrollback, etc.) via ChannelOpen.
```

Steps 4–9 are UNCHANGED from the native-QUIC cold-reattach path. `run_reattach_session` already handles all of this. The only change is that step 3 (inner auth) is inserted before step 4, and the outer function signature accepts `Box<dyn NoshTransport>` instead of `quinn::Connection`.

The reattach token (`[u8; 16]`, bound to the SSH identity) is the same token used in native QUIC mode. There is no separate WebTransport reattach token. Inner auth serves as the equivalent of the TLS re-run in native QUIC cold reattach (both prove identity before the session registry lookup).

**Datagram behaviour in WebTransport mode:** WebTransport datagrams are unreliable and unordered — semantically identical to QUIC RFC 9221 datagrams. `send_datagram` / `receive_datagram` on a `wtransport::Connection` map 1:1 to quinn's API. `send_burst` and `build_state_diff` are unchanged; they call `NoshTransport::send_datagram` which dispatches to whichever backend is active.

### 6. Security hardening items (MODIFIED — multiple crates)

**SEC-01 (threat-model doc):** Documentation only. New file `docs/SECURITY.md` expanding `docs/999.1-SECURITY.md` and `docs/999.7-SECURITY.md` to cover the WebTransport topology threat surface (proxy-terminates-outer-TLS, inner-auth-mandatory, proxy trust model, IP metadata leakage).

**SEC-02 (interactive TOFU prompt):** MODIFIED in `nosh-client/src/client.rs` and the new `inner_auth.rs`. Currently `HostKeyVerifier::verify_server_cert` writes to `known_hosts` silently on TOFU (the existing code at `verifier.rs` line 82–84). For WebTransport mode the TOFU check happens inside `run_inner_auth_client`, which runs at the application level — it is straightforward to prompt the user (print fingerprint to stderr, read `yes/no` from stdin) before calling `record_known_host`. For native QUIC mode, the TOFU prompt must be injected into `HostKeyVerifier`, which is trickier because it runs inside the TLS handshake thread. The cleanest approach: add a `ToFuPolicy` enum (`Silent` / `Interactive`) to `HostKeyVerifier::new`; `Interactive` prints to stderr and reads from a channel, blocking until the user answers. The blocking call inside the TLS verifier is acceptable (the connection is waiting for auth anyway).

**SEC-04 / 999.2 (client trust-boundary hardening):** MODIFIED in `nosh-client/src`. Harden the client against a malicious server sending oversized or malformed messages. Already partially addressed by `MAX_RUNS` in datagram decoding and the OSC OOM bound (SEC-03, v1.3). The remaining items: cap `PtyData` payload size on receive, validate `ChannelAccept`/`ChannelReject` channel IDs are within expected range, reject unexpected `ChannelOpen` from server when client has not requested a server-initiated channel.

**999.7 (OSC OOM bound re-check):** Investigation in `nosh-server/src/terminal.rs`. The Phase-16 mitigation reasoning was found incorrect; the actual fix must be verified by reading the current `osc_dispatch` accumulation path.

---

## Component Responsibility Map

| Component | Status | Crate | What Changes |
|-----------|--------|-------|--------------|
| `NoshTransport` trait | NEW | `nosh-proto` | New file `transport_trait.rs`; `NoshSendStream`, `NoshRecvStream` |
| `QuinnTransport` wrapper | NEW | `nosh-server`, `nosh-client` | Thin `impl NoshTransport for quinn::Connection` wrappers |
| `WtransportServerConnection` | NEW | `nosh-server` | `impl NoshTransport for wtransport::Connection` |
| `WtransportClientConnection` | NEW | `nosh-client` | `impl NoshTransport for wtransport::Connection` |
| `build_wt_server_config` | NEW | `nosh-server` | Calls existing `rustls::ServerConfig` path, wraps for wtransport |
| `build_wt_client_config` | NEW | `nosh-client` | Calls existing `rustls::ClientConfig` path, wraps for wtransport |
| `run_wt_accept_loop` | NEW | `nosh-server` | Parallel to `run_accept_loop`; accepts `wtransport::Incoming` |
| `inner_auth.rs` (server) | NEW | `nosh-server` | Challenge-response handshake; reuses `RawEd25519Signer` |
| `inner_auth.rs` (client) | NEW | `nosh-client` | Client side of above; reuses `AgentSigner`, known_hosts logic |
| `Message` enum | MODIFIED | `nosh-proto` | Append `InnerAuthChallenge/Response/Complete/Fail` (discriminants 18–21) |
| `ChannelEvent` enum | MODIFIED | `nosh-server` | `Stream` variant changes to boxed trait streams |
| `handle_connection` | MODIFIED | `nosh-server` | Accepts `Box<dyn NoshTransport>`; routes to inner auth or not |
| `run_session` | MODIFIED | `nosh-server` | Generic over `NoshTransport` instead of `quinn::Connection` |
| `run_reattach_session` | MODIFIED | `nosh-server` | Same generics change |
| `run_channel_task` | MODIFIED | `nosh-server` | Stream types become boxed trait streams |
| `build_server_config` | UNCHANGED | `nosh-server` | Still returns `quinn::ServerConfig` for native mode |
| `AuthorizedKeysVerifier` | MODIFIED (minor) | `nosh-auth` | Extract `check_authorized_key(spki, &[NoshPublicKey]) -> bool` |
| `HostKeyVerifier` | MODIFIED | `nosh-auth` | Add `ToFuPolicy` for interactive prompt (SEC-02) |
| `keys.rs` | MODIFIED (minor) | `nosh-auth` | Add `extract_spki_from_bytes` for inner auth use |
| `SequencedOutputBuffer` | UNCHANGED | `nosh-server` | No change |
| `SessionRegistry` / `SessionSlot` | UNCHANGED | `nosh-server` | No change |
| `TerminalState` | UNCHANGED | `nosh-server` | No change (999.7 is an investigation, not a design change) |
| `build_state_diff` / `send_burst` | UNCHANGED | `nosh-server` | Call `NoshTransport::send_datagram` via the trait |
| `ClientScreen`, `PredictionOverlay` | UNCHANGED | `nosh-client` | No change |
| `ClientIdentity` | UNCHANGED | `nosh-client` | Reused as-is by inner auth |
| `nosh-proto/src/transport.rs` | UNCHANGED | `nosh-proto` | Still produces `quinn::TransportConfig`; only used in native mode |

---

## Data Flow Diagrams

### Native QUIC mode (existing — unchanged)

```
Client                                  Server
  |                                       |
  |--- QUIC+TLS handshake (UDP/443) ----->|
  |    (mutual SSH-key auth inside TLS)   |
  |                                       |
  |--- [bidi stream 0] SessionOpen ------>|
  |<-- SessionOpened { token } -----------|
  |                                       |
  |<-- [datagrams] StateDiff -------------|  (terminal state, lossy)
  |--- [datagrams] EpochAck ------------->|
  |--- [stream 0] PtyData (keystrokes) -->|
  |<-- [stream 0] PtyData (output) -------|
  |                                       |
  |--- [stream 0] ChannelOpen(Scrollback)->|
  |<-- ChannelAccept ----------------------|
  |--- [stream N] channel-id varint ------>|  (new bidi stream)
  |<--> [stream N] scrollback pages ------>|
```

### WebTransport mode (new — behind HTTP/3 proxy)

```
Client              HTTP/3 Proxy              Server
  |                      |                      |
  |-- QUIC+TLS (443) --->|                      |
  |   (proxy cert only)  |                      |
  |                      |-- HTTP/3 upstream -->|
  |                      |   (WebTransport)     |
  |<===== WebTransport tunnel (HTTP/3 CONNECT) =================>|
  |                                                              |
  |  [inner handshake — on bidi stream 0, before SessionOpen]   |
  |<-- InnerAuthChallenge { server_nonce, server_spki } ---------|
  |    (client checks server_spki against known_hosts)           |
  |--- InnerAuthResponse { client_nonce, client_spki, sig } ---->|
  |    (server checks sig, checks client_spki vs authorized_keys)|
  |<-- InnerAuthComplete { server_sig } -------------------------|
  |    (client verifies server_sig)                              |
  |                                                              |
  |--- SessionOpen (or Reattach) -------------------------------->|
  |<-- SessionOpened / ReattachOk -------------------------------|
  |                                                              |
  |<-- [datagrams via WebTransport] StateDiff -------------------|
  |--- [bidi stream] PtyData / ChannelOpen etc. --------------->|
```

### Migration handover in WebTransport mode

```
Client                                             Server
  |                                                  |
  | [WebTransport session A — established, running]  |
  |  ... network change (IP change, NAT timeout) ... |
  |  [Session A transport lost]                      |
  |                                                  |
  |--- new WebTransport session B (UDP/443) -------->|
  |<-> inner SSH-key handshake (steps 1–4) ----------|
  |    [identity verified — same NoshPublicKey]      |
  |                                                  |
  |--- Reattach { token, last_acked_seq } ---------->|
  |    [registry lookup by identity]                 |
  |<-- ReattachOk { new_token, replaying_from_seq } -|
  |<-- PtyData (replay from SequencedOutputBuffer) --|
  |                                                  |
  |--- ChannelOpen(Scrollback) -------------------->|  (re-open channels)
  |<-- ChannelAccept --------------------------------|
  |<-- [datagrams] StateDiff resumes ---------------|
```

---

## Architecture Patterns

### Pattern 1: Thin trait wrapper, not redesign

The `NoshTransport` trait wraps the existing quinn and wtransport APIs at the exact points they are used: `send_datagram`, `datagram_send_buffer_space`, `max_datagram_size`, `accept_bi`, `open_bi`, `remote_address`, `close`. It does not attempt to abstract QUIC semantics (connection IDs, 0-RTT, migration) because WebTransport mode replaces those with the inner-auth + cold-reattach path. The trait is narrow by design.

Trade-off: `Box<dyn NoshTransport>` incurs a vtable dispatch on every datagram send and stream open. For datagrams this is negligible (one dispatch per tick, not per byte). For stream I/O the codec path is already async I/O bound. The ergonomic benefit (zero code duplication across 400+ lines of session pump) far outweighs this.

### Pattern 2: Inner auth reuses existing crypto, not a new dependency

The inner handshake signs raw bytes with `RawEd25519Signer::sign`. The signer is already the abstraction that works for both `AgentSigner` (Unix, hardware keys) and `FileSigner` (Windows, on-disk key). No new crypto crate is needed. The challenge byte framing uses `SHA-256` via the ring provider already in the dependency tree (accessed via `ring::digest::digest`).

Trade-off: the inner handshake does not support ECDSA or RSA in this milestone (Ed25519 only, same as the existing TLS path). This is a known limitation documented in the security model.

### Pattern 3: WebTransport mode is a feature flag, not a separate binary

Both `nosh-server` and `nosh-client` expose `--webtransport` flags and `--wt-url` (client) / `--wt-bind` (server) arguments when the `webtransport` Cargo feature is active. The `main.rs` for each binary dispatches to either the native QUIC accept loop or the WebTransport accept loop based on the flag. The session pump, channel logic, and terminal model are completely shared.

---

## Anti-Patterns to Avoid

### Anti-Pattern 1: Skipping the inner auth in WebTransport mode

**What happens:** Relying on the proxy to perform client authentication (e.g. mTLS at the proxy layer). The server then has no way to verify which SSH key the client holds, breaking the `authorized_keys` gate and the identity-scoped session registry.

**Why wrong:** The threat model requires end-to-end SSH-key mutual auth. The proxy terminates the outer TLS — it does not and cannot verify the nosh `authorized_keys` on behalf of the server. Without inner auth, any client that can reach the WebTransport endpoint is authenticated.

**Instead:** Always run `run_inner_auth_server` on every new WebTransport session before processing any `SessionOpen` or `Reattach` frame.

### Anti-Pattern 2: Making the inner handshake fieldful on failure

**What happens:** `InnerAuthFail` carries a reason code (`UnknownKey`, `BadSignature`, `Expired`, etc.).

**Why wrong:** This creates a session-existence oracle and a key-enumeration oracle — an attacker can determine whether a given public key is in `authorized_keys` by attempting the handshake and reading the failure code. This is the same reason `ReattachErr` is fieldless.

**Instead:** `InnerAuthFail` must remain fieldless and opaque. Log the reason server-side only, never in the wire message.

### Anti-Pattern 3: Routing datagrams over streams in WebTransport mode

**What happens:** Because WebTransport datagrams have no delivery guarantee, a developer might be tempted to send `StateDiff` over a reliable stream instead, to avoid loss.

**Why wrong:** This defeats the entire point of the datagram channel — latest-state-wins, loss-tolerant. Reliable delivery of every diff introduces head-of-line blocking for the terminal state stream and makes the client's confirmed grid lag behind the server's output under loss. The datagram loss tolerance is the feature, not a bug to work around.

**Instead:** Send `StateDiff` as WebTransport datagrams exactly as in native QUIC mode. Accept that some diffs are lost; the epoch-ack + `last_acked_snapshot` model self-corrects. If the path truly cannot support datagrams, that is a configuration error (proxy blocking datagrams), not an application concern.

### Anti-Pattern 4: Putting `wtransport` behind the transport trait incorrectly by calling `build_state_diff` inside the trait

**What happens:** Trying to make the transport trait do more than I/O — passing session state or terminal state through it.

**Why wrong:** The existing `build_state_diff` and `send_burst` are pure functions that take a `SessionSlot` reference and a connection reference. They must stay pure. The trait is only the I/O boundary.

**Instead:** The trait exposes only the connection-level I/O operations. Session state, terminal state, and buffer management stay in `nosh-server`'s existing modules.

---

## Dependency-Ordered Build Sequence

The four main work areas have the following dependencies:

```
Transport trait (nosh-proto + Quinn wrappers)
    ↓
WebTransport session accept + outer TLS wiring
    ↓
Inner SSH-key handshake (nosh-auth refactor + new inner_auth modules)
    ↓
Migration handover (run_reattach_session on NoshTransport + re-open channels)
    ↓
Security hardening pass (SEC-01 doc, SEC-02 TOFU prompt, SEC-04 client hardening, 999.7)
    ↓
Interactive UAT (carried-forward backlog + new M7 end-to-end path)
```

### Phase A — Transport abstraction seam

**Goal:** Introduce `NoshTransport` / `NoshSendStream` / `NoshRecvStream` traits and the quinn wrapper impls. Refactor `run_session`, `run_reattach_session`, `handle_connection`, `run_channel_task`, `run_scrollback_sender_task` to use the trait. All existing tests must still pass unchanged (the quinn wrapper is a pass-through).

**Files new:** `crates/nosh-proto/src/transport_trait.rs`, `crates/nosh-server/src/quinn_transport.rs` (or inline in `server.rs`), `crates/nosh-client/src/quinn_transport.rs`

**Files modified:** `nosh-proto/src/lib.rs` (re-export), `nosh-server/src/server.rs` (signatures), `nosh-server/src/channel.rs` (ChannelEvent::Stream types), `nosh-client/src/client.rs` (connect returns `Box<dyn NoshTransport>`)

**Gate:** All existing integration tests pass. `cargo test --workspace` green. No functional change.

### Phase B — WebTransport endpoint + outer TLS wiring

**Goal:** Add `wtransport` to workspace dependencies (feature-gated). Implement `WtransportServerConnection` and `WtransportClientConnection` as `impl NoshTransport`. Add `build_wt_server_config`, `run_wt_accept_loop`, `build_wt_client_config`, and `connect_wt`. Wire `--webtransport` CLI flag in both binaries.

**Files new:** `crates/nosh-server/src/wt_transport.rs`, `crates/nosh-client/src/wt_transport.rs`

**Files modified:** `Cargo.toml` (workspace dep: `wtransport = { version = "0.7", optional = true }`), `crates/nosh-server/src/server.rs` (new accept loop), `crates/nosh-server/src/main.rs` (CLI flag), `crates/nosh-client/src/main.rs` (CLI flag)

**Gate:** A WebTransport session can be established client→server. The inner session pump runs over WebTransport streams. No inner auth yet — the test uses a stub `skip_inner_auth` mode gated to `#[cfg(test)]` only. The `--webtransport` flag without inner auth should REJECT connections (non-test builds must not have the stub path).

### Phase C — Inner SSH-key handshake

**Goal:** Implement `InnerAuthChallenge / Response / Complete / Fail` message variants (appended after discriminant 17). Implement `run_inner_auth_server` and `run_inner_auth_client`. Refactor `nosh-auth/src/keys.rs` to expose `check_authorized_key` and `extract_spki_from_bytes`. Wire inner auth into `handle_connection_wt` (server) and the WebTransport connect path (client).

**Files new:** `crates/nosh-server/src/inner_auth.rs`, `crates/nosh-client/src/inner_auth.rs`

**Files modified:** `nosh-proto/src/messages.rs` (4 new variants, append-only), `nosh-auth/src/keys.rs` (extract helpers), `nosh-server/src/server.rs` (call inner auth in WT path), `nosh-client/src/client.rs` (call inner auth in WT path)

**Gate:** End-to-end WebTransport session with full inner SSH-key mutual auth. Unauthorised client (key not in `authorized_keys`) is rejected with `InnerAuthFail`. Wrong server key (known_hosts mismatch) closes the client connection with an error. Test for: (1) successful inner auth with valid keys, (2) rejection with unknown client key, (3) rejection with mismatched server key, (4) `InnerAuthFail` is fieldless in all failure paths.

### Phase D — Migration handover (cold reattach over WebTransport)

**Goal:** Prove that `run_reattach_session` works over `Box<dyn NoshTransport>` (Phase A already did the refactor). The new work is: the client detects WebTransport session loss, re-connects, re-runs inner auth, then sends `Reattach`. Test for: byte-exact replay from `SequencedOutputBuffer`, token rotation, channel re-open after reattach.

**Files modified:** `nosh-client/src/client.rs` (detect loss, reconnect loop, inner auth then reattach), `nosh-server/src/server.rs` (no change — `run_reattach_session` already handles this)

**Gate:** A WebTransport session that is forcibly closed (simulate network change by closing the wtransport connection) causes the client to reconnect, re-auth, and resume with byte-exact replay. The `ReattachOk.replaying_from_seq` matches the client's `last_acked_seq`. Channels re-opened after `ResumeComplete`.

### Phase E — Security hardening + documentation

**Goal:** SEC-01 threat-model doc. SEC-02 interactive TOFU prompt. SEC-04 client hardening (PtyData cap, channel ID validation). 999.7 OSC OOM investigation + fix if the bound is absent.

**Files new:** `docs/SECURITY.md` (or extend `docs/999.1-SECURITY.md`)

**Files modified:** `nosh-auth/src/verifier.rs` (ToFuPolicy), `nosh-auth/src/keys.rs` (ToFuPolicy threading), `nosh-client/src/inner_auth.rs` (TOFU prompt in WT path), `nosh-client/src/client.rs` (TOFU prompt in native path), `nosh-server/src/terminal.rs` (OSC OOM fix if needed), `nosh-client/src/client.rs` (PtyData recv cap, channel ID guard)

**Gate:** (1) Fresh `known_hosts` (first contact) triggers an interactive fingerprint prompt on both native and WT modes. (2) A server sending a 100 MB `PtyData` frame is rejected by the client before allocation. (3) `InnerAuthFail` and `ReattachErr` have no fields after any code change. (4) OSC OOM: either confirm the existing bound is correct or add a test that caps accumulation before vte.

### Phase F — Interactive UAT

**Goal:** A guided step-by-step validation session: carried-forward backlog (Phase 19 Windows alt-screen re-test, 999.3 rendering pack, 999.4 Windows predictive-echo, green Windows CI) and the new M7 remote-access path (WebTransport end-to-end with a real HTTP/3 reverse proxy, TOFU prompt on first contact, roaming via cold reattach behind proxy).

**No new code.** This phase is a human-driven interactive test, not automated. Each item is confirmed one at a time.

---

## Critical Invariants That Must Not Break

| Invariant | Where enforced | Risk in v1.4 |
|-----------|---------------|-------------|
| `Message` discriminant ordering — append-only | `nosh-proto/src/messages.rs`, codec test | Phase C adds 4 variants; must append after discriminant 17 (ScrollbackCredit) |
| `ReattachErr` is fieldless (no-oracle) | `messages.rs` comment + test | `InnerAuthFail` must also be fieldless — enforce the same invariant by test |
| Env sanitization on every shell/exec open | `session.rs` env whitelist | No change to session.rs in v1.4; invariant preserved |
| `SSH_AUTH_SOCK` never forwarded via env | `session.rs` ENV_DENY_DOC | No change; invariant preserved |
| Inner auth mandatory in WebTransport mode | `server.rs` wt connection handler | A `#[cfg(not(test))]` guard must prevent the `skip_inner_auth` stub from reaching production builds |
| `InnerAuthFail` is fieldless in all failure paths | New test in `inner_auth.rs` | Server-side logging of failure reason is fine; wire message must always be the fieldless variant |
| Token bytes never logged | `messages.rs` `variant_name()`, callers | InnerAuth variants carry signatures (not tokens), but the no-logging discipline extends to `client_sig` and `server_sig` — log fingerprints, not raw bytes |
| Pre-auth DoS cap applies to WebTransport mode too | `run_wt_accept_loop` | Must replicate the `Semaphore`-based pre-auth cap from `run_accept_loop`; WebTransport sessions do not skip it |
| `SequencedOutputBuffer` not replayed on scrollback channel | `registry.rs` — scrollback uses separate query, not the buffer | No change; invariant preserved |
| One epoch per tick (burst datagrams share an epoch) | `server.rs send_burst` | The NoshTransport trait wraps `send_datagram` — the burst loop logic and epoch management are unchanged |

---

## Sources

- `/home/bharris/github.com/bharrisau/nosh/crates/nosh-proto/src/messages.rs` — current 18 Message variants (SessionOpen through ScrollbackCredit, discriminants 0–17), ChannelType enum, append-only invariant
- `/home/bharris/github.com/bharrisau/nosh/crates/nosh-server/src/server.rs` — `build_server_config`, `handle_connection`, `run_session`, `run_reattach_session`, `send_burst`, `build_state_diff`; all use `quinn::Connection` by concrete type (no trait exists)
- `/home/bharris/github.com/bharrisau/nosh/crates/nosh-server/src/channel.rs` — `ChannelEvent::Stream(quinn::SendStream, quinn::RecvStream)` — the surface area that must be abstractd
- `/home/bharris/github.com/bharrisau/nosh/crates/nosh-client/src/client.rs` — `build_client_config`, `make_endpoint`, `connect`; quinn concrete types throughout
- `/home/bharris/github.com/bharrisau/nosh/crates/nosh-auth/src/verifier.rs` — `HostKeyVerifier`, `AuthorizedKeysVerifier`; the logic to extract into `check_authorized_key`
- `/home/bharris/github.com/bharrisau/nosh/crates/nosh-server/src/registry.rs` — `SequencedOutputBuffer`, `SessionRegistry`; transport-agnostic, no change needed
- `/home/bharris/github.com/bharrisau/nosh/.planning/PROJECT.md` — v1.4 scope: WT-*, SEC-01/02, SEC-04, 999.7, interactive UAT
- `/home/bharris/github.com/bharrisau/nosh/.planning/MILESTONES.md` — v1.3 delivered: channel mux (Phase 21), scrollback (Phase 22), ChannelEvent seam
- https://docs.rs/wtransport/latest/wtransport/connection/struct.Connection.html — `send_datagram`, `receive_datagram`, `open_bi` (double-await), `accept_bi` API confirmed; version 0.7.1
- https://github.com/BiagioFesta/wtransport — `with_custom_tls(TlsServerConfig/TlsClientConfig)` confirmed on ServerConfig and ClientConfig builders; reuses existing rustls configs
- https://docs.rs/wtransport/latest/wtransport/endpoint/struct.Endpoint.html — `Endpoint::server()`, `Endpoint::client()`, `accept()`, `connect()` API
