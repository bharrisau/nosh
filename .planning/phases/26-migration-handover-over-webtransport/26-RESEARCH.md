# Phase 26: Migration Handover over WebTransport — Research

**Researched:** 2026-06-14
**Domain:** WebTransport reconnect loop, cold-reattach wiring, MH-2 double-attach guard, session-loss detection
**Confidence:** HIGH — grounded entirely in direct reads of the production codebase (registry.rs, server.rs, main.rs, wt_transport.rs, inner_auth.rs)

---

<user_constraints>
## User Constraints (from CONTEXT.md)

### Locked Decisions
- **D-01:** Persistent, Mosh-style reconnect. On WebTransport session loss the client keeps retrying with bounded exponential backoff, shows the existing reconnecting banner + `~.` abort instructions, and resumes on success. No hard time limit.
- **D-02:** Each reconnect attempt re-runs the full inner handshake (Phase 25) on the new WebTransport session, then sends `Reattach` — never reattaches before inner auth completes (MH-1 state-machine guard).
- **D-03:** Server-side `run_reattach_session` + `SequencedOutputBuffer` byte-exact replay are reused unchanged.
- **D-04:** Concurrent same-token reattach resolves atomically to exactly one active session via `SessionRegistry::reattach` — MH-2 double-attach guard. (Note: CONTEXT.md says `try_reattach` / `HashMap::remove`, but the real implementation is `SessionRegistry::reattach` using `SlotState` — see MH-2 finding below.)
- **D-05:** Reattach token is rotated on every successful reattach.

### Claude's Discretion
- Session-loss detection mechanism: recommend treating a datagram/stream write error OR a datagram-receive timeout (keepalive miss) as "session lost".
- Keystrokes typed during the reconnect gap: recommend bounded queue, flushed on successful reattach, cleared on `~.` abort.
- Backoff schedule (initial delay, multiplier, cap): planner's call.

### Deferred Ideas (OUT OF SCOPE)
None new — this phase wires proven machinery into the WebTransport reconnect loop.
</user_constraints>

<phase_requirements>
## Phase Requirements

| ID | Description | Research Support |
|----|-------------|------------------|
| WT-06 | A client survives a network change in WebTransport mode — detects session loss, reconnects, re-runs inner handshake, resumes via 1-RTT cold reattach with byte-exact replay; concurrent same-token reattach resolves atomically to exactly one active session | All research focus areas below apply directly to WT-06 |
</phase_requirements>

---

## Summary

Phase 26 is principally a wiring exercise: the three components needed (client reconnect loop, inner-auth re-run, cold-reattach path) all already exist. What does not yet exist is WT-specific: the session-loss detection signal that triggers the reconnect in WebTransport mode, and an integration test that forces a simulated session drop.

The MH-2 double-attach guard is **already fully implemented** and does not require any new code. `SessionRegistry::reattach` atomically transitions `Orphaned → Reconnecting` under the registry mutex; a second call for a `Reconnecting` slot returns `NotOrphaned`. The existing test `reattach_active_or_reconnecting_is_rejected` (registry.rs:1624) covers the concurrent-same-token case at the unit level. Phase 26 adds an integration test that exercises the full path over a real WebTransport connection.

The client reconnect loop is **also substantially implemented**. The outer `loop` in `main.rs` already handles the WT path (`#[cfg(feature = "webtransport")]`), calls `run_inner_auth_client`, dispatches to `fresh_session_on_stream` or `reattach_session_on_stream` based on whether a token exists, handles `PumpOutcome::TransportDrop` with bounded-exponential backoff, and shows the "reconnecting…" banner. The only gaps are: (1) session-loss detection inside `run_pump` currently fires on datagram/stream *read* errors and the 5 s datagram silence + `conn.is_closed()` gate — this works for native QUIC but requires verification that WebTransport session close surfaces identically; (2) the keystroke buffering during the gap is not yet implemented; (3) the SC#4 integration test is absent.

Token rotation (`D-05`) uses the mint-then-commit pattern (`mint_token_candidate` / `commit_token`) already in `run_reattach_session` (server.rs:1639-1658) — the WT path inherits this unchanged.

**Primary recommendation:** the planner should structure this phase as: (a) verify session-loss detection is correct in WT mode (small audit of the `PumpOutcome::TransportDrop` return paths); (b) add optional keystroke buffering; (c) write the SC#4 integration test that drops a WT session and confirms seamless resume.

---

## Architectural Responsibility Map

| Capability | Primary Tier | Secondary Tier | Rationale |
|------------|-------------|----------------|-----------|
| Session-loss detection | Client (run_pump) | — | Datagram/stream read errors and silence timer already in run_pump |
| Reconnect loop + backoff | Client (main.rs outer loop) | — | The `loop` in main() is the reconnect supervisor |
| Inner auth re-run on new WT session | Client (inner_auth.rs) | — | `run_inner_auth_client` called per WT connect attempt; already wired |
| Reattach frame dispatch | Client (main.rs `reattach_session_on_stream`) | — | Already calls `send_reattach` on the authenticated stream |
| Two-factor token+identity validation | Server (registry.rs `reattach`) | — | Atomic under registry lock |
| MH-2 double-attach guard | Server (registry.rs `SlotState`) | — | Already implemented via `Orphaned → Reconnecting` state machine |
| Byte-exact replay | Server (server.rs `run_reattach_session`) | — | Unchanged from v1.1 |
| Token rotation | Server (slot `mint_token_candidate` / `commit_token`) | — | W1-safe pattern already in run_reattach_session |
| Keystroke buffering during gap | Client (main.rs input path) | — | New bounded queue, flushed on reattach, cleared on `~.` |

---

## Standard Stack

No new crates. This phase is pure wiring of existing code. All crates listed are already in the workspace.

### Core (already present)
| Crate | Version | Purpose |
|-------|---------|---------|
| `wtransport` | 0.7.1 | WebTransport session (WT transport) |
| `nosh-client` (`wt_transport.rs`) | — | `WtransportTransport impl NoshTransport` |
| `nosh-client` (`inner_auth.rs`) | — | `run_inner_auth_client` — re-run per reconnect |
| `nosh-server` (`server.rs`) | — | `run_reattach_session` — unchanged |
| `nosh-server` (`registry.rs`) | — | `SessionRegistry::reattach` + MH-2 guard |

### No new packages needed
The integration test (`nosh-tests`) already uses `wtransport`-feature builds. No `## Package Legitimacy Audit` section is required.

---

## MH-2 Guard Status — Primary Research Finding

**Finding: MH-2 is ALREADY fully implemented. The guard does NOT use `HashMap::remove`; it uses an atomic `SlotState` transition.**

Reading `crates/nosh-server/src/registry.rs`:

- `SlotState` (line 219-226): three variants — `Active`, `Orphaned`, `Reconnecting`.
- `SessionRegistry::reattach` (line 676-728):
  1. Acquires the registry `inner` Mutex.
  2. Finds the first slot in the identity's `Vec` whose `token()` matches the presented token.
  3. Checks `slot.state() != SlotState::Orphaned` — if `Active` or `Reconnecting`, returns `Err(ReattachReject::NotOrphaned)` (line 712-713).
  4. Calls `slot.mark_reconnecting()` while STILL UNDER the registry lock (line 718) — this is the atomicity point.
  5. Releases the lock, returns `Ok(slot)`.

This is strictly correct and raceless: the `Orphaned → Reconnecting` transition happens under the same mutex that serialises all `reattach` calls. Two concurrent `reattach` attempts with the same token will race to acquire the lock; the winner transitions the slot to `Reconnecting`; the loser sees `state() == Reconnecting` and gets `NotOrphaned`.

The CONTEXT.md note about `HashMap::remove` is an outdated design sketch, NOT the implementation. The real implementation is superior: it retains the slot in the registry so the slot's `Arc` identity is stable (the orphan-exit watcher's `Arc::ptr_eq` check, described at line 668, remains valid across the reattach).

**What Phase 26 needs to add for MH-2:** an integration test that submits two concurrent `Reattach` frames with the same token over real WebTransport connections and asserts exactly one `ReattachOk` and one `ReattachErr`. This test does NOT require code changes to the registry — it proves the existing guard holds under concurrent WebTransport connections.

Existing unit tests covering MH-2 (registry.rs):
- `reattach_active_or_reconnecting_is_rejected` (line 1624) — covers both Active rejection and Reconnecting rejection (second call after first succeeds). [VERIFIED: direct read]
- `reattach_matches_token_within_identity` (line 1548) — happy-path + same-Arc invariant. [VERIFIED: direct read]
- `reattach_wrong_identity_is_notfound` (line 1583) — no-oracle cross-identity test. [VERIFIED: direct read]

---

## Client Reconnect Loop — What Exists vs What is New

### What exists (reusable, no changes needed)

Reading `crates/nosh-client/src/main.rs`:

**The outer reconnect supervisor loop** (line ~1334) handles both native QUIC and WebTransport paths. The WT path (lines 1354-1480) already:
1. Calls `connect_wt` to open a new WebTransport session.
2. Calls `run_inner_auth_client` on the new WT session — fatal errors (host-key mismatch, EKM binding failure, TOFU decline) break out of the loop with `exit_code=1`; transient errors (`!fatal`) trigger backoff and retry.
3. Dispatches to `reattach_session_on_stream` (if `token.is_some()`) or `fresh_session_on_stream`.
4. On `PumpOutcome::TransportDrop`: shows "reconnecting…\r\n" banner, waits `backoff`, doubles backoff up to `BACKOFF_MAX` (10 s), then re-enters the loop.
5. `quit_during_backoff` handles `~.` and Ctrl-C during the backoff window.
6. `is_fatal_connect_error` classifies errors — the inner-auth fatal markers (`"host key mismatch"`, `"inner auth: host key"`, `"inner auth: ekm mismatch"`, `"not accepted"`) are already wired.

**`reattach_session_on_stream`** (line 1754):
- Calls `send_reattach` directly on the authenticated `ctrl_send` (no second `open_bi` — the inner auth stream IS the control stream).
- Calls `await_reattach_reply` for `ReattachOk` / `ReattachErr`.
- Updates `token_out` with `new_token` (D-05 rotation).
- On `ReattachErr`: clears token, returns `CleanExit(1)`.
- On `ReattachOk`: rebases `highest_applied = replaying_from_seq`, calls `run_pump`.

Backoff constants: `BACKOFF_INITIAL = 250 ms`, `BACKOFF_MAX = 10 s`, doubling per attempt. [VERIFIED: direct read line 55-56]

### What is new (Phase 26 work)

1. **Session-loss detection for WT sessions** — see section below. The existing `run_pump` `TransportDrop` return paths (stream read error at line 2125, datagram read error at line 2271, silence+`conn.is_closed()` at line 2305) already handle loss detection. Phase 26 must verify these paths fire correctly when a WebTransport session is closed (as opposed to a QUIC migration, which should NOT trigger TransportDrop). Since `WtransportTransport::is_closed()` delegates to `quic_connection().close_reason().is_some()` (wt_transport.rs:133), and `wtransport::Connection::receive_datagram()` returns an error when the session is closed, these paths already work. The plan should add an explicit comment documenting this.

2. **Keystroke buffering** — see section below.

3. **SC#4 integration test** — forcing a WebTransport session drop and verifying seamless resume.

---

## Session-Loss Detection over WebTransport

Reading `run_pump` (main.rs ~line 2021+) and `WtransportTransport` (wt_transport.rs):

### Existing loss-detection paths (both fire correctly for WT)

**Path A — stream read error** (line 2123-2126):
```rust
Err(e) => {
    tracing::warn!("reliable stream error, triggering reconnect: {e}");
    return Ok(PumpOutcome::TransportDrop);
}
```
When a WebTransport session is dropped, `read_message_ns` on the control stream returns an error. This fires `TransportDrop` immediately.

**Path B — datagram read error** (line 2268-2272):
```rust
Err(e) => {
    tracing::warn!("datagram channel error, triggering reconnect: {e}");
    return Ok(PumpOutcome::TransportDrop);
}
```
`WtransportTransport::read_datagram` calls `self.0.receive_datagram().await?` — returns error on session close. Fires `TransportDrop`.

**Path C — silence + connection-closed gate** (line 2284-2312):
After 5 s of datagram silence, the arm fires if `conn.is_closed()` is true. `WtransportTransport::is_closed()` returns `self.0.quic_connection().close_reason().is_some()` — correctly reflects a closed WT session. Shows the connection-loss overlay. Does NOT return `TransportDrop` from this arm — it shows the overlay and waits for Path A or B to fire.

### Critical asymmetry: native-QUIC migration must NOT trigger TransportDrop

For native QUIC, path migration (Wi-Fi → cellular) keeps the QUIC connection alive via connection IDs — `read_datagram` keeps working, no error fires. For WebTransport, session loss requires starting a new WT session (new HTTP/3 CONNECT upgrade). Path A and B fire on actual session death, not on path changes within a live session. This is correct for both cases.

**No code changes needed to session-loss detection for the WT path.** [ASSUMED: relies on wtransport's error model firing when a session closes — confirmed by the API: `receive_datagram` returns `ConnectionError` on close, which propagates as an `anyhow::Error`]

### What Phase 26 should do

Add a doc comment or a `tracing::info!` call in the WT reconnect loop that explicitly describes the loss-detection mechanism, and include the SC#4 integration test that exercises Path A or B on a forced session drop.

---

## Pending Decision: Network-Change Simulation in SC#4 Test

**Resolution (planner question from CONTEXT.md, resolved here against the built transport):**

The SC#4 test must force a WebTransport session drop + client rebind to simulate a network change. The concrete mechanism with `wtransport` 0.7.1:

**Recommended approach: Server-side connection close via `NoshTransport::close()`**

From `WtransportTransport` (wt_transport.rs:136-139):
```rust
fn close(&self, code: u32, reason: &[u8]) {
    self.0.close(VarInt::from_u32(code), reason)
}
```

In the integration test:
1. Establish a WT session and get a running `run_pump`.
2. The test server obtains the `WtransportTransport` (or the underlying `wtransport::Connection`) and calls `conn.close(1, b"network-change-sim")`.
3. The client's `read_datagram()` or `read_message_ns()` returns an error → `PumpOutcome::TransportDrop`.
4. The outer reconnect loop re-enters, calls `connect_wt` again, re-runs inner auth, then sends `Reattach`.
5. The test verifies: (a) the session resumed on the server side with the same session_id; (b) the PTY state was replayed; (c) the new token differs from the original.

**Alternative (rebind local socket):** `wtransport::Endpoint::rebind` may exist but is not part of the stable public API confirmed in the workspace. Do not use it.

**Alternative (write error injection):** Calling `ctrl_send.reset(1)` from inside the running pump to simulate a stream error is more complex and architecturally messier than a clean server-close.

**Verdict:** Use server-side `conn.close(code, reason)` as the simulation mechanism. This is deterministic, uses the existing `NoshTransport::close` trait method, and maps cleanly to what a proxy shutdown (the primary real-world trigger) would look like. [ASSUMED: based on `wtransport::Connection::close` semantics from docs; confirmed the method exists in wt_transport.rs]

---

## Token Rotation — D-05 Confirmation

Reading `crates/nosh-server/src/registry.rs` (lines 427-465) and `crates/nosh-server/src/server.rs` (lines 1631-1658):

`run_reattach_session` uses the **W1-safe mint-then-commit pattern**:
1. `slot.mint_token_candidate()` — generates a new UUID without storing it (line 1639).
2. Sends `ReattachOk { new_token, ... }` on the wire (line 1641-1646).
3. On write success: `slot.commit_token(new_token)` (line 1658) — now the slot holds the new token.
4. On write failure: re-orphans WITHOUT committing — client still holds the prior token and can retry.

The client side (`reattach_session_on_stream`, main.rs:1774): updates `*token_out = Some(new_token)` immediately on reading `ReattachOk`.

This path is UNCHANGED for the WebTransport reattach. The WT reattach calls `reattach_session_on_stream` which calls `send_reattach` → `await_reattach_reply` → server runs `run_reattach_session` (generic over `NoshTransport`). Token rotation happens identically. [VERIFIED: direct read]

---

## Keystroke Buffering During the Reconnect Gap

**Recommendation (Claude's Discretion):**

**Design:** A bounded `VecDeque<Vec<u8>>` of keystroke byte batches, created per `run_pump` invocation at the scope where `highest_applied` and `token` live (main.rs, just above the `loop`). The `stdin` arm in `run_pump` currently forwards keystrokes directly via `send_input` — when `send_input` returns an error the pump returns `TransportDrop`. Buffering must intercept this.

**Implementation strategy:**
- Keep buffering as an `Option<VecDeque<Vec<u8>>>` — `None` means "no buffering" (preserve existing native-QUIC behaviour unchanged, or enable only in WT mode). Both D-01 and the existing reconnect loop apply to both WT and native-QUIC, so the buffer can be transport-agnostic.
- When `send_input` fails (stream write error), push the batch to the buffer and signal `TransportDrop`. The `run_pump` returns. The outer loop stores the buffer (or passes it back in, since `run_pump` is called from `reattach_session_on_stream`).
- On successful reattach (after `ReattachOk`), flush the buffered batches to the new stream before entering the pump proper. Ordering is correct: `ReattachOk` → `commit_token` → flush buffer → resume datagram receive.
- `~.` abort clears the buffer (drop it without sending).

**Simpler alternative:** Buffer keystroke bytes in a `Vec<u8>` (single flattened byte string, not per-batch), flush as a single `send_input` on reattach. Simpler, loses batch granularity but batch granularity doesn't matter here (the server sees one PtyData chunk).

**Bound:** 64 KiB is generous for keystrokes typed in a few seconds of reconnect window. Beyond that, drop oldest (ring) or drop all and notify. Recommend `MAX_KEYSTROKE_BUFFER = 64 * 1024` bytes.

**Interaction with predictor:** Keystrokes in the buffer have already been through `predictor.on_input` (they were sent speculatively before the transport dropped). On reconnect the predictor is reset (fresh `run_pump`). The buffered bytes replayed on the new stream will be confirmed by the next datagram, correcting the display. No special predictor interaction needed.

**Planner decision:** whether to implement buffering as a first-class feature in this phase, or treat it as a "nice to have" after SC#1-SC#3 pass. The success criteria (WT-06, SC#1-SC#4) do not explicitly require keystroke buffering. Recommend implementing it as it completes the "seamless resume" promise (SC#4 asks for "no visible shell disruption"), but it can be a separate sub-task within the phase.

---

## Architecture Patterns

### System Architecture Diagram

```
[WebTransport session A — running]
         |
    session drops (transport close / path failure)
         |
    run_pump: datagram Err or stream Err → PumpOutcome::TransportDrop
         |
    outer loop (main.rs):
    1. eprintln!("reconnecting...\r")
    2. backoff.sleep() | quit_during_backoff(~.)
    3. connect_wt() → new WT session B
    4. run_inner_auth_client() on session B control stream
       [inner auth: EKM, nonces, SSH-key mutual challenge-response]
       → (ctrl_send, ctrl_recv) authenticated stream pair
    5. token.is_some() → reattach_session_on_stream()
         send_reattach(ctrl_send, token, last_acked_seq)
         await_reattach_reply → ReattachOk { new_token, replaying_from_seq }
         token_out = Some(new_token)
         highest_applied = replaying_from_seq
         run_pump(...) on the new session
         [server replays SequencedOutputBuffer, client re-opens channels]
    6. flush keystroke buffer (if any)
    7. re-enter pump — session resumed

[Server side]
  accept_bi (new WT session) → run_inner_auth_server → read Reattach
  registry.reattach(token, identity):
    Orphaned → Reconnecting (atomic, under lock)
    → Ok(slot) or Err(NotOrphaned) [MH-2 guard]
  mint_token_candidate → send ReattachOk { new_token }
  → commit_token (only after ReattachOk sent)
  replay SequencedOutputBuffer → pump resumes → mark_active
```

### Recommended Project Structure

No new files are needed. All changes are modifications to existing files:

```
crates/nosh-client/src/
    main.rs            # keystroke buffering + session-loss detection comment
nosh-tests/
    tests/             # SC#4 WebTransport reattach integration test
```

### Pattern 1: WT Reattach via Authenticated Stream (existing, not new)

`reattach_session_on_stream` already reuses the inner-auth stream for `Reattach` — the server's `handle_connection_wt` gate ensures no `Reattach` arrives before inner auth completes (MH-1 guard). The planner does NOT need to add a new stream open for the reattach frame. [VERIFIED: main.rs lines 1748-1799 and inner_auth.rs return value]

### Pattern 2: Token as Caller Responsibility

`SessionRegistry::reattach` does NOT rotate the token — it only validates and transitions state. The caller (`run_reattach_session`) is responsible for calling `mint_token_candidate` and `commit_token` AFTER a successful `ReattachOk` write. This pattern is already correct and the WT path inherits it. [VERIFIED: registry.rs:665-671 docstring]

### Anti-Patterns to Avoid

- **Calling `open_bi()` before inner auth returns** — the inner auth stream IS the control stream. `reattach_session_on_stream` correctly sends `Reattach` on `ctrl_send` without a second `open_bi()`. The server accepts exactly one stream per WT connection in the pre-auth phase.
- **Retrying on `ReattachErr`** — `ReattachErr` is terminal (the session is gone or identity mismatch). Current code sets `token = None` and returns `CleanExit(1)`, which is correct. Do not change this to retry.
- **Treating datagram silence alone as session loss** — the BUG-C comment at line 2285 documents this: datagram silence with `conn.is_closed() == false` is just an idle shell, not a dropped connection. The guard is already in place.
- **Logging the token or `new_token`** — callers must log only `identity.fingerprint()`. The D-07 invariant is documented throughout the codebase and must be maintained in new test code too.

---

## Don't Hand-Roll

| Problem | Don't Build | Use Instead |
|---------|-------------|-------------|
| Concurrent reattach exclusion | Custom mutex + HashMap remove logic | `SessionRegistry::reattach` — already atomic via `SlotState` |
| Token rotation | Generate-and-swap | `slot.mint_token_candidate()` + `slot.commit_token()` — W1-safe |
| Byte-exact replay | Re-read PTY from scratch | `SequencedOutputBuffer::replay_from` — already in registry |
| Inner auth on reconnect | New handshake protocol | `run_inner_auth_client` — reuse unchanged |
| Session-loss detection | Custom keepalive protocol | `NoshTransport::is_closed()` + stream/datagram read errors — already fires |

---

## Common Pitfalls

### Pitfall 1: Calling `open_bi` for the Reattach Frame

**What goes wrong:** Planner adds a `conn.open_bi()` before calling `send_reattach`, creating a second stream the server is not expecting. The server's WT handler has already consumed the one `accept_bi` call for the inner auth stream and is now waiting for `Reattach` on that same stream.

**Why it happens:** The native QUIC `reattach_session` (line 1647) does call `conn.open_bi()` — that pattern is correct for native QUIC where there is no pre-authenticated stream. The WT path has `reattach_session_on_stream` which does NOT call `open_bi` — this is the correct path.

**How to avoid:** Use `reattach_session_on_stream`, not `reattach_session`. The difference is documented in the function docstring at line 1748.

### Pitfall 2: Committing the Token Before Confirming the Write

**What goes wrong:** Calling `slot.rotate_token()` (commit-immediately) instead of `mint_token_candidate` + `commit_token` (W1-safe). If `ReattachOk` write fails after the token is committed, the client holds the old token but the slot holds the new token — the session becomes permanently un-reattachable.

**How to avoid:** `run_reattach_session` already uses the W1-safe pattern (server.rs lines 1639-1658). Do not change this pattern.

### Pitfall 3: Resetting `highest_applied` Incorrectly on Reattach

**What goes wrong:** Setting `*highest_applied = replaying_from_seq - 1` (off-by-one). This was the ROAM-02 BLOCKER that dropped one chunk per reconnect cycle.

**How to avoid:** The existing code (main.rs line 1679, 1783) sets `*highest_applied = replaying_from_seq` (inclusive, next-expected-seq convention). Do not change this.

### Pitfall 4: MH-1 Violation — Reattach Before Inner Auth Completes

**What goes wrong:** The reconnect loop dispatches to `reattach_session_on_stream` before `run_inner_auth_client` returns `Ok`. If inner auth is skipped or bypassed, the server's pre-auth gate will reject the `Reattach` frame with `InnerAuthFail` rather than processing it, and the session state is left inconsistent.

**How to avoid:** The existing code already enforces this — `run_inner_auth_client` must return `Ok` before dispatching to `reattach_session_on_stream`. The server's `handle_connection_wt` enforces the same order server-side.

### Pitfall 5: Keystroke Buffer Not Cleared on `ReattachErr`

**What goes wrong:** If `ReattachErr` is returned (session is gone), the buffered keystrokes must be discarded — they belong to a dead session. Replaying them to a fresh `SessionOpen` would be incorrect.

**How to avoid:** Clear the buffer when `ReattachErr` is handled (the code sets `token = None` and returns `CleanExit(1)` — also clear the keystroke buffer at this point, or simply don't flush it when token is None).

---

## Code Examples

### Server-side reattach entry point (existing, unchanged)

```rust
// Source: crates/nosh-server/src/server.rs line 1583
pub(crate) async fn run_reattach_session(
    conn: Box<dyn NoshTransport>,
    peer: SocketAddr,
    identity: nosh_auth::NoshPublicKey,
    mut send: Box<dyn NoshSendStream>,
    mut recv: Box<dyn NoshRecvStream>,
    reattach_params: ([u8; 16], u64), // (token, last_acked_seq)
    registry: Arc<crate::registry::SessionRegistry>,
) -> anyhow::Result<()>
```
This signature accepts `Box<dyn NoshTransport>` — it is already generic and works for WT connections unchanged.

### Registry reattach with atomic MH-2 guard (existing)

```rust
// Source: crates/nosh-server/src/registry.rs line 676
pub fn reattach(
    &self,
    token: &[u8; 16],
    identity: &NoshPublicKey,
) -> Result<Arc<SessionSlot>, ReattachReject>
// Errors: NotFound | IdentityMismatch | NotOrphaned
// On success: slot is Orphaned → Reconnecting (atomic under registry lock)
```

### Client WT reattach dispatch (existing)

```rust
// Source: crates/nosh-client/src/main.rs line 1754
async fn reattach_session_on_stream(
    conn: &dyn NoshTransport,
    mut ctrl_send: Box<dyn NoshSendStream>,
    mut ctrl_recv: Box<dyn NoshRecvStream>,
    token: [u8; 16],
    last_acked_seq: u64,
    highest_applied: &mut u64,
    resize: &mut platform::ResizeWatcher,
    token_out: &mut Option<[u8; 16]>,
    predict_mode: PredictDisplayMode,
    status: bool,
) -> anyhow::Result<PumpOutcome>
// Sends Reattach on the authenticated ctrl_send (no open_bi call)
```

### Forcing a WT session drop in an integration test (new — recommended pattern)

```rust
// Source: recommended pattern (ASSUMED — based on WtransportTransport::close implementation)
// In the integration test server or test harness:
//   1. Get a reference to the wtransport::Connection for the active session
//   2. Close it with an application error code:
server_conn.close(VarInt::from_u32(1), b"test-network-change");
// The client's read_datagram() / read_message_ns() returns Err → TransportDrop
// The outer loop reconnects, re-auths, sends Reattach
```

---

## Open Questions

1. **Token persistence across process restart**
   - What we know: Token is held in `main.rs` as `let mut token: Option<[u8; 16]>` — in memory only per process lifetime. If the client process exits and restarts, the token is gone.
   - What's unclear: Does WT-06 require surviving a client process restart, or only a network change within a running client?
   - Recommendation: WT-06 says "client survives a network change" — this implies a running process, not a restart. In-memory token is sufficient. No change needed.

2. **SC#4 test infrastructure — access to the server-side wtransport::Connection**
   - What we know: Integration tests use `spawn_server_wt` (inferred from Phase 24/25 test patterns). The test needs to close a specific WT session to trigger reconnect.
   - What's unclear: Does the test server expose a handle to the active WT connection? Phase 24/25 integration tests may have established a pattern. The planner should check the Phase 24/25 test files for the exact server-spawn API.
   - Recommendation: Expose a test-only channel (similar to `server_open_tx` on `SessionSlot`) that lets the test request a forced close of the current WT session.

3. **Scrollback channel re-open after reattach**
   - What we know: `run_pump` calls `client::open_channel(conn, send, recv, scrollback_channel_id, ChannelType::Scrollback)` at startup (line ~1979). On reattach, `run_pump` is called fresh — the scrollback channel is re-opened automatically.
   - What's unclear: Is the same `EvenIdAllocator` state preserved across reconnects, or reset? Reading the code: `id_alloc` is created fresh inside `run_pump` each call. Channel IDs restart from 2. The server's channel_map is empty on reattach (server.rs:1771 — "Channel state is empty on reattach — the client must re-open"). This is correct.
   - Recommendation: No action needed; the pattern is already correct.

---

## Environment Availability

Step 2.6: No new external dependencies for this phase. All required tools (Rust, cargo, wtransport, tokio) are verified present from Phase 24/25 completion.

---

## Assumptions Log

| # | Claim | Section | Risk if Wrong |
|---|-------|---------|---------------|
| A1 | `wtransport::Connection::receive_datagram()` returns an error when the session is closed (not a hang), which causes `run_pump` Path B to fire `TransportDrop` | Session-loss detection | If it hangs, session-loss detection relies on Path A (stream read) or Path C (silence+is_closed). Mitigation: the integration test validates this by observing reconnect after a forced close |
| A2 | Server-side `conn.close()` on the WT connection causes the client's `receive_datagram`/`read_message_ns` to return an error promptly (not after a long timeout) | Network-change simulation | If there is a long timeout, SC#4 test would be slow. Check wtransport::Connection close semantics if the test is unexpectedly slow |
| A3 | The keystroke buffer design (bounded VecDeque, flushed on reattach) does not require changes to `run_pump`'s function signature | Keystroke buffering | If buffering requires passing state across the pump boundary, the signature of `reattach_session_on_stream` would need an output parameter. Prefer accumulating in the outer loop instead |

**All critical findings (MH-2 guard status, existing reconnect loop, token rotation pattern, session-loss paths) were VERIFIED by direct codebase reads.**

---

## Sources

### Primary (HIGH confidence — direct codebase reads)

- `crates/nosh-server/src/registry.rs` lines 206-728 — `SlotState`, `SessionSlot`, `SessionRegistry::reattach`, all token management methods, existing MH-2 tests
- `crates/nosh-server/src/server.rs` lines 1583-1799 — `run_reattach_session` full body, W1-safe token mint/commit pattern
- `crates/nosh-client/src/main.rs` lines 1-200, 1260-1600, 1636-1800, 2044-2430 — outer reconnect loop, backoff constants, `PumpOutcome`, `reattach_session_on_stream`, `fresh_session_on_stream`, `run_pump` loss-detection arms
- `crates/nosh-client/src/wt_transport.rs` — `WtransportTransport` impl: `is_closed`, `close`, `read_datagram`, `export_keying_material`
- `crates/nosh-client/src/inner_auth.rs` lines 1-65 — `run_inner_auth_client` API: returns authenticated `(ctrl_send, ctrl_recv)` pair
- `crates/nosh-client/src/client.rs` lines 539-660 — `send_reattach`, `await_reattach_reply`, `ReattachOutcome`
- `.planning/phases/26-migration-handover-over-webtransport/26-CONTEXT.md` — locked decisions D-01..D-05
- `.planning/REQUIREMENTS.md` — WT-06 definition
- `.planning/ROADMAP.md` — Phase 26 success criteria SC#1-SC#4

### Secondary (MEDIUM confidence — prior research)

- `.planning/research/ARCHITECTURE.md` §"Migration handover" — design rationale, confirmed by codebase reads
- `.planning/research/SUMMARY.md` §"Phase 4: Migration Handover" — original research findings

---

## Metadata

**Confidence breakdown:**
- MH-2 guard status: HIGH — read the method body and all related tests directly
- Client reconnect loop: HIGH — read all relevant sections of main.rs directly
- Token rotation: HIGH — read run_reattach_session and SessionSlot token methods directly
- Session-loss detection: HIGH for stream/datagram error paths; MEDIUM for WT-specific behaviour (one ASSUMED claim, mitigated by integration test)
- Keystroke buffering: MEDIUM — design is clear but not yet validated in integration

**Research date:** 2026-06-14
**Valid until:** 2026-07-14 (codebase is authoritative; no external API drift risk)
