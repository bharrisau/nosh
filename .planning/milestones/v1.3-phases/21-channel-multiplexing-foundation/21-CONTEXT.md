# Phase 21: Channel Multiplexing Foundation - Context

**Gathered:** 2026-06-11
**Status:** Ready for planning
**Mode:** Interactive discuss (autonomous run)

<domain>
## Phase Boundary

Logical channels are negotiated control-first over the existing session control
stream using OPEN / ACCEPT / REJECT before any data stream is bound. Each
reliable channel rides its own QUIC bidirectional stream (no in-stream framing,
no head-of-line blocking between channels) and has per-channel credit-based flow
control. The wire format stays stable via an append-only `Message` discriminant
order, enforced by a discriminant-stability test that is the **first commit** of
the mux work. The layer is proven end-to-end with a simple echo channel before
scrollback (Phase 22) adds a real consumer.

In scope (MUX-01…MUX-06): control-first negotiation, concurrent per-stream
channels, credit-based per-channel flow control, clean lifecycle (half/full
close, id-parity collision prevention), mobility behaviour (survive migration;
re-establish over control channel on cold reattach), discriminant-stability test.

Out of scope this phase (declared but REJECTed by the mux): port forwarding
(FWD-01), agent forwarding (FWD-02), file transfer (XFER-01). Scrollback
(SCROLL-*) is Phase 22 — the first real consumer of this layer.
</domain>

<decisions>
## Implementation Decisions

### Channel-id allocation (MUX-04) — disjoint parity spaces (client-even / server-odd)
Each side owns a disjoint id space so simultaneous opens from both ends cannot
collide; no open-time arbitration round-trip is needed. **Direction is fixed by
ROADMAP success criterion #2: client-initiated channels use EVEN ids,
server-initiated channels use ODD ids.** (The discussion question proposed the
HTTP/2 direction — client-odd/server-even — but the ROADMAP is the authoritative
contract and its success criteria are what verification checks; the direction is
functionally arbitrary for collision-avoidance, so we align to the locked
ROADMAP rather than introduce a conflicting input.) Channel id 0 is reserved for
the control channel (the existing session control stream — OPEN/ACCEPT/REJECT are
new appended `Message` variants on it, NOT a new stream).

### Stream binding (MUX-02) — Channel-id varint prefix on the new stream
After the control-channel handshake (OPEN → ACCEPT), the opener writes the
channel-id as a varint at the very start of the freshly opened QUIC bidi stream;
the receiver maps stream→channel on first read, before any channel payload.
Rationale: QUIC stream ids are connection-local and change semantics across
migration, so channel state must never be keyed on the transport stream id.
Binding by an application-level channel-id prefix keeps the mapping stable
across QUIC connection migration.

### Per-channel flow control (MUX-03) — 256 KiB byte-credit window
Credit-based windows counted in bytes. Initial window 256 KiB per channel;
credits replenish as the consumer drains its buffer (consumer advertises
additional credit). Byte-granular (not message-count) so variable-size payloads
(scrollback pages in Phase 22) map cleanly. A slow consumer on one channel
exhausts only its own window and cannot stall the connection or other channels.

### Echo channel (proving fixture) — Test-only, not a shipped channel type
The echo channel that proves the mux layer lives only in integration tests; it
is NOT registered as a production channel type. Keeps the production attack
surface minimal (nothing extra to rate-limit or harden) while still exercising
OPEN/ACCEPT/REJECT, per-stream binding, flow-control credits, and clean
close end-to-end.

### Discriminant stability (MUX-06) — gating first commit
The discriminant-stability enforcement test for the `Message` enum is the FIRST
commit of the phase. New mux variants (OPEN/ACCEPT/REJECT) are appended AFTER
the current last variant (`TerminalControl`); inserting before any existing
variant corrupts deployed connections (postcard encodes variants by positional
discriminant). The test must fail loudly if the discriminant order ever shifts.

### REJECT is opaque (MUX-01)
REJECT carries no reason code — no oracle that distinguishes "unknown channel
type" from "not permitted". A rejected channel leaks nothing.
</decisions>

<code_context>
## Existing Code Insights

- `crates/nosh-proto/src/messages.rs` — the `Message` enum (postcard-encoded,
  append-only discriminants). Last variant is `TerminalControl(TerminalControlPayload)`
  at the end; new mux variants append after it. Existing comments already document
  the append-only invariant and a SessionClose discriminant-stability check pattern
  in `codec.rs` (~line 247) to model the new mux-discriminant test on.
- `crates/nosh-proto/src/codec.rs` — framing/encode-decode; existing
  discriminant-stability test precedent lives here.
- `crates/nosh-proto/src/transport.rs` — QUIC stream helpers.
- The session control stream already carries `Message` frames (SessionOpen,
  resize, Ack, TerminalControl…). Channel id 0 == this existing control stream;
  do not introduce a second control stream.
- Migration + cold-reattach machinery already exists (M3 phases / `reattach`
  tests). MUX-05 re-establishment rides on cold-reattach: after resume, the
  client re-sends OPEN for each channel it wants (channels are per-connection
  state, never byte-replayed).
</code_context>

<specifics>
## Specific Ideas

- Prove the layer with an echo channel integration test that exercises the full
  lifecycle: OPEN → ACCEPT → channel-id prefix on stream → bidirectional bytes
  under flow-control credits → half-close → full-close, plus a REJECT path.
- Order of work implied by the goal: (1) discriminant-stability test commit, then
  (2) OPEN/ACCEPT/REJECT control negotiation, (3) per-stream channel binding,
  (4) credit-based flow control, (5) echo-channel proof, (6) migration +
  cold-reattach re-establishment.
</specifics>

<deferred>
## Deferred Ideas

- Port forwarding (FWD-01), agent forwarding (FWD-02), file transfer (XFER-01):
  the mux declares/REJECTs these channel types this milestone; real
  implementations are later work.
- Server-initiated channels (the even-id space) are reserved by the parity
  scheme but not exercised until a feature needs them (e.g. remote port forward).
</deferred>

<canonical_refs>
## Canonical References

- `.planning/REQUIREMENTS.md` — MUX-01…MUX-06 (the locked requirements for this phase) and the SCROLL-* requirements that consume this layer next.
- `INIT.md` — §M5 / topology notes: control-first multiplexing (OPEN/ACCEPT/REJECT on control channel id 0 before binding a stream), per-channel flow-control windows, host-key/discriminant stability (borrowed from quicshell).
- `crates/nosh-proto/src/messages.rs` — the `Message` enum to append to (append-only discriminants).
- `crates/nosh-proto/src/codec.rs` — existing discriminant-stability test precedent (~line 247).
- `CLAUDE.md` — M5 milestone path and the load-bearing transport decisions (two channel types by reliability need; reliable streams carry control/scrollback/forwarding).
</canonical_refs>
