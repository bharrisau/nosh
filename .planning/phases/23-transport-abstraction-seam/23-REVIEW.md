---
phase: 23-transport-abstraction-seam
reviewed: 2026-06-13T10:00:00Z
depth: standard
files_reviewed: 6
files_reviewed_list:
  - crates/nosh-proto/src/transport_trait.rs
  - crates/nosh-proto/src/transport_trait_tests.rs
  - crates/nosh-proto/src/lib.rs
  - crates/nosh-server/src/quinn_transport.rs
  - crates/nosh-server/src/channel.rs
  - crates/nosh-server/src/server.rs
findings:
  critical: 0
  warning: 2
  info: 2
  total: 4
status: issues_found
---

# Phase 23: Code Review Report

**Reviewed:** 2026-06-13
**Depth:** standard
**Files Reviewed:** 6
**Status:** issues_found

## Summary

Phase 23 introduces a transport abstraction seam (`NoshTransport` / `NoshSendStream` / `NoshRecvStream`) as a pure no-behaviour-change refactor. The critical items raised during planning — specifically the `finish().await` correctness risk, the `MAX_FRAME_LEN` DoS guard, and the auth-before-boxing ordering — are all handled correctly. No blockers were found.

Two warnings are raised: one is a subtle documentation inaccuracy in the `stopped()` trait that could mislead a future WebTransport implementor into incorrect behaviour; the other is a misleading code comment in `quinn_transport.rs` that asserts the wrong method is being dispatched. Two info items cover minor dead-code and a test blind spot.

## Warnings

### WR-01: `NoshSendStream::stopped` doc says "half-close confirmation" but that is not quinn's semantic

**File:** `crates/nosh-proto/src/transport_trait.rs:148–150`

**Issue:** The doc comment on `NoshSendStream::stopped` reads:

> Wait until the peer acknowledges the stream has stopped (half-close confirmation). Typically called after `finish()`.

Quinn's `SendStream::stopped()` actually resolves in two distinct cases: (a) the peer sends `STOP_SENDING` (meaning it is aborting the receive), or (b) the local side has called `finish()` and the peer acknowledges receipt of all data (`Ok(None)`). The first case is NOT a confirmation of successful delivery — it is the peer rejecting the stream. The wrapper at `quinn_transport.rs:117–120` silently discards the `Option<VarInt>` result, so callers cannot distinguish between "peer read all data" and "peer sent STOP_SENDING with an error code". The doc comment actively suggests this method is a drain-completion signal, which is only half true.

This does not affect the current Quinn implementation (the timeout at all call sites in `server.rs` and `channel.rs` absorbs both outcomes correctly). However, the trait documentation is the contract that the Phase 24 WebTransport implementor will rely on. A WebTransport author who reads this doc may implement `stopped()` to resolve only on clean peer acknowledgement, while the Quinn wrapper also resolves on peer abort — making the two implementations behave differently across the same trait.

**Fix:** Update the doc comment and the wrapper to be explicit about both resolution conditions:

```rust
/// Resolves when either:
/// - the peer acknowledges receipt of all stream data after a `finish()` (clean drain), OR
/// - the peer sends a STOP_SENDING frame (aborting the stream, `Some(error_code)`)
///
/// The returned `anyhow::Result<()>` discards the stop code; callers that need to
/// distinguish abort from clean drain must use the underlying transport's native API.
/// Typically called with a timeout after `finish()` as a best-effort drain wait.
async fn stopped(&mut self) -> anyhow::Result<()>;
```

---

### WR-02: Misleading comment in `QuinnRecvStream::read_exact` claims `AsyncReadExt` method is used, but inherent method dispatch wins

**File:** `crates/nosh-server/src/quinn_transport.rs:136–138`

**Issue:**

```rust
async fn read_exact(&mut self, buf: &mut [u8]) -> anyhow::Result<()> {
    // AsyncReadExt::read_exact — imported at module level as `AsyncReadExt as _`
    Ok(self.0.read_exact(buf).await.map(|_| ())?)
}
```

The comment asserts that `tokio::io::AsyncReadExt::read_exact` is being called (imported via `use tokio::io::AsyncReadExt as _`). This is incorrect. Rust's method resolution gives priority to inherent methods over trait methods; `quinn::RecvStream` has its own inherent `read_exact(&mut self, buf: &mut [u8]) -> Result<(), ReadExactError>` method. That inherent method is what gets called — not `AsyncReadExt::read_exact`.

The `#[allow(unused_imports)]` on the `AsyncReadExt` import line is itself evidence that the import may be going unused. The net result is correct (quinn's native `read_exact` is the right call), and the `.map(|_| ())` is a redundant no-op since the inherent method already returns `Result<(), _>`. But the false comment will mislead anyone auditing this wrapper or investigating an `AsyncReadExt` import warning.

**Fix:** Update the comment and remove the redundant `.map(|_| ())`:

```rust
async fn read_exact(&mut self, buf: &mut [u8]) -> anyhow::Result<()> {
    // Calls quinn::RecvStream::read_exact (inherent method, not AsyncReadExt).
    // Returns Result<(), ReadExactError>; the ? converts ReadExactError -> anyhow::Error.
    Ok(self.0.read_exact(buf).await?)
}
```

Remove or correct the `#[allow(unused_imports)]` comment on the `AsyncReadExt` import and verify whether the import is genuinely needed. If `quinn::RecvStream` implements `tokio::io::AsyncRead`, bringing `AsyncReadExt` into scope would shadow the inherent `read_exact` — but since the inherent method exists and is what Rust picks, the import does nothing for this call site.

---

## Info

### IN-01: `MockRecvStream::read_exact` in object-safety test does not write to the output buffer

**File:** `crates/nosh-proto/src/transport_trait_tests.rs:77–79`

**Issue:** The `MockRecvStream` used in the `nosh_recv_stream_is_object_safe` test implements `read_exact` as a no-op that returns `Ok(())` without writing any bytes into `buf`. This is technically incorrect for a mock of `read_exact` (which is contracted to fill `buf` completely). It is harmless here because the test never actually calls `read_exact` on the mock — it only checks that `Box<dyn NoshRecvStream>` compiles. However, if this mock is later reused in a test that does call `read_exact`, it would silently return uninitialised bytes as if they were valid data, potentially producing confusing failures.

**Fix:** Either document the mock as "object-safety only, do not use for data tests", or have it return an error:

```rust
async fn read_exact(&mut self, _buf: &mut [u8]) -> anyhow::Result<()> {
    anyhow::bail!("MockRecvStream: read_exact not implemented (object-safety mock only)")
}
```

---

### IN-02: `QuinnTransport::accept_bi` / `open_bi` error mapping loses quinn-specific error context

**File:** `crates/nosh-server/src/quinn_transport.rs:73–81`

**Issue:** Both `accept_bi` and `open_bi` map the `quinn::ConnectionError` to `anyhow::Error` via the `?` operator, which discards the specific variant (`ConnectionLost`, `ApplicationClosed`, `TimedOut`, etc.). In `server.rs:578–586` the caller matches the `anyhow::Error` with a comment "map connection-close variants to Ok(()) ... we check the underlying error string as a heuristic." Inspecting the error as a string is fragile and will break if quinn changes its error formatting.

This is not a Phase 23 regression — the comment and heuristic existed before this phase. The trait abstraction has made the underlying error type opaque, slightly worsening the situation, but the operational impact is confined to potentially mistreating a connection-closed event as a transport error rather than a clean exit (a cosmetic logging difference, not data loss).

**Fix:** No immediate action required for this phase. For Phase 24, consider adding a `ConnectionClosed` error variant to `NoshTransport`'s error surface, or returning a typed `NoshConnectionError` from `accept_bi`/`open_bi` so callers can distinguish clean close from transport loss without inspecting error strings.

---

_Reviewed: 2026-06-13_
_Reviewer: Claude (gsd-code-reviewer)_
_Depth: standard_
