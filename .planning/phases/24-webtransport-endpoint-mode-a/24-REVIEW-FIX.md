---
phase: 24-webtransport-endpoint-mode-a
fixed_at: 2026-06-13T08:15:00Z
review_path: .planning/phases/24-webtransport-endpoint-mode-a/24-REVIEW.md
iteration: 1
findings_in_scope: 5
fixed: 4
skipped: 1
status: partial
---

# Phase 24: Code Review Fix Report

**Fixed at:** 2026-06-13T08:15:00Z
**Source review:** .planning/phases/24-webtransport-endpoint-mode-a/24-REVIEW.md
**Iteration:** 1

**Summary:**
- Findings in scope: 5 (CR-01, WR-01, WR-02, WR-03, IN-01)
- Fixed: 4 (CR-01, WR-01, WR-03, IN-01)
- Skipped: 1 (WR-02 — no action needed, no warning fires)

## Fixed Issues

### CR-01: Semaphore permit held for entire session lifetime

**Files modified:** `crates/nosh-server/src/wt_transport.rs`
**Commit:** 6a10b0f
**Applied fix:** Removed `let _permit = permit;` at the top of the spawned task. The permit is now dropped explicitly via `drop(permit)` immediately after `session_request.accept().await` succeeds and before `handle_connection_wt` is called. This narrows the semaphore hold to the accept/handshake window only, matching the `run_accept_loop` pattern in `server.rs` (D-13 parity). The `max_concurrent_handshakes` cap now correctly bounds half-open handshakes rather than total active sessions.

### WR-01: Off-by-one in privilege-boundary port check

**Files modified:** `crates/nosh-server/src/wt_transport.rs`
**Commit:** 1308a27
**Applied fix:** Changed `port <= 1024` to `port < 1024` in `make_wt_endpoint`. Port 1024 is unprivileged on Linux; only ports 1–1023 require `CAP_NET_BIND_SERVICE`. The spurious warning for port 1024 is now suppressed.

### WR-03: Misleading expect message in wt03 test

**Files modified:** `crates/nosh-client/tests/webtransport.rs`
**Commit:** c80aeeb
**Applied fix:** Replaced `.expect("spawn_wt_server returned None")` with `.expect("spawn_wt_server must succeed (endpoint bind or identity generation failed)")`. The old message was misleading because `spawn_wt_server(None)` never returns `None` — the real failure modes are endpoint bind failure or self-signed identity generation failure.

### IN-01: Unused import in common test module

**Files modified:** `crates/nosh-client/tests/common/mod.rs`
**Commit:** 0b82730
**Applied fix:** Removed `use nosh_proto::transport_trait::NoshTransport;`. The type was only mentioned in a comment (line 316); `QuinnTransport` satisfies the `NoshTransport` bound implicitly and no code path uses the import directly.

## Skipped Issues

### WR-02: `use tokio::io::AsyncWriteExt as _` may be unused in server wt_transport.rs

**File:** `crates/nosh-server/src/wt_transport.rs:51`
**Reason:** Verified no action needed — no warning fires. Build with `RUSTFLAGS="-D unused_imports"` confirms the import IS used: both `write_all` (line 149) and `flush` (line 153) on `wtransport::SendStream` are resolved via `AsyncWrite`/`AsyncWriteExt`, not as inherent methods. The module-level import is correct and intentional. The inconsistency with the client (which scopes the import inside `flush()`) reflects that the client only needs `flush` via the trait while the server needs both methods.

---

**Verification performed:**
- `cargo build -p nosh-server --features webtransport` — clean, no warnings
- `cargo build -p nosh-client --features webtransport` — clean, no warnings
- `cargo test -p nosh-client --features webtransport --test webtransport` — all 3 tests pass (wt01, wt02, wt03)
- `cargo test --workspace` — 0 failures across all suites

_Fixed: 2026-06-13T08:15:00Z_
_Fixer: Claude (gsd-code-fixer)_
_Iteration: 1_
