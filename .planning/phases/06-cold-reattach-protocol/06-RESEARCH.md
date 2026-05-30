# Phase 6: Cold Reattach Protocol — Research

**Researched:** 2026-05-30
**Confidence:** HIGH (all touchpoints read in-tree: registry.rs, server.rs, session.rs, client.rs, messages.rs, codec.rs, persistence.rs harness; cross-checked against PITFALLS #8/#9/#10 and ARCHITECTURE reattach section)

This phase is **additive on top of completed Phase 5**. Phase 5 already shipped `SessionRegistry` (keyed by SSH identity), `SessionSlot` (held as `Arc`, instance-keyed via `Arc::ptr_eq`), `SequencedOutputBuffer` (monotonic `u64` seq + 64 KiB drop-oldest ring + truncation flag + `lowest_retained_seq`), `orphan()`, `remove_slot()` (idempotent, `Arc::ptr_eq`), the reaper, and the orphan-exit watcher in `server.rs`'s `TransportLost` arm. Do **not** rebuild any of it.

---

## "What do I need to know to plan this well?"

### 1. The load-bearing safety invariant: preserve the slot `Arc` instance on reattach

`server.rs::run_session`'s `TransportLost` arm spawns a watcher:
```rust
let watcher_slot = slot.clone();          // an Arc<SessionSlot>
tokio::spawn(async move {
    let _exit = wait_task.await;          // awaits the ORPHANED shell's eventual exit
    watcher_registry.remove_slot(&watcher_slot);   // Arc::ptr_eq removal
});
```
`remove_slot` removes by `Arc::ptr_eq` (pointer identity), NOT by `session_id`. This was a deliberate Phase 5 choice (see the doc-comment on `remove_slot`) so that "once Phase 6 reattach swaps a live connection back onto a slot under the same `session_id`, an exit watcher from a PRIOR orphan generation can never remove the freshly-reattached slot."

**Consequence for Phase 6:** reattach MUST resume the SAME `Arc<SessionSlot>` instance that was orphaned (look it up in the registry, clone the `Arc`, rebind I/O onto it). It must NOT construct a new `SessionSlot` with the same `session_id`. The orphaned shell's `wait_task` is owned by the watcher task spawned at orphan time; the orphaned shell keeps running across the disconnect. The watcher stays valid: if the shell exits while orphaned (before reattach), the watcher removes the slot and a later `Reattach` for that token finds nothing → `ReattachErr`. If reattach happens first, the watcher is still bound to the same `Arc`; if the shell exits AFTER a successful reattach, the SAME watcher correctly removes the now-reattached slot (the reattach session loop must itself handle the shell-exit teardown too — see §6).

There is exactly one subtlety: after reattach, a NEW session loop drives the slot and may itself spawn its own exit handling. The original watcher (from the orphan) is STILL alive and STILL `Arc::ptr_eq`-bound. That is fine and desirable — `remove_slot` is idempotent, so whichever observer sees the shell exit first removes the slot; the other's call is a harmless no-op. The plan must NOT abort or leak the original watcher; it must remain the durable exit observer for the slot's lifetime.

### 2. Registry keying: identity is primary, token selects within identity

ARCHITECTURE.md sketches `HashMap<SessionToken, Arc<SessionSlot>>`, but Phase 5 shipped `HashMap<[u8;32] identity_key, Vec<Arc<SessionSlot>>>`. **Reconcile per CONTEXT.md: identity stays the primary key; the token is a selector WITHIN an authenticated identity.** This is also the security shape we want — `reattach(token, identity)` first scopes to the identity's slots, then matches the token. A token alone never reaches across identities.

Two viable representations (Claude's discretion, D-5th bullet):
- **(A) Token stored on the slot.** Add a `token: Mutex<[u8;16]>` (or a small newtype) field to `SessionSlot`; `reattach` scans `slots[identity]` for a slot whose token matches AND whose state is `Orphaned`. O(n) over the per-identity Vec (n ≤ cap = 5) — fine, matches the existing O(n) Vec scans in `orphan()`.
- **(B) Side index.** A `HashMap<token, Arc<SessionSlot>>` next to the identity map. More moving parts; must be kept in sync with eviction/removal. **Prefer (A)** — it reuses the established per-identity Vec-scan pattern and there is no second structure to desync on `remove_slot`/`orphan` eviction. The plan should pick (A) unless it finds a concrete reason not to.

Token type: a fixed `[u8; 16]` from `Uuid::new_v4().into_bytes()` (122-bit CSPRNG, `uuid` already a dep). Carrying it as `[u8;16]` (not a `String`) keeps the wire compact and the type `Copy`. Do NOT derive `Debug`/log it.

### 3. New `Message` variants (D-03) — additive & postcard-compatible

postcard encodes enum variants by their **index** (varint discriminant). Appending variants to the END of the existing enum is backward-compatible; reordering or inserting is NOT. Add the five new variants AFTER `SessionClose`:
```rust
    SessionOpened { token: [u8; 16] },
    Reattach { token: [u8; 16], last_acked_seq: u64 },
    ReattachOk { new_token: [u8; 16], replaying_from_seq: u64, truncated: bool },
    ReattachErr,                       // opaque, NO fields (D-07)
    Ack { seq: u64 },
```
Notes:
- `ReattachErr` carries **no fields** — uniform/opaque (D-07). No reason string, no code; bad-token and wrong-identity produce byte-identical frames.
- `ReattachOk` carries `replaying_from_seq` (the seq the server will replay from — equals `last_acked_seq+1` normally, or `lowest_retained_seq` if truncation happened) and a `truncated: bool` (D-09). `new_token` is the rotated single-use token (D-05).
- `SessionOpened` and `ReattachOk` both deliver a token; per CONTEXT discretion they may share a type but the messages stay distinct (different semantics; `ReattachOk` also carries replay metadata).
- Existing codec (`read_message`/`write_message`, length-delimited postcard) needs NO change — new variants serialize through it automatically. Add a `messages::session_variants_round_trip`-style unit test covering the 5 new variants (mirror the existing test in `codec.rs`).

### 4. Server first-frame dispatch (D-04)

Today `handle_connection` accepts one bidi stream then calls `run_session`, which reads the first frame and REQUIRES `SessionOpen`. Phase 6 generalizes the first-frame read to branch:
- `SessionOpen{..}` → fresh-session path (existing Phase 3/5 flow), and AFTER building+registering the slot, the server must **send `SessionOpened{token}`** as the slot's initial token (new step — the client needs the token to later reattach).
- `Reattach{token, last_acked_seq}` → reattach path: `registry.reattach(token, peer_identity)`.
- anything else → `conn.close(CLOSE_PROTOCOL, ...)` (existing behavior).

Cleanest structural move: pull the first-frame read UP into `handle_connection` (it already owns `peer_identity`), branch there, and pass the parsed open params into `run_session` (so `run_session` no longer re-reads the first frame). Alternatively keep the read inside a dispatcher fn. The plan picks the shape; the must-have is: the dispatch sees `peer_identity` (already extracted at server.rs:208) so two-factor auth is enforceable.

### 5. `registry.reattach` — two-factor, uniform error, atomic state guard (D-06/D-07/D-12)

Signature (suggested): `fn reattach(&self, token: &[u8;16], identity: &NoshPublicKey) -> Result<Arc<SessionSlot>, ReattachReject>` where `ReattachReject` is a private/internal enum the server collapses to a single opaque `ReattachErr` on the wire.

Algorithm, all under ONE registry lock acquisition for the lookup+state transition (atomicity, Pitfall #10 / D-12):
1. Scope to `slots[identity.key32()]` (the identity factor — factor 1 is already enforced by the TLS handshake + `AuthorizedKeysVerifier`; this is the binding check that the token's slot belongs to the SAME identity).
2. Find a slot whose token matches `token`.
3. Reject (uniform) if: no such identity entry, no slot matches the token, the matched slot is `Active` or `Reconnecting` (D-12 mutual exclusion), or the slot's shell already exited (slot already removed → not found). EVERY rejection path returns the SAME opaque error to the wire.
4. On success: transition the slot `Orphaned → Reconnecting` **inside the same lock**, return the `Arc` clone. The `Reconnecting` state blocks a concurrent second `Reattach` (the two-clients-one-session race). After replay+rebind completes, transition `Reconnecting → Active`.

`SlotState` gains a `Reconnecting` variant. Audit every existing `match`/comparison on `SlotState` (in `registry.rs`: `orphan()` counts `== Orphaned`, `reap_once()` checks `!= Orphaned` to keep, `orphan_count()`/`total_orphans()` filter `== Orphaned`). A `Reconnecting` slot must be treated like a NON-orphan for cap/reaper purposes: it is mid-rebind, must NOT be idle-reaped, must NOT count toward the orphan cap, must NOT be LRU-evicted. Concretely: the reaper's `if slot.state() != Orphaned { return true /*keep*/ }` already protects `Reconnecting` (it's not `Orphaned`, so it's kept — good). `orphan_count`/`total_orphans` only count `Orphaned`, so `Reconnecting` is excluded — good. Verify these read correctly after adding the variant; add asserts.

Token rotation (D-05): on successful reattach, generate a fresh `[u8;16]` via `Uuid::new_v4()`, store it on the slot (replacing the old), and return it so the server sends it in `ReattachOk{new_token}`. The old token is now invalid (single-use). Same on fresh open: `SessionOpened{token}` carries the slot's first token.

**Never log the token** (D-07). Log only `identity.fingerprint()` + an outcome string ("reattach accepted" / "reattach rejected"). The existing code already logs `identity.fingerprint()` — reuse that pattern.

### 6. Replay path (D-08/D-09) — `SequencedOutputBuffer` read + trim

The buffer today only supports `push()` + introspection (`next_seq`, `truncated`, `lowest_retained_seq`). Phase 6 adds:
- **`replay_from(&self, last_acked_seq: u64) -> (Vec<(u64, Bytes)>, u64 /*replaying_from*/, bool /*truncated_below_request*/)`** (or an iterator). Returns chunks with `seq > last_acked_seq`. If `last_acked_seq + 1 < lowest_retained_seq` (the requested resume point fell out of the ring), set the truncation indicator and replay from `lowest_retained_seq` instead. No duplicated/dropped bytes within what the ring holds (Pitfall #9). Because the ring is a `VecDeque<(u64,Bytes)>`, this is a filter over the deque.
- **`trim_acked(&mut self, acked_seq: u64)`** (D-08 continuous ack): drop ring entries with `seq <= acked_seq` and decrement `total_bytes`. The 64 KiB drop-oldest cap remains a hard backstop (do NOT remove it). Ack-trim and cap-eviction are complementary: trim removes acked bytes proactively; the cap bounds un-acked growth. `lowest_retained_seq`/`truncated` semantics already exist for the cap path — trimming acked (already-delivered) bytes is NOT a data-loss truncation and should NOT set the `truncated` flag (the client already has those bytes). Be careful: only set `truncated` when bytes the client has NOT acked are dropped by the CAP, never by the ack-trim. Add unit tests for both.

Edge: `last_acked_seq` semantics. The client tracks the highest seq it has APPLIED. seqs start at 0 (first chunk is seq 0). On a fresh session the client has applied nothing; it never sends `Reattach` on a fresh session, so `last_acked_seq` only appears on reconnect. Use `u64`; the server replays `seq > last_acked_seq`. If the client has applied seq N, it sends `last_acked_seq = N`, server replays N+1.. (matches ARCHITECTURE Pattern 3). A sentinel for "applied nothing yet" on reconnect (e.g. client reconnects before receiving ANY chunk) — `last_acked_seq` should then request from the lowest retained; simplest is the client sends `last_acked_seq = 0` only if it applied seq 0, else it needs an Option or a "have I applied anything" flag. **Recommendation:** client tracks `Option<u64> highest_applied`; on reattach send `last_acked_seq = highest_applied` and have the server replay `seq > last_acked_seq` when present. To keep the `[u8;16]+u64` wire simple, encode "nothing applied" as the client NOT having reattached at all in v1.1 in-memory mode the client always has at least the SessionOpened token and may have applied 0..; sending `last_acked_seq=0` with a separate understanding is ambiguous (did it apply seq 0 or nothing?). **Cleanest:** make `last_acked_seq` an `i64`-style "highest applied or -1". But to keep `u64`, define the field as "next expected seq" = `highest_applied + 1`, with 0 meaning "send everything from seq 0". Server replays `seq >= next_expected`. The plan must pick ONE convention, document it in the `Message` doc-comment, and test the "applied nothing" and "applied some" cases. (Either convention works; pick the one with the cleanest off-by-one story and ASSERT it in a test.)

### 7. Continuous acking (D-08) — client sends `Ack{seq}`, server trims

- **Client:** after applying a `PtyData` chunk, track the highest seq it has rendered. BUT — current `PtyData` carries no seq number. Two options:
  - (a) The client counts received `PtyData` frames assuming in-order delivery on the reliable stream (QUIC streams ARE in-order), so the Nth `PtyData` after `SessionOpened`/`ReattachOk{replaying_from_seq}` corresponds to a known seq. This requires the client to know the starting seq (0 on fresh open; `replaying_from_seq` on reattach) and increment. Fragile but no wire change to `PtyData`.
  - (b) Add a seq to `PtyData` or introduce a seq-carrying output frame. This is a wider wire change.
  - **Recommendation:** (a) — the stream is reliable+ordered, the server assigns seqs in send order, so client-side counting from the known start seq is exact. Document the invariant ("PtyData frames are seq-contiguous from the session's start seq on the current connection") and keep it in scope. The client sends `Ack{seq}` on a coarse cadence (time interval, e.g. every 500ms-1s, OR every K KiB — Claude's discretion D-1st bullet). Send `Ack` on the SAME bidi stream client→server (it already carries `PtyData`/`Resize` up).
- **Server:** in the session pump, handle inbound `Ack{seq}` → `slot.trim_acked(seq)` (add a slot delegate that locks `output_buf` and trims). Coarse cadence means low lock churn.

The fresh-open path must establish the client's seq baseline at 0; the reattach path establishes it at `replaying_from_seq` from `ReattachOk`. The headless test driver (`run_session_collect` / `collect_until_close`) currently ignores control frames — extend a NEW headless reattach driver rather than perturbing the existing one (keeps Phase 5 tests green).

### 8. Client reconnect supervisor (D-10/D-11)

Wrap the existing connect→`run_interactive` lifecycle in a supervisor loop in `nosh-client` (`client.rs` + `main.rs`):
- Hold in memory: server addr, host, the current reattach token (from `SessionOpened`, then rotated by each `ReattachOk`), and `highest_applied` seq. (D-01 in-memory; D-02 the token is an opaque `[u8;16]` so a future disk-persist is a pure additive change — do not bake assumptions that block it, e.g. don't make the token a process-lifetime-only handle.)
- On transport loss (the interactive loop returns via the `Err(_) => break` arm — distinguish "shell closed cleanly via SessionClose" from "transport dropped"), enter reconnect: re-dial with exponential backoff (capped interval, e.g. 250ms→…→cap 5-10s), retry **indefinitely** until: the user quits (explicit escape — Claude's discretion D-3rd bullet; pick a chord that won't collide with shell input, e.g. a specific key recognized only during the "reconnecting" state, OR a signal; document it), the shell exits, or a terminal `ReattachErr` (session gone) ends the loop with a clear message.
- On each reconnect: full fresh `connect()` (re-runs TLS mutual handshake — factor 1), open bidi, send `Reattach{token, last_acked_seq}` as the FIRST frame, await `ReattachOk{new_token, replaying_from_seq, truncated}` or `ReattachErr`. On `ReattachOk`: store `new_token`, set seq baseline to `replaying_from_seq`, if `truncated` print a one-line "output truncated" stderr notice, then resume the pump. On `ReattachErr`: terminal — stop retry, print "session ended", restore terminal, exit.
- Minimal stderr `reconnecting…` notice (D-10; full status bar deferred to M4). The `RawModeGuard` must stay correct across reconnects — keep it alive for the whole supervised session so the terminal is restored exactly once on final exit (don't drop/re-enter raw mode per reconnect attempt).
- **`ReattachErr` is terminal** (D-11): the loop does NOT retry on `ReattachErr` (the session is gone — retrying would loop forever). It only retries on transport-level dial/handshake failures.

### 9. Tests to add (roadmap Success Criteria 1–4)

Mirror the `persistence.rs` harness (`spawn_server_with_registry`, `client::*`, in-process server, `total_orphans()` introspection). Add a new integration test file (e.g. `crates/nosh-client/tests/reattach.rs`):
- **SC#1 happy path (ROAM-02):** open a session, capture the `SessionOpened` token, produce known output, simulate transport loss (drop the conn like `persistence.rs` does — orphans the slot), reconnect on a NEW endpoint with the SAME client key, send `Reattach{token, last_acked_seq}`, assert `ReattachOk`, assert replayed bytes since `last_acked_seq` are byte-exact (no dup/gap). Build a headless reattach driver (don't reuse `run_session_collect` which assumes `SessionOpen`).
- **SC#2/SC#3 no-oracle negative (IDENT-02):** orphan a session for identity A; reconnect with a DIFFERENT client key B presenting A's valid token; assert `ReattachErr`. Also present a bogus token with key A; assert `ReattachErr`. Assert BOTH rejections are byte-identical frames (encode both `ReattachErr` and diff, or assert the same opaque variant with no distinguishing field) — this proves no oracle. Token never appears in logs (structural: `ReattachErr` has no fields; assert at the type level).
- **SC#4 mutual exclusion (D-12):** orphan a session; begin a reattach (drive it into `Reconnecting`, or hold it Active) and from a second connection attempt `Reattach` for the same token → second is rejected. Practically: keep client 1 ACTIVE (still attached) and have client 2 (same key) attempt `Reattach` with client 1's token → rejected because slot is `Active` not `Orphaned`. A tighter `Reconnecting`-state race test can use a unit test on `registry.reattach` (two calls; second sees `Reconnecting`).
- **Unit tests on `registry.reattach`** (no shell needed where possible): token match within identity, wrong identity rejected, `Active`/`Reconnecting` rejected, token rotation produces a different token, `replay_from`/`trim_acked` correctness incl. the truncation-below-request case.
- **proto unit test:** the 5 new variants round-trip through the codec; `ReattachErr` is fieldless.

Keep the existing 16 nosh-server tests + client/proto tests green. Additive enum variants + additive registry methods + a new `Reconnecting` state should not break any Phase 5 test, but RE-RUN the full workspace suite — the `SlotState` change is the one place a non-exhaustive match could surface (Rust will force exhaustiveness, so it's compile-checked).

---

## Pitfall checklist for this phase (from PITFALLS.md #8/#9/#10)

- **#8 token ≠ sole factor:** reattach re-runs the full TLS handshake (factor 1, already enforced by `AuthorizedKeysVerifier` on every connect) AND binds the token to a slot whose `identity == peer_identity` (factor 2). Negative test: valid token + wrong key → reject.
- **#9 seq resync dup/gap:** monotonic `u64` (already), replay `seq > last_acked_seq`, truncation indicator when the request predates `lowest_retained_seq`. Test: diff replayed bytes against the server-side log.
- **#10 two-clients-one-session race:** atomic `Orphaned → Reconnecting` transition under the registry lock; `Active`/`Reconnecting` slots reject `Reattach`. Test: concurrent/second reattach rejected.

## Anti-patterns to avoid

- Do NOT key the registry by token (ARCHITECTURE's sketch) — identity stays primary; token is an intra-identity selector. (CONTEXT explicit reconciliation.)
- Do NOT construct a new `SessionSlot` on reattach — resume the existing `Arc` instance so the orphan-exit watcher's `Arc::ptr_eq` stays valid (§1).
- Do NOT add a reason/code to `ReattachErr` — uniform/opaque, no oracle (D-07).
- Do NOT log the token (D-07).
- Do NOT drop the 64 KiB cap when adding ack-trim — it's the un-acked safety backstop (D-08).
- Do NOT hold a registry/slot mutex across `.await` or I/O (Phase 5 Anti-Pattern #2) — collect under the lock, act after release; the reattach lookup+state-transition is brief field ops only.
- Do NOT perturb `run_session_collect`/`collect_until_close` (Phase 5 tests depend on them) — add new reattach-aware drivers.

## Validation Architecture

(No Nyquist VALIDATION.md required — `nyquist_validation_enabled=false` for this run.) Validation is the four roadmap success criteria, each mapped to an integration test above, plus registry/proto unit tests.
