---
phase: 26-migration-handover-over-webtransport
fixed_at: 2026-06-14T00:00:00Z
review_path: .planning/phases/26-migration-handover-over-webtransport/26-REVIEW.md
iteration: 1
findings_in_scope: 6
fixed: 6
skipped: 0
status: all_fixed
---

# Phase 26: Code Review Fix Report

**Fixed at:** 2026-06-14T00:00:00Z
**Source review:** .planning/phases/26-migration-handover-over-webtransport/26-REVIEW.md
**Iteration:** 1

**Summary:**
- Findings in scope: 6
- Fixed: 6
- Skipped: 0

## Fixed Issues

### WR-01: D-07 Token Leak in Assertion Error Message

**Files modified:** `crates/nosh-client/tests/wt_reattach.rs`
**Commit:** fix(26): WR-01 fix D-07 token leak in assertion error message
**Applied fix:** Replaced `assert_ne!(new_token, token, ...)` with a boolean comparison that does NOT print the token bytes on failure. The new assertion uses `assert!(new_token != token, "reattach token must rotate on success (D-05 rotation) (D-07: token bytes not displayed)")`, preserving adversarial intent while respecting D-07 security invariant.

### WR-02: Race-Prone Fixed Sleep for Orphan-Transition Wait

**Files modified:** `crates/nosh-client/tests/wt_reattach.rs`, `crates/nosh-client/tests/webtransport.rs`
**Commit:** fix(26): WR-02 replace race-prone fixed sleep with poll loop for orphan transition
**Applied fix:** Replaced hardcoded `tokio::time::sleep(Duration::from_millis(300))` with a bounded poll loop that checks `server.registry.total_orphans()` every 50ms up to a 10-second deadline. The loop breaks as soon as the orphan count exceeds 0, and panics with a clear message on timeout. This removes timing assumptions and prevents CI flakes.

### WR-03: Duplicated client_config_with_pinning Helper

**Files modified:** `crates/nosh-client/tests/common/mod.rs`, `crates/nosh-client/tests/wt_reattach.rs`, `crates/nosh-client/tests/webtransport.rs`
**Commit:** fix(26): WR-03 move duplicated client_config_with_pinning helper to common module
**Applied fix:** Moved the identical `client_config_with_pinning` helper function into `crates/nosh-client/tests/common/mod.rs` as a public function (feature-gated for `webtransport`). Updated both `wt_reattach.rs` and `webtransport.rs` to call `common::client_config_with_pinning(...)` and removed their local copies. This ensures single-source-of-truth maintenance.

### IN-01: Missing Early Assertion on Original Output

**Files modified:** `crates/nosh-client/tests/wt_reattach.rs`
**Commit:** fix(26): IN-01 add assertion for original output before drop
**Applied fix:** Added an assertion after the initial PtyData read loop to confirm that `output_before_drop` contains the "MARK26A" marker. This ensures the PTY was actually running before the connection is dropped, preventing misleading error messages if the marker never appeared originally.

### IN-02: Doc-Comment Line-Number Off by 2

**Files modified:** `crates/nosh-server/src/wt_transport.rs`
**Commit:** fix(26): IN-02 correct doc comment line number for is_closed method
**Applied fix:** Corrected the doc comment reference from `wt_transport.rs:133` to `wt_transport.rs:131` to match the actual line number of the `is_closed()` method body.

### IN-03: Non-Canonical Import Path

**Files modified:** `crates/nosh-client/tests/wt_reattach.rs`
**Commit:** fix(26): IN-03 use canonical import path for read_message_ns and write_message_ns
**Applied fix:** Changed the import from `nosh_proto::transport_trait::{read_message_ns, write_message_ns}` to the canonical crate-root re-export `nosh_proto::{Message, read_message_ns, write_message_ns}`, matching the pattern used in other test files. Also removed unused `wtransport::ClientConfig` import.

---

**Verification:** All WebTransport tests pass (`cargo test -p nosh-client --features webtransport --test wt_reattach --test webtransport` - 5 tests passed). Full workspace builds successfully with no regressions.

_Fixed: 2026-06-14T00:00:00Z_
_Fixer: Claude (gsd-code-fixer)_
_Iteration: 1_
