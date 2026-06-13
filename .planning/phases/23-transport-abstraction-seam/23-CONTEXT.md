# Phase 23: Transport Abstraction Seam - Context

**Gathered:** 2026-06-13 (batched all-phase discussion v1.4)
**Status:** Ready for planning

<domain>
## Phase Boundary

Introduce a transport abstraction (`NoshTransport` / `NoshSendStream` / `NoshRecvStream`) so the session pump runs identically over native QUIC and (later) WebTransport. This phase is a **pure, no-behavioural-change refactor** — every existing test must pass unchanged. No WebTransport code lands here; this is the dependency-zero seam that Phases 24–26 build on.
</domain>

<decisions>
## Implementation Decisions

### Trait surface
- **D-01:** Introduce three traits — `NoshTransport` (wraps `send_datagram`, `datagram_send_buffer_space`, `max_datagram_size`, `accept_bi`, `open_bi`, `remote_address`, `close`), `NoshSendStream`, `NoshRecvStream`. Thin I/O boundary only; everything above it (codec, terminal model, registry, predictor, scrollback buffer) is untouched.
- **D-02:** `ChannelEvent::Stream` changes to carry `Box<dyn NoshSendStream>` + `Box<dyn NoshRecvStream>` (ROADMAP SC#4). Dynamic dispatch is accepted — the per-call overhead is irrelevant next to network I/O, and it keeps `run_channel_task` / `run_scrollback_sender_task` transport-agnostic without monomorphising the whole pump.
- **D-03:** `run_session`, `run_reattach_session`, `send_burst`, `build_state_diff`, `handle_connection`, `run_channel_task`, `run_scrollback_sender_task` all become generic over `NoshTransport` (or take boxed trait objects) rather than concrete `quinn::Connection` / `quinn::SendStream` / `quinn::RecvStream`.
- **D-04:** The Quinn concrete wrapper (`QuinnConnection: NoshTransport`, plus stream wrappers) is a **pure pass-through** — no added logic, no behavioural change. This is the SC#3 acceptance bar.

### Verification posture
- **D-05:** `cargo test --workspace` passes with **zero test modifications**. If a test needs changing, that is evidence the refactor changed behaviour — stop and reconsider. (This is the phase's whole point and its strongest guard.)

### Claude's Discretion
- **Trait location:** Recommend the traits live in `nosh-proto` (`nosh-proto/src/transport.rs` or similar) — the ARCHITECTURE researcher read the actual source and recommended this; it avoids spawning a new crate for a thin trait. STACK.md sketched a separate `nosh-transport` crate as an alternative; only split it out if `nosh-proto` would gain a heavy dep (it won't — the trait is dependency-light). Planner decides final module path.
- Exact async-trait mechanism (native `async fn` in traits on current Rust vs `async-trait` crate vs returning `impl Future`) — planner's call; prefer native async-fn-in-trait if the toolchain supports it cleanly, else `async-trait`.
</decisions>

<specifics>
## Specific Ideas

- The seam exists solely to avoid duplicating ~400 lines of session pump for the WebTransport path. Keep it minimal — resist adding capability to the trait that no caller needs yet (YAGNI; widen it in Phase 24 if WebTransport genuinely needs more).
- `wtransport`'s `open_bi()` returns an intermediate `OpeningBiStream` (must be `.await`ed again) whereas `quinn`/`accept_bi` yield the pair directly — design the trait's `open_bi` signature so this asymmetry is hidden inside the wtransport wrapper (Phase 24), not leaked into the trait. Note this now so the trait shape chosen here doesn't fight WebTransport later.
</specifics>

<canonical_refs>
## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### Phase scope & decisions
- `.planning/ROADMAP.md` §"Phase 23" — goal + 4 success criteria (boxed trait streams, pure pass-through, zero test change)
- `.planning/REQUIREMENTS.md` — WT-01
- `.planning/research/SUMMARY.md` §"Phase 1: Transport Abstraction Seam" + §"Architecture Approach" — the seam rationale and the exact method set the trait must wrap

### Architecture (read before designing the trait)
- `.planning/research/ARCHITECTURE.md` — the `NoshTransport`/`NoshSendStream`/`NoshRecvStream` design, the `ChannelEvent::Stream` seam, and which functions become generic
- Source seams the refactor touches (from ARCHITECTURE.md): `crates/nosh-server/src/server.rs` (`run_session`, `run_reattach_session`, `send_burst`, `build_state_diff`, `handle_connection`), `crates/nosh-server/src/channel.rs` (`ChannelEvent::Stream`), `crates/nosh-server/src/registry.rs` (`SequencedOutputBuffer` — transport-agnostic already)

### Project invariants
- `CLAUDE.md` §"Architecture: the load-bearing decisions" — QUIC-as-sole-transport, two-channel model (the trait must preserve both datagram and reliable-stream semantics)
</canonical_refs>

<code_context>
## Existing Code Insights

### Reusable Assets
- The entire session pump is reused verbatim — the refactor only changes the *types* it is generic over, not its logic.
- `SequencedOutputBuffer` / `SessionRegistry` (`registry.rs`) are already transport-agnostic — no change needed beyond the stream type the channel layer hands them.

### Established Patterns
- All session code currently uses `quinn::Connection`, `quinn::SendStream`, `quinn::RecvStream` by concrete type (confirmed by source read) — there is NO existing transport trait; this phase creates the first one.
- The append-only `Message` wire format is untouched here (no new variants in Phase 23).

### Integration Points
- The trait is the single seam Phases 24 (WebTransport wrapper), 25 (inner auth runs over the trait's streams), and 26 (reattach over the trait) all plug into.
</code_context>

<deferred>
## Deferred Ideas

None — this phase is deliberately scoped to the seam only. WebTransport implementation is Phase 24.

## Pending Decisions — Re-Ask Before Planning

None for Phase 23 — all decisions are answerable now and captured above. (This is the only v1.4 phase with no dependency-blocked questions.)
</deferred>

---

*Phase: 23-transport-abstraction-seam*
*Context gathered: 2026-06-13*
