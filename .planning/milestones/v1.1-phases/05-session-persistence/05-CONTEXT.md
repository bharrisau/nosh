# Phase 5: Session Persistence - Context

**Gathered:** 2026-05-30
**Status:** Ready for planning

<domain>
## Phase Boundary

Make a server-side session (PTY + shell + terminal state + output buffer) survive a client's QUIC disconnect: it becomes an *orphaned* session that lives until the shell exits, bounded by a per-identity cap and an optional idle timeout. This is the foundation Phase 6 (cold reattach) reconnects to. Phase 5 introduces the `SessionRegistry`/`SessionSlot` (keyed by the SSH identity threaded in Phase 4), the orphan lifecycle, and the output ring buffer. NOT in scope: the reattach protocol itself, replay, the `Reattach` message, or connection migration (Phase 6/7).
</domain>

<decisions>
## Implementation Decisions

### Which disconnects persist
- **D-01:** Orphan a session ONLY on an unexpected transport-level disconnect (connection lost, idle/keepalive timeout, failed/aborted path). An explicit client `SessionClose` message and a normal shell exit (PTY EOF) tear the session down immediately, exactly as today (server.rs ~315-361). Typing `exit` or quitting the client cleanly must NOT leave a lingering session.
- **D-02:** Concretely: in the `run_session` outcome split, the `None` branch must be subdivided — `SessionClose`/clean protocol end → teardown (SIGHUP + reap, current behavior); transport error / connection lost → transition the session to Orphaned (do NOT SIGHUP, keep the PTY master open).

### Session lifecycle & the unified activity timestamp
- **D-03:** Each session carries a single `last_active: Instant` timestamp, updated while a client is attached (on session activity) and frozen at the moment of orphaning. This ONE timestamp drives BOTH the idle timeout and LRU eviction (reuse the existing `Session.idle_since` seam at session.rs:128 as the orphan marker; `last_active` generalizes it).
- **D-04:** State machine: Active (client attached) → Orphaned (transport lost, PTY kept alive) → [reattached in Phase 6] / [reaped on idle-timeout or shell-exit]. Phase 5 implements Active and Orphaned and the reaping transitions; the Reconnecting/reattach transition is Phase 6.

### Per-identity cap + eviction (LRU)
- **D-05:** Per-identity cap on ORPHANED sessions, default **5**. The cap bounds persisted-session memory.
- **D-06:** On exceeding the cap when a session is newly orphaned: **evict the least-recently-active orphan** (the orphan with the oldest `last_active`, i.e. orphaned longest) to make room — LRU. Eviction means SIGHUP that orphan's shell and reap it. The just-orphaned (most-recently-active) session is retained.
- **D-07:** An attached/Active session is NEVER evicted — only Orphaned sessions are eligible for eviction. (Active sessions don't count against the orphan cap; the cap is specifically about persisted/detached sessions.)

### Idle timeout
- **D-08:** Configurable idle timeout for orphaned sessions, default **0 = disabled** (Mosh behavior). Measured as `now - last_active`; only applies while Orphaned; reset/cleared on reattach (an attached session never times out).
- **D-09:** Configuration surface: a `--idle-timeout-secs` clap CLI flag (following the existing flag pattern in nosh-server/main.rs) AND a `NOSH_IDLE_TIMEOUT_SECS` environment-variable fallback (handy for systemd/container deploys). CLI flag takes precedence over env; default 0 when neither set.

### Output buffer
- **D-10:** `SequencedOutputBuffer` — monotonic u64 sequence numbers assigned to every outgoing PTY chunk from the moment of session open (needed by Phase 6 replay). Bounded ring at **64 KiB**.
- **D-11:** On overflow, **drop oldest** bytes (keep the most-recent 64 KiB) AND record that truncation occurred (e.g. a flag / lowest-retained sequence number) so Phase 6 reattach can surface an "output truncated" marker to the client. Latest output always survives.

### Claude's Discretion
- Exact `SessionRegistry` / `SessionSlot` data structures (HashMap keyed by identity fingerprint → list of slots, etc.) and where the `Arc<SessionRegistry>` is shared into connection handlers — per ARCHITECTURE.md.
- The background zombie-reaper task design (periodic `child.try_wait()` over orphaned sessions) — required by PERSIST-01 to avoid zombies; cadence is implementer's choice.
- How `last_active` is updated during an attached session (per-frame vs coarse tick) — pick something cheap; coarse is fine.
- Internal representation of the truncation marker.
</decisions>

<specifics>
## Specific Ideas

- The single `last_active` timestamp serving both idle-timeout and LRU eviction came from the user explicitly — keep it unified; do not introduce two parallel timestamps.
- Reuse, don't reinvent: the disconnect detection already exists in `run_session`'s `select!` loop; Phase 5 reroutes the transport-loss case to orphaning instead of the current unconditional SIGHUP+reap.
</specifics>

<canonical_refs>
## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### Requirements & roadmap
- `.planning/REQUIREMENTS.md` — PERSIST-01 (orphan survives, MasterPty open, reaper), PERSIST-02 (idle timeout default 0), PERSIST-03 (per-identity cap before first orphan)
- `.planning/ROADMAP.md` §"Phase 5: Session Persistence" — goal + 5 success criteria
- `.planning/research/SUMMARY.md` §"Phase 2: Session Persistence" — design + the three correctness requirements
- `.planning/research/ARCHITECTURE.md` — `SessionRegistry`/`SessionSlot`/`SequencedOutputBuffer` struct sketches (registry.rs, new)
- `.planning/research/PITFALLS.md` — Pitfall #7 (SIGHUP kills shell — keep MasterPty open), #5 (unbounded orphan memory — cap), #6 (zombie reaper)

### Code touchpoints (verified this session)
- `crates/nosh-server/src/server.rs:184-367` — `run_session`; the `session_outcome` split at ~315-361 is where transport-loss vs clean-close is decided (today both paths SIGHUP+reap)
- `crates/nosh-server/src/session.rs:113-137` — `Session` struct (`identity` now non-optional after Phase 4; `idle_since` orphan seam at :128; `master` MasterPty must stay open)
- `crates/nosh-server/src/main.rs:14-49` — clap `Args` (pattern for the new `--idle-timeout-secs` flag)
- `crates/nosh-server/src/server.rs:101-133` — `run_accept_loop` (where the shared `Arc<SessionRegistry>` will be threaded in)
</canonical_refs>

<code_context>
## Existing Code Insights

### Reusable Assets
- `Session.idle_since: Option<Instant>` (session.rs:128) — the v1.0 orphan seam; generalize into the `last_active` mechanism (D-03).
- `Session.identity: NoshPublicKey` (non-optional, Phase 4) — the registry key (its `fingerprint()` is the map key candidate).
- `run_session`'s `select!` already detects client disconnect (`None` outcome) vs shell exit (`Some(code)`); Phase 5 subdivides the disconnect case.
- clap-based CLI in main.rs — add `--idle-timeout-secs` + env fallback alongside existing flags.

### Established Patterns
- The server SIGHUPs + reaps on disconnect today (server.rs:350-360); Phase 5 must NOT do this for the orphan path (Pitfall #7).
- Per-session `tracing` span (server.rs:219) — extend with orphan/reattach lifecycle events.

### Integration Points
- New `nosh-server/src/registry.rs` (`SessionRegistry`, `SessionSlot`, `SequencedOutputBuffer`), `Arc`-shared from `run_accept_loop` into each `handle_connection`.
- The output pump (server.rs:234-249) feeds chunks into the `SequencedOutputBuffer` as it sends them (so buffered == sent-with-seq).
</code_context>

<deferred>
## Deferred Ideas

- The `Reattach{token, last_acked_seq}` protocol, token generation/validation, output replay, and the Reconnecting state — Phase 6.
- Surfacing the truncation marker to the user (UI/notice) — the buffer *records* truncation in Phase 5; *displaying* it on reattach is Phase 6.
- Connection migration (same-connection path change) — Phase 7; distinct from orphan/reattach.
- Named/numbered session selection when an identity has multiple orphans — out of scope for v1.1 (M5+).

None of these are implemented in Phase 5.
</deferred>

---

*Phase: 05-session-persistence*
*Context gathered: 2026-05-30*
