# Phase 12: Server Terminal State Model - Discussion Log

> **Audit trail only.** Decisions captured in CONTEXT.md.

**Date:** 2026-06-01
**Phase:** 12-server-terminal-state-model
**Mode:** discuss (interactive, via /gsd:autonomous --interactive, pipelined while Phase 10 fixes ran)
**Areas discussed:** Echo-state signal, Model scope, Resize behavior

## Area: Echo-state signal (prediction-safety foundation, SEC-01/Phase 15)
Surfaced subtlety: termios ECHO is on the slave side, NOT in master output — vte can't see password mode directly.
**Options:** Observable private modes / Cursor visibility only / Server-side termios echo probe
**User selection:** Observable private modes (DECTCEM, alt-screen, bracketed-paste, app-cursor-keys) → D-12-01, D-12-01a

## Area: Model scope
**Options:** Viewport grid + common subset / Viewport + broader emulation / Include scrollback
**User selection:** Include scrollback in the model (OVERRIDE of viewport-only recommendation) → D-12-02
Note: datagram StateDiff still syncs only the visible viewport (D-12-02a); scrollback sync is later (M5).

## Area: Resize behavior
**Options:** Resize grid + let app repaint / Reflow text
**User selection:** Resize grid, let app repaint (no reflow) → D-12-03

## Locked (not discussed): vte 0.15.0 (not termwiz), unit-tested in isolation before QUIC, OSC 52 detectable, push_output_and_parse feeds both buffers, SequencedOutputBuffer unchanged.
