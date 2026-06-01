# Phase 11: Datagram Wire Protocol - Discussion Log

> **Audit trail only.** Decisions captured in CONTEXT.md.

**Date:** 2026-06-01
**Phase:** 11-datagram-wire-protocol
**Mode:** discuss (interactive, via /gsd:autonomous --interactive, pipelined while Phase 10 executed)
**Areas discussed:** Large-repaint strategy, Cell+style encoding, Epoch semantics

## Area: Large-repaint strategy (success criterion #4 — the flagged open decision)
**Options:** Cursor-priority partial / Skip-frame / Reliable-stream fallback
**Tension surfaced:** a naive full 80x24 repaint (1920 cells) exceeds a typical ~1200B QUIC
datagram, yet SC#3 requires the payload stay capped — so the strategy is load-bearing.
**User selection:** Cursor-priority partial update → D-11-01, D-11-01a, D-11-01b

## Area: Cell + style encoding granularity
**Options:** Run-length runs / Per-cell sparse list / Model on termwiz::Change
**User selection:** Run-length runs → D-11-02, D-11-02a

## Area: Epoch semantics
**Options:** Monotonic tick counter never resets / Screen-generation resets on repaint+resize
**User selection:** Monotonic tick counter, never resets → D-11-03, D-11-03a

## Locked (not discussed): postcard/serde no new crate, StateDiff fields, round-trip+size-cap tests.
