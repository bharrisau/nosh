---
plan: 04-B
title: "nosh-server — Thread Authenticated Identity into Session"
status: complete
completed: 2026-05-30
wave: 2
tasks_total: 6
tasks_complete: 6
key-files:
  created:
    - crates/nosh-server/src/session.rs
    - crates/nosh-server/src/server.rs
deviations: none
---

# Plan B Summary

## What Was Built

Wired the authenticated peer SSH identity (extracted from the TLS handshake) into `Session.identity` as a non-optional `NoshPublicKey` field — enforced by the type system:

1. **`Session.identity: NoshPublicKey`** (not `Option<NoshPublicKey>`) — compiler rejects constructing a Session without it (D-01). Updated doc comment to remove "None for this spike / M3 seam" language (D-03).

2. **`session::open()` takes `NoshPublicKey`** (not `Option<NoshPublicKey>`) — D-02. Single call site in `server.rs` updated.

3. **`CLOSE_AUTH = 2`** constant added alongside `CLOSE_OK` and `CLOSE_PROTOCOL`.

4. **`extract_peer_identity(conn: &quinn::Connection) -> Option<NoshPublicKey>`** helper function — downcasts `conn.peer_identity()` to `Vec<CertificateDer<'static>>`, calls `nosh_auth::keys::extract_spki_from_cert`, then `nosh_auth::nosh_key_from_spki`.

5. **`handle_connection`** calls `extract_peer_identity(&conn)` after `drop(permit)` and before `conn.accept_bi()`. On `None`: `tracing::error!` + `conn.close(CLOSE_AUTH)` + `return Ok(())` (D-04/D-05).

6. **Per-session span** includes `identity = %fingerprint` where `fingerprint = sess.identity.fingerprint()` — raw key bytes never logged (D-06/D-07).

## Tasks Completed

| Task | Description | Commit |
|------|-------------|--------|
| B-1 | Session.identity non-optional, session::open() signature updated | da37e35 |
| B-2 | CLOSE_AUTH=2, extract_peer_identity() helper | 9d7a3db |
| B-3 | Identity extraction in handle_connection (D-04/D-05) | b219f82 |
| B-4 | run_session identity param, session::open() call fixed (D-02) | 2500a13 |
| B-5 | Fingerprint in per-session tracing span (D-06/D-07) | 2950f35 |
| B-6 | Verification: cargo test --workspace exits 0 | (no commit — pure check) |

## Test Results

- `cargo test --workspace`: **32 passed, 3 ignored** — all AUTH (6) and SESS (6) integration tests pass without assertion changes, transport (4), nosh-auth unit (11), proto (4), nosh-server unit (1)

## Deviations

None. All tasks completed exactly as specified.

## Self-Check: PASSED

- `Session.identity` is `pub identity: NoshPublicKey` (no Option<>) ✓
- `handle_connection` calls `extract_peer_identity(&conn)` after handshake, before `accept_bi()` ✓
- On `None`: `conn.close(CLOSE_AUTH.into(), ...)` + `tracing::error!` + `return Ok(())` ✓
- `session::open` accepts `NoshPublicKey` (not `Option<>`) ✓
- `tracing::info_span!` includes `identity = %fingerprint` ✓
- Raw `key32` bytes never logged ✓
- `cargo test --workspace` exits 0, no assertion changes ✓
