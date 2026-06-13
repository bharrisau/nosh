# nosh Security Model

**Version:** 1.4 (Phase 27 hardened)
**Last reviewed:** 2026-06-14
**Status:** Internet-exposure ready (Mode A)

> This is general security-engineering information for operators and security reviewers. It is not a formal third-party audit. See "Verify before relying on this" at the end.

---

## 1. Scope and Topology

### Mode A (committed deliverable)

nosh binds directly to UDP/443 as a QUIC server. The server is exposed to the internet with no intervening proxy. QUIC termination, SSH-key authentication, and session management all happen on the same host.

**Trust boundary:** client ←→ internet ←→ nosh-server (single host)

**Deployment requirements:**
- Firewall: allow UDP/443 from arbitrary sources
- SSH keys: server must have `authorized_keys` for authenticating users
- Host key: server's Ed25519 key is pinned by clients via `known_hosts` (TOFU on first connection)

### Mode B (WebTransport behind proxy — future stretch)

nosh runs as a WebTransport client behind an HTTP/3 reverse proxy (e.g. Envoy). The proxy terminates QUIC and forwards nosh traffic over HTTP/3. This is **not a committed v1.4 deliverable** and is explicitly marked as stretch-only in the roadmap.

**Trust boundary:** client ←→ internet ←→ HTTP/3 proxy ←→ nosh-server (two hosts)

**Critical difference:** In Mode B, the proxy sees all traffic between client and server. The outer TLS layer (Let's Encrypt cert from a sidecar) provides HTTPS identity and proxy-friendliness, but it does **not** provide end-to-end authentication. The inner SSH-key handshake is the authoritative trust anchor. A terminating or compromised proxy cannot MITM the authentication because the inner handshake is bound via RFC 9266 EKM (see §3 below).

---

## 2. Assets and Trust Boundaries

### Assets

| Asset | Location | Value to Attacker | Protection Mechanism |
|-------|----------|-------------------|---------------------|
| SSH private key | Client (ssh-agent or on-disk) | Impersonation of legitimate user | ssh-agent IPC protections; file permissions; never transmitted |
| Server host key | Server (generated at first start) | Impersonation of server; MITM | TOFU pinning in `known_hosts`; manual fingerprint confirm on first connect |
| Session state (PTY, scrollback) | Server (per-session memory) | Access to live shell session; credential exfiltration | Bound by `AuthLimits` (64 concurrent, 5s timeout); post-auth only |
| Reattach token | Server (single-use, rotated) | Session hijacking after disconnect | Single-use; rotated on every successful reattach; only issued after inner-auth completes |
| Clipboard contents | Client (local terminal) | Data exfiltration | OSC 52 read rejected at server; client-side defense-in-depth validation |
| Local terminal display | Client (stdout) | Social engineering; prompt spoofing | Title/clipboard escape stripping; scheme whitelists |

### Trust Boundaries

1. **Client ←→ Network:** The client's SSH private key never leaves the agent. The SSH host key is verified via TOFU (interactive fingerprint prompt on first connect, Phase 25).
2. **Network ←→ Server:** Pre-authentication traffic is bounded by `AuthLimits` (64 concurrent half-open connections, 5s auth timeout). Post-authentication traffic runs over established QUIC streams.
3. **Proxy ←→ Server (Mode B only):** The proxy terminates the outer TLS but cannot read or modify the inner SSH-key handshake because it is bound via RFC 9266 EKM (see §3).

---

## 3. The Proxy Trust Model (Mode B)

### What the Outer TLS Provides

In Mode B (WebTransport behind an HTTP/3 proxy), the outer TLS layer uses a CA-signed certificate (typically Let's Encrypt from a sidecar cert-manager). This provides:

- **Confidentiality:** Network observers cannot read the traffic between client and proxy.
- **HTTPS identity:** The proxy presents a browser-friendly cert that clients can verify via the OS CA trust store.
- **Proxy friendliness:** HTTP/3 proxies understand the traffic and can apply routing/policy.

### What the Inner SSH-Key Handshake Provides

The inner SSH-key handshake (Phase 25) runs **inside** the WebTransport tunnel and provides:

- **End-to-end identity:** The client proves possession of an SSH private key that is registered in the server's `authorized_keys`. The server proves possession of a host key that is pinned in the client's `known_hosts`.
- **RFC 9266 EKM channel binding:** The inner handshake is bound to the outer TLS session via the `tls-exporter` channel binding. A malicious proxy that terminates the outer TLS cannot replay or modify the inner handshake without breaking the EKM proof.
- **Fallback to CSPRNG nonce:** If `tls-exporter` is unavailable (e.g., the proxy doesn't support it), both client and server contribute 32 bytes of CSPRNG entropy to the handshake. This is explicitly documented in Phase 25 as a fall-forward path; EKM is preferred but not mandatory.

### What a Compromised Proxy Can and Cannot Do

**Cannot do (because inner-auth is authoritative):**
- MITM the SSH-key authentication. Even if the proxy terminates the outer TLS, it cannot forge a valid inner-auth handshake without the client's private key or the server's host key.
- Reattach to a session without completing the inner handshake. The reattach token is only issued after `InnerAuthComplete`; `Reattach` messages are rejected in `Unauthenticated` or `ChallengeExchanged` states.
- Learn the client's SSH private key. The signing operation happens inside ssh-agent (or on-disk) and never traverses the network in plaintext.

**Can do (because outer TLS is terminated):**
- Observe that a client is connecting to a nosh server (traffic analysis).
- Drop the connection (DoS). The client will reconnect via its exponential backoff loop.
- See the byte stream of the encrypted inner handshake (but cannot decrypt or modify it without breaking the EKM binding).

**Composition statement:** outer-CA-cert-but-inner-auth-is-authoritative. The outer TLS is a convenience layer (HTTPS identity, proxy routing). The inner SSH-key handshake, bound via RFC 9266 EKM (or CSPRNG fallback), is the end-to-end trust anchor. A terminating or compromised proxy is contained because it cannot MITM the authentication.

---

## 4. Attacker Capabilities

This section assumes an internet-exposed Mode A deployment. Adjust for Mode B accordingly (proxy becomes an additional network actor).

### Passive Network Adversary

**Can:** Observe QUIC packets on UDP/443; see that a client is connecting to a nosh server (traffic analysis); measure packet sizes and timing.
**Cannot:** Read the encrypted TLS payload; inject packets; impersonate client or server without the private keys.

### Active MITM (Pre-Inner-Auth)

**Can:** Intercept the QUIC handshake; present a fraudulent cert to the client (if using Mode B without EKM); attempt to downgrade the transport.
**Cannot:** Complete the inner SSH-key handshake without the client's private key; bypass the RFC 9266 EKM binding (if active); reattach to a session without `InnerAuthComplete`.

### Unauthenticated Attacker (Pre-Auth DoS)

**Can:** Flood UDP/443 with junk packets; open many half-open QUIC connections; exhaust the 64-slot `AuthLimits` semaphore.
**Cannot:** Read or modify post-authentication traffic; cause memory exhaustion via malformed QUIC packets (quinn's anti-amplification holds; see docs/999.1-SECURITY.md).

### Authenticated Attacker (Malicious Client)

**Can:** Run arbitrary commands in their own shell session; send malicious terminal escape sequences via PTY output; attempt OSC 52 clipboard read; attempt to exfiltrate data via title/hyperlink injection.
**Cannot:** Escape the per-session memory bounds (PTY, scrollback); access another user's session; read the server's host key from disk; bypass OSC accumulation bounds (SEC-05); bypass client-side escape stripping (SEC-04 D-02..D-07).

### Compromised Server (Malicious Server)

**Can:** Send arbitrary terminal control sequences to the client; attempt to inject via title/clipboard/hyperlink; send rapid resize commands; send oversized `PtyData` frames.
**Cannot:** Exfiltrate clipboard contents (OSC 52 read rejected D-16-01a); bypass client-side scheme whitelists (SEC-04 D-07); cause client OOM via OSC accumulation (SEC-05); bypass channel-ID validation (SEC-04 D-06).

---

## 5. STRIDE Threat Table

| Category | Threat | Mitigation | Status |
|----------|--------|------------|--------|
| **Spoofing** | Attacker impersonates server without host key | Server host key pinned in `known_hosts` (TOFU, Phase 25 interactive fingerprint prompt) | ✅ Mitigated |
| **Spoofing** | Attacker impersonates client without SSH private key | Inner SSH-key handshake (Ed25519 signature verified against `authorized_keys`) | ✅ Mitigated |
| **Spoofing** | Proxy impersonates server in Mode B | RFC 9266 EKM channel binding (or CSPRNG nonce fallback) binds inner handshake to outer TLS | ✅ Mitigated |
| **Spoofing** | Attacker learns if a key exists (key-existence oracle) | `InnerAuthFail` is fieldless — no reason code, no oracle (Phase 25 decision) | ✅ Mitigated |
| **Tampering** | OSC 52 clipboard write injection | Server-side `osc_dispatch` drops malformed selections; client-side escape stripping (WR-01) | ✅ Mitigated (SEC-04 D-05) |
| **Tampering** | Title injection (CR/LF overwrite prompt) | Client-side title filter strips `\r`, `\n`, `\x07`, `\x1b` (SEC-04 D-05) | ✅ Mitigated |
| **Tampering** | OSC 8 hyperlink XSS injection | Server-side scheme whitelist (http/https/mailto/file); client-side defense-in-depth validation (SEC-04 D-07) | ✅ Mitigated |
| **Tampering** | Channel ID spoofing | Client validates even parity, non-zero, not u32::MAX (SEC-04 D-06) | ✅ Mitigated |
| **Tampering** | DCS/PM/APC injection | No client vte parser; `TerminalControlPayload` has no DCS/PM/APC variant; vte default no-ops on server (SEC-04 D-04) | ✅ Mitigated |
| **Repudiation** | Attacker denies performing an action | Out of scope — nosh does not log shell commands; logging is the user's shell responsibility | ⚠️ Accepted |
| **Information Disclosure** | Clipboard exfiltration via OSC 52 read | Server-side `osc_dispatch` drops `?` form (D-16-01a); client-side defense-in-depth (SEC-04 D-03) | ✅ Mitigated |
| **Information Disclosure** | Reattach token leakage | Token only issued after `InnerAuthComplete`; single-use; rotated on every successful reattach | ✅ Mitigated |
| **Denial of Service** | OSC OOM via unbounded accumulation | `osc_prefilter` bounds at 1 MiB (`OSC_ACCUMULATION_MAX`); parser resync on overflow (SEC-05) | ✅ Mitigated |
| **Denial of Service** | Pre-auth connection flood | `AuthLimits { max_concurrent: 64, auth_timeout: 5s }`; excess connections refused (docs/999.1-SECURITY.md) | ✅ Mitigated |
| **Denial of Service** | PtyData recv exhaustion | `MAX_PTYDATA_FRAME_BYTES` (1 MiB) cap on client (SEC-04 D-06) | ✅ Mitigated |
| **Denial of Service** | Resize storm (server → client) | `MIN_RESIZE_INTERVAL_MS` (300ms) constant formalizes existing debounce (SEC-04 D-06) | ⚠️ Partial (see note below) |
| **Elevation of Privilege** | Env var privilege escalation | Environment-variable sanitization on every shell/exec (LD_*, DYLD_*, BASH_ENV, ENV, IFS, SHELLOPTS, PYTHONPATH, NODE_OPTIONS) — CLAUDE.md invariant | ✅ Mitigated |
| **Elevation of Privilege** | Agent forwarding via env var | `SSH_AUTH_SOCK` never forwarded via environment; agent forwarding uses dedicated channel (future) | ✅ Mitigated |

**Note on resize rate-limit (DoS):** `MIN_RESIZE_INTERVAL_MS` is defined as a named constant (300ms) to formalize the existing `RESIZE_DEBOUNCE` behavior. However, this bounds **client-initiated** resizes (SIGWINCH coalescing via `ResizeWatcher`). Server-initiated dimension changes flow through `StateDiff` datagrams, which are already bounded by the transport's 1 MiB datagram cap. There is no explicit rate-limit on how many `Resize` messages a malicious server can send on the control stream, but the practical impact is limited (resize events are cheap to process and the client re-reads `terminal::size()` authoritatively).

---

## 6. Shipped Mitigations

### Pre-authentication Caps

**File:** `crates/nosh-server/src/server.rs` (lines 35-48)

```rust
AuthLimits { max_concurrent: 64, auth_timeout: 5s }
```

- `tokio::Semaphore` caps concurrent half-open handshakes at 64.
- Auth-completion timeout of 5s bounds how long a half-open connection can hold a permit.
- Datagram buffers: 1 MiB receive + 1 MiB send (crates/nosh-proto/src/transport.rs).
- Worst-case pre-auth memory: ≈ 64 × ~2 MiB datagram buffers ≈ 128 MiB.

See `docs/999.1-SECURITY.md` for full analysis (anti-amplification, fuzzing results, dependency advisories).

### OSC OOM Bound (SEC-05)

**File:** `crates/nosh-server/src/terminal.rs` (line 77)

```rust
pub const OSC_ACCUMULATION_MAX: usize = 1_048_576; // 1 MiB
```

- **Mechanism:** `osc_prefilter` in `TerminalState::advance()` scans bytes before feeding to vte. Tracks `in_osc` and `osc_byte_count` shadow state. On overflow (>1 MiB), feeds safe prefix to parser, replaces parser with `vte::Parser::default()` (ground state), resets counters.
- **Category-agnostic:** The prefilter fires on ALL OSC sequences (0, 2, 52, 8, 1337, etc.) at the byte level. OSC categories that fall through to the `_ =>` arm in `osc_dispatch` are still bounded.
- **Regression test:** `oversized_multi_chunk_osc_is_bounded_then_resyncs` (10 MiB OSC 2 title in 4 KiB chunks).
- **CI gate:** `osc-bound-regression` job in `.github/workflows/ci.yml` (required check).
- **Fuzz target:** `fuzz/fuzz_targets/osc_accumulation.rs` with invocation `cargo +nightly fuzz run osc_accumulation -- -max_len=2097152 -max_total_time=120`.

See `docs/999.7-SECURITY.md` for full design analysis (resync behavior, storage caps, regression test).

### Client Hardening (SEC-04)

#### Title Escape Stripping (D-05)

**File:** `crates/nosh-client/src/main.rs` (line ~2162)

```rust
let clean_title: String = title
    .chars()
    .filter(|&c| c != '\x07' && c != '\x1b' && c != '\r' && c != '\n')
    .collect();
```

- Strips BEL (`\x07`), ESC (`\x1b`), CR (`\r`), LF (`\n`).
- Prevents prompt-overwrite social engineering (PITFALLS.md SEC-2).

#### Clipboard Selection Validation (D-05)

**File:** `crates/nosh-client/src/main.rs` (line ~2127)

```rust
let sel: String = String::from_utf8_lossy(&selection)
    .chars()
    .filter(|&c| c != '\x07' && c != '\x1b')
    .collect();
if !matches!(sel.as_str(), "c" | "p" | "s" | "q" | "0".."9") {
    continue; // drop frame with invalid selection
}
```

- Validates selection against whitelist {c, p, s, q, 0-9} before OSC 52 re-emission.
- Prevents unexpected clipboard operations on some terminals.

#### OSC 52 Read Rejection (D-03)

**File:** `crates/nosh-client/src/main.rs` (line ~2130)

```rust
if data == b"?" {
    continue; // drop OSC 52 clipboard-read
}
```

- Defense-in-depth confirmation of server-side drop (D-16-01a).

#### PtyData Recv Cap (D-06)

**File:** `crates/nosh-client/src/main.rs` (line 61)

```rust
const MAX_PTYDATA_FRAME_BYTES: usize = 1_048_576; // 1 MiB
```

- Applied in `Message::PtyData` arm; `TransportDrop` on violation.
- Tighter than `MAX_FRAME_LEN` (16 MiB); treats oversized PtyData as a transport anomaly.

#### Channel-ID Range Validation (D-06)

**File:** `crates/nosh-client/src/client.rs` (line ~803)

```rust
if channel_id == 0 || channel_id % 2 != 0 || channel_id == u32::MAX {
    return Err(ChannelError::InvalidChannelId);
}
```

- Enforces even parity (client-initiated), non-zero, not u32::MAX.
- Defense-in-depth check in `await_channel_accept()`.

#### Resize Rate-Limit Constant (D-06)

**File:** `crates/nosh-client/src/main.rs` (line 53)

```rust
const MIN_RESIZE_INTERVAL_MS: u64 = 300;
```

- Formalizes existing ~300ms debounce as a security property.
- Note: This bounds client-initiated resizes (SIGWINCH coalescing). Server-initiated resizes flow through datagrams.

#### OSC 8 Hyperlink Scheme Whitelist (D-07)

**File:** `crates/nosh-server/src/terminal.rs` (line ~1165)

```rust
let uri_lower = uri.to_lowercase();
let is_whitelisted = uri_lower.starts_with("http://")
    || uri_lower.starts_with("https://")
    || uri_lower.starts_with("mailto:")
    || uri_lower.starts_with("file:");
```

- Server-side scheme whitelist in `osc_dispatch` (OSC 8).
- Client-side defense-in-depth validation on re-emit.
- Strips dangerous schemes (javascript:, data:, vbscript:, etc.).

#### DCS/PM/APC No-Op (D-04)

**File:** `crates/nosh-client/src/main.rs` (line ~2122)

```rust
// D-04: no client vte parser + no DCS/PM/APC TerminalControlPayload variant — scope-fenced no-op
Ok(_) => {} // drop unknown TerminalControlPayload variants
```

- vte default trait impls handle DCS/PM/APC as no-ops on the server.
- No client vte parser; no DCS/PM/APC `TerminalControlPayload` variant.

### Inner Auth (Phase 25)

**Files:** `crates/nosh-client/src/client.rs`, `crates/nosh-server/src/server.rs`

- RFC 9266 EKM channel binding (`tls-exporter` from outer WebTransport session).
- CSPRNG-nonce fallback (32 bytes from each peer) if EKM unavailable.
- Four-step mutual challenge-response: `ClientInit`, `ServerChallenge`, `ClientResponse`, `Complete`.
- `InnerAuthFail` is fieldless (no reason code, no key-existence oracle).
- State machine enforces `Unauthenticated → ChallengeExchanged → Authenticated` transitions.
- `SessionOpen` and `Reattach` only accepted in `Authenticated` state.

### TOFU Prompt (Phase 25)

**File:** `crates/nosh-client/src/main.rs` (blocking fingerprint confirm)

- Interactive fingerprint prompt on first host-key connect.
- User must type "yes" to accept the fingerprint.
- Stored in `known_hosts` for subsequent connections.

### Environment Variable Sanitization (v1.0)

**File:** `crates/nosh-server/src/session.rs` (exec/spawn path)

- Strips `LD_*`, `DYLD_*`, `BASH_ENV`, `ENV`, `IFS`, `SHELLOPTS`, `PYTHONPATH`, `NODE_OPTIONS`.
- Whitelists `TERM`, `LC_*`/locale, `TZ`.

### SSH_AUTH_SOCK Protection (v1.0)

**File:** `crates/nosh-server/src/session.rs` (exec/spawn path)

- `SSH_AUTH_SOCK` never forwarded via environment.
- Agent forwarding (future) uses a dedicated channel.

---

## 7. Residual Risks

### RUSTSEC-2023-0071 (rsa 0.9.10 Marvin timing attack)

**Status:** Accepted (parsing-only)

**Impact:** Timing side-channel on RSA *decryption* operations.

**Mitigation:** nosh uses `rsa` (via `ssh-key`) only for **parsing** RSA public keys from `authorized_keys`/`known_hosts`. No RSA decryption operations occur on the server's hot path. The side-channel is not exercised.

**Re-evaluate:** If `ssh-key`/`rsa` ships a patched release, or consider restricting to Ed25519/ECDSA identities.

See `docs/999.1-SECURITY.md` for full advisory scan results.

### ssh-agent Compromise

**Status:** Out of scope

**Impact:** If the ssh-agent process is compromised, the attacker gains access to all loaded SSH private keys.

**Mitigation:** This is outside nosh's control. Operators should follow ssh-agent hardening best practices (socket permissions, agent lifetime).

### Physical Client Access

**Status:** Out of scope

**Impact:** An attacker with physical access to the client machine can read `known_hosts`, access on-disk identity keys, or intercept ssh-agent socket traffic.

**Mitigation:** This is outside nosh's control. Operators should use full-disk encryption, secure shell configuration, and physical security measures.

### quinn Per-Connection Allocation

**Status:** Accepted (bounded in count)

**Impact:** quinn's exact per-half-open-connection allocation is not publicly quantified.

**Mitigation:** The 64-permit half-open cap bounds the *count*, which is the controlling factor. Worst-case pre-auth memory is ~128 MiB (64 × ~2 MiB datagram buffers) plus quinn's per-connection transport state.

See `docs/999.1-SECURITY.md` for full analysis.

### No Retry/Address-Validation Hardening

**Status:** Accepted (low priority)

**Impact:** Spoofed-source packets can trigger the initial handshake before the 3× anti-amplification limit applies.

**Mitigation:** The 3× limit (RFC 9000 §8.1) is enforced. Optional hardening: call `Incoming::retry()` in the accept loop to add address validation before committing connection state. Not currently implemented.

See `docs/999.1-SECURITY.md` for full analysis.

---

## 8. Operator Checklist (Mode A Deployment)

Before exposing nosh to the internet in Mode A (direct UDP/443 binding):

- [ ] **Firewall:** Allow UDP/443 from arbitrary sources. Confirm no other QUIC/UDP services conflict.
- [ ] **SSH keys:** Verify `~/.ssh/authorized_keys` contains only legitimate Ed25519/ECDSA/RSA public keys. Remove stale entries.
- [ ] **Host key:** Generate a strong Ed25519 host key on first server start (`nosh-server` auto-generates). Record the fingerprint in a secure location.
- [ ] **known_hosts:** Distribute the server's host key fingerprint to clients via a secure channel (OOB verification). TOFU is convenient but manual fingerprint confirm is stronger.
- [ ] **Resource caps:** Verify `AuthLimits` (64 concurrent, 5s timeout) is sufficient for expected load. Adjust if needed.
- [ ] **CI gate:** Confirm `osc-bound-regression` job is green (must pass on every push).
- [ ] **Dependencies:** Run `cargo audit` and `cargo deny check` to verify no new advisories or bans.
- [ ] **Fuzzing:** Periodically re-run `cargo +nightly fuzz run osc_accumulation -- -max_len=2097152 -max_total_time=120` to catch OSC OOM regressions.
- [ ] **Logging:** Enable structured logging (tracing) in production. Monitor logs for `InnerAuthFail` spikes (may indicate probing).
- [ ] **Monitoring:** Track half-open connection count ( semaphore permits). Alarm if consistently at 64 (DoS indicator).
- [ ] **Backup:** `known_hosts` and `authorized_keys` are not backed up by nosh. Operator must manage backup/restore of these files.

### Mode B Deployment (Future Stretch)

If deploying in Mode B (WebTransport behind Envoy), additional steps:

- [ ] **Proxy cert:** Obtain a CA-signed certificate for the proxy (Let's Encrypt or internal CA).
- [ ] **EKM support:** Verify the proxy supports RFC 9266 `tls-exporter` (wtransport 0.7.1 requirement). If not, the CSPRNG-nonce fallback applies (explicitly documented in Phase 25).
- [ ] **Proxy policy:** Configure Envoy to forward WebTransport traffic without inspection. The inner SSH-key handshake is encrypted; MITM by the proxy is prevented by EKM binding.
- [ ] **Network topology:** Ensure client can reach the proxy on UDP/443 (or TCP/443 if proxy terminates HTTP/3).

---

## Verify Before Relying on This

- Confirm the GitHub Actions **`audit` job is green** after pushing (scans `Cargo.lock` against RustSec DB).
- The fuzz corpora are committed under `fuzz/corpus/`. Re-run `cargo +nightly fuzz run <target>` periodically and after any decoder / `quinn` / `vte` change to catch regressions.
- Re-run `cargo audit` / `cargo deny check` on dependency bumps. Revisit RUSTSEC-2023-0071 when an `rsa` fix lands.
- This assessment covers Mode A (raw UDP/443 exposure). Mode B (WebTransport behind proxy) needs its own review when M7 is built.
- All mitigations cited in §6 reference actual code locations. Verify the constants, functions, and test names exist in the current HEAD.

---

**Document owner:** Security lead (to be appointed)
**Review cycle:** Before each v1.x release and after any dependency upgrade
**Change history:**
- 2026-06-14: Initial version (Phase 27 SEC-01, documenting SEC-04 and SEC-05 mitigations)
