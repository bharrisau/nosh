# Pitfalls Research

**Domain:** QUIC-based roaming remote shell (Rust) — M0–M2 architecture spike
**Researched:** 2026-05-29
**Confidence:** HIGH (quinn/rustls via Context7 + official docs; PTY and security via official specs + known CVEs)

---

## MUST-ADDRESS IN THIS MILESTONE — Security Footguns

These three are non-negotiable for M2 (session core). They are privilege-escalation vectors that become much harder to retrofit once "it works."

### FOOTGUN-1: Environment Variable Injection into the Shell

**What goes wrong:**
The server spawns a login shell with environment variables inherited from the client's session-open request. If the client can supply `LD_PRELOAD`, `BASH_ENV`, `ENV`, `IFS`, `SHELLOPTS`, `PYTHONPATH`, `NODE_OPTIONS`, or similar, an attacker who compromises the client transport can inject arbitrary code into the server process or any process that shell spawns.

**Why it happens:**
Developers building the "environment passthrough" feature (forwarding `TERM`, `LC_*`, `TZ` so the remote shell looks right) pass the whole env dict rather than a filtered whitelist.

**How to avoid:**
Implement a **deny-all, explicit-allow-list** approach. On shell/exec open, construct the server-side environment from scratch:
- Allow: `TERM`, `LC_ALL`, `LC_CTYPE`, `LC_MESSAGES`, `LC_COLLATE`, `LC_MONETARY`, `LC_NUMERIC`, `LC_TIME`, `LANG`, `TZ`
- Deny everything else the client sends, including any var matching `LD_*`, `DYLD_*`, `BASH_ENV`, `ENV`, `IFS`, `SHELLOPTS`, `PYTHONPATH`, `NODE_OPTIONS`, `RUBYLIB`, `PERL5LIB`, `JAVA_TOOL_OPTIONS`, `_JAVA_OPTIONS`, `JAVA_OPTIONS`
- Merge with the server's own minimal environment (PATH, HOME, USER, SHELL sourced from `/etc/passwd`)
- Never pass the client env dict to `CommandBuilder` directly

**Warning signs:**
Any code path that does `cmd.env(client_supplied_key, client_supplied_val)` without filtering, or uses `std::env::vars()` to forward current process env.

**Phase to address:**
M2 (session core) — must exist before the first PTY spawn. This is not a "clean up later" item.

---

### FOOTGUN-2: SSH_AUTH_SOCK Forwarded via Environment

**What goes wrong:**
The remote shell process has `SSH_AUTH_SOCK` in its environment pointing to a Unix socket on the server. Any process or user on the server who can read that process's `/proc/<pid>/environ` can hijack the socket and use your SSH keys to authenticate anywhere those keys work. Root on the server can always do this.

**Why it happens:**
`SSH_AUTH_SOCK` is a normal env var that the user's local shell has set. Environment passthrough naively includes it.

**How to avoid:**
`SSH_AUTH_SOCK` must be explicitly excluded from the allowed-list above — it never passes through the env path. In a future milestone (M5), agent forwarding is implemented as a dedicated QUIC stream with an explicit protocol; the server binds a new local socket and sets `SSH_AUTH_SOCK` to *that* socket, scoped to the session, rather than forwarding the client's existing socket path.

**Warning signs:**
Any test that verifies `SSH_AUTH_SOCK` is present in the spawned shell's environment is a bug, not a success.

**Phase to address:**
M2 — the deny-all env whitelist handles this automatically if `SSH_AUTH_SOCK` is simply not in the allow list.

---

### FOOTGUN-3: Unauthenticated Connection Memory Exhaustion

**What goes wrong:**
QUIC's handshake involves multiple round trips during which the server holds per-connection state. An attacker flooding the server with half-open connections can exhaust server memory before any authentication check runs. CVE-2024-22189 (quic-go) demonstrated a real variant: flooding `NEW_CONNECTION_ID` frames causes unbounded memory growth.

**Why it happens:**
Connection state is allocated before authentication succeeds.

**How to avoid:**
- Cap the number of simultaneous in-progress (pre-auth) connections — start with a hard limit of ~256 or a configurable value
- Use quinn's `max_concurrent_uni_streams` / `max_concurrent_bidi_streams` to limit stream proliferation per connection
- Apply per-IP rate limiting on connection initiation (even a simple token bucket in front of `Endpoint::accept()`)
- Complete auth within a timeout (e.g. 10 seconds from connection open); abort with `connection.close()` on timeout

**Warning signs:**
No limit on `Endpoint::accept()` loop; no timeout on auth completion; unbounded `Vec` or `HashMap` keyed on connection ID.

**Phase to address:**
M1 (auth) — structure the accept loop with a bound before the first auth check. Reinforce in M2.

---

## Critical Pitfalls

### Pitfall 1: Datagrams Silently Disabled — `datagram_receive_buffer_size` Defaults to `None`

**What goes wrong:**
`Connection::max_datagram_size()` returns `None`, `send_datagram()` returns `SendDatagramError::UnsupportedByPeer`, and all datagram sends are silently dropped or return errors. The streams work fine; you never see datagrams at all.

**Why it happens:**
`TransportConfig::datagram_receive_buffer_size` defaults to `None`, which **disables incoming datagrams entirely**. Both endpoints must set a non-`None` value for datagrams to flow. If either side leaves the default, datagrams are refused at the QUIC layer — the peer is forbidden from sending them. This is the M0 spike's primary gotcha.

**How to avoid:**
Explicitly set `datagram_receive_buffer_size` on **both** client and server `TransportConfig` before establishing the connection:
```rust
transport_config.datagram_receive_buffer_size(Some(1024 * 1024)); // e.g. 1 MiB
```
In M0, assert that `connection.max_datagram_size()` returns `Some(_)` as the first test after connection establishment.

**Warning signs:**
`max_datagram_size()` returns `None` after a successful handshake; `send_datagram()` returns `UnsupportedByPeer`.

**Phase to address:**
M0 — this is the literal first thing the spike tests.

---

### Pitfall 2: Datagram Size Exceeds Per-Path MTU — Silent Drop or Error

**What goes wrong:**
A terminal state-sync datagram larger than the current path MTU estimate is either silently dropped (by the OS UDP layer with fragmentation blocked) or causes `SendDatagramError::TooLarge`. The session appears to stall or lose updates without any clear error.

**Why it happens:**
`max_datagram_size()` fluctuates during the connection lifetime as quinn's DPLPMTUD binary-search probes the path. The minimum guaranteed value is "a little over a kilobyte" (~1200 bytes overhead-adjusted). On degraded paths or after black-hole detection resets the estimate, the limit can drop. Sending a fixed-size payload without checking the current limit causes intermittent failures that are hard to reproduce.

**How to avoid:**
- Always call `connection.max_datagram_size()` before each `send_datagram()` call and fragment or discard the payload if it exceeds the limit
- For the terminal state-sync object, design the payload to be compressible to under ~1100 bytes under any circumstance (reserve headroom for QUIC framing)
- Do not send datagrams larger than the minimum guaranteed size (~1200 bytes) in protocol design; anything larger must use streams
- quinn's `MtuDiscoveryConfig` can tune DPLPMTUD behaviour; keep the default (starts at 1200, binary-searches up)

**Warning signs:**
Intermittent `SendDatagramError::TooLarge`; terminal updates stalling on poor Wi-Fi; screen state diverges without reconnect.

**Phase to address:**
M0 (must be aware in spike); M4 (must be enforced in predictive-echo state-sync design).

---

### Pitfall 3: Idle Timeout Fires on an Interactive Shell That Is "Quiet"

**What goes wrong:**
The QUIC connection drops with `ConnectionError::TimedOut` during an interactive session where the user is reading output but not typing — e.g. watching `tail -f`, waiting for a build, or pausing at a prompt. This manifests as the session dying silently on slow/idle workloads.

**Why it happens:**
`max_idle_timeout` defaults to 30 seconds. The idle clock resets on any QUIC frame — streams, datagrams, ACKs — but if neither side sends application data and `keep_alive_interval` is not set (default: `None`), the connection expires. For a remote shell, "quiet" is normal. 

**How to avoid:**
Set `keep_alive_interval` on the **client** side only (one side is sufficient per the quinn docs):
```rust
transport_config.keep_alive_interval(Some(Duration::from_secs(15)));
```
Set `max_idle_timeout` to something session-appropriate (e.g. several minutes for a shell, not 30 seconds):
```rust
transport_config.max_idle_timeout(Some(Duration::from_secs(300).try_into()?));
```
Do not set `max_idle_timeout(None)` (infinite) — the docs warn this can cause permanently hung futures if the path malfunctions.

**Warning signs:**
Session drops after ~30 seconds of no typing; error is `ConnectionError::TimedOut` not `Reset`; happens reliably when watching long-running commands.

**Phase to address:**
M0 (set reasonable defaults in spike); validate in M2 with interactive shell testing.

---

### Pitfall 4: ALPN Mismatch Fails the QUIC Handshake with a Cryptic Error

**What goes wrong:**
The quinn connection fails at the TLS handshake with a `no_application_protocol` alert (QUIC error code 0x178). Both sides think they have a valid QUIC setup but the connection never opens.

**Why it happens:**
QUIC mandates ALPN; if the server's `alpn_protocols` list and the client's list share no entry, the handshake is aborted per RFC 9001. Developers often forget to set ALPN on one side, or set different byte strings (`b"nosh"` vs `b"nosh/1"`). This is distinct from the TLS certificate verification error — the handshake never gets that far.

**How to avoid:**
Define a single canonical ALPN identifier constant (e.g. `pub const ALPN: &[u8] = b"nosh/0";`) shared by client and server, imported from a `proto` module. Set it on both sides before first use:
```rust
// server
rustls_server_config.alpn_protocols = vec![ALPN.to_vec()];
// client
rustls_client_config.alpn_protocols = vec![ALPN.to_vec()];
```
In M0, assert after handshake that `handshake_data.protocol == Some(ALPN.to_vec())`.

**Warning signs:**
`ConnectionError` or `TransportError` during handshake with error code 0x178; "no application protocol" in error message.

**Phase to address:**
M0 — define the ALPN constant on day one.

---

### Pitfall 5: Custom `ServerCertVerifier` / `ClientCertVerifier` That "Pins the Key" But Skips Signature Validation

**What goes wrong:**
The custom verifier returns `Ok(ServerCertVerified::assertion())` from `verify_server_cert` after checking the public key, but `verify_tls13_signature` is implemented as a stub that also returns `Ok(HandshakeSignatureValid::assertion())` — meaning the peer's `CertificateVerify` signature is never actually checked. A MITM can present a cert with the correct pinned public key but sign the handshake transcript with any private key.

**Why it happens:**
The canonical "skip PKI" example in the quinn docs (`SkipServerVerification`) is intentionally all-stubs because it's a dev/testing helper. Developers copy it as the starting point for key-pinning auth without realising that the signature methods must be properly implemented.

**How to avoid:**
The correct "pin the key" verifier must:
1. In `verify_server_cert`: extract the public key from the raw certificate/SubjectPublicKeyInfo, compare it to the pinned value, return `Err` on mismatch.
2. In `verify_tls13_signature`: call `rustls_platform_verifier` or delegate to the `CryptoProvider`'s signer — do not stub this. Use `provider.signature_verification_algorithms` to verify the transcript signature against the extracted public key.
3. In `supported_verify_schemes`: return only the schemes your keys actually support (e.g. only `Ed25519` if only Ed25519 keys are used).

Additionally: for ECDSA keys in TLS 1.3, rustls **does not enforce curve matching** for you — if `ECDSA_NISTP256_SHA256` is returned by `supported_verify_schemes`, your `verify_tls13_signature` implementation must check that the public key is actually on P-256, not merely that it's an ECDSA key.

**Warning signs:**
Tests pass against a deliberately invalid key; `verify_tls13_signature` is a one-liner returning `Ok(assertion())`.

**Phase to address:**
M1 — auth is the milestone where the verifier is written. Write tests that present a cert with the right key but a forged signature.

---

### Pitfall 6: RFC 7250 Raw Public Key — `requires_raw_public_keys()` Opt-In is Separate from Verifier Logic

**What goes wrong:**
You implement a custom `ClientCertVerifier` (server-side) and `ServerCertVerifier` (client-side) designed to accept raw public keys, but the TLS handshake still negotiates X.509 and sends self-signed certs (or fails with `UnsolicitedCertificateTypeExtension`). The RPK extension is never activated.

**Why it happens:**
rustls exposes `requires_raw_public_keys()` as a provided method on `ClientCertVerifier` / `ResolvesClientCert::only_raw_public_keys()`. These must return `true` to signal RPK intent in the ClientHello/ServerHello extensions. If left at the default `false`, the RPK extension is not advertised, and the handshake falls back to X.509 — or fails with the RFC 7250 § 4.2 compliance bug (issue #2257): a server that gets a ClientHello advertising both X.509 and RPK may send `UnsolicitedCertificateTypeExtension` instead of accepting X.509.

**How to avoid:**
If using RPK:
- Override `requires_raw_public_keys()` → `true` in both client and server verifiers
- Use `CertificateDer` wrapping a `SubjectPublicKeyInfo` DER blob, not a full X.509 cert
- Verify that the rustls version in use has resolved issue #2257 (check the changelog); fall back to self-signed-cert pinning if not

If using the self-signed-cert-pinning fallback (acceptable for M1):
- Keep `requires_raw_public_keys()` at `false`
- Generate an ephemeral self-signed X.509 cert whose Subject Public Key Info embeds the SSH public key
- The verifier skips chain validation but verifies the SPKI matches the pinned key

**Warning signs:**
Handshake fails with `UnsolicitedCertificateTypeExtension`; `CertificateVerify` contains a full cert chain rather than a raw key blob; verifier's `verify_server_cert` receives a cert with unexpected structure.

**Phase to address:**
M1 — design decision: start with self-signed-cert-pinning (simpler, no RPK bug exposure), add RPK in a follow-up once rustls RPK maturity is confirmed.

---

### Pitfall 7: ssh-agent RSA Key Returns `ssh-rsa` (SHA-1) When `rsa-sha2-256`/`rsa-sha2-512` Was Required

**What goes wrong:**
The agent signing call returns a signature using the legacy `ssh-rsa` algorithm (SHA-1), but the TLS `CertificateVerify` message requires `rsa-sha2-256` or `rsa-sha2-512` (the SHA-2 family). The signature verification fails on the peer side, and the handshake is rejected.

**Why it happens:**
The OpenSSH agent protocol's `SSH_AGENTC_SIGN_REQUEST` message includes a flags field. To request SHA-2 variants, the caller must explicitly set flag `SSH_AGENT_RSA_SHA2_256` (0x2) or `SSH_AGENT_RSA_SHA2_512` (0x4). Older agent code or wrappers that omit the flags field default to the legacy algorithm. The `ssh-agent-client-rs` or `russh` agent implementations may or may not set these flags correctly by default — verify at implementation time.

**How to avoid:**
- For RSA keys, always request the signing operation with explicit `rsa-sha2-256` or `rsa-sha2-512` flags in the agent sign request
- Map the TLS `SignatureScheme` to the correct agent flag: `RSA_PKCS1_SHA256` → flag 0x2, `RSA_PKCS1_SHA512` → flag 0x4
- After receiving the signature from the agent, verify the returned algorithm name matches what was requested before inserting it into the TLS message
- For Ed25519 and ECDSA keys, no flag is needed — the algorithm is fixed

**Warning signs:**
Agent returns signature type `ssh-rsa` when `rsa-sha2-256` was expected (this may appear as a warning log in some agent implementations); TLS handshake failure on RSA host keys but not Ed25519 keys.

**Phase to address:**
M1 — test with RSA keys specifically, not just Ed25519.

---

### Pitfall 8: TLS Transcript Signed via ssh-agent Uses the Wrong Bytes

**What goes wrong:**
The `CertificateVerify` signature verification fails even though the agent signing call succeeded. The agent signed the correct private key but over the wrong input: the raw transcript hash rather than the TLS 1.3 `CertificateVerify` message structure, or vice versa.

**Why it happens:**
TLS 1.3 `CertificateVerify` signs a specific structure: 64 space bytes, a context string (`TLS 1.3, client CertificateVerify` or `TLS 1.3, server CertificateVerify`), a 0x00 separator, then the transcript hash (RFC 8446 §4.4.3). The agent only signs raw bytes — it has no knowledge of TLS framing. Passing only the transcript hash (without the prefix structure) or computing the hash at the wrong point in the handshake produces a structurally correct but cryptographically invalid signature.

**How to avoid:**
Construct the full `CertificateVerify` input per RFC 8446 §4.4.3 before passing to the agent:
```
input = repeat(0x20, 64) || context_string || 0x00 || transcript_hash
```
Where `transcript_hash = Hash(Handshake Context)`. This entire input — not just the hash — is what gets signed. The agent call then signs this byte string as-is (for Ed25519/ECDSA) or hashes-and-signs it (for RSA with rsa-sha2-256).

Note: rustls's `verify_tls13_signature` passes the pre-constructed message (already including the prefix) to your verifier. Match the signer to produce bytes in the same format.

**Warning signs:**
Auth works with a software key (where you control the signing path) but fails when routed through ssh-agent; `CertificateVerify` parse errors on the peer; signature mismatch errors that are key-type-agnostic.

**Phase to address:**
M1 — write an explicit unit test that constructs the TLS 1.3 CertificateVerify input by hand and verifies agent round-trip.

---

## Technical Debt Patterns

| Shortcut | Immediate Benefit | Long-term Cost | When Acceptable |
|----------|-------------------|----------------|-----------------|
| `SkipServerVerification` stub as the actual verifier | Unblocks M0 quickly | No auth at all; MITM trivial | M0 spike only, never land on main with this active |
| Full environment passthrough from client to server | Simpler session-open protocol | Privilege escalation footgun | Never; whitelist from day one |
| `max_idle_timeout(None)` (infinite) | No dropped sessions in testing | Hung futures on broken paths | Never in production; use long but finite timeout |
| Hard-coded datagram buffer size without checking `max_datagram_size()` | Simpler send path | Silent drops or errors on constrained paths | Only for M0 where path is loopback; must fix in M2 |
| Not calling `portable-pty`'s `wait()` before dropping the PTY pair | Simpler cleanup | Zombie child processes accumulate | Never; implement Drop properly |

---

## Integration Gotchas

| Integration | Common Mistake | Correct Approach |
|-------------|----------------|------------------|
| quinn + rustls ALPN | Forgetting to set `alpn_protocols` on one side | Define `ALPN` constant in `proto` crate, set on both sides in every config |
| rustls dangerous verifier | Copying `SkipServerVerification` verbatim for key pinning | Implement real signature verification in `verify_tls13_signature` |
| ssh-agent RSA signing | Not setting SHA-2 flags in sign request | Map `SignatureScheme` → agent flags before every RSA sign call |
| quinn datagrams | Leaving `datagram_receive_buffer_size` at `None` | Explicitly set non-`None` on both endpoints |
| portable-pty + tokio | Calling blocking `Child::wait()` on the async thread | Spawn `wait()` in `tokio::task::spawn_blocking` |
| PTY client raw mode | Not restoring terminal on panic or network drop | Use RAII guard implementing `Drop` to call `disable_raw_mode()` |

---

## Performance Traps

| Trap | Symptoms | Prevention | When It Breaks |
|------|----------|------------|----------------|
| SIGWINCH burst on window drag fires a resize per pixel row | PTY resize floods the QUIC stream; interactive latency spikes | Coalesce resize events with a 30–50 ms debounce timer | Immediate; any drag of the terminal window |
| Polling `MasterPty::read()` in a tight loop | 100% CPU on the server | Use `tokio::io::AsyncReadExt` on the PTY master fd | Always; even in spike |
| Sending datagrams without checking `max_datagram_size()` in a loop | Intermittent `TooLarge` errors on path-MTU reduction | Check before send; clamp or skip | On lossy/constrained network paths |
| Reading large `recv_datagram()` in the async executor without yielding | Starves other tasks sharing the executor | Use `tokio::task::spawn_blocking` or bounded buffer | With large terminal output bursts |

---

## Security Mistakes

| Mistake | Risk | Prevention |
|---------|------|------------|
| Env passthrough includes `LD_PRELOAD` / `BASH_ENV` | Arbitrary code execution in shell context | Deny-all env whitelist at session open (MUST-ADDRESS M2) |
| `SSH_AUTH_SOCK` forwarded via env | SSH key hijacking by server root | Exclude from env whitelist; agent forwarding uses dedicated stream in M5 |
| `verify_tls13_signature` stubbed as `Ok(assertion())` | MITM can forge any handshake | Delegate to `CryptoProvider` signature verification algorithms |
| ECDSA curve not checked in custom verifier | Cross-curve signature forgery | Check curve in `verify_tls13_signature` for ECDSA — rustls does not do this for you |
| Unlimited half-open connections | Memory exhaustion DoS (CVE-2024-22189 pattern) | Cap pre-auth connections; abort on auth timeout (MUST-ADDRESS M1) |
| Infinite `max_idle_timeout` | Hung futures on broken path | Use a finite timeout (minutes, not infinite) |
| PTY spawned without setsid / controlling terminal isolation | Shell signals can escape to server process | Verify `portable-pty` calls `setsid()` on slave side (it does on Linux); do not give child a reference to server's controlling terminal |

---

## UX Pitfalls

| Pitfall | User Impact | Better Approach |
|---------|-------------|-----------------|
| Client terminal left in raw mode after abrupt disconnect | Shell appears broken (no echo, garbled input) after `nosh` exits | RAII guard on client that calls `disable_raw_mode()` in `Drop` |
| Exit code not propagated from PTY child | Callers (`make`, scripts) see exit 0 on remote failure | Poll `Child::try_wait()` in the event loop; forward exit code in session-close control message |
| SIGWINCH not sent after reconnect | Remote editor/pager wrong size after cold reattach | Send a resize event as part of session resume handshake |
| Shell output after "connection closed" races with terminal restore | Garbled output on exit | Flush and drain the PTY master before restoring terminal mode |

---

## "Looks Done But Isn't" Checklist

- [ ] **Datagrams enabled:** Assert `connection.max_datagram_size()` returns `Some(_)` — if `None`, one side left `datagram_receive_buffer_size` at default
- [ ] **Auth actually authenticates:** Test that a connection attempt with an unknown key is rejected — not just that a known key is accepted
- [ ] **Signature actually verified:** Test that a cert with the correct pinned key but a forged `CertificateVerify` signature is rejected
- [ ] **RSA key path tested:** If only tested with Ed25519, the agent RSA SHA-2 flag path is untested
- [ ] **Terminal raw mode restored:** Kill the client process with SIGKILL during a session; verify the local terminal is still usable
- [ ] **Zombie cleanup:** Disconnect mid-session; verify `ps aux | grep Z` shows no zombie PTY children
- [ ] **Env whitelist enforced:** Start a session, check the shell's environment for `LD_PRELOAD` and `SSH_AUTH_SOCK` — both must be absent
- [ ] **ALPN validated post-handshake:** Log `handshake_data.protocol` and assert it equals the expected ALPN token
- [ ] **Keep-alive configured:** Leave a connected session idle for 60 seconds with no input; it must not drop

---

## Recovery Strategies

| Pitfall | Recovery Cost | Recovery Steps |
|---------|---------------|----------------|
| Datagrams silently disabled | LOW | Add `datagram_receive_buffer_size(Some(N))` to TransportConfig on both sides; retest M0 |
| ALPN mismatch | LOW | Introduce shared `ALPN` constant; update both sides |
| Auth verifier stub in production | HIGH | Full rewrite of verifier; audit all connections made during the window |
| Env sanitization missing | HIGH | Add whitelist + test; audit server for signs of exploitation before shipping |
| Zombie PTY children | MEDIUM | Add explicit `Drop` impl calling `kill()` + `wait()` on the child |
| Terminal raw mode not restored | MEDIUM | Add RAII guard with `Drop`; test with deliberate panic |

---

## Pitfall-to-Phase Mapping

| Pitfall | Prevention Phase | Verification |
|---------|------------------|--------------|
| Datagrams silently disabled | M0 | Assert `max_datagram_size()` returns `Some(_)` in M0 integration test |
| Datagram size exceeds MTU | M0 (awareness) / M4 (enforcement) | Test on loopback (always passes) and across a simulated constrained path |
| Idle timeout drops quiet session | M0 (set keep-alive) / M2 (validate) | Leave interactive session idle 2× timeout; session must survive |
| ALPN mismatch | M0 | Post-handshake ALPN assertion in M0 test |
| Verifier stub passes forged signatures | M1 | Negative test: forged CertificateVerify must be rejected |
| RPK vs self-signed-cert path confusion | M1 | Decide early; one path only in spike |
| ssh-agent RSA SHA-1 fallback | M1 | Explicit RSA key test with SHA-2 verification |
| TLS transcript bytes wrong | M1 | Unit test: known key + known transcript + expected signature |
| Env variable injection (FOOTGUN-1) | M2 | Integration test: attempt to inject `LD_PRELOAD`; verify absent from shell env |
| SSH_AUTH_SOCK via env (FOOTGUN-2) | M2 | Integration test: verify `SSH_AUTH_SOCK` absent from shell env |
| Half-open connection DoS (FOOTGUN-3) | M1 | Load test: flood with unauthenticated connections; memory must stay bounded |
| Terminal raw mode not restored | M2 | Kill client with SIGKILL mid-session; verify local terminal usable |
| Zombie PTY children | M2 | Disconnect mid-session; verify no zombies |
| SIGWINCH burst storms | M2 | Drag terminal window; verify resize events coalesced |
| Exit code not propagated | M2 | `exit 42` in remote shell; local client must report exit 42 |

---

## Sources

- Quinn 0.11.8 `TransportConfig` docs (Context7 + docs.rs): `datagram_receive_buffer_size` defaults to `None`; `max_idle_timeout` defaults to 30 s; `keep_alive_interval` defaults to `None` — https://docs.rs/quinn/0.11.8/quinn/struct.TransportConfig.html
- Quinn `Connection::max_datagram_size()` docs (Context7): returns `None` when disabled locally or by peer — https://docs.rs/quinn/0.11.8/quinn/struct.Connection.html
- rustls `ServerCertVerifier` trait docs: ECDSA curve enforcement not done by rustls — https://docs.rs/rustls/latest/rustls/client/danger/trait.ServerCertVerifier.html
- rustls issue #2257: `UnsolicitedCertificateTypeExtension` non-compliance with RFC 7250 — https://github.com/rustls/rustls/issues/2257
- Quinn certificate guide: `SkipServerVerification` pattern and `dangerous()` API — https://quinn-rs.github.io/quinn/quinn/certificate.html
- OpenSSH agent RSA SHA-2 flags (asyncssh issue #795): `SSH_AGENT_RSA_SHA2_256` flag 0x2, `SSH_AGENT_RSA_SHA2_512` flag 0x4 — https://github.com/ronf/asyncssh/issues/795
- CVE-2024-22189: QUIC `NEW_CONNECTION_ID` frame flooding → memory exhaustion — https://ogma.in/cve-2024-22189-mitigating-memory-exhaustion-attack-in-quic-s-connection-id-mechanism
- RFC 8446 §4.4.3: TLS 1.3 `CertificateVerify` input construction (64 spaces + context + 0x00 + transcript hash)
- RFC 9221: Unreliable Datagram Extension to QUIC — https://datatracker.ietf.org/doc/html/rfc9221
- QUIC ALPN mandatory (`no_application_protocol` error 0x178): QUIC base-drafts wiki — https://github.com/quicwg/base-drafts/wiki/ALPN-IDs-used-with-QUIC
- SSH agent forwarding socket hijacking — https://rabexc.org/posts/pitfalls-of-ssh-agents
- portable-pty zombie process pitfall — https://docs.rs/portable-pty/latest/portable_pty/
- Terminal raw mode not restored on SIGKILL — https://github.com/slopus/happy/issues/423
- LD_PRELOAD privilege escalation mechanics — https://www.elttam.com/blog/env
- INIT.md §5, §12, §13 (quicshell env sanitization and DoS hardening design notes)

---
*Pitfalls research for: QUIC-based roaming remote shell (nosh), M0–M2 spike surface*
*Researched: 2026-05-29*
