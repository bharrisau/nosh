# Stack Research

**Domain:** QUIC-based roaming remote shell (Rust) — M0–M2 architecture-validation spike
**Researched:** 2026-05-29
**Confidence:** HIGH (all crate versions verified against docs.rs/crates.io live data)

---

## Recommended Stack

### Core Technologies

| Technology | Version | Purpose | Why Recommended |
|------------|---------|---------|-----------------|
| `quinn` | 0.11.9 | QUIC transport (client + server) | Only mature async/tokio QUIC crate in Rust; RFC 9221 datagram support built in; `rustls` backend is first-class; active maintenance (quinn-rs org) |
| `rustls` | 0.23.40 | TLS 1.3 (used via quinn's crypto layer) | RFC 7250 raw public key support shipped in 0.23.16 (Oct 2024); `AlwaysResolvesServerRawPublicKeys` and `AlwaysResolvesClientRawPublicKeys` ready to use; custom `SigningKey`/`Signer` traits enable ssh-agent delegation |
| `tokio` | 1.52.x (LTS until Mar 2027) | Async runtime | quinn's default and only production-quality async I/O for QUIC; 1.47.x and 1.51.x are current LTS lines |
| `portable-pty` | 0.9.0 | Cross-platform PTY (Linux + future Windows) | wezterm's battle-tested PTY abstraction; trait-based so ConPTY can drop in for M6; `native_pty_system()` → `openpty()` → `spawn_command()` pattern is clean |
| `ssh-key` | 0.6.7 | Parse OpenSSH public/private keys, authorized_keys, known_hosts | RustCrypto project; pure Rust; parses all common key types (Ed25519, ECDSA, RSA); `authorized_keys` and `known_hosts` module support built in; `ed25519` feature enables keygen/sign/verify |
| `ssh-agent-client-rs` | 1.1.2 | ssh-agent protocol client | Provides `Client::sign(&mut self, key: impl Into<Identity>, data: &[u8]) -> Result<Signature>`; connects via Unix socket; synchronous API suitable for use inside blocking `rustls::sign::Signer::sign()` |

### Supporting Libraries

| Library | Version | Purpose | When to Use |
|---------|---------|---------|-------------|
| `ed25519-dalek` | 2.2.0 | Ed25519 public key material (verify agent signatures in tests; derive SPKI) | Use stable 2.2.0; 3.0.0-pre.7 exists (2026-05-06) but is pre-release — avoid in spike |
| `rcgen` | 0.14.8 | Generate ephemeral self-signed X.509 certs (cert-pinning fallback) | Use if RPK config is too involved for initial spike — see RPK vs Cert-Pinning section below |
| `vte` | 0.15.0 | VT/ANSI parser for server-side terminal state model | Alacritty project; implements Paul Williams state machine; use for M2 terminal model; `Perform` trait is the extension point |
| `bytes` | 1.x | Zero-copy byte buffer (already a quinn transitive dep) | Frame serialisation / datagram payload handling |
| `tracing` | 0.1.x | Structured async logging (already a quinn transitive dep) | Prefer over `log` for async spans |

### Development Tools

| Tool | Purpose | Notes |
|------|---------|-------|
| `cargo nextest` | Faster test runner | Parallel test execution; useful for integration tests that bind ports |
| `quinn` `qlog` feature | QUIC event logging | Enable for transport-level debugging during spike; `TransportConfig` events |
| OpenSSL or ssh-keygen | Generate test keys | Generate Ed25519 test keys for authorized_keys/known_hosts fixtures; no Rust dep needed |

---

## Cargo.toml Sketch

```toml
[dependencies]
# Transport
quinn = { version = "0.11", default-features = false, features = ["runtime-tokio", "rustls-ring"] }

# TLS / Crypto
rustls = { version = "0.23", features = [] }  # pulled transitively through quinn; pin here for direct use

# SSH key material
ssh-key = { version = "0.6", features = ["ed25519", "std"] }
ssh-agent-client-rs = "1.1"
ed25519-dalek = "2.2"

# PTY
portable-pty = "0.9"

# Async runtime
tokio = { version = "1", features = ["full"] }

# Terminal model (M2)
vte = "0.15"

# Cert generation fallback
rcgen = "0.14"

[dev-dependencies]
# Nothing extra needed for spike tests
```

**Note on quinn features:** `rustls-ring` bundles `ring` as the crypto backend; `rustls-aws-lc-rs` is the other option. `ring` is simpler to build on Linux (no system library dependency). Use `aws-lc-rs` later if FIPS is ever required.

---

## Critical Design Decisions

### RPK vs Self-Signed-Cert Key Pinning

**Recommendation: Start with cert-pinning (M1), upgrade to RPK in a follow-up.**

**Rationale:**

rustls 0.23.16+ ships `AlwaysResolvesServerRawPublicKeys` and `AlwaysResolvesClientRawPublicKeys`, so RPK is technically available. However, there is a known compliance caveat: GitHub issue #2257 ("UnsolicitedCertificateTypeExtension is not RFC 7250 compliant") was open as of late 2024, indicating the initial 0.23.16 implementation had a wire-format compliance gap. The current 0.23.40 state of that issue is not fully verified.

The cert-pinning path (`rustls::client::danger::ServerCertVerifier` / `rustls::server::danger::ClientCertVerifier` with custom `verify_server_cert()`/`verify_client_cert()` that extract and compare the SubjectPublicKeyInfo rather than walking a PKI chain) is battle-tested, fully supported today, and interoperable with quinn's `dangerous_configuration` feature. It is also what the INIT.md brief calls the "portable fallback" and explicitly accepts as the first implementation path.

**Cert-pinning implementation pattern:**

```rust
// Client side: pin the server's host key
struct HostKeyVerifier {
    expected_spki: Vec<u8>, // from known_hosts or TOFU first-contact
    crypto_provider: Arc<rustls::crypto::CryptoProvider>,
}

impl rustls::client::danger::ServerCertVerifier for HostKeyVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        // Parse cert, extract SPKI, compare to pinned key
        // Return Ok(ServerCertVerified::assertion()) on match
    }
    fn verify_tls12_signature(...) { ... }
    fn verify_tls13_signature(...) { ... }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> { ... }
}

// Server side: pin the client key against authorized_keys
struct AuthorizedKeysVerifier {
    authorized_keys: Vec<ssh_key::PublicKey>,
    crypto_provider: Arc<rustls::crypto::CryptoProvider>,
}

impl rustls::server::danger::ClientCertVerifier for AuthorizedKeysVerifier {
    // verify_client_cert: extract SPKI from cert, compare to authorized_keys entries
    ...
}
```

Wire into quinn:

```rust
let rustls_client = rustls::ClientConfig::builder()
    .dangerous()
    .with_custom_certificate_verifier(Arc::new(host_key_verifier))
    .with_client_cert_resolver(Arc::new(AlwaysResolvesClientRawPublicKeys::new(certified_key)));
    // OR: .with_no_client_auth() for unauthenticated client initially

let quinn_client = quinn::ClientConfig::new(Arc::new(
    quinn::crypto::rustls::QuicClientConfig::try_from(rustls_client)?
));
```

**Confidence: HIGH** — cert-pinning path is fully documented and used in production (WireGuard-style SSH crates). RPK upgrade is LOW-MEDIUM confidence given the #2257 compliance issue; validate on a later spike branch.

---

### ssh-agent → rustls Signing Integration

The `rustls::sign::SigningKey` trait is:

```rust
pub trait SigningKey: Debug + Send + Sync {
    fn choose_scheme(&self, offered: &[SignatureScheme]) -> Option<Box<dyn Signer>>;
    fn public_key(&self) -> Option<SubjectPublicKeyInfoDer<'_>>;
}

pub trait Signer: Send + Sync {
    fn sign(&self, message: &[u8]) -> Result<Vec<u8>, Error>;
    fn scheme(&self) -> SignatureScheme;
}
```

`Signer::sign()` is **synchronous and blocking** (rustls doc explicitly acknowledges this as a limitation targeting PKCS#11/HSM scenarios). This matches `ssh-agent-client-rs`, which is also synchronous.

**Integration pattern:**

```rust
struct AgentSigningKey {
    public_key: ssh_key::PublicKey,
    agent_socket_path: PathBuf,
    spki_der: Vec<u8>, // cached from public_key
}

impl rustls::sign::SigningKey for AgentSigningKey {
    fn choose_scheme(&self, offered: &[SignatureScheme]) -> Option<Box<dyn Signer>> {
        // For Ed25519: match SignatureScheme::ED25519
        // For ECDSA P-256: match SignatureScheme::ECDSA_NISTP256_SHA256
        // For RSA: match SignatureScheme::RSA_PSS_SHA256 or RSA_PKCS1_SHA256
        if offered.contains(&SignatureScheme::ED25519) {
            Some(Box::new(AgentSigner {
                public_key: self.public_key.clone(),
                socket_path: self.agent_socket_path.clone(),
                scheme: SignatureScheme::ED25519,
            }))
        } else { None }
    }

    fn public_key(&self) -> Option<SubjectPublicKeyInfoDer<'_>> {
        Some(SubjectPublicKeyInfoDer::from(self.spki_der.as_slice()))
    }
}

impl rustls::sign::Signer for AgentSigner {
    fn sign(&self, message: &[u8]) -> Result<Vec<u8>, rustls::Error> {
        let mut client = ssh_agent_client_rs::Client::connect(&self.socket_path)
            .map_err(|_| rustls::Error::General("agent connect failed".into()))?;
        let identity = ssh_key::PublicKey::from(/* ... */);
        let sig = client.sign(identity, message)
            .map_err(|_| rustls::Error::General("agent sign failed".into()))?;
        // Extract raw signature bytes from ssh_key::Signature
        Ok(sig.as_bytes().to_vec())
    }

    fn scheme(&self) -> SignatureScheme { self.scheme }
}
```

**Key notes:**
- Ed25519: `SignatureScheme::ED25519` maps directly; ssh-agent signs with Ed25519 natively.
- ECDSA P-256: `SignatureScheme::ECDSA_NISTP256_SHA256`; ssh-agent `sign` flag `0` (default).
- RSA: ssh-agent uses `sign` flag `4` for `rsa-sha2-256` (maps to `RSA_PSS_SHA256` or `RSA_PKCS1_SHA256`). Flag `8` for `rsa-sha2-512`. The `ssh-agent-client-rs` `sign()` method accepts the identity but does not currently expose flags in the public API (v1.1.2); may need to call the lower-level protocol directly or use `ssh-agent-lib` for RSA flag control.
- The blocking `sign()` call happens inside the TLS handshake on a tokio thread; wrap in `tokio::task::spawn_blocking` at the point where you drive the quinn/rustls handshake if needed, or accept the brief block on the handshake task.

**Confidence: MEDIUM-HIGH** — Ed25519 path is straightforward; RSA flag handling needs validation against `ssh-agent-client-rs` v1.1.2 source.

---

### QUIC DATAGRAM Configuration (RFC 9221)

Datagrams are **disabled by default** in quinn. Enable by setting a non-None receive buffer size on `TransportConfig`:

```rust
let mut transport = quinn::TransportConfig::default();
// Enable incoming datagrams with a 1 MiB receive buffer
transport.datagram_receive_buffer_size(Some(1 << 20)); // 1 MiB
transport.datagram_send_buffer_size(1 << 20);          // 1 MiB send buffer
// Tune keep-alive and idle timeout for the spike
transport.keep_alive_interval(Some(Duration::from_secs(15)));
transport.max_idle_timeout(Some(Duration::from_secs(60).try_into().unwrap()));

let mut server_config = quinn::ServerConfig::with_crypto(Arc::new(
    quinn::crypto::rustls::QuicServerConfig::try_from(rustls_server_config)?
));
server_config.transport_config(Arc::new(transport));
```

**Sending/receiving datagrams alongside streams:**

```rust
// Same Connection object — streams and datagrams are multiplexed automatically
let conn: quinn::Connection = /* ... */;

// Send unreliable datagram
conn.send_datagram(Bytes::from(payload))?;  // non-blocking, drops if buffer full
// or:
conn.send_datagram_wait(Bytes::from(payload)).await?;  // back-pressures

// Receive datagram
let datagram: Bytes = conn.read_datagram().await?;

// Open reliable bidirectional stream on the same connection
let (mut send, mut recv) = conn.open_bi().await?;
```

`conn.max_datagram_size()` returns `None` if the peer didn't negotiate datagram support or if `datagram_receive_buffer_size` is None on the local endpoint. Always check before sending.

**Confidence: HIGH** — verified from quinn 0.11 docs and TransportConfig API.

---

### PTY Spawn and I/O (portable-pty 0.9.0)

```rust
use portable_pty::{native_pty_system, CommandBuilder, PtySize};

let pty_system = native_pty_system();
let pair = pty_system.openpty(PtySize {
    rows: 24,
    cols: 80,
    pixel_width: 0,
    pixel_height: 0,
})?;

// Spawn a login shell
let mut cmd = CommandBuilder::new("/bin/bash");
cmd.arg("-l"); // login shell flag; or use "login" as the command
// Environment sanitization goes here — strip LD_*, BASH_ENV, etc. before spawn
let mut child = pair.slave.spawn_command(cmd)?;

// I/O handles (the master side)
let reader = pair.master.try_clone_reader()?;  // Box<dyn Read + Send>
let writer = pair.master.take_writer()?;       // Box<dyn Write + Send>

// Resize
pair.master.resize(PtySize { rows: 40, cols: 120, pixel_width: 0, pixel_height: 0 })?;

// Wait for child exit
let status = child.wait()?;
```

**Note:** `try_clone_reader()` creates a cloneable reader; `take_writer()` consumes the writer handle (call once). The Read/Write impls are synchronous — bridge to tokio with `tokio::task::spawn_blocking` or `tokio::io::BufReader::new(tokio::fs::File::from_std(...))` via the raw fd (Linux).

**Confidence: HIGH** — verified from docs.rs/portable-pty 0.9.0 API.

---

## Alternatives Considered

| Category | Recommended | Alternative | Why Not |
|----------|-------------|-------------|---------|
| QUIC crate | `quinn` 0.11 | `s2n-quic` (AWS) | s2n-quic is production-grade but uses its own TLS layer (s2n-tls), not rustls — harder to wire ssh-agent signing. quinn's rustls integration is the native path |
| QUIC crate | `quinn` 0.11 | `quiche` (Cloudflare, via C FFI) | FFI, not pure Rust; async story is messier |
| Crypto backend | `rustls-ring` | `rustls-aws-lc-rs` | aws-lc-rs requires a C build step (cmake); ring is pure Rust build. For the spike, simpler wins |
| SSH agent | `ssh-agent-client-rs` | `russh` agent support | russh carries the full SSH protocol implementation; agent-only use pulls in large transitive deps. `ssh-agent-client-rs` is minimal and purpose-built |
| SSH agent | `ssh-agent-client-rs` | `ssh-agent-lib` | `ssh-agent-lib` is for *writing* an agent server, not a client |
| PTY | `portable-pty` | `nix` raw pty | nix raw PTY is Linux-only; portable-pty's trait abstraction already bakes in the M6 Windows path |
| Terminal parser | `vte` | `termwiz` (also wezterm) | termwiz is a full terminal emulator; vte is just the parser state machine. For M2 server-side terminal model, vte + custom `Perform` impl is the right granularity |
| Cert generation | `rcgen` | `x509-cert` (RustCrypto) | x509-cert is a lower-level DER builder with no signing convenience; rcgen's `SigningKey` trait + `CertificateParams::self_signed()` is ergonomic |

---

## What NOT to Use

| Avoid | Why | Use Instead |
|-------|-----|-------------|
| `rustls` < 0.23.16 | No RPK support; older API shapes | `rustls` 0.23.40 |
| `quinn` < 0.11 | Pre-0.11 used a different rustls wiring (`quinn::ServerConfigBuilder`); API completely changed | `quinn` 0.11.x |
| `ed25519-dalek` 3.0.0-pre.*  | Pre-release API may change | `ed25519-dalek` 2.2.0 (stable) |
| `rustls-native-certs` for host verification | Platform cert store verifies PKI chains; nosh never uses PKI chains — it pins keys | Custom `ServerCertVerifier` or RPK |
| `openssh` crate | Spawns a subprocess `ssh`; provides no primitives we need | `ssh-key` + `ssh-agent-client-rs` |
| `tokio-rustls` directly | Not needed — quinn handles the TLS/QUIC integration internally; tokio-rustls is for TCP+TLS | Only quinn |
| `mio` or raw `epoll` | quinn requires tokio; mixing runtimes creates pain | tokio only |

---

## Version Compatibility

| Package | Compatible With | Notes |
|---------|-----------------|-------|
| `quinn` 0.11.9 | `rustls` 0.23.x | quinn 0.11 requires rustls 0.23; the conversion path is `rustls::ServerConfig` → `quinn::crypto::rustls::QuicServerConfig::try_from()` → `quinn::ServerConfig::with_crypto()` |
| `quinn` 0.11.9 | `tokio` 1.x | `runtime-tokio` feature; tokio 1.52.x is current |
| `ssh-key` 0.6.7 | `ed25519-dalek` 2.x | ssh-key's `ed25519` feature pulls `ed25519-dalek` as a transitive dep; pin to same major |
| `portable-pty` 0.9.0 | `tokio` 1.x | No direct tokio dep; bridge via `spawn_blocking` |
| `rcgen` 0.14.8 | `rustls` 0.23.x | rcgen 0.14.x integrates with rustls 0.23 `CertifiedKey` |

---

## Stack Patterns by Phase

**M0 — QUIC echo spike:**
- quinn client + server, `rustls::ServerConfig::builder().with_no_client_auth().with_single_cert(rcgen-generated cert)`, datagram + stream on one connection.
- Goal: confirm RFC 9221 datagrams and bidi streams are independent on one connection.

**M1 — Auth (cert-pinning first):**
- `AgentSigningKey` / `AgentSigner` delegating to `ssh-agent-client-rs`.
- Server: custom `ClientCertVerifier` checking presented cert's SPKI against `authorized_keys` entries parsed by `ssh-key`.
- Client: custom `ServerCertVerifier` checking server cert's SPKI against `known_hosts` (TOFU on first connection).
- Defer RPK upgrade until after M1 is working end-to-end.

**M2 — PTY session core:**
- `portable-pty` 0.9.0 on Linux; env sanitization before `spawn_command`.
- `vte` 0.15.0 for terminal state; `PtySize` resize with ~40 ms burst coalescing.

---

## Sources

- https://docs.rs/crate/quinn/latest (version 0.11.9, features list — verified)
- https://docs.rs/quinn/latest/quinn/struct.Connection.html (send_datagram, read_datagram, max_datagram_size API — verified)
- https://docs.rs/quinn/latest/quinn/struct.TransportConfig.html (datagram_receive_buffer_size — verified)
- https://quinn-rs.github.io/quinn/quinn/certificate.html (QuicServerConfig::try_from wiring pattern — verified)
- https://docs.rs/rustls/latest/rustls/ (version 0.23.40 confirmed)
- https://docs.rs/rustls/latest/rustls/client/danger/trait.ServerCertVerifier.html (requires_raw_public_keys, verify_server_cert API — verified)
- https://docs.rs/rustls/latest/rustls/server/struct.AlwaysResolvesServerRawPublicKeys.html (RPK resolver API — verified)
- https://docs.rs/rustls/latest/rustls/client/struct.AlwaysResolvesClientRawPublicKeys.html (RPK client resolver — verified)
- https://docs.rs/rustls/latest/rustls/sign/struct.CertifiedKey.html (CertifiedKey for RPK and cert-pinning — verified)
- https://github.com/rustls/rustls/releases/tag/v%2F0.23.16 (RPK added in 0.23.16, PR #2062 — verified)
- https://github.com/rustls/rustls/issues/2257 (RPK compliance caveat — LOW confidence on current resolution status)
- https://docs.rs/crate/rustls/latest/source/src/manual/howto.rs (custom SigningKey/Signer delegation pattern — verified)
- https://docs.rs/ssh-agent-client-rs/latest/ssh_agent_client_rs/ (version 1.1.2, sign() method — verified)
- https://github.com/nresare/ssh-agent-client-rs (sign API, list_identities, synchronous — verified)
- https://docs.rs/portable-pty/latest/portable_pty/ (version 0.9.0, openpty/spawn_command/resize API — verified)
- https://docs.rs/ssh-key/latest/ssh_key/ (version 0.6.7, authorized_keys/known_hosts modules — verified)
- https://docs.rs/crate/rcgen/latest (version 0.14.8, SigningKey trait, self_signed() — verified)
- https://docs.rs/crate/ed25519-dalek/latest (version 2.2.0 stable; 3.0.0-pre.7 pre-release noted — verified)
- https://github.com/alacritty/vte (version 0.15.0 current — verified)
- https://github.com/tokio-rs/tokio/releases (tokio 1.52.x current; 1.47.x and 1.51.x LTS — verified)

---
*Stack research for: nosh QUIC remote shell — M0–M2 architecture-validation spike*
*Researched: 2026-05-29*
