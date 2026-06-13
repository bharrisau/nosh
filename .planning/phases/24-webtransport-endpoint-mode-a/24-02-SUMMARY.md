---
phase: 24-webtransport-endpoint-mode-a
plan: "02"
subsystem: nosh-client transport seam
tags: [refactor, transport-abstraction, quinn-wrapper, trait-object, nosh-client]
dependency_graph:
  requires: [23-02]
  provides: [24-04-wt-dialer-target]
  affects: [nosh-client, nosh-proto, nosh-server]
tech_stack:
  added: []
  patterns:
    - NoshTransport trait-object dispatch in nosh-client session pump
    - QuinnTransport/QuinnSendStream/QuinnRecvStream pass-through wrappers (client-side mirror of server-side Phase 23)
    - Box<dyn NoshSendStream> / Box<dyn NoshRecvStream> blanket impls in nosh-proto
    - rtt() and is_closed() added to NoshTransport trait for BUG-C silence detection + predictor culling
key_files:
  created:
    - crates/nosh-client/src/quinn_transport.rs
  modified:
    - crates/nosh-proto/src/transport_trait.rs
    - crates/nosh-server/src/quinn_transport.rs
    - crates/nosh-client/src/lib.rs
    - crates/nosh-client/src/client.rs
    - crates/nosh-client/src/main.rs
    - crates/nosh-client/src/channel.rs
    - crates/nosh-client/Cargo.toml
    - crates/nosh-client/tests/channel_mux.rs
    - crates/nosh-client/tests/common/mod.rs
    - crates/nosh-client/tests/migration.rs
    - crates/nosh-client/tests/persistence.rs
    - crates/nosh-client/tests/predict.rs
    - crates/nosh-client/tests/reattach.rs
    - crates/nosh-client/tests/render.rs
    - crates/nosh-client/tests/session.rs
    - crates/nosh-client/tests/sync.rs
decisions:
  - key: rtt-and-is-closed-on-trait
    decision: "Added rtt() (default Duration::ZERO) and is_closed() (default false) to NoshTransport trait rather than removing BUG-C silence-detection and predictor RTT culling from run_pump. Removing them would regress the loss overlay fix and prediction epoch accuracy."
  - key: dyn-trait-obj-coercion
    decision: "All stream-taking functions in client.rs use &mut dyn NoshSendStream/NoshRecvStream (not generics) since generics with ?Sized cannot coerce &mut S to &mut dyn without Sized. Test call sites use &mut *boxed_stream for deref coercion."
  - key: box-blanket-impls
    decision: "Added Box<dyn NoshSendStream>: NoshSendStream and Box<dyn NoshRecvStream>: NoshRecvStream blanket impls in nosh-proto so spawn_ctrl_drain and run_scrollback_drain_task work with Box<dyn...> parameters."
metrics:
  duration: ~3600s
  completed: "2026-06-13"
  tasks: 2
  files_changed: 18
---

# Phase 24 Plan 02: Client Session Pump Genericised over NoshTransport

The client session pump (`fresh_session`, `reattach_session`, `run_pump`) is now fully generic over the Phase 23 `NoshTransport` trait. This mirrors the Phase 23 server-side refactor and is the hidden prerequisite for Plan 04's WebTransport dialer.

## What was built

**Task 1 — Client Quinn wrapper + client.rs retarget (commit 14da0e0):**

Created `crates/nosh-client/src/quinn_transport.rs` — a verbatim copy of the server-side wrapper (`crates/nosh-server/src/quinn_transport.rs`) with 155 lines. Exports `QuinnTransport`, `QuinnSendStream`, `QuinnRecvStream` implementing `NoshTransport`, `NoshSendStream`, `NoshRecvStream` respectively. Pure pass-through; the synchronous `finish()` body (DO NOT .await) is preserved.

Retargeted all `client.rs` session helpers from concrete Quinn types to trait objects:
- `open_session`, `open_session_with_token` → take `&dyn NoshTransport`; return `Box<dyn NoshSendStream>` / `Box<dyn NoshRecvStream>`
- `send_input`, `send_resize`, `send_reattach`, `send_ack`, `collect_until_close`, `send_channel_open`, `await_channel_accept` → take `&mut dyn NoshSendStream`/`NoshRecvStream`
- `await_reattach_reply`, `reattach_collect`, `run_session_collect` → retargeted similarly
- `open_channel` → returns `Option<(Box<dyn NoshSendStream>, Box<dyn NoshRecvStream>)>`
- `datagram_roundtrip`, `stream_echo_roundtrip`, `concurrent_roundtrip` → take `&dyn NoshTransport`

**Task 2 — main.rs pump + test harness (commit 505fe8f):**

Updated `fresh_session`, `reattach_session`, and `run_pump` in `main.rs` to take `&dyn NoshTransport` and `&mut dyn NoshSendStream`/`NoshRecvStream`. The native-QUIC connect path boxes the connection immediately after `client::connect(...)` returns:
```rust
let conn: Box<dyn NoshTransport> = Box::new(QuinnTransport(quinn_conn));
```

Updated `channel.rs` `run_scrollback_drain_task` to accept `Box<dyn NoshSendStream>`/`NoshRecvStream`.

Updated all integration test files with minimal syntactic changes (no assertion changes):
- `tests/common/mod.rs`: `session_marker_usable` wraps conn internally
- `tests/session.rs`, `migration.rs`, `persistence.rs`, `reattach.rs`, `predict.rs`, `render.rs`, `sync.rs`, `channel_mux.rs`: add `let qt = QuinnTransport(conn.clone())` where needed; wrap raw quinn streams from `conn.open_bi()` in `Box::new(QuinnSendStream(s))`/`Box::new(QuinnRecvStream(r))` for reattach paths; replace `&mut ctrl_send` with `&mut *ctrl_send` for deref-coercion to `&mut dyn NoshSendStream`

## Deviations from Plan

### Auto-added missing critical functionality

**1. [Rule 2 - Missing Functionality] Added rtt() and is_closed() to NoshTransport trait**
- Found during: Task 2
- Issue: `run_pump` uses `conn.rtt()` for predictor culling accuracy (D-17-02a) and `conn.close_reason().is_some()` for the BUG-C loss-overlay silence gate. Neither is on the `NoshTransport` trait as defined in Phase 23.
- Fix: Added `fn rtt() -> Duration` (default `Duration::ZERO`) and `fn is_closed() -> bool` (default `false`) to the `NoshTransport` trait with default implementations. Quinn wrappers in both nosh-client and nosh-server override them to delegate to `conn.rtt()` and `conn.close_reason().is_some()`. Without this, WebTransport connections would show zero-RTT prediction culling (suboptimal UX) and the BUG-C idle-shell overlay fix would silently regress.
- Files modified: `crates/nosh-proto/src/transport_trait.rs`, `crates/nosh-client/src/quinn_transport.rs`, `crates/nosh-server/src/quinn_transport.rs`
- Commits: 14da0e0, 505fe8f

**2. [Rule 2 - Missing Functionality] Added Box<dyn NoshSendStream>: NoshSendStream and Box<dyn NoshRecvStream>: NoshRecvStream blanket impls**
- Found during: Task 2
- Issue: `spawn_ctrl_drain` and `run_scrollback_drain_task` take owned stream values. After the refactor, callers hold `Box<dyn NoshSendStream>`; without a blanket impl, the tokio::spawn closures cannot move these into the async block as `impl NoshSendStream`.
- Fix: Added blanket impls in `nosh-proto/src/transport_trait.rs` delegating all methods through `**self`.
- Files modified: `crates/nosh-proto/src/transport_trait.rs`
- Commit: 14da0e0

## Known Stubs

None. This is a pure refactor — no new stub patterns introduced. All existing session, reattach, and scrollback logic is preserved byte-for-byte.

## Threat Surface Scan

No new network endpoints, auth paths, file access patterns, or schema changes introduced. The refactor is transport-agnostic at the type level only; wire format is unchanged.

## Self-Check: PASSED

- `crates/nosh-client/src/quinn_transport.rs` exists (155 lines, contains `impl NoshTransport for QuinnTransport`) ✓
- `crates/nosh-client/src/client.rs` contains `dyn NoshTransport` ✓
- Commit 14da0e0 exists ✓
- Commit 505fe8f exists ✓
- `cargo test --workspace`: all 21 test suites pass, 0 failures ✓
