---
phase: 02
phase_name: ssh-key-mutual-auth
date: 2026-05-29
fix_scope: critical_warning
findings_in_scope: 1
fixed: 1
skipped: 0
iteration: 1
status: all_fixed
---

# Code Review Fix: Phase 02 — SSH-Key Mutual Auth

Applied fixes for the in-scope (Critical + Warning) findings from 02-REVIEW.md.

## Fixed

### WR-01 — Pre-auth semaphore permit held for the entire session
**File:** crates/nosh-server/src/server.rs
**Commit:** `fix(02): release pre-auth permit on handshake completion (WR-01)`

The accept-loop `OwnedSemaphorePermit` was moved into the connection task and
held until the whole connection (handshake + authenticated echo session) ended,
so `max_concurrent` (default 64) capped total live sessions instead of pre-auth
half-open state as D-13 intends. 64 long-lived authenticated sessions could
exhaust the pool and cause new connections — including new auth attempts — to be
refused.

Fix: the permit is now passed into `handle_connection` and `drop`ped the instant
`incoming.await` resolves (handshake/auth complete). On the timeout and
handshake-error paths the permit is released automatically on early return.
Net effect: the cap bounds unauthenticated state only; authenticated sessions no
longer consume pre-auth capacity. No auth-path behaviour changed.

## Skipped

The two Info findings (INFO-01 `/dev/urandom` seeding panic-on-failure;
INFO-02 known_hosts non-atomic world-readable append) are out of the default
critical_warning scope and were intentionally not auto-fixed. Both are
low-impact hardening notes scoped to the Linux spike and documented in
02-REVIEW.md for future hardening.

## Verification after fix

- `cargo build --workspace --all-targets`: pass
- `cargo test --workspace`: pass (nosh-auth 6, auth integration 6, transport 5,
  proto 3; 3 live `--ignored` tests not run)
- `cargo clippy --workspace --all-targets`: clean

## Status: all_fixed (in-scope)
