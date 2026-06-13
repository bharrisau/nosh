# Phase 26: Migration Handover over WebTransport - Context

**Gathered:** 2026-06-13 (batched all-phase discussion v1.4)
**Status:** Ready for planning

<domain>
## Phase Boundary

A nosh client survives a network change in WebTransport mode by transparently reconnecting, re-running the inner SSH-key handshake, and resuming the server-side session via 1-RTT cold reattach with byte-exact replay. Reuses the v1.1 cold-reattach machinery — server-side `run_reattach_session` is unchanged (already generic over `NoshTransport` after Phase 23). The work is the client reconnect loop and the double-attach guard.
</domain>

<decisions>
## Implementation Decisions

### Reconnect policy — user decision
- **D-01:** **Persistent, Mosh-style reconnect.** On WebTransport session loss the client keeps retrying with **bounded exponential backoff**, shows the existing reconnecting banner + `~.` abort instructions, and resumes on success. **No hard time limit** — this is the roaming promise. (Chosen over fixed-N-retries and over honouring the server idle-timeout.)
- **D-02:** Each reconnect attempt re-runs the full inner handshake (Phase 25) on the new WebTransport session, then sends `Reattach` — never reattaches before inner auth completes (MH-1 state-machine guard).

### Reuse v1.1 machinery
- **D-03:** Server-side `run_reattach_session` + `SequencedOutputBuffer` byte-exact replay are reused unchanged (generic over the transport trait after Phase 23). No new reattach protocol.
- **D-04:** Concurrent same-token reattach resolves **atomically to exactly one active session** via `SessionRegistry::try_reattach` using atomic `HashMap::remove` (NOT get-then-remove) — MH-2 double-attach guard. The losing client gets `ReattachErr`.
- **D-05:** Reattach token is **rotated on every successful reattach** so an intercepted token cannot be replayed.

### Claude's Discretion
- **Session-loss detection mechanism:** recommend treating a datagram/stream write error OR a datagram-receive timeout (keepalive miss) as "session lost" and triggering the reconnect loop. Planner picks exact thresholds; reuse v1.2's connection-loss detection where it already exists.
- **Keystrokes typed during the reconnect gap:** recommend the predictive-echo overlay continues to render locally (unconfirmed), and raw keystrokes are buffered in a bounded queue, flushed to the server on successful reattach; drop/clear the buffer if reconnect ultimately aborts (`~.`). Planner finalises the buffer bound. (Mirrors the v1.3 "keystroke snaps to live" instinct — don't lose user input across a brief gap, but don't grow an unbounded buffer.)
- Backoff schedule (initial delay, multiplier, cap) — planner's call; cap the per-attempt delay so a returning network resumes promptly (e.g. cap at a few seconds), unbounded total duration.
</decisions>

<specifics>
## Specific Ideas

- The felt experience target (SC#4): a simulated network change causes a **seamless resume** with no visible shell disruption beyond a brief reconnecting notice — same bar v1.1 set for native-QUIC migration, now over WebTransport where transport-layer migration is unavailable.
- This is the phase that makes "remote access over a flaky link, behind a proxy" actually deliver the roaming promise — the whole reason WebTransport mode exists rather than plain TCP.
</specifics>

<canonical_refs>
## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### Phase scope & decisions
- `.planning/ROADMAP.md` §"Phase 26" — goal + 4 success criteria
- `.planning/REQUIREMENTS.md` — WT-06
- `.planning/research/SUMMARY.md` §"Phase 4: Migration Handover" + §"Critical Pitfalls" (MH-1, MH-2)

### Reuse target (read before wiring)
- `.planning/research/ARCHITECTURE.md` §"Migration handover" — client reconnect loop; `run_reattach_session` is unchanged
- `crates/nosh-server/src/registry.rs` — `SequencedOutputBuffer`, `SessionRegistry::try_reattach` (the atomic `HashMap::remove` MH-2 fix)
- `crates/nosh-server/src/server.rs` — `run_reattach_session` replay path
- `.planning/milestones/v1.1-ROADMAP.md` (archived) — the original cold-reattach design (IDENT-02, ROAM-02) being reused
- v1.2 connection-loss banner / `~.` abort UX (QoL pack) — reuse for the reconnecting notice
</canonical_refs>

<code_context>
## Existing Code Insights

### Reusable Assets
- `run_reattach_session`, `SequencedOutputBuffer`, the two-factor reattach token, channel re-open-after-`ResumeComplete` — all from v1.1, all reused.
- v1.2's connection-loss banner + `~.` local-quit escape — reused for the reconnecting UX.

### Established Patterns
- Channels are ephemeral per-connection; the client re-opens them via the control stream after `ResumeComplete` (v1.3 decision) — applies identically here on the new WebTransport session.
- Two-factor reattach (TLS/inner-auth re-run + identity-scoped token selector, no oracle) — the inner handshake (Phase 25) is the "re-run auth" half on the WebTransport path.

### Integration Points
- Client reconnect loop → Phase 25 inner handshake → `Reattach` → server `run_reattach_session` (unchanged). Token rotation hooks into the existing reattach-success path.
</code_context>

<deferred>
## Deferred Ideas

None new — this phase is wiring proven machinery into the WebTransport reconnect loop.

## Pending Decisions — Re-Ask Before Planning

1. **Network-change simulation method in tests** — *Dependency: Phase 24 (the WebTransport transport API determines how a test forces a client IP swap / session drop).*
   RE-ASK TRIGGER (research/planner, autonomous): after Phase 24 completes, before Phase 26 planning — decide how the SC#4 simulated-network-change test forces a WebTransport session drop + client rebind (e.g. drop the udp socket / rebind local addr / inject a write error). Depends on the concrete wtransport client API wired in Phase 24. Not a user question — planner resolves against the built transport.
</deferred>

---

*Phase: 26-migration-handover-over-webtransport*
*Context gathered: 2026-06-13*
