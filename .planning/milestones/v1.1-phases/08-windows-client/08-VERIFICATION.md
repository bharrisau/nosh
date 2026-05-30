---
phase: 08-windows-client
verified: 2026-05-30T14:30:00Z
status: human_needed
score: 4/4 must-haves verified (static); 2 require human/CI sign-off
re_verification: false
human_verification:
  - test: "CI Windows cross-compile gate (D-01 / SC#1) actually executes green"
    expected: ".github/workflows/windows-cross.yml runs on push/PR and `cargo check -p nosh-client --target x86_64-pc-windows-gnu` exits 0 after installing gcc-mingw-w64-x86-64-posix"
    why_human: "mingw is absent on this Linux host (verified: ring's build.rs fails at C-compiler discovery before any nosh Rust code is type-checked). No machine has yet compiled the Windows code path — Rust-level Windows correctness is verified by static #[cfg] inspection only. Push to GitHub and confirm the workflow is green."
  - test: "Interactive Windows session (D-02 / SC#3) on a real Windows host"
    expected: "Run docs/windows-client-test.md checklist: (1) connect+auth via on-disk Ed25519 key, (2) raw mode, (3) Ctrl-C forwarded as 0x03, (4) Windows Terminal resize reflows the remote PTY within ~100ms via EventStream Event::Resize, (5) TERM=xterm-256color / LANG=en_US.UTF-8 and UTF-8 renders, (6) encrypted key rejected"
    why_human: "Windows console raw mode, EventStream Event::Resize delivery, and locale rendering cannot be exercised on Linux. NON-BLOCKING per D-02 — phase is human_needed by design."
---

# Phase 8: Windows Client Verification Report

**Phase Goal:** A native Windows client (no WSL) connects to and authenticates against a Linux nosh server, with a working interactive session including resize and correct locale.
**Verified:** 2026-05-30T14:30:00Z
**Status:** human_needed
**Re-verification:** No — initial verification

## Goal Achievement

This phase was developed entirely on Linux; the Windows build cannot be compiled here (mingw cross-compiler absent, no sudo — independently confirmed: `cargo check --target x86_64-pc-windows-gnu` fails at `ring`'s C-compiler discovery, NOT at any nosh Rust code). Windows-correctness was therefore verified STATICALLY by tracing every `#[cfg]` boundary, plus the full Linux-side test suite.

### Observable Truths

| # | Truth (ROADMAP Success Criteria) | Status | Evidence |
|---|----------------------------------|--------|----------|
| 1 | SC#1: `cargo check --target x86_64-pc-windows-gnu` passes; the client crate cross-compiles cleanly without WSL or a C toolchain | ? UNCERTAIN (CI) | Rust code statically correct (all Unix symbols gated — see below). CI workflow exists with the correct mingw+CC_ env fix. BUT the gate has NEVER executed: ring's build.rs fails at C-compiler discovery before nosh code is type-checked locally, and CI has not yet run. Routed to human/CI sign-off. |
| 2 | SC#2: authenticates with an on-disk unencrypted Ed25519 key via `--identity-file`; key held in narrowest scope (ZeroizeOnDrop), never logged | ✓ VERIFIED | `identity_file_mutual_auth_happy_path` passes (real PTY session over file-key auth). FileSigner: zeroizes transient seed (signer.rs:178-180), holds only the ZeroizeOnDrop dalek key, manual Debug prints only SHA256 fingerprint (signer.rs:201-209), rejects encrypted keys with guidance (signer.rs:162-169). |
| 3 | SC#3: raw VT mode via `enable_raw_mode()`; `EventStream` `Event::Resize` → PTY resize (Windows console events, not SIGWINCH) | ✓ VERIFIED (static) / human runtime | RawModeGuard uses cross-platform `crossterm::terminal::enable_raw_mode` (client.rs:274). `ResizeWatcher` #[cfg(windows)] branch matches `Event::Resize` via EventStream (platform.rs:96-117); #[cfg(unix)] keeps SIGWINCH. Runtime behavior is D-02 human_needed. |
| 4 | SC#4: propagates TERM (default xterm-256color) and LANG (default en_US.UTF-8); best-effort file-permission warning with documented Windows ACL limitation | ✓ VERIFIED | `collect_client_env` injects TERM/LANG defaults when unset, excludes SSH_AUTH_SOCK/LD_* (client.rs:295-318). `warn_if_loose_permissions`: #[cfg(unix)] mode()&0o077 check, #[cfg(not(unix))] ACL-gap warning, non-fatal (signer.rs:214-236; proven non-fatal by `loose_permissions_warns_but_loads`). |

**Score:** 4/4 must-haves substantively achieved in code (2 require human/CI confirmation of runtime/compile, as designed).

### #[cfg] Boundary Trace (the highest-value static check)

Every Unix-only symbol confirmed gated (grep + line-by-line inspection):

| Symbol | Location | Gate | Status |
|--------|----------|------|--------|
| `ssh_agent_client_rs::Client` | signer.rs:67 (AgentSigner::sign) | struct+impls `#[cfg(unix)]` (signer.rs:38,64) | ✓ GATED |
| `ssh_agent_client_rs::Client` | client.rs:85-86 (ssh_agent_connect) | `#[cfg(unix)]` (client.rs:84) | ✓ GATED |
| `use nosh_auth::AgentSigner` | client.rs:16 | `#[cfg(unix)]` (client.rs:15) | ✓ GATED |
| `from_agent` | client.rs:60 | `#[cfg(unix)]` (client.rs:59) | ✓ GATED |
| `std::os::unix::fs::PermissionsExt` / `mode()` | signer.rs:217,219 | inside `#[cfg(unix)]` block (signer.rs:215) | ✓ GATED |
| `tokio::signal::unix::Signal` | platform.rs:39,60,62 | field+block `#[cfg(unix)]` (platform.rs:38,57) | ✓ GATED |
| `tokio::signal::unix` in main.rs | — | NONE present (replaced by `platform::ResizeWatcher`/`quit_signal`) | ✓ REMOVED |
| `/dev/urandom` | — | NONE present (replaced by getrandom — CR-01 fix) | ✓ REMOVED |

`ssh-agent-client-rs` is under `[target.'cfg(unix)'.dependencies]` in BOTH nosh-auth and nosh-client Cargo.toml. nosh-proto and nosh-server contain ZERO platform cfg gates (grep confirmed) — the constraint "all #[cfg] confined to nosh-client" holds, with nosh-auth carrying only the documented unix dependency-availability gate (not a behavioral fork).

### CR-01 Fix Verification (ungated /dev/urandom — the review's critical finding)

Both halves of CR-01 are correctly and completely fixed:
- `signer.rs` `getrandom_seed` (line 243-245): now `getrandom::getrandom(buf)`, AND gated `#[cfg(test)]`; its only caller `generate()` is also `#[cfg(test)]` (line 111-117) — keeps getrandom off the production tree.
- `tests/common/mod.rs` `fill_random` (line 63-65): now `getrandom::getrandom(buf)`.
- `getrandom = "0.2"` is in `[dev-dependencies]` of both crates (NOT `[dependencies]`); present in Cargo.lock (v0.2.17). Production `cargo build -p nosh-auth` has no getrandom in tree.

### Required Artifacts

| Artifact | Expected | Status | Details |
|----------|----------|--------|---------|
| `crates/nosh-auth/src/signer.rs` | FileSigner (zeroize, encrypted-key detect, redacted Debug); cfg(unix) AgentSigner | ✓ VERIFIED | struct FileSigner + from_path present; all checks confirmed |
| `crates/nosh-client/src/client.rs` | from_identity_file (un-gated); from_agent cfg(unix); collect_client_env TERM/LANG | ✓ VERIFIED | All present and correctly gated |
| `crates/nosh-client/src/platform.rs` | ResizeWatcher (#[cfg]-split) + cross-platform quit_signal | ✓ VERIFIED | WR-01 stream_done fix present |
| `crates/nosh-client/src/main.rs` | resolve_identity (3 branches) + identity_file arg + platform resize/quit | ✓ VERIFIED | WR-02 --identity warning present |
| `crates/nosh-client/tests/identity_file.rs` | Linux headless FileSigner e2e + missing-file error | ✓ VERIFIED | 2 tests pass |
| `.github/workflows/windows-cross.yml` | windows-gnu check w/ mingw install | ✓ VERIFIED (static) | WR-03/WR-04 fixes present; never executed (see SC#1) |
| `docs/windows-client-test.md` | NON-BLOCKING human test w/ checklist | ✓ VERIFIED | All required elements + "NON-BLOCKING"/"human_needed" |

### crossterm Cargo.toml Check

`crossterm = { version = "0.29", features = ["events", "event-stream"] }` — `use-dev-tty` NOT enabled (crossterm #935 avoided). `futures = "0.3"` present for StreamExt. Windows-viable feature set. ✓ VERIFIED

### Behavioral Spot-Checks (Linux)

| Behavior | Command | Result | Status |
|----------|---------|--------|--------|
| FileSigner unit tests | `cargo test -p nosh-auth` | 15 passed, 1 ignored (agent) | ✓ PASS |
| FileSigner e2e auth | `cargo test -p nosh-client --test identity_file` | 2 passed | ✓ PASS |
| Client builds | `cargo build -p nosh-client` | Finished, no errors | ✓ PASS |
| No Phase 8 regression | `cargo test -p nosh-client --tests` | auth 6, session 6, transport 4, reattach 3, persistence 3, identity_file 2 all pass | ✓ PASS |
| D-01 failure mode | `cargo check --target x86_64-pc-windows-gnu` | Fails at ring C-compiler discovery (mingw absent), NOT at nosh Rust code | ✓ EXPECTED (confirms deferral soundness) |

Note: `migration_survives_path_change` passed this run; it is a known pre-existing Phase 7 flaky test being fixed separately and is explicitly NOT counted against Phase 8.

### Requirements Coverage

| Requirement | Status | Evidence |
|-------------|--------|----------|
| WIN-01 (cross-compiles, no WSL/C toolchain) | ? CI | Code statically correct; gate exists but unexecuted (SC#1) |
| WIN-02 (on-disk Ed25519, narrow scope, zeroize) | ✓ SATISFIED | SC#2 verified e2e on Linux |
| WIN-03 (raw VT mode + resize via console events) | ✓ SATISFIED (static) | SC#3 code correct; runtime human_needed |
| WIN-04 (TERM/locale propagation) | ✓ SATISFIED | SC#4 verified |

### Anti-Patterns Found

None. No TBD/FIXME/XXX debt markers in modified files. No stubs (all Windows paths are real API surface, not placeholders). Review findings CR-01, WR-01..WR-04 all confirmed fixed in code; IN-01 (workflow_dispatch) and IN-02 (CARGO_INCREMENTAL) were Info-only and intentionally not applied — acceptable.

### Gaps Summary

No blocking gaps. The phase goal is substantively achieved in code: FileSigner auth works end-to-end (proven on Linux), the platform split is correct and complete, all Unix symbols are properly gated, security invariants (env sanitization, no SSH_AUTH_SOCK forwarding, TERM/LANG whitelist) hold, and CR-01 + all warnings are fixed.

Two items legitimately require sign-off OUTSIDE this Linux environment, both of which are deferred BY DESIGN (D-01 → CI, D-02 → human):

1. **SC#1 compile gate has never executed anywhere.** This is the one genuine residual risk: because ring's C build fails before nosh code locally, no machine has yet type-checked the Windows code path. Static #[cfg] inspection found the gating complete and correct, but the authoritative proof is the CI run. The deferral to CI is SOUND (the failure mode here is confirmed to be a missing toolchain, not a code error), and the CI workflow is correctly configured to supply that toolchain.
2. **SC#3/D-02 interactive Windows behavior** is NON-BLOCKING and human_needed by explicit phase design.

Status is `human_needed` (not `passed`) per the Step 9 decision tree: human verification items exist and the phase was explicitly designed to terminate in this state.

---

_Verified: 2026-05-30T14:30:00Z_
_Verifier: Claude (gsd-verifier, adversarial static + Linux-side dynamic)_
