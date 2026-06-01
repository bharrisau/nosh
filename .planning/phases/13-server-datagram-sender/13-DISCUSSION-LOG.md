# Phase 13: Server Datagram Sender - Discussion Log

> **Audit trail only.** Decisions captured in CONTEXT.md.

**Date:** 2026-06-01
**Phase:** 13-server-datagram-sender
**Mode:** discuss (interactive, via /gsd:autonomous --interactive, pipelined while Phase 11 executed)
**Areas discussed:** Loss model / diff baseline, Reliable PtyData coexistence, Idle + ResumeComplete behavior

## Area: Loss-tolerance / diff baseline
**Options:** Last-sent + periodic keyframe (recommended) / Acked-epoch / Full-screen-every-tick
**User selection + reasoning (OVERRIDE of recommendation):** "I think the acked epoch makes
sense for an unreliable channel? Keyframe isn't great unless it is acked." → Acked-epoch model.
Captured as D-13-01 (+01a epoch-ack channel, +01b resume subsumes keyframe via baseline reset,
+01c Phase 13 = server side + test-client acks; real client acks in Phase 14).

## Area: Reliable PtyData coexistence
**Options:** Additive keep both (recommended) / Datagram sole live path
**User selection:** Additive — keep both → D-13-04

## Area: Idle + ResumeComplete behavior
**Options:** Skip unchanged + keyframe-on-resume (recommended) / Send every tick
**User selection:** Skip unchanged + keyframe-on-resume → D-13-02a, D-13-03 (keyframe-on-resume
realized via acked-baseline reset per D-13-01b)

## Locked (not discussed): ~16ms tick / one diff per tick (not per chunk), conn.send_datagram(),
both pumps get the arm, ResumeComplete gate, integration test asserts client read_datagram non-empty.
