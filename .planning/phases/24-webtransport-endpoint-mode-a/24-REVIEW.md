---
phase: 24-webtransport-endpoint-mode-a
reviewed: 2026-06-13T07:41:00Z
depth: deep
files_reviewed: 9
files_reviewed_list:
  - crates/nosh-server/src/wt_transport.rs
  - crates/nosh-client/src/wt_transport.rs
  - crates/nosh-server/src/main.rs
  - crates/nosh-client/src/main.rs
  - crates/nosh-server/src/server.rs
  - crates/nosh-proto/src/transport_trait.rs
  - crates/nosh-client/src/quinn_transport.rs
  - crates/nosh-server/src/quinn_transport.rs
  - crates/nosh-client/tests/webtransport.rs
findings:
  critical: 1
  warning: 3
  info: 3
  total: 7
status: fixed
---

# Phase 24: Code Review Report

**Reviewed:** 2026-06-13T07:41:00Z
**Depth:** deep
**Files Reviewed:** 9
**Status:** issues_found

## Summary

Phase 24 adds WebTransport Mode A to both server and client. The implementation correctly reproduces all six API-difference adaptations called out in the research (async `finish()`, consuming `stop()`, double-await `open_bi()`, `quic_connection()` for `datagram_send_buffer_space`, `max_datagram_size()` not passing through the raw quinn value, and the `SendDatagramError` three-variant map). The pre-auth DoS semaphore and auth-timeout are present and structurally parallel to the native accept loop.

The **auth-bypass gate is correctly structured** — the `#[cfg(any(test, feature = "test-support"))]` / `#[cfg(not(...))]` pair is a compile-time constant, not a runtime branch, so there is no code path in a release build that reaches the bypass. The pattern is clean.

One **blocker** exists: the server-side `handle_connection_wt` is `pub(crate)` but the whole module is only compiled under `#[cfg(feature = "webtransport")]`. The function's test-bypass code is gated correctly, but the function calls `conn.accept_bi()` and immediately enters the session pump *without any `accept_bi` stream count limit* — a single unauthenticated connection can open an unbounded number of bidi streams before inner auth is wired in Phase 25. In the current Phase 24 code this is a real BLOCKER because release builds close the connection immediately (before stream accept), but test-support builds do not — and because test-support is a dev-dep feature rather than a release feature this is contained; however there is a **larger structural problem**: the `run_wt_accept_loop` function accepts connections and wraps them in a `Box<dyn NoshTransport>` *before* calling `handle_connection_wt`, meaning the `_permit` (pre-auth semaphore slot) is held until `handle_connection_wt` returns — which for the test-support path is the entire session lifetime, not just the auth phase. This exhausts the semaphore for long-lived sessions, capping the whole server at `max_concurrent` simultaneous sessions rather than just `max_concurrent` simultaneous half-open handshakes.

Three **warnings** cover: an off-by-one in the privilege-boundary check, an unused import in the server `wt_transport.rs`, and missing `spawn_wt_server(None)` shell-skip guard in `wt03`.

Three **info** items cover: the known unused-import in the test file (flagged by the review prompt), a bare `use rustls;` style issue in client wt_transport, and a minor doc comment imprecision.

---

## Critical Issues

### CR-01: Semaphore permit held for entire session lifetime, not just the auth phase

**File:** `crates/nosh-server/src/wt_transport.rs:348-370`

**Issue:** In `run_wt_accept_loop`, the semaphore permit (`_permit`) is moved into the spawned task and held from connection arrival until `handle_connection_wt` returns (line 349: `let _permit = permit;`). In the **native QUIC path** (`run_accept_loop` in `server.rs`), the permit is released as soon as the TLS handshake + identity extraction is complete, before `handle_connection` is called. The WT loop's permit stays alive for the entire session — including the shell session proper — so `max_concurrent_handshakes` becomes an effective cap on simultaneous **sessions**, not simultaneous **handshakes**. With the default of 64, a server with 64 active sessions will silently drop every new incoming WebTransport connection with no error to the client, even from legitimate users. This is a DoS via session exhaustion that the native QUIC path does not have.

The CONTEXT.md explicitly states "parity with `run_accept_loop`" (D-13). The research's Pattern 6 pseudocode moves `_permit` into the task but the comment says "Hold the permit for the duration of the auth phase" — the auth phase ends when `handle_connection_wt` is called, not when it returns.

**Fix:** Release the permit before (or at the start of) the session pump. The simplest change is to drop the permit after the three-step accept completes and before `handle_connection_wt` is called:

```rust
tokio::spawn(async move {
    let result = tokio::time::timeout(auth_timeout, async {
        let session_request = incoming.await
            .context("WebTransport IncomingSession resolve")?;
        let conn: Connection = session_request.accept().await
            .context("WebTransport session accept")?;
        // Auth phase complete — release the pre-auth slot so the semaphore
        // only caps half-open handshakes, not active sessions (parity with
        // run_accept_loop which releases before handle_connection).
        drop(permit);   // <-- release here
        let transport: Box<dyn NoshTransport> = Box::new(WtransportTransport(conn));
        handle_connection_wt(transport, registry, shell).await
    }).await;
    // ...
});
```

Note: in Phase 24, `handle_connection_wt` in **release** builds closes immediately (no inner auth), so the issue is latent but will become acute when Phase 25 wires the real session pump. Fix now to match the native path contract.

---

## Warnings

### WR-01: Off-by-one in privilege-boundary port check

**File:** `crates/nosh-server/src/wt_transport.rs:291`

**Issue:** The privilege-hint message is emitted when `port <= 1024`. Port 1024 is unprivileged on Linux (the convention is ports < 1024 are privileged, i.e. `port < 1024` or equivalently `port <= 1023`). As written, binding port 1024 — which succeeds without root on any standard Linux system — produces a spurious warning "port 1024 requires root or setcap". This is a minor correctness issue but will confuse operators who choose port 1024 as an unprivileged alternative to 443.

**Fix:**
```rust
if port < 1024 {
    format!(
        "bind WebTransport endpoint to {addr}: port {port} requires root or \
        `setcap CAP_NET_BIND_SERVICE`. Use --port 4433 for dev/CI."
    )
```

### WR-02: `use tokio::io::AsyncWriteExt as _` may be unused in server wt_transport.rs

**File:** `crates/nosh-server/src/wt_transport.rs:51`

**Issue:** The import `use tokio::io::AsyncWriteExt as _;` is present at the module level. In `WtransportSendStream`, `write_all` calls `self.0.write_all(data).await?` and `flush` calls `self.0.flush().await?`. If `wtransport::SendStream` exposes `write_all` and `flush` as inherent methods (not via `tokio::io::AsyncWrite`), this import is unused and the compiler will warn. Conversely, if those methods come from `AsyncWrite`/`AsyncWriteExt`, the import is needed. The client-side `wt_transport.rs` makes this explicit by importing `AsyncWriteExt as _` *inside* the `flush()` method body (line 165), which is the correct scoped pattern. The server file imports it at the module top-level, which may produce a dead-import warning if `wtransport::SendStream::write_all` is an inherent method.

This is a build-noise warning, not a bug, but the inconsistency between the two `wt_transport.rs` files is confusing: the client scopes it to the method body; the server hoists it to the module top. One of the two is wrong or redundant.

**Fix:** Align with the client pattern — either scope the import inside `flush()` only (if `write_all` is inherent), or if both methods need it, hoist it in both files consistently. At minimum, verify with `cargo build --features webtransport 2>&1 | grep unused` that no warning fires.

### WR-03: `wt03_raw_quic_downgrade_rejected` panics if `spawn_wt_server` returns `None`

**File:** `crates/nosh-client/tests/webtransport.rs:195-197`

**Issue:** `spawn_wt_server` returns `Option<WtTestServer>`. In the common harness the function returns `None` only when no shell is passed **and** the test body needs to skip — but looking at the actual implementation in `common/mod.rs:265`, `spawn_wt_server` always returns `Some(...)` regardless of the `shell` parameter (it is `Option<String>` for the shell override, not a skip guard). The `.expect("spawn_wt_server returned None")` in `wt03` will never fire, but the two shell-based tests (`wt01`, `wt02`) correctly check `have_sh()` before calling `spawn_wt_server` and use `.expect(...)` only after that guard. `wt03` passes `None` for the shell and calls `.expect(...)` without the `have_sh()` guard.

More subtly: the `wt03` test spawns a WT server with `shell: None`, meaning the server will call `handle_connection_wt` which (in test-support builds) tries to call `conn.accept_bi()` and then `run_session` — which will fail or hang waiting for a shell if `/bin/sh` is not present and a test-support auth bypass connection arrives. The test then creates a raw-QUIC client that should fail before reaching a session, so this is unlikely to matter in practice. But the missing `have_sh()` guard is inconsistent with the pattern in `wt01`/`wt02` and could cause a confusing test failure on a minimal CI image.

**Fix:** Add the guard for consistency and defensive correctness:
```rust
async fn wt03_raw_quic_downgrade_rejected() {
    // wt03 does not rely on the shell, but the server task is still spawned.
    // Ensure have_sh() or accept a None shell gracefully.
    let server = common::spawn_wt_server(None)
        .await
        .expect("spawn_wt_server must succeed");
    // ...
```
(The `have_sh()` guard is not strictly needed here since no shell session is expected, but the `.expect("returned None")` message is misleading because `spawn_wt_server(None)` never returns `None` — fix the message to reflect the real failure mode.)

---

## Info

### IN-01: Known unused import — `nosh_proto::transport_trait::NoshTransport` in common test module

**File:** `crates/nosh-client/tests/common/mod.rs:17`

**Issue:** `use nosh_proto::transport_trait::NoshTransport;` is imported but the only use in the file is in a comment on line 316 ("Wrap the quinn::Connection in QuinnTransport so it satisfies `&dyn NoshTransport`"). The actual code uses `QuinnTransport` directly (line 317: `let qt = QuinnTransport(conn.clone())`). The `NoshTransport` bound is satisfied implicitly. This produces an unused-import compiler warning. The prompt calls this out as a known item.

**Fix:** Either remove the import or add an `#[allow(unused_imports)]` attribute if the import is kept for documentation purposes.

### IN-02: Bare `use rustls;` in client wt_transport.rs

**File:** `crates/nosh-client/src/wt_transport.rs:54`

**Issue:** The import `use rustls;` (line 54) is a bare module import. It is used on line 260 as `rustls_client_cfg: rustls::ClientConfig`. While this compiles, the conventional Rust style is to import the type directly (`use rustls::ClientConfig;`) or not import the module path at all and use the fully-qualified path inline. The bare `use rustls;` is unusual and may generate a lint warning depending on edition/toolchain settings.

**Fix:** Replace with `use rustls::ClientConfig;` (used in the function parameter on line 260) and update the parameter type to `ClientConfig`. Or remove the `use rustls;` and spell the type as `rustls::ClientConfig` inline — which already works via the workspace dep.

### IN-03: `handle_connection_wt` doc comment says "accepts first bidi stream" but that only happens in the test-support path

**File:** `crates/nosh-server/src/wt_transport.rs:373-411`

**Issue:** The function doc comment (line 373-381) says "After the inner-auth gate the dispatch is identical to `handle_connection`" and "accept_bi() → read first frame → SessionOpen/Reattach". This is accurate only in the `test-support` path. In release builds, the function closes the connection and returns before reaching `accept_bi()`. The doc comment should note this clearly — a reader of the doc-only view will not see the release path closes immediately.

**Fix:** Add a sentence to the doc comment: "In release builds (no `test-support` feature), the function closes the connection immediately and returns — the `accept_bi` dispatch is only reached in test-support mode. Phase 25 will replace the close with the real inner SSH-key handshake."

---

_Reviewed: 2026-06-13T07:41:00Z_
_Reviewer: Claude (gsd-code-reviewer)_
_Depth: deep_
