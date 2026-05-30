# Phase 8: Windows Client — Research

**Researched:** 2026-05-30
**Confidence:** HIGH (codebase verified by direct read; crate APIs cross-checked against STACK.md, which verified docs.rs live data)
**Scope:** What the planner needs to plan Phase 8 well. Builds on the v1.1 STACK.md / PITFALLS.md research already in `.planning/research/`. This file resolves those generic findings against the *current settled code* (post–Phase 6 reconnect supervisor).

---

## 1. Phase goal restated

A native Windows client (no WSL) connects to and authenticates against a Linux nosh server, signing from an on-disk OpenSSH Ed25519 private key, with a working interactive session (raw VT mode, terminal resize, correct locale). All platform-specific work is confined to `nosh-client` behind `#[cfg]` gates; `nosh-proto` / `nosh-auth` / `nosh-server` stay platform-agnostic.

Requirements: **WIN-01** (connect+auth, cross-compiles), **WIN-02** (on-disk Ed25519 signing, narrow scope, zeroized), **WIN-03** (raw VT mode + resize via console events, not SIGWINCH), **WIN-04** (TERM/locale propagation).

Automated gate (D-01): `cargo check --target x86_64-pc-windows-gnu` from Linux CI. Real interactive behavior (D-02): DOCUMENTED human Windows test, NON-BLOCKING (phase marked `human_needed`).

---

## 2. Current code — exact surfaces to extend

### 2.1 The signer trait boundary (nosh-auth — stays platform-agnostic)

`crates/nosh-auth/src/signer.rs`:

```rust
pub trait RawEd25519Signer: Send + Sync + std::fmt::Debug {
    fn sign(&self, msg: &[u8]) -> anyhow::Result<[u8; 64]>;
    fn public_key32(&self) -> [u8; 32];
}
```

Two existing impls: `AgentSigner` (ssh-agent) and `InProcessEd25519Signer` (wraps `ed25519_dalek::SigningKey`; has `from_ssh_private(&ssh_key::PrivateKey)` already). **`FileSigner` is a third sibling impl in this same file.** It loads an on-disk OpenSSH Ed25519 private key and signs in-process. Critically, `InProcessEd25519Signer::from_ssh_private` *already does the exact load logic* (`private.key_data().ed25519()` → `ed25519_dalek::SigningKey::from_bytes(&kp.private.to_bytes())`) — `FileSigner` is essentially: read file → detect encryption → reuse that conversion → hold the dalek key → zeroize on drop.

**Design choice for D-05 (zeroize):** `ed25519_dalek::SigningKey` already implements `ZeroizeOnDrop` (dalek 2.x zeroizes its secret on drop). The `[u8; 32]` seed extracted via `kp.private.to_bytes()` is the transient that must be zeroized. `ssh_key::PrivateKey` itself wraps key material in `Zeroizing` internally. So the discipline is: keep the `ssh_key::PrivateKey` and the `[u8;32]` seed in the narrowest scope (the `FileSigner::from_path` constructor), zeroize the seed copy explicitly (`zeroize::Zeroize::zeroize(&mut seed)` or hold it in `zeroize::Zeroizing<[u8;32]>`), and let `FileSigner` hold only the `ed25519_dalek::SigningKey` (which is itself zeroize-on-drop). Add the `zeroize` crate to nosh-auth (it is already a transitive dep via ed25519-dalek; promote to a direct dep for the explicit seed scrub).

**Why FileSigner holds a dalek key, not the ssh_key::PrivateKey:** keeping the dalek key matches `InProcessEd25519Signer` exactly and lets the same `sign()` body be reused. The `ssh_key::PrivateKey` (and any decrypted intermediate) is dropped at end of constructor — satisfying Pitfall 12's "load, build signer, drop" discipline. `FileSigner` is NOT held across an await point beyond what `AgentSigner`/`InProcessEd25519Signer` already are (the `Arc<dyn RawEd25519Signer>` lives for the connection, but it only holds the 32-byte dalek secret, the same exposure `InProcessEd25519Signer` already accepts for the server host key — and dalek zeroizes it on final drop).

**Debug must not leak key bytes (D-05):** `FileSigner`'s `Debug` impl must NOT print the key. Implement `Debug` manually to print only `FileSigner { fingerprint }` (mirror `NoshPublicKey`'s manual Debug at keys.rs:44). The trait requires `Debug`; the derived impl on a struct holding a dalek key would not print the secret (dalek's Debug is redacted), but a manual impl is the safe, explicit choice and documents intent.

### 2.2 ClientIdentity (nosh-client/src/client.rs:24-66)

```rust
pub struct ClientIdentity { signer: Arc<dyn RawEd25519Signer> }
impl ClientIdentity {
    pub fn from_signer(signer: Arc<dyn RawEd25519Signer>) -> Self { ... }
    pub fn from_agent(socket_path: PathBuf, identity_pub: Option<&Path>) -> anyhow::Result<Self> { ... }
}
```

Add a third constructor `from_identity_file(path: &Path) -> anyhow::Result<Self>` that builds a `FileSigner` and wraps it. This is platform-agnostic (works on Linux too — D-03 opt-in) so it lives in the un-gated part of client.rs. It returns the same `Self { signer: Arc::new(file_signer) }` shape. No `#[cfg]` here.

`build_client_config` (client.rs:71-103) is unchanged: it already takes `&ClientIdentity` and uses `identity.signer` generically through `mint_self_signed_cert` + `AgentSigningKey`. **`FileSigner` slots in with zero changes to the cert/handshake path.**

### 2.3 main.rs — the platform-gated surface (nosh-client/src/main.rs)

This is where ALL `#[cfg]` gates live. Current settled state (post–Phase 6 reconnect supervisor):

- **Args (lines 47-69):** `--addr`, `--port`, `--host`, `--identity` (agent key selector), `--known_hosts`. Add `--identity-file <PathBuf>` (D-03).
- **Auth selection (lines 100-103):** currently HARD-requires `SSH_AUTH_SOCK`, then `ClientIdentity::from_agent`. This must become:
  - If `--identity-file` is given → `ClientIdentity::from_identity_file(path)` (all platforms; relax the hard `SSH_AUTH_SOCK` requirement — do not read the env var at all on this branch).
  - Else, on `#[cfg(unix)]` → existing agent path (require `SSH_AUTH_SOCK`).
  - Else, on `#[cfg(windows)]` with no `--identity-file` → error: agent is unavailable on Windows; `--identity-file` is required. (D-04: on Windows `--identity-file` is the ONLY path.)
  - Default `--identity-file` on Windows when omitted (D-09 discretion): least-surprising is to default to `%USERPROFILE%\.ssh\id_ed25519` (via `dirs::home_dir()`) and error clearly if absent, rather than forcing the flag. Document the default.
- **SIGWINCH handler (line 128):** `tokio::signal::unix::{signal, SignalKind}` and `winch` are Unix-only APIs. The `use` at line 30 and the `winch` signal install + the `winch.recv()` arm in `run_pump` must be `#[cfg(unix)]`-gated. SIGINT (line 125) is also Unix `tokio::signal::unix`; the reconnect-quit escape currently uses it. **This is the largest cross-platform refactor**: `signal(SignalKind::interrupt())` and `signal(SignalKind::window_change())` do not exist on Windows.
- **Resize on Windows (D-08):** use `crossterm::event::EventStream` and match `Event::Resize`. On resize, re-read `crossterm::terminal::size()` for the authoritative dimensions (Pitfall 14), apply the same ~40 ms debounce (`RESIZE_DEBOUNCE`), and send the existing `Message::Resize` via `client::send_resize`. The EventStream also surfaces `Event::Key` — but the client reads keystrokes from `tokio::io::stdin()` already; on Windows, raw-mode stdin reads work via crossterm's console setup, so **keystroke input stays on the stdin read path** and EventStream is used ONLY for resize (and, optionally, Ctrl-C detection). Keeping input on stdin avoids re-plumbing the whole pump for two platforms.
- **SIGINT-equivalent on Windows:** `tokio::signal::ctrl_c()` is cross-platform and is the right replacement for the reconnect-window quit. The current code uses `tokio::signal::unix::Signal` typed parameters threaded through `fresh_session`/`reattach_session`/`run_pump`. The cleanest refactor (Claude's discretion, D's note) is to abstract the resize trigger behind a small platform module or a `#[cfg]`-split helper rather than threading a Unix `Signal` type through three function signatures.

### 2.4 Env forwarding (D-09)

`collect_client_env()` (client.rs:228-236) already whitelists `TERM`, `LANG`, `TZ`, `LC_*` and forwards them in `SessionOpen`. It does NOT currently default `TERM` or `LANG` when unset — it only forwards what exists. main.rs:114 defaults `TERM` to `xterm-256color` for the *local* `term` variable passed to `open_session`, but the `env` vec from `collect_client_env()` would omit `TERM`/`LANG` entirely if unset on Windows (where neither is typically set).

**D-09 requires:** ensure the forwarded env includes `TERM` (default `xterm-256color`) and `LANG` (default `en_US.UTF-8`) when unset. Cleanest: add defaulting inside `collect_client_env()` (or a small wrapper) so the remote Linux shell always receives both. This is platform-agnostic logic (helps headless tests too) but is most impactful on Windows. The server already deny-by-default re-filters env, and `TERM`/`LANG`/`LC_*` are on its whitelist, so no server change is needed.

### 2.5 Key-file permission warning (D-10)

Best-effort, non-fatal. On `#[cfg(unix)]`: check `std::fs::metadata(path)?.permissions().mode() & 0o077 != 0` (group/other access) → warn. On `#[cfg(windows)]`: `std::fs::Permissions` cannot read ACLs — emit a documented best-effort note (e.g. warn that ACL-based protection is not verified and the user should ensure the file is not shared). Never hard-refuse. This belongs in `from_identity_file` (or a helper it calls) since it is tied to loading a key file. Use `std::os::unix::fs::PermissionsExt` under `#[cfg(unix)]`.

---

## 3. Cargo.toml changes (nosh-client + nosh-auth)

### nosh-client/Cargo.toml (line 29 today: `crossterm = "0.28"`)

```toml
crossterm = { version = "0.29", features = ["events", "event-stream"] }
futures = "0.3"   # StreamExt for EventStream
```

- Do NOT enable `use-dev-tty` (crossterm #935 — breaks Windows event-stream build).
- `ssh-agent-client-rs` (line 28) is Unix-socket-only; it must NOT be a hard dependency on Windows. Gate it: move it under `[target.'cfg(unix)'.dependencies]`. The agent code path in client.rs (`from_agent`, `ssh_agent_connect`) and signer.rs (`AgentSigner`) must then be `#[cfg(unix)]`-gated too — OR keep `AgentSigner` compiled everywhere but gate the *client* agent entrypoint. **Decision for the planner:** the cleanest minimal change is to gate `ssh-agent-client-rs` to unix in BOTH nosh-auth and nosh-client, and `#[cfg(unix)]`-gate `AgentSigner` (signer.rs) and `from_agent`/`ssh_agent_connect` (client.rs). This keeps the Windows build free of the Unix-socket crate. NOTE: this DOES place a `#[cfg(unix)]` in nosh-auth — re-read the constraint: "ALL platform `#[cfg]` gates confined to nosh-client." See §6 for the resolution.

### nosh-auth/Cargo.toml

```toml
zeroize = "1"  # promote from transitive (ed25519-dalek dep) to direct, for explicit seed scrub
# ssh-key already has ed25519/std/alloc. Add "encryption" gated to windows so
# is_encrypted()/decrypt() detection compiles; on Linux is_encrypted() works
# WITHOUT the encryption feature (it only reads the cipher name), so the feature
# is only needed if we ever call decrypt() — which we do NOT in v1.1 (D-06).
```

**Encrypted-key detection (D-06):** `ssh_key::PrivateKey::is_encrypted()` is available WITHOUT the `encryption` feature — it inspects the key's cipher field. We only *detect* and error; we never call `decrypt()` (deferred to P2/WIN-06). So the `encryption` feature is NOT required for v1.1. STACK.md suggested gating `encryption` to Windows for `decrypt()`, but since v1.1 only detects, **the encryption feature is not needed at all** — confirm `is_encrypted()` is callable with current features (it is; it is on `PrivateKey` unconditionally). This simplifies the Cargo change: no target-gated ssh-key feature needed.

---

## 4. The ssh-agent-on-Windows problem (the real cross-compile blocker)

`cargo check --target x86_64-pc-windows-gnu` will FAIL today because `ssh-agent-client-rs` (a Unix-domain-socket client) is an unconditional dependency of both nosh-auth and nosh-client. This is THE thing the automated gate (D-01) exists to catch.

Resolution path (the planner must pick and the checker must verify it compiles for windows-gnu):
1. Make `ssh-agent-client-rs` a `[target.'cfg(unix)'.dependencies]` entry in nosh-auth AND nosh-client.
2. `#[cfg(unix)]`-gate `AgentSigner` + its impls (signer.rs) and `from_agent` + `ssh_agent_connect` (client.rs) and the `AgentSigner`/`AgentSigningKey` re-export wherever `AgentSigner` is named. (`AgentSigningKey` does NOT depend on ssh-agent — it wraps any `RawEd25519Signer` — keep it un-gated.)
3. The auth integration test `agent_ed25519_handshake_live` (auth.rs:206) and `agent_ed25519_sign_roundtrip` (signer.rs:326) are already `#[ignore]` + Unix; gate them `#[cfg(unix)]` so the Windows test build does not reference `AgentSigner`/`test_support` agent bits.

Toolchain note for CI (D-01): `cargo check --target x86_64-pc-windows-gnu` requires the target installed (`rustup target add x86_64-pc-windows-gnu`) AND the gnu linker is NOT invoked by `cargo check` (check does no linking), so no `mingw-w64` is strictly needed for `check`. `ring` 0.17.x ships precompiled Windows asm, so even a future `cargo build` would not need NASM. The CI step is therefore: `rustup target add x86_64-pc-windows-gnu && cargo check -p nosh-client --target x86_64-pc-windows-gnu`. There is no CI workflow file in the repo today (`.github/workflows/` does not exist) — the plan should ADD the target-add + check as a documented command (and, if a CI file is created, as a job step), but the binding automated gate is simply running that command and getting exit 0.

---

## 5. Testing strategy

- **Linux headless `FileSigner` test (D-03 opt-in pays off here):** the existing `TestKey` harness (`tests/common/mod.rs`) already exposes `ssh_private() -> ssh_key::PrivateKey`. A new test writes that key to a temp file via `PrivateKey::write_openssh_file(path, LineEnding::LF)`, builds `ClientIdentity::from_identity_file(path)`, and runs the existing `mutual_auth` happy-path against an in-process server authorizing that key. This validates `FileSigner` end-to-end on Linux CI — no Windows box needed for the signing logic itself.
- **Encrypted-key error test (D-06):** write an encrypted OpenSSH key fixture (or construct one) and assert `from_identity_file` returns an error whose message guides the user (unencrypted key / ssh-agent on Linux) and does NOT contain key bytes.
- **Permission-warning test (D-10, Unix):** chmod a key file to 0644, assert a warning is emitted (and loading still succeeds — non-fatal).
- **Cross-compile gate (D-01):** `cargo check -p nosh-client --target x86_64-pc-windows-gnu` exits 0.
- **Human Windows test (D-02, NON-BLOCKING):** a documented manual procedure file (e.g. `docs/windows-client-test.md` or a section in the phase notes) listing the steps: build on Windows, run against a Linux server with `--identity-file`, verify raw mode, resize in Windows Terminal, and locale rendering. Phase is marked `human_needed`; the operator records PASSED. Mirrors Phase 7's live-check pattern.

---

## 6. The "#[cfg] confined to nosh-client" constraint vs. ssh-agent gating

The locked constraint says all platform `#[cfg]` gates live in nosh-client, keeping nosh-auth platform-agnostic. But `AgentSigner` (in nosh-auth) depends on `ssh-agent-client-rs`, a Unix-only crate that breaks the Windows build.

**Resolution (recommend to planner; surface as a plan decision):** "platform-agnostic" for nosh-auth means *no functional behavior forks by platform* — the SPKI/cert/sign logic is identical everywhere. Gating an optional, Unix-only *dependency* (ssh-agent) is a build-availability gate, not a behavioral fork, and is the standard Rust idiom (`[target.'cfg(unix)'.dependencies]`). The cleanest interpretation that keeps the spirit of the constraint:

- Gate `ssh-agent-client-rs` to `cfg(unix)` in nosh-auth and `#[cfg(unix)]` the `AgentSigner` type. This is a SINGLE, well-contained cfg in nosh-auth tied purely to dependency availability. `FileSigner`, `RawEd25519Signer`, `InProcessEd25519Signer`, `AgentSigningKey`, cert minting — all stay un-gated and identical on every platform.
- ALL the *behavioral* platform splits (SIGWINCH vs EventStream, auth-path selection, permission check, env defaulting) live in nosh-client per the constraint.

This is the minimal, idiomatic resolution. The planner should document it explicitly in the plan that touches nosh-auth so the checker does not flag the nosh-auth cfg as a constraint violation. (Alternative — keep `AgentSigner` fully compiled on Windows by replacing `ssh-agent-client-rs` with a Windows-named-pipe shim — is out of scope: Pageant is WIN-05, deferred.)

---

## 7. Wave / plan shape (recommendation to planner)

The work has a natural dependency spine:

1. **FileSigner + ssh-agent gating (nosh-auth) + ClientIdentity::from_identity_file (nosh-client) + Linux headless tests.** Foundation: everything else depends on the signer existing and the Windows build no longer pulling ssh-agent. (WIN-01 partial, WIN-02, D-03/04/05/06/10.) — Wave 1.
2. **Cargo deps + crossterm 0.29 bump + futures + cross-compile gate + the main.rs platform split (auth selection, SIGWINCH→EventStream resize, ctrl_c, env defaulting).** Depends on Wave 1 (needs `from_identity_file` to exist for the auth-selection branch). (WIN-01, WIN-03, WIN-04, D-07/08/09.) — Wave 2.
3. **Documentation + human-test procedure + mark human_needed.** Depends on Wave 2. (D-02.) — Wave 3, or fold into Wave 2's plan as a final task.

A 2-plan structure (Wave 1: auth/signer foundation; Wave 2: terminal/cargo/main.rs + docs) is clean and avoids over-splitting. A 3rd tiny plan for the human-test doc is optional; folding it into Wave 2 keeps the count tight.

---

## 8. Key risks / landmines for the planner

- **windows-gnu vs msvc:** the gate is `windows-gnu` (D-01) specifically because it cross-compiles from Linux without a Windows toolchain. `cargo check` does no linking, so neither mingw nor MSVC is needed for the gate. Do NOT switch the gate to msvc (needs the MS linker, unavailable on Linux CI).
- **ssh-agent-client-rs is the blocker** (§4) — if it is not gated to unix, the windows-gnu check fails immediately. This is the #1 thing to get right.
- **Threading the Unix `Signal` type** through `fresh_session`/`reattach_session`/`run_pump` (main.rs) is the messiest cross-platform edit. Recommend a small `#[cfg]`-split helper / platform module for the resize+quit triggers rather than `#[cfg]`-ing inside three signatures.
- **Keystroke input on Windows:** keep it on `tokio::io::stdin()` (crossterm raw mode makes this work); use EventStream ONLY for resize. Re-plumbing input through EventStream for two platforms is unnecessary scope.
- **`is_encrypted()` needs no feature** — do not add the ssh-key `encryption` feature for v1.1 (we only detect, never decrypt). Adding it is harmless but unnecessary; the planner should keep deps minimal.
- **Never log key bytes** (D-05): manual `Debug` for `FileSigner`; error messages on encrypted/invalid keys must not echo file contents.

---

## Sources

- Codebase (direct read, post–Phase 6): `crates/nosh-auth/src/signer.rs`, `keys.rs`, `lib.rs`, `Cargo.toml`; `crates/nosh-client/src/client.rs`, `main.rs`, `lib.rs`, `Cargo.toml`; `crates/nosh-client/tests/{auth.rs,common/mod.rs}`; workspace `Cargo.toml`.
- `.planning/research/STACK.md` §2 (crossterm 0.29, event-stream, #935), §3 (ssh-key on-disk loading, is_encrypted/decrypt, encryption feature deps; ring precompiled Windows asm).
- `.planning/research/PITFALLS.md` #12 (key in memory — narrow scope + zeroize), #13 (ACL gap), #14 (WINDOW_BUFFER_SIZE_RECORD vs SIGWINCH → EventStream + re-read size()), #15 (VT processing legacy hosts).
- `.planning/phases/08-windows-client/08-CONTEXT.md` (locked decisions D-01..D-10).
- ed25519-dalek 2.x ZeroizeOnDrop on SigningKey; ssh-key Zeroizing-wrapped key material (STACK.md sources, docs.rs verified).

---
*Research for Phase 8 Windows Client. Planning-ready.*
