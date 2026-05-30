---
phase: 08-windows-client
reviewed: 2026-05-30T08:00:00Z
depth: standard
files_reviewed: 11
files_reviewed_list:
  - crates/nosh-auth/src/signer.rs
  - crates/nosh-auth/src/lib.rs
  - crates/nosh-auth/Cargo.toml
  - crates/nosh-client/src/client.rs
  - crates/nosh-client/src/lib.rs
  - crates/nosh-client/src/main.rs
  - crates/nosh-client/src/platform.rs
  - crates/nosh-client/Cargo.toml
  - crates/nosh-client/tests/identity_file.rs
  - .github/workflows/windows-cross.yml
  - docs/windows-client-test.md
findings:
  critical: 1
  warning: 4
  info: 3
  total: 8
status: issues_found
---

# Phase 8: Code Review Report

**Reviewed:** 2026-05-30T08:00:00Z
**Depth:** standard
**Files Reviewed:** 11
**Status:** issues_found

## Summary

Phase 8 adds a native Windows client path: `FileSigner` (on-disk Ed25519 key), `crossterm`-based resize handling, platform-split `#[cfg]` gates in `platform.rs` and `main.rs`, and a CI cross-compile check. The core architectural decisions are sound — `#[cfg(unix)]` / `#[cfg(windows)]` boundaries are well-contained, `use-dev-tty` is correctly absent, `crossterm` features are correctly limited to `["events", "event-stream"]`, `FileSigner` zeroizes its seed and redacts Debug output, and the `resolve_identity` control flow is correct on all target platforms.

One critical defect was found: `getrandom_seed()` in `nosh-auth` opens `/dev/urandom` with no `#[cfg]` gate. This is not a production binary issue (the function is only reachable from test/test-support code), but it causes the test suite to panic on Windows when tests are eventually run there, and it means the CI gate (which only does `cargo check`, not `cargo test`) would not catch the failure. Four warnings cover: a potential EventStream-exhaustion spin in the Windows resize path, silent discard of `--identity` on Windows, fragile CI C-compiler discovery, and a misleading comment in the CI workflow. Three info items cover: a missing `workflow_dispatch` trigger, absence of `CARGO_INCREMENTAL=0`, and the missing human-validation test result.

---

## Critical Issues

### CR-01: `getrandom_seed()` hard-codes `/dev/urandom` with no `#[cfg(unix)]` gate — Windows tests panic

**File:** `crates/nosh-auth/src/signer.rs:238-243`

**Issue:** `getrandom_seed()` opens `/dev/urandom` unconditionally and calls `expect()` on both the open and the read. On Windows `/dev/urandom` does not exist; the function will panic at runtime. The function compiles on Windows (no link error), so the CI `cargo check` gate passes silently. The function is called by `InProcessEd25519Signer::generate()` (signer.rs:114), which is in turn called by every test that generates a throwaway key:
- `signer.rs` tests: `inprocess_sign_verifies` (line 557), `minted_cert_spki_matches_key` (line 568)
- `verifier.rs:227`: `InProcessEd25519Signer::generate()` inside a test
- `tests/common/mod.rs:63-67`: `fill_random()` has the identical pattern (`/dev/urandom` + `unwrap()`) and is called by every test that invokes `TestKey::generate()` — including the Phase 8 integration tests in `identity_file.rs`.

This means that if the test suite is ever run on a Windows host (or in a future Windows CI job), every test that calls `TestKey::generate()` will panic with a confusing OS-level error rather than a clear platform-unsupported message.

The production binary is not affected: `InProcessEd25519Signer::generate()` is not called from any non-test code path (server uses `from_ssh_private`; client uses `FileSigner` or `AgentSigner`).

**Fix:** Replace the `/dev/urandom` implementation of `getrandom_seed` with the `getrandom` crate (already a transitive dependency via `ring`), which is cross-platform:

```rust
fn getrandom_seed(buf: &mut [u8; 32]) {
    getrandom::getrandom(buf).expect("getrandom failed");
}
```

Add `getrandom = "0.2"` (or `"0.3"`) to `[dev-dependencies]` in `nosh-auth/Cargo.toml` rather than `[dependencies]` to avoid adding it to the production dependency tree. Apply the identical fix to `fill_random()` in `crates/nosh-client/tests/common/mod.rs:63-67`.

---

## Warnings

### WR-01: Windows `EventStream` exhaustion causes `next_resize()` to return immediately on every call — potential spin in `run_pump`

**File:** `crates/nosh-client/src/platform.rs:89-101`

**Issue:** The Windows branch of `next_resize()` loops over `self.stream.next().await` until it sees a `Resize` event. The comment at line 99 notes that if `EventStream` is "somehow exhausted" the loop exits and the function returns. This is not just theoretical: if the underlying Windows console handle is closed or an unrecoverable error occurs, `EventStream` can return `None` permanently. Once exhausted, every subsequent call to `next_resize()` will return immediately (the `while let Some(...)` exits instantly). In the `tokio::select!` inside `run_pump`, the `_ = resize.next_resize()` arm then fires on every iteration of the loop, setting `resize_deadline` to `now + 40ms`. Forty milliseconds later the `resize_sleep` arm fires and sends a `Resize` frame. This then repeats indefinitely — the pump spins and floods the server with `Resize` messages for the lifetime of the session.

The correct fix is to distinguish "no resize yet" from "stream permanently done" and treat the latter as an unrecoverable condition or silently suppress further resize attempts.

**Fix:**

```rust
// In ResizeWatcher, add a `done` flag:
pub struct ResizeWatcher {
    #[cfg(windows)]
    stream: crossterm::event::EventStream,
    #[cfg(windows)]
    stream_done: bool,
}

// In next_resize (windows branch):
#[cfg(windows)]
{
    if self.stream_done {
        std::future::pending::<()>().await;  // park permanently
        return;
    }
    use futures::StreamExt;
    while let Some(ev) = self.stream.next().await {
        if matches!(ev, Ok(crossterm::event::Event::Resize(_, _))) {
            return;
        }
    }
    self.stream_done = true;
    std::future::pending::<()>().await;
}
```

### WR-02: `--identity` flag is silently ignored on Windows — no diagnostic

**File:** `crates/nosh-client/src/main.rs:68-71` (Args definition) and `main.rs:95-134` (`resolve_identity`)

**Issue:** The `--identity` argument (a path to a `.pub` key file for selecting which ssh-agent key to use) is defined in `Args` without any platform restriction. On Windows, `resolve_identity` never reaches the `#[cfg(unix)]` branch that consumes it (Branch 2 is entirely elided). A Windows user who passes `--identity %USERPROFILE%\.ssh\id_ed25519.pub` will get no error, no warning, and the flag will be silently discarded. If the user is trying to select a specific key for `--identity-file`, the correct flag is `--identity-file`, and the silent discard of `--identity` is misleading.

**Fix:** Add a runtime warning when `--identity` is provided on a non-Unix platform:

```rust
fn resolve_identity(args: &Args) -> anyhow::Result<ClientIdentity> {
    // Warn if --identity is supplied on a platform where it has no effect.
    #[cfg(not(unix))]
    if args.identity.is_some() {
        tracing::warn!(
            "--identity is only used on Unix (ssh-agent); \
             on Windows use --identity-file instead"
        );
    }
    // ... rest of function unchanged ...
}
```

### WR-03: CI workflow C-compiler discovery may fail on certain Ubuntu images

**File:** `.github/workflows/windows-cross.yml:36`

**Issue:** The CI step installs `gcc-mingw-w64-x86-64` (the Win64 meta-package). On Ubuntu 22.04 (which `ubuntu-latest` maps to on GitHub Actions as of mid-2026), this package installs the versioned binary `x86_64-w64-mingw32-gcc-12` but whether the unversioned `x86_64-w64-mingw32-gcc` symlink is set up depends on `update-alternatives` state. The `ring` crate's `build.rs` invokes the C compiler via the `cc` crate, which probes for `x86_64-w64-mingw32-gcc` (unversioned). If the alternatives symlink is not configured, `cargo check` fails with a "C compiler not found" error that has nothing to do with the Rust code.

Additionally, Ubuntu 22.04 ships both `-win32` and `-posix` threading variants; `ring` requires POSIX thread semantics for the target.

**Fix:** Replace the install step with the explicit posix variant and ensure the linker is configured:

```yaml
- name: Install MinGW cross-compiler
  run: |
    sudo apt-get update
    sudo apt-get install -y gcc-mingw-w64-x86-64-posix
    # Ensure the cc crate finds the correct binary name.
    sudo update-alternatives --set \
      x86_64-w64-mingw32-gcc \
      /usr/bin/x86_64-w64-mingw32-gcc-posix 2>/dev/null || true
```

Alternatively, set the explicit env var so `cc` doesn't probe by name:

```yaml
env:
  CC_x86_64_pc_windows_gnu: x86_64-w64-mingw32-gcc-posix
```

### WR-04: CI workflow `cargo check` scope comment is factually wrong — may mask future cfg leaks

**File:** `.github/workflows/windows-cross.yml:47-48`

**Issue:** The comment says: "Only check nosh-client: nosh-server, nosh-auth, and nosh-proto have no Windows-specific code and are not gated (#[cfg] only in nosh-client)." This is factually wrong: `nosh-auth` has a `#[cfg(unix)]`-gated `AgentSigner` type and its `ssh-agent-client-rs` dependency is under `[target.'cfg(unix)'.dependencies]`. The claim propagates a false assumption that could cause a reviewer to skip checking whether `nosh-auth` is correctly gated before concluding the CI pass is sufficient.

The `cargo check -p nosh-client` command itself is correct — it transitively checks `nosh-auth` for Windows compilation. The bug is only in the comment, but inaccurate comments in CI files are a maintenance hazard.

**Fix:** Correct the comment:

```yaml
# Only check the client binary. nosh-auth IS checked transitively as a dep.
# nosh-auth has #[cfg(unix)]-gated AgentSigner and a unix-conditional dep
# (ssh-agent-client-rs); cargo check --target windows validates those gates.
# nosh-server is excluded because it has no Windows-specific code or gates.
run: cargo check -p nosh-client --target x86_64-pc-windows-gnu
```

---

## Info

### IN-01: CI workflow has no `workflow_dispatch` trigger — cannot be manually re-run from GitHub UI

**File:** `.github/workflows/windows-cross.yml:12-16`

**Issue:** The workflow triggers only on `push` and `pull_request` to `main`. There is no `workflow_dispatch:` trigger, so it cannot be re-run manually from the GitHub Actions UI without pushing a new commit. This makes it harder to re-run after a transient MinGW download failure or a flaky ring build.

**Fix:** Add `workflow_dispatch: {}` to the `on:` block.

### IN-02: CI step does not set `CARGO_INCREMENTAL=0` — incremental artifacts can interfere with cross-compile caching

**File:** `.github/workflows/windows-cross.yml`

**Issue:** Incremental compilation artifacts built for the Linux host can bleed into the cross-compilation step when the cache key is shared. Setting `CARGO_INCREMENTAL=0` in CI is a standard practice that prevents incremental artifacts from causing false cache hits and occasional build corruption.

**Fix:** Add to the `cargo check` step:

```yaml
env:
  CARGO_INCREMENTAL: "0"
```

### IN-03: `docs/windows-client-test.md` human validation sign-off is unrecorded — phase has open human-test requirement

**File:** `docs/windows-client-test.md:109-123`

**Issue:** The phase is marked `human_needed` and the validation checklist (D-02) exists but the operator sign-off section is blank. The six validation items (connection, raw mode, Ctrl-C forwarding, terminal resize, locale, encrypted key rejection) have not been recorded as PASSED or FAILED. This is expected for an in-progress phase but should be noted so the phase is not marked complete until the sign-off is filled in.

**Fix:** Record the test result in the operator sign-off section of `docs/windows-client-test.md` after running on a real Windows host. No code change required.

---

_Reviewed: 2026-05-30T08:00:00Z_
_Reviewer: Claude (gsd-code-reviewer)_
_Depth: standard_
