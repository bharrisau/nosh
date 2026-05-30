# Phase 6: Cold Reattach Protocol - Context

**Gathered:** 2026-05-30
**Status:** Ready for planning

<domain>
## Phase Boundary

A client whose QUIC connection died reconnects (new connection, full fresh TLS mutual handshake) and resumes its orphaned Phase-5 session in 1 RTT, with byte-exact output continuity and no possibility of session hijacking. Phase 6 adds the reattach control messages, the reattach token (issued/rotated by the server, held by the client), two-factor authorization, the Reconnecting state + mutual exclusion, output replay from a client-supplied sequence number, and continuous sequence acking. NOT in scope: connection migration (same-connection path change — Phase 7), disk-persisted cross-process reattach (designed-for but not implemented), Windows client (Phase 8), datagram/predictive echo (M4).
</domain>

<decisions>
## Implementation Decisions

### Reattach scope — in-memory, designed for disk
- **D-01:** v1.1 ships **in-memory** reattach: the client holds the reattach token in memory and auto-reconnects when its link drops while the client process is alive (Wi-Fi→cellular, sleep/resume where the process survives). A fully-closed-and-relaunched client cannot reattach in v1.1.
- **D-02:** Shape the wire protocol and token so adding disk-persistence later is **additive** — the token is an opaque value carried on the wire; persisting it (plus server addr + host-key fingerprint) to a client state file in a future milestone must require NO wire/message change. Do not hard-code assumptions that block this.

### Reattach messages (nosh-proto additions)
- **D-03:** New `Message` variants (additive to the existing enum; postcard-compatible):
  - `SessionOpened { token }` — server → client immediately after a successful `SessionOpen`, delivering the initial reattach token.
  - `Reattach { token, last_acked_seq }` — client → server as the FIRST frame on a reconnect (in place of `SessionOpen`).
  - `ReattachOk { new_token }` — server → client on success; carries the **rotated** token; the server then replays buffered output after `last_acked_seq`. May also carry a truncation indicator (see D-09).
  - `ReattachErr` — server → client on failure; **opaque/uniform** (see D-06).
  - `Ack { seq }` — client → server, periodic; highest sequence number the client has received+applied (see D-08).
- **D-04:** Dispatch: `handle_connection` reads the first frame and branches — `SessionOpen` → fresh session (Phase 3/5 path, then emit `SessionOpened`); `Reattach` → reattach path; anything else → protocol close.

### Token lifetime — single-use, rotated
- **D-05:** The reattach token is **single-use**: the server issues a fresh token in `ReattachOk` on every successful reattach and immediately invalidates the previous one. A captured/replayed token is usable at most once. Tokens are CSPRNG-generated (122-bit, e.g. `uuid` v4 already in nosh-server) and unguessable. The token selects WHICH orphaned session within the authenticated identity (an identity may hold up to the per-identity cap of orphans from Phase 5).

### Two-factor authorization (anti-hijack)
- **D-06:** Reattach requires BOTH factors: (1) the full SSH/TLS mutual handshake re-runs on the new connection (same `authorized_keys` path as a fresh connect — Phase 4 threads the identity), AND (2) the presented token must match a session whose bound identity equals the handshake identity. Either factor alone is insufficient.
- **D-07:** `ReattachErr` is **uniform and opaque** — a valid token presented with the wrong SSH key returns the SAME error as an unknown/expired token. No oracle for session existence. The token is NEVER logged; log only the identity fingerprint + an outcome.

### Output continuity — continuous acks + replay
- **D-08:** **Continuous acking.** During a live session the client periodically sends `Ack { seq }` (highest applied sequence). The server trims the Phase-5 `SequencedOutputBuffer` to drop acked bytes, keeping only un-acked output (plus the 64 KiB drop-oldest cap as a hard safety bound from Phase 5 D-10/D-11). Ack cadence is coarse (e.g. time- or byte-interval), not per-chunk.
- **D-09:** On reattach, the server replays buffered output with sequence > `last_acked_seq`, with **no duplicated or dropped bytes**. If the requested `last_acked_seq` is older than the buffer's `lowest_retained_seq` (truncation happened during a long/slow disconnect), the server surfaces a truncation indicator (via `ReattachOk`) so the client can show an "output truncated" notice, then replays from the lowest retained sequence.

### Reconnect UX — indefinite auto-retry
- **D-10:** When the link drops while the client is running, the client auto-reconnects with **exponential backoff (capped interval), retrying indefinitely** until the session ends (terminal `ReattachErr`, e.g. session gone/cap-evicted; or shell exit) or the user explicitly quits. Print a minimal `reconnecting…` notice on stderr (full status bar stays deferred to M4).
- **D-11:** Provide an explicit client-side quit/abort path so the user can break out of an indefinite retry (e.g. a documented escape sequence or signal) — restore the terminal cleanly on exit. A terminal `ReattachErr` (session no longer exists) ends retry with a clear message rather than looping forever.

### State machine — mutual exclusion
- **D-12:** Extend the Phase-5 lifecycle with a **Reconnecting** state. A `Reattach` for a session currently Active (a client still attached) or already Reconnecting is **rejected** — prevents the two-clients-one-session race. Transition: Orphaned → Reconnecting (on accepted Reattach) → Active (replay done, pumps rebound). Guard the transition atomically in the registry.

### Claude's Discretion
- Exact `Ack` cadence (time interval vs byte threshold) and where the client tracks highest-applied seq.
- Backoff curve / cap interval for D-10.
- The escape/quit mechanism for D-11 (key chord vs signal) — pick something that won't collide with normal shell input; document it.
- Internal token store representation in the registry (token → slot index within identity).
- Whether `SessionOpened` token + `ReattachOk` new_token reuse one token type/struct.
</decisions>

<specifics>
## Specific Ideas

- The user chose the more capable option on scope (design-for-disk), token (single-use rotation), reconnect (indefinite retry), and continuity (continuous acks) — bias the implementation toward robustness over minimalism on these four axes, but keep everything within the Phase 6 boundary (no actual disk store, no migration).
- Continuous acks EXTEND Phase 5's buffer — they are not a rewrite. Phase 5 gives monotonic seq + a bounded ring + truncation flag; Phase 6 adds the `Ack` message, the trim-on-ack logic, and the replay-from-seq read path.
</specifics>

<canonical_refs>
## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### Requirements & roadmap
- `.planning/REQUIREMENTS.md` — ROAM-02 (1-RTT cold reattach, sequenced replay), IDENT-02 (two-factor reattach auth bound to SSH identity)
- `.planning/ROADMAP.md` §"Phase 6: Cold Reattach Protocol" — goal + 4 success criteria (replay continuity, two-factor, no-oracle negative test, state-machine mutual exclusion)
- `.planning/research/SUMMARY.md` §"Phase 3: Cold Reattach Protocol" + §"Critical Pitfalls" #8/#9/#10
- `.planning/research/PITFALLS.md` — #8 (token as sole factor → hijack), #9 (seq resync dup/gap), #10 (two-clients-one-session race) — the definitive "looks done but isn't" checklist for this phase
- `.planning/research/ARCHITECTURE.md` — reattach protocol section (Message variants, SequencedOutputBuffer, registry token lookup); NOTE its sketch keys the registry by token, but Phase 5 keys by SSH identity — reconcile: identity is the primary key, token selects the slot within an identity.

### Dependency — Phase 5 (must be executed first)
- `.planning/phases/05-session-persistence/05-CONTEXT.md` — orphan lifecycle, `last_active`, `SequencedOutputBuffer` (D-10/D-11), per-identity cap/LRU. Phase 6 extends these.
- `crates/nosh-server/src/registry.rs` (created in Phase 5) — `SessionRegistry`/`SessionSlot`/`SequencedOutputBuffer` are the types Phase 6 extends with token lookup, Reconnecting state, ack-trim, and replay.

### Code touchpoints
- `crates/nosh-proto/src/messages.rs` — `Message` enum (add the 5 variants, D-03); additive postcard encoding.
- `crates/nosh-server/src/server.rs` — `handle_connection` first-frame dispatch (D-04); `run_session` rebind on reattach.
- `crates/nosh-client/src/client.rs`, `main.rs` — client reconnect loop (D-10/D-11), token holding (D-01), ack sending (D-08), `last_acked_seq` tracking.
</canonical_refs>

<code_context>
## Existing Code Insights

### Reusable Assets
- Phase 5 `SequencedOutputBuffer` (registry.rs) — seq numbering + bounded ring + truncation flag already exist; Phase 6 adds ack-trim + replay-read.
- Phase 5 `SessionRegistry` keyed by SSH identity fingerprint — add a token→slot index.
- `nosh_proto::{read_message, write_message}` + postcard codec — adding enum variants is backward-compatible.
- `uuid` v4 (in nosh-server) — CSPRNG token source (D-05).
- Phase 4 `Session.identity: NoshPublicKey` + `fingerprint()` — the identity half of the two-factor check.

### Established Patterns
- First-frame protocol dispatch already exists (server.rs:193 reads SessionOpen first); Phase 6 generalizes it to SessionOpen | Reattach.
- Client raw-mode + pump loop in client.rs — the reconnect loop wraps the existing connect+session flow.

### Integration Points
- Server: registry gains token lookup + Reconnecting guard; handle_connection branches on first frame; output pump feeds buffer + honors acks.
- Client: a reconnect supervisor around the connect→session lifecycle that re-dials on drop, sends Reattach with last_acked_seq, and stores the rotated token.
</code_context>

<deferred>
## Deferred Ideas

- Disk-persisted reattach token / cross-process reattach (relaunch `nosh` and reattach) — designed-for (D-02) but NOT implemented in v1.1.
- Connection migration (same QUIC connection survives a path change, zero reattach) — Phase 7; distinct from cold reattach.
- Named/numbered selection among multiple orphans for one identity — M5+; v1.1 token uniquely selects the session.
- Full connection-status bar / latency UI — M4 (D-10 keeps only a minimal stderr notice).
- 0-RTT reattach — explicitly deferred (1-RTT is the locked default).

None implemented in Phase 6 beyond the explicit decisions above.
</deferred>

---

*Phase: 06-cold-reattach-protocol*
*Context gathered: 2026-05-30*
