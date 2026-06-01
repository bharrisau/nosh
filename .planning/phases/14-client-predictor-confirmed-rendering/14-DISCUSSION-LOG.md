# Phase 14: Client Predictor — Confirmed Rendering - Discussion Log

> **Audit trail only.** Decisions captured in CONTEXT.md.

**Date:** 2026-06-01
**Phase:** 14-client-predictor-confirmed-rendering
**Mode:** discuss (interactive, /gsd:autonomous --interactive, pipelined while Phase 12 executed)
**Areas discussed:** Render architecture, Startup/gap display, ClientScreen type reuse

## Render architecture
Options: Framebuffer-diff compositor (Mosh Display) / Direct diff-replay / Full repaint
Selection: Framebuffer-diff compositor (Mosh-style) -> D-14-01, D-14-01a (compositor seam for Phase 15 overlay + ConnectionLossOverlay)

## Startup & loss display
Options: Datagram-only blank-until-first-frame / Fall back to PtyData display
Selection: Datagram-only; blank until first keyframe (acked-epoch=0 -> full keyframe within ~1 tick) -> D-14-02

## ClientScreen types
Options: Reuse nosh-proto types / Client-specific render types
Selection: Reuse nosh-proto types (fg/bg Option<u8>, CellStyle) -> D-14-04

## Derived/locked: PtyData still advances highest_applied for reattach Ack but no longer displays (D-14-03);
## real client emits the Phase 13 datagram epoch-ack here (D-14-03a); apply-if-epoch>last (D-14-05).
