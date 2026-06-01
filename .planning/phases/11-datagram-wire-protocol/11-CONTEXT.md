# Phase 11: Datagram Wire Protocol - Context

**Gathered:** 2026-06-01
**Status:** Ready for planning

<domain>
## Phase Boundary

A sparse, size-bounded terminal-diff wire format exists in `nosh-proto` (new module
`nosh-proto/src/datagram.rs`) — the shared interface every subsequent server and client
prediction component builds on. This phase delivers ONLY the wire format: the `StateDiff`
type, `encode_datagram` / `decode_datagram`, the size-cap guarantee, and the documented
large-repaint decision. It does NOT emit datagrams (Phase 13), build the server state model
(Phase 12), or do any client rendering/prediction (Phase 14/15). Requirement: SYNC-01.

</domain>

<decisions>
## Implementation Decisions

### Large-repaint strategy (D-11-01) — the open design decision (success criterion #4)
- **D-11-01:** **Cursor-priority partial update.** When the set of changed cells would exceed
  the datagram cap, `encode_datagram` includes changed cells prioritized by proximity to the
  cursor, fills until the cap, and DEFERS the remaining cells. Because diffs are computed
  against the last *acked* state, deferred cells naturally reappear in subsequent ticks until
  confirmed — nothing is lost. Everything stays on the loss-tolerant datagram path; the
  reliable stream is NOT coupled in (avoids reintroducing head-of-line blocking).
- **D-11-01a:** This decision MUST be documented in a code comment at the encode callsite
  (success criterion #4). Reject the alternatives explicitly in that comment: skip-frame
  (a persistently-large screen like full-screen vim could fail to converge) and
  reliable-stream fallback (channel coupling + HOL blocking, against the design ethos).
- **D-11-01b:** `encode_datagram` MUST be total: for ANY input it returns a payload provably
  `< max_datagram_size() - 100`. The size cap is enforced by the cursor-priority fill loop,
  not assumed. The size-cap unit test (success criterion #3) drives a full 80x24 repaint and
  asserts the bound.

### Cell + style encoding (D-11-02)
- **D-11-02:** **Run-length runs.** Contiguous changed cells sharing a style are encoded as a
  run `(row, start_col, style, chars)` rather than one entry per cell. Far more compact for
  line edits and repaints — makes the 80x24 cap realistic and reduces datagram count. The
  style captures SGR attributes: fg/bg color, bold, italic, underline, reverse (exact attr
  set is researcher/planner discretion but must round-trip).
- **D-11-02a:** Do NOT model the diff unit on `termwiz::Change` — keep termwiz out of the
  proto crate's public wire contract (this type is foundational; minimize dependency
  coupling). A small purpose-built run/style struct, postcard-serialized.

### Epoch semantics (D-11-03)
- **D-11-03:** **Monotonic tick counter, never resets.** `epoch: u64` is the server-side
  state version, incremented on every emitted diff, never resetting for the session's life.
  The client applies a diff only if `epoch` > last-applied epoch (staleness/ordering check).
  A terminal resize is just another diff carrying new `dimensions` — no generation reset.
  This pairs cleanly with the Phase 14/15 predictor confirmation logic.
- **D-11-03a:** `epoch` is DISTINCT from the reliable-stream `seq` (next-expected-seq
  convention used by `Message::PtyData`/`Ack`/`Reattach` in messages.rs). Datagrams are
  latest-state-wins (epoch); the reliable stream is sequential (seq). Do not conflate them.

### Locked by REQUIREMENTS/success criteria (NOT relitigated)
- Serialization: **postcard + serde, NO new serialization crate** (SYNC-01; mirrors codec.rs
  which already documents a future postcard→prost migration behind one module).
- `StateDiff` carries: changed cells only (sparse, as runs per D-11-02), `epoch: u64`,
  terminal dimensions, cursor position.
- Tests required: round-trip (decoded == original for all valid inputs) + size-cap.

### Claude's Discretion
- Exact struct field layout, the style/attribute bitset representation, the run struct shape.
- The cursor-distance ordering metric (row-major spiral vs Manhattan vs same-row-first) for
  the partial-update fill, provided the cap is guaranteed and the cursor cell is included.
- Whether `encode_datagram` takes `max_datagram_size` as a parameter or a const (must be
  testable against the 80x24 case).

</decisions>

<specifics>
## Specific Ideas

- Architecture invariant (CLAUDE.md): datagram frames carry the loss-tolerant, latest-state-
  wins state-sync object; reliable streams carry everything else. This wire format is the
  datagram-side contract — keep it self-contained and free of reliable-stream concepts.
- `transport.rs` already enables RFC 9221 datagrams (datagram_send/receive_buffer_size).
  Use quinn's `Connection::max_datagram_size()` as the cap source at the (future) callsite;
  this phase just needs the encode function to honor a provided cap.

</specifics>

<canonical_refs>
## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### Requirement & success criteria
- `.planning/REQUIREMENTS.md` — **SYNC-01** (sparse size-bounded datagram wire format,
  postcard/serde, round-trip + size-cap tests). Phase 11 section of `.planning/ROADMAP.md`
  for the 4 success criteria (note #4: document the large-repaint decision at the callsite).

### Architecture & pitfalls
- `.planning/research/ARCHITECTURE.md` — datagram vs reliable-stream split, state-sync object.
- `.planning/research/PITFALLS.md` — datagram MTU/coalescing pitfalls (one diff per ~16ms
  tick, not per chunk — Phase 13, but informs the type design); RFC 9221 enablement (PITFALL 1).
- `.planning/research/FEATURES.md` — predictive echo / state-sync feature context.
- `CLAUDE.md` — load-bearing decisions: datagram frames = loss-tolerant latest-state-wins;
  no new serialization crate; postcard.

### Existing code to mirror / integrate with
- `crates/nosh-proto/src/codec.rs` — postcard encode/decode + length-prefix pattern, the
  ProtoError type, and the "wire format behind one module" convention to replicate.
- `crates/nosh-proto/src/messages.rs` — the reliable-stream `Message` enum + the `seq`
  next-expected convention (epoch is the datagram analog but DISTINCT — D-11-03a).
- `crates/nosh-proto/src/transport.rs` — RFC 9221 datagram enablement + buffer sizing.
- `crates/nosh-proto/src/lib.rs` — module exports; `datagram` module + public `encode_datagram`/
  `decode_datagram`/`StateDiff` re-exports go here.

</canonical_refs>

<code_context>
## Existing Code Insights

### Reusable Assets
- `codec.rs` postcard pattern (`postcard::to_allocvec` / `from_bytes`, ProtoError) is the
  direct template for `encode_datagram`/`decode_datagram`.
- `transport.rs` datagram buffer config confirms the datagram channel is live end-to-end.

### Established Patterns
- "One module owns the wire format" (codec.rs comment, D-04 migration note) — datagram.rs
  follows the same encapsulation so a future codec swap is localized.
- serde derive on proto types (messages.rs `#[derive(Serialize, Deserialize, ...)]`).

### Integration Points
- `nosh-proto/src/lib.rs` exports. Future consumers: Phase 12 server state model (produces
  `StateDiff`s), Phase 13 server datagram sender (calls `encode_datagram` at the ~16ms tick
  with `Connection::max_datagram_size()`), Phase 14 client (calls `decode_datagram`).

</code_context>

<deferred>
## Deferred Ideas

- Coalescing diffs into one datagram per ~16ms tick — Phase 13 (SYNC-03). The type must be
  expressible as a net diff, but the tick/coalescing logic is not in this phase.
- `ResumeComplete` gating so datagrams don't apply during cold-reattach replay — Phase 13.
- Any client-side application/rendering of `StateDiff` — Phase 14.

</deferred>

---

*Phase: 11-datagram-wire-protocol*
*Context gathered: 2026-06-01*
