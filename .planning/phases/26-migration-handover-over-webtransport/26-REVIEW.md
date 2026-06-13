---
phase: 26-migration-handover-over-webtransport
reviewed: 2026-06-14T00:00:00Z
depth: deep
files_reviewed: 3
files_reviewed_list:
  - crates/nosh-client/tests/wt_reattach.rs
  - crates/nosh-client/tests/webtransport.rs
  - crates/nosh-server/src/wt_transport.rs
findings:
  critical: 0
  warning: 3
  info: 3
  total: 6
status: issues_found
---

# Phase 26: Code Review Report

**Reviewed:** 2026-06-14T00:00:00Z
**Depth:** deep
**Files Reviewed:** 3
**Status:** issues_found

## Summary

Phase 26 is deliberately test-only: no new production logic was added (only a doc-comment block on `run_wt_accept_loop`). The two new integration tests and one new test helper were reviewed at **deep** depth, tracing call chains into `SessionRegistry`, `client::send_reattach`/`await_reattach_reply`, `run_inner_auth_client`, `connect_wt`, and the common test harness (`spawn_wt_server_real_auth`, `WtTestServer`).

**Adversarial assessment:** Both integration tests are correctly structured to fail if the underlying guarantees regress. The `wt06_seamless_resume_over_webtransport` test asserts `ReattachOutcome::Ok`, byte-exact replay of a marker, token rotation, orphan-count baseline return, and MH-1 ordering (reconnect -> inner auth -> Reattach). The `wt06_concurrent_same_token_one_winner` test asserts exactly one winner / one loser for concurrent reattach attempts with the same token, proving the `state != Orphaned` guard in `SessionRegistry::reattach`. Neither test calls the native-QUIC `reattach_collect` path -- both correctly drive `connect_wt -> run_inner_auth_client -> send_reattach` over WebTransport.

**Key concerns:** Three warnings related to a D-07 token leak in an assertion error message, a race-prone fixed sleep for orphan-transition detection, and a duplicated helper function. Three info-level items cover a missing early assertion on original output, a doc-comment line-number off-by-two, and an import-path inconsistency.

## Warnings

### WR-01: D-07 Token Leak in Assertion Error Message

**File:** `crates/nosh-client/tests/wt_reattach.rs:246-249`
**Issue:** The assertion `assert_ne!(new_token, token, "new_token must differ from token (D-05 rotation)")` will emit the `Debug` representation of both `[u8; 16]` token arrays in the failure message if the assertion fires. This violates the D-07 security invariant ("no reattach token bytes logged/printed"). Even though these are throwaway test tokens, the invariant should be respected to prevent copy-paste into production code or CI log ingestion of token-looking byte arrays.
**Fix:**
```rust
// Replace the assert_ne! with:
if new_token == token {
    panic!("D-05 rotation violation: new_token equals original token (token bytes NOT displayed per D-07)");
}
```

### WR-02: Race-Prone Fixed Sleep for Orphan-Transition Wait

**File:** `crates/nosh-client/tests/wt_reattach.rs:195` and `crates/nosh-client/tests/webtransport.rs:374`
**Issue:** Both tests use `tokio::time::sleep(Duration::from_millis(300))` as a hardcoded wait for the server to transition a dropped session to Orphaned. The server's drain sequence includes up to 5-second timeouts for the reader-handle join and the input-writer task (server.rs:1513,1523). While local testing resolves near-instantly, CI environments under load could exceed 300ms, causing a spurious test failure where the reattach attempt fires before the slot is Orphaned (resulting in `ReattachOutcome::Err` instead of `Ok` for the seamless-resume test, or both attempts getting `Err` for the concurrent test).
**Fix:** Replace the fixed sleep with a bounded poll loop:
```rust
// Replace: tokio::time::sleep(Duration::from_millis(300)).await;
// With:
let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
loop {
    if server.registry.total_orphans() > 0
        || tokio::time::Instant::now() >= deadline
    {
        break;
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
}
assert!(server.registry.total_orphans() > 0, "slot never transitioned to Orphaned after drop");
```

### WR-03: Duplicated `client_config_with_pinning` Helper

**File:** `crates/nosh-client/tests/webtransport.rs:517-522` and `crates/nosh-client/tests/wt_reattach.rs:32-37`
**Issue:** The identical `client_config_with_pinning` helper is defined in two separate test files, both of which already share `mod common;`. If the function signature or builder API changes (e.g., a `wtransport` upgrade), both copies must be updated independently, risking drift and split-brain bug fixes.
**Fix:** Move the helper into `crates/nosh-client/tests/common/mod.rs` (which is shared by both test binaries via `mod common;`) and remove both local copies. Note: both test files must still compile as independent binaries (`cargo test --test webtransport` and `cargo test --test wt_reattach`), but `mod common;` is part of both crates, so a function defined in `common/mod.rs` is available to both through the `common::` prefix.

## Info

### IN-01: Missing Early Assertion on Original Output

**File:** `crates/nosh-client/tests/wt_reattach.rs:150-175`
**Issue:** The `output_before_drop` buffer is collected but never asserted on after the while loop. If the PTY fails to produce MARK26A (e.g., shell didn't start), the while loop exits at the 8-second deadline without panicking, then the test proceeds to reconnect and try reattach -- only failing later at the replayed-output assertion (line 290-294) with the misleading message "replayed output must contain MARK26A." The real root cause (marker never printed originally) is masked.
**Fix:** Add an assertion after the initial read loop:
```rust
// After line 175 (end of the PtyData read loop on the first session):
assert!(
    String::from_utf8_lossy(&output_before_drop).contains("MARK26A"),
    "MARK26A marker was never printed in the original session output (PTY likely not running)"
);
```

### IN-02: Doc-Comment Line-Number Off by 2

**File:** `crates/nosh-server/src/wt_transport.rs:392`
**Issue:** The doc comment states `WtransportTransport::is_closed() returns true when quic_connection().close_reason().is_some() (wt_transport.rs:133)`. The actual `is_closed()` method body is at line 131, not 133 (line 133 is the `close()` method). Trivial drift, but incorrect line numbers misdirect future readers.
**Fix:** Replace `wt_transport.rs:133` with `wt_transport.rs:131` in the doc comment.

### IN-03: Non-Canonical Import Path

**File:** `crates/nosh-client/tests/wt_reattach.rs:22`
**Issue:** `read_message_ns` and `write_message_ns` are imported via `nosh_proto::transport_trait::{...}` (the internal module path) instead of the crate-root re-export `nosh_proto::{read_message_ns, write_message_ns}` (the canonical path used in `webtransport.rs` line 293 and the rest of the test suite). Both paths resolve identically, but the inconsistency within the same test crate is a quality smell.
**Fix:** Change line 22 from:
```rust
use nosh_proto::transport_trait::{read_message_ns, write_message_ns};
```
to merge with line 21:
```rust
use nosh_proto::{Message, read_message_ns, write_message_ns};
```

---

_Reviewed: 2026-06-14T00:00:00Z_
_Reviewer: Claude (gsd-code-reviewer)_
_Depth: deep_
