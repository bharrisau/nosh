---
phase: 16-qol-feature-pack-windows-ci-gate
plan: "03"
subsystem: CI
tags: [ci, windows, msvc, harden]
dependency_graph:
  requires: []
  provides: [windows-msvc-ci-gate, linux-ci-gate]
  affects: [.github/workflows/ci.yml]
tech_stack:
  added: []
  patterns: [github-actions, cargo-build-locked, windows-latest-msvc]
key_files:
  created:
    - .github/workflows/ci.yml
  modified: []
  deleted:
    - .github/workflows/windows-cross.yml
decisions:
  - "D-16-04: native windows-latest MSVC replaces Linux GNU cross-compile — catches MSVC/winapi issues the cross-compile misses"
  - "D-16-04b: green Actions run is a human verification item — user must push to GitHub and confirm both jobs green"
  - "cargo build (not cargo check) used for build-windows — linker/ABI failures fail the run; not false-green (HARDEN-02)"
  - "Only nosh-client built on Windows — nosh-server has portable-pty/nix (Unix-only); nosh-auth compiles transitively"
  - "Swatinem/rust-cache@v2 used over actions/cache@v4 for standard Rust CI caching"
metrics:
  duration_minutes: 5
  completed_date: "2026-06-02"
  tasks_completed: 1
  tasks_total: 2
  files_changed: 2
requirements: [HARDEN-02]
---

# Phase 16 Plan 03: Native Windows CI Gate Summary

**One-liner:** Linux + windows-latest MSVC CI gate via `ci.yml` with `cargo build --locked -p nosh-client --target x86_64-pc-windows-msvc`; old GNU cross-compile `windows-cross.yml` retired.

## What Was Built

Authored `.github/workflows/ci.yml` with two jobs triggered on every push and PR to `main`:

1. **`linux` job** (`ubuntu-latest`): `cargo build --locked` + `cargo test --locked` — the primary gate for the whole workspace.

2. **`build-windows` job** (`windows-latest`): native MSVC build of `nosh-client` only for `x86_64-pc-windows-msvc` via `cargo build --locked --target x86_64-pc-windows-msvc -p nosh-client`. Uses `dtolnay/rust-toolchain@stable` with the MSVC target + `Swatinem/rust-cache@v2`.

Deleted `.github/workflows/windows-cross.yml` (the retired Linux-hosted GNU cross-compile that used `cargo check`).

## Key Design Decisions

- **`cargo build` not `cargo check`** (D-16-04): the build-windows job catches linker and ABI issues that `cargo check` cannot detect. Any compile error fails the run — this is the "not false-green" requirement for HARDEN-02.
- **`windows-latest` not `ubuntu-latest`**: native MSVC Build Tools are pre-installed on `windows-latest` (Windows Server 2022); no MinGW or `apt-get` step needed.
- **`ring` 0.17.x**: ships precompiled `x86_64-windows` asm objects; no NASM needed for the MSVC build path.
- **Only `nosh-client`**: `nosh-server` has `portable-pty` + `nix` (Unix-only); building it on `windows-latest` would fail. `nosh-auth` compiles transitively as a dependency of `nosh-client`.
- **D-16-04b**: the green Actions run confirming HARDEN-02 is satisfied is a **human verification item** (see below).

## Deviations from Plan

None — plan executed exactly as written. Task 2 is a human verification item by design (D-16-04b); it is not a machine-verifiable step.

## Human Verification Required (D-16-04b — HARDEN-02 Green-Run Sign-off)

The CI authoring is complete. HARDEN-02 is **fully authored** but the green-run confirmation requires a real GitHub Actions run, which cannot be machine-verified from this sandbox (origin/main is stale with ~59 unpushed commits; `gh push` was not available).

**Steps for the user:**
1. Push the branch to GitHub: `git push origin main` (or open a PR).
2. Ensure Actions are enabled: GitHub repo → Settings → Actions → General → allow Actions.
3. Open the repo → Actions tab → the `CI` workflow run for your push.
4. Confirm BOTH jobs are green: `cargo test (Linux)` and `cargo build nosh-client (Windows MSVC, HARDEN-02)`.
5. Confirm the windows job actually compiled (read the "Build Windows client" step log — it must show `cargo build ... --target x86_64-pc-windows-msvc -p nosh-client` succeeding, NOT skipped).
6. Note: public repo = free Windows minutes; private repo = 2x rate. No repo secrets needed (build-only).

**HARDEN-02 is signed off once the user confirms both jobs green on a real push.**

## Threat Surface Scan

No new network endpoints, auth paths, file access patterns, or schema changes introduced. CI authoring only — no application code changed. Matches threat register:
- T-16-09 (false-green): mitigated — `cargo build` used, no `continue-on-error`.
- T-16-10 (third-party actions): accepted — community-standard actions pinned by major tag, no secrets exposed.
- T-16-SC (package installs): accepted — no new packages; `--locked` against committed `Cargo.lock`.

## Known Stubs

None. This plan is CI configuration only.

## Self-Check: PASSED

- `.github/workflows/ci.yml` exists: confirmed (created, committed at 287e5c7)
- `.github/workflows/windows-cross.yml` deleted: confirmed (removed via `git rm`, committed)
- Commit 287e5c7 exists: confirmed (`feat(16-03): native Windows MSVC CI gate + retire windows-cross.yml`)
- `windows-latest` in build-windows job: confirmed
- `cargo build --locked --target x86_64-pc-windows-msvc -p nosh-client` in run step: confirmed
- No `cargo check` in any `run:` step: confirmed (2 grep hits are in YAML comments only)
- D-16-04b comment in YAML: confirmed
