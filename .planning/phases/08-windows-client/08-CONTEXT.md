# Phase 8: Windows Client - Context

**Gathered:** 2026-05-30
**Status:** Ready for planning

<domain>
## Phase Boundary

A native Windows client (no WSL) connects to and authenticates against a Linux nosh server, signing from an on-disk OpenSSH Ed25519 private key, with a working interactive session including raw VT mode, terminal resize, and correct locale. Platform-specific work is confined to `nosh-client` behind `#[cfg]` gates; nosh-proto / nosh-auth / nosh-server are not forked. NOT in scope: native Windows *server* / ConPTY (M6), Windows ssh-agent / Pageant (deferred), interactive passphrase prompt for encrypted keys (P2), the disk-persisted reattach store (Phase 6 designed-for, not built).
</domain>

<decisions>
## Implementation Decisions

### Verification scope
- **D-01:** Automated gate = `cargo check --target x86_64-pc-windows-gnu` from Linux CI (the gnu target cross-compiles without a Windows toolchain / no xwin). This satisfies the "cross-compiles cleanly, no WSL/C toolchain" success criterion.
- **D-02:** Real interactive behavior (raw mode, resize, on-disk key signing against a live Linux server) is validated by a DOCUMENTED human Windows test, NON-BLOCKING for autonomous completion (phase marked `human_needed`; operator runs it on a real Windows box and records PASSED). Mirrors Phase 7's live-check pattern.

### Identity-file / signing
- **D-03:** Add a new `--identity-file <path>` flag backed by a new `FileSigner` (implements the existing `RawEd25519Signer` trait) that loads an on-disk OpenSSH Ed25519 private key and signs directly. The flag works on ALL platforms (opt-in) — this also lets Linux headless tests exercise `FileSigner`.
- **D-04:** Auth path selection: on Linux, ssh-agent (`SSH_AUTH_SOCK` + `--identity`) stays the DEFAULT; `--identity-file` is an opt-in override. On Windows, `--identity-file` is the ONLY auth path (ssh-agent/Pageant deferred) — there is no agent fallback.
- **D-05:** Key material is held in the narrowest possible scope and zeroized: the loaded private key uses `ZeroizeOnDrop` (or equivalent), is dropped as soon as the signer/cert is built, and is NEVER written to logs or error messages (the v1.0 "never handle the private key" invariant has a documented, Windows-scoped exception here).

### Encrypted keys
- **D-06:** If the on-disk key is passphrase-encrypted, DETECT it (`ssh-key` `is_encrypted()`) and ERROR with clear, actionable guidance (use an unencrypted key, or ssh-agent on Linux). NO interactive passphrase prompt in v1.1 — deferred to P2 (WIN-06). Unencrypted Ed25519 keys are the supported v1.1 path.

### Terminal: raw mode + resize (platform split)
- **D-07:** Bump `crossterm` 0.28 → 0.29 (Windows raw mode + `EventStream`; do NOT enable the `use-dev-tty` feature — Unix-only, breaks the Windows `event-stream` build per crossterm #935). Add `futures` for the `EventStream` async bound if needed.
- **D-08:** Raw mode via the existing `RawModeGuard` (`crossterm::terminal::enable_raw_mode`, already cross-platform). Resize handling is `#[cfg]`-split: keep the SIGWINCH handler under `#[cfg(unix)]`; on `#[cfg(windows)]` use `crossterm::event::EventStream` `Event::Resize` (Windows console resize events, NOT SIGWINCH) → the existing `Resize` protocol message. Preserve the ~40 ms resize coalescing on both paths.

### Locale
- **D-09:** Propagate `TERM` (default `xterm-256color` if unset) and `LANG` (default `en_US.UTF-8` if unset) so the remote Linux shell renders correctly from a Windows client. Reuse the existing client env-forwarding allowlist.

### Key-file permissions
- **D-10:** Emit a best-effort, NON-fatal warning on startup if the key file's permissions look loose or cannot be verified. On Windows, document that `std::fs::Permissions` cannot read ACLs (best-effort only — a proper `GetNamedSecurityInfo` check is out of scope). Do NOT hard-refuse to load the key.

### Claude's Discretion
- Whether the SIGWINCH/EventStream split lives in main.rs or a small platform module.
- Exact zeroization crate/approach for D-05 (`zeroize` is the obvious choice).
- The `futures`/EventStream wiring details and resize-coalescing reuse.
- Default `--identity-file` path on Windows if the user omits it (e.g. `%USERPROFILE%\.ssh\id_ed25519`) vs requiring the flag explicitly — pick the least-surprising behavior and document it.
</decisions>

<specifics>
## Specific Ideas

- The signing abstraction already exists (`RawEd25519Signer` trait + `from_signer`/`from_agent` on `ClientIdentity`); `FileSigner` is a third constructor (`from_identity_file`), NOT a rewrite. Keep nosh-auth/nosh-proto/nosh-server free of `#[cfg(windows)]` — all platform gates live in nosh-client.
- Empty discuss selection: the user accepted all four recommended defaults (verification scope, cross-platform identity-file, encrypted-key error, best-effort perm warning) without override.
</specifics>

<canonical_refs>
## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### Requirements & roadmap
- `.planning/REQUIREMENTS.md` — WIN-01 (Windows client connects+authenticates, cross-compiles), WIN-02 (on-disk Ed25519 signing, narrow scope), WIN-03 (raw VT mode + resize via console events), WIN-04 (TERM/locale propagation)
- `.planning/ROADMAP.md` §"Phase 8: Windows Client" — goal + 4 success criteria
- `.planning/research/SUMMARY.md` §"Phase 5: Windows Client" + §"Gaps to Address" (ACL gap, encrypted-key P2)
- `.planning/research/STACK.md` — crossterm 0.29 (event-stream, no use-dev-tty #935), ssh-key 0.6.7 on-disk loading (`read_openssh_file`/`is_encrypted`/`decrypt`, `encryption` feature gated to Windows), ring precompiled Windows asm (no NASM), `x86_64-pc-windows-msvc`/`-gnu` build notes
- `.planning/research/PITFALLS.md` — #12 (private key in memory — narrow scope + zeroize), #13 (ACL gap), #14 (WINDOW_BUFFER_SIZE_RECORD vs SIGWINCH → EventStream), #15 (VT processing in legacy console hosts)

### Code touchpoints
- `crates/nosh-client/src/client.rs:21-66` — `ClientIdentity` (`from_signer`/`from_agent`; add `from_identity_file` → `FileSigner`); `:208-222` — `RawModeGuard` (already cross-platform)
- `crates/nosh-client/src/main.rs:39-70` — clap `--identity` (agent) + `SSH_AUTH_SOCK` requirement (add `--identity-file`; relax the hard SSH_AUTH_SOCK requirement when `--identity-file` is given); the SIGWINCH handler to `#[cfg(unix)]`-gate
- `crates/nosh-auth/src/signer.rs` — `RawEd25519Signer` trait + `InProcessEd25519Signer` (the pattern `FileSigner` follows); on-disk key loading via `ssh-key`
- `crates/nosh-client/Cargo.toml:29` — crossterm 0.28 → 0.29; add `futures`; gate `ssh-key` `encryption` feature for the Windows target
</canonical_refs>

<code_context>
## Existing Code Insights

### Reusable Assets
- `RawEd25519Signer` trait + `InProcessEd25519Signer` (signer.rs) — `FileSigner` is a sibling impl loading from disk via `ssh-key`.
- `ClientIdentity::{from_signer, from_agent}` (client.rs) — add `from_identity_file`.
- `RawModeGuard` (client.rs:208) — crossterm raw mode, already cross-platform.
- Existing client env-forwarding allowlist (client.rs ~225) handles TERM/LANG/LC_*/TZ — reuse for D-09.
- Phase 4 `NoshPublicKey` + `from_ssh_public` — derive the public key from the loaded private key for cert minting.

### Established Patterns
- The client mints a self-signed cert whose SPKI is the SSH key and signs CertificateVerify via the `RawEd25519Signer` (client.rs:81-85) — `FileSigner` slots in here unchanged.
- `#[ignore]`-gated / human-only checks already exist; the Windows interactive test follows that documentation pattern (D-02).

### Integration Points
- main.rs argument parsing + auth-path selection (agent vs file); `#[cfg]` resize split.
- Cargo.toml dep changes (crossterm bump, futures, target-gated encryption feature).
- New `FileSigner` in nosh-auth; new `from_identity_file` in nosh-client — no server/proto changes.
</code_context>

<deferred>
## Deferred Ideas

- Native Windows server / ConPTY — M6 (PLAT-01); v1.1 is client-only.
- Windows ssh-agent / Pageant (named-pipe) integration — deferred (WIN-05); replaces the on-disk-key exception later.
- Interactive passphrase prompt for encrypted keys — P2 (WIN-06).
- Proper Windows ACL permission check (`GetNamedSecurityInfo`) — out of scope; v1.1 is best-effort (D-10).
- macOS support — deferred (PLAT-02).

None implemented in Phase 8 beyond the decisions above.
</deferred>

---

*Phase: 08-windows-client*
*Context gathered: 2026-05-30*
