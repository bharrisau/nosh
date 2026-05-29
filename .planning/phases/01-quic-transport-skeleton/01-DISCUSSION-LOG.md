# Phase 1: QUIC Transport Skeleton - Discussion Log

> **Audit trail only.** Do not use as input to planning, research, or execution agents.
> Decisions captured in 01-CONTEXT.md — this log preserves the discussion.

**Date:** 2026-05-29
**Phase:** 01-quic-transport-skeleton
**Mode:** discuss (default, interactive — run inline via /gsd:autonomous --interactive)
**Areas discussed:** Workspace/crate layout, Wire framing & serialization, Dev port & test cert seam, Proof / verification approach

## Area Selection

User selected all four proposed gray areas to discuss.

## Questions & Answers

### Workspace / crate layout
- **Options presented:** Full multi-crate now / Minimal, split later / Single crate, modules
- **Selected:** Full multi-crate now (nosh-proto, nosh-auth stub, nosh-server, nosh-client)

### Wire framing & serialization
- **Options presented:** Codec now — postcard / Codec now — CBOR / Raw bytes for M0
- **Selected:** "Other" — user asked "We can do with postcard - but why not like protobufs or capn-proto?"
- **Follow-up (plain text):** Claude analyzed the four options for nosh's small control-frame use case:
  - Cap'n Proto ruled out (zero-copy benefit irrelevant for tiny frames; heavier API)
  - Protobuf = real contender; advantages (IDL, field-number evolution) only pay off with a non-Rust peer or a published independently-implementable spec
  - postcard = lean, serde-native, no codegen; not self-describing, manual cross-version evolution
  - CBOR = self-describing middle ground (quicshell's choice)
  - Claude recommended postcard for the Rust-only lockstep spike, isolated behind a Message enum, with protobuf as the named graduation path.
- **User decision:** Option 1 — postcard now, promote to protobuf later if a non-Rust peer or published spec appears.

### Dev port & test cert seam
- **Options presented:** Configurable, default 4433 / Default 443
- **Selected:** Configurable, default 4433 (443 = production target). Cert seam = rcgen self-signed placeholder swapped for SSH-key verifiers in Phase 2 (noted as the approach either way).

### Proof / verification approach
- **Options presented:** Both: tests + demo / Integration tests only / Demo only
- **Selected:** Both — integration tests as the gate + a runnable two-binary demo.

## Deferred Ideas
- Protobuf wire spec (only if non-Rust peer / published spec) — D-04
- SSH auth — Phase 2; PTY/session — Phase 3; migration/state-sync — M3/M4

## Notes
- Codec choice was the only area requiring freeform discussion; the other three were settled directly from the proposed options.
