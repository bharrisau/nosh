# Phase 13: Server Datagram Sender - Context

**Gathered:** 2026-06-01
**Status:** Ready for planning

<domain>
## Phase Boundary

The server emits coalesced terminal-state diffs over QUIC datagrams from the session pump
(one `StateDiff` per ~16 ms tick, not per PTY chunk), gated by a `ResumeComplete` signal so
they never corrupt a partial cold-reattach replay. Both `run_session` and
`run_reattach_session` get the sender arm. Requirement: SYNC-03. This phase is the SERVER
sender + the minimal protocol needed for robust delivery; the real client renderer/predictor
is Phase 14/15.

</domain>

<decisions>
## Implementation Decisions

### Loss-tolerance / diff baseline (D-13-01) — acked-epoch model (user override)
- **D-13-01:** **Acked-epoch model.** The server diffs the current visible screen against the
  last screen state the CLIENT has ACKNOWLEDGED (by `epoch`), and keeps including unconfirmed
  changes in successive diffs until they are acked. Datagrams are unreliable (drop, no
  retransmit), so this is the principled recovery mechanism: anything lost is simply still
  un-acked and gets re-sent. Rationale (user): a periodic keyframe is NOT sufficient on a
  lossy channel — if the keyframe itself drops, the client waits a full interval and never
  knows; only an ack closes the loop. Rejected: "last-sent + periodic keyframe" and
  "full-screen-every-tick".
- **D-13-01a (epoch-ack channel — added this phase, server side + protocol):** Add a small
  client→server **epoch ack** to `nosh-proto` carrying the client's last-applied `epoch`.
  Preferred carrier: a tiny **datagram** (same channel; a lost ack is self-correcting — the
  server keeps sending until a newer ack arrives — so no reliability needed). Researcher/
  planner confirms datagram-vs-control-message carrier. The server maintains, per connection,
  the last-acked `epoch` and the confirmed screen snapshot at that epoch, and diffs current
  vs that snapshot.
- **D-13-01b (resume subsumes keyframe):** On cold-reattach the server resets its notion of
  the client's acked baseline to "nothing" → the first post-`ResumeComplete` diff is naturally
  the FULL current screen. No separate keyframe path is needed — the acked-epoch baseline
  reset IS the resume keyframe. (Supersedes the Phase 12/earlier "keyframe-on-resume" framing:
  it's achieved via baseline reset, not a special-case full frame.)
- **D-13-01c (scope boundary with Phase 14):** Phase 13 implements the server sender + the
  epoch-ack message + server-side ack handling, and the Phase 13 INTEGRATION TEST uses a
  minimal test client that SENDS epoch acks to exercise the full acked-epoch loop end-to-end.
  The REAL client emitting acks during normal rendering is Phase 14 (PREDICT-01). Until a
  client acks, the server's acked-epoch stays at its initial value → it keeps sending the full
  state (safe, just more bytes) — never silently drops cells.
- **Note on Phase 11 prose:** Phase 11's `StateDiff` is the wire format only; "diff against
  last-acked state" (Phase 11 D-11-01 prose) is realized HERE by D-13-01 — no code conflict,
  Phase 11 just defines the type whose `epoch` is the ack target.

### Tick cadence & idle (D-13-02)
- **D-13-02:** `diff_interval` = ~16 ms tick (≈60 fps, roadmap-locked). The `select!` arm
  mirrors the existing `migration_poll` interval-arm pattern (server.rs:398-432) with
  `MissedTickBehavior::Skip`. One diff per tick — NOT one per PTY chunk (bursty output like
  `cat largefile` coalesces into one diff/tick).
- **D-13-02a (skip unchanged):** If `TerminalState` is unchanged since the last sent diff AND
  the client is caught up (acked == current epoch), send NOTHING that tick (no empty
  datagrams). If there are unconfirmed changes (acked < current), keep re-sending the
  outstanding diff each tick until acked.

### ResumeComplete gating (D-13-03)
- **D-13-03:** Datagrams are SUPPRESSED for a connection until a `ResumeComplete` signal fires
  after cold-reattach replay completes (server.rs:721 "replay complete" is the natural site).
  A fresh `run_session` (new session, no replay) signals `ResumeComplete` immediately/at start
  so its datagrams flow normally. Exact signal mechanism (atomic flag / watch / oneshot on the
  slot or connection task) is Claude's discretion.

### Reliable PtyData coexistence (D-13-04)
- **D-13-04:** **Additive.** Phase 13 only ADDS the datagram sender; the reliable PtyData
  stream keeps flowing unchanged (still feeds `SequencedOutputBuffer`, today's client display,
  and cold-reattach replay). Zero regression to the working display. Output is briefly carried
  both ways — accepted transitional cost. The client migrating to render FROM datagrams is
  Phase 14.

### Locked by REQUIREMENTS/roadmap (NOT relitigated)
- One diff per ~16 ms tick via a `select!` `diff_interval.tick()` arm; `conn.send_datagram()`.
- Integration test: real test client + server, type chars, assert client `conn.read_datagram()`
  receives non-empty `StateDiff` frames.
- Both `run_session` AND `run_reattach_session` carry the sender arm with the same gate.

### Claude's Discretion
- ResumeComplete signal primitive; per-connection last-acked snapshot storage; how the
  TerminalState exposes current vs snapshot for diff computation; ack carrier confirmation
  (datagram vs control); the StateDiff-input shape handed to `encode_datagram` each tick.

</decisions>

<specifics>
## Specific Ideas

- Acked-epoch unifies loss-recovery and reattach-freshness: one mechanism, no special cases.
- Losing an epoch-ack is harmless (server resends until a newer ack lands) — so the ack itself
  rides the unreliable datagram channel, not the reliable stream.
- `encode_datagram` (Phase 11) already enforces the size cap with cursor-priority partial fill;
  when the acked baseline is far behind (e.g. just after resume), the first diffs naturally hit
  that partial path and converge over a few ticks — consistent with the deferred-cells design.

</specifics>

<canonical_refs>
## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### Requirement & success criteria
- `.planning/REQUIREMENTS.md` — **SYNC-03**. `.planning/ROADMAP.md` Phase 13 section (4 criteria).

### Wire format & state model this phase wires together
- `crates/nosh-proto/src/datagram.rs` (Phase 11 — `StateDiff`, `encode_datagram`/`decode_datagram`,
  `epoch`). The epoch-ack message is added near here in nosh-proto.
- Phase 12 `TerminalState` (`crates/nosh-server/src/...`) — the source of current screen + the
  snapshot/diff read API used each tick. (Phase 12 builds it; Phase 13 consumes it.)

### Session pump integration sites
- `crates/nosh-server/src/server.rs` — `run_session` `select!` loop + `migration_poll`
  interval-arm pattern (398-432) to mirror; `run_reattach_session` (~623+); replay completion
  at ~721 (`ResumeComplete` site); the existing reliable PtyData send path (push_output + frame).
- `crates/nosh-proto/src/messages.rs` — Message enum + `Ack { seq }` (reliable-stream,
  next-expected-seq — DISTINCT from the new datagram epoch-ack).
- `crates/nosh-proto/src/transport.rs` — datagram enablement / buffer sizes.

### Architecture
- `CLAUDE.md` — datagram = loss-tolerant latest-state-wins state-sync; reliable streams for
  everything else. `.planning/research/ARCHITECTURE.md`, `PITFALLS.md` (coalescing: one diff
  per tick not per chunk; reattach/datagram replay race — the ResumeComplete gate).

</canonical_refs>

<code_context>
## Existing Code Insights

### Reusable Assets
- `migration_poll` interval arm (server.rs:398-432) is the exact template for `diff_interval`.
- Phase 11 `encode_datagram(diff, cursor, cap)` + `Connection::max_datagram_size()` for the cap.
- `SequencedOutputBuffer` / reattach replay machinery — UNCHANGED; the gate just defers
  datagram emission until replay finishes.

### Established Patterns
- `tokio::select!` arms in the session loop; `MissedTickBehavior::Skip` for intervals.
- Per-connection state lives in the connection task; slot holds shared session state.

### Integration Points
- New `diff_interval` arm in BOTH `run_session` and `run_reattach_session`.
- `ResumeComplete` raised at replay completion (~721) and at fresh-session start.
- epoch-ack: client→server datagram parsed in the connection task; updates last-acked epoch.

</code_context>

<deferred>
## Deferred Ideas

- Real client rendering from datagrams + emitting epoch acks during normal use — Phase 14 (PREDICT-01).
- Speculative local echo overlay — Phase 15.
- Connection-loss overlay / OSC52 / title — Phase 16.

</deferred>

---

*Phase: 13-server-datagram-sender*
*Context gathered: 2026-06-01*
