# Phase 16: QoL Feature Pack + Windows CI Gate - Discussion Log

> **Audit trail only.** Decisions in CONTEXT.md.

**Date:** 2026-06-01
**Phase:** 16-qol-feature-pack-windows-ci-gate
**Mode:** discuss (interactive, /gsd:autonomous --interactive, pipelined while Phase 12 executed)
**Areas discussed:** OSC 52 clipboard, Title propagation, Windows CI structure, --predict flag

## OSC 52 clipboard (QOL-02)
Selection: Re-emit OSC 52 to local stdout (passthrough; client handles it). Server forwards
write-only over reliable stream, never the read/query form. -> D-16-01/01a/01b

## Title propagation (QOL-03)
Selection: Re-emit OSC 0/2 to local stdout (passthrough) -> D-16-02

## Windows CI (HARDEN-02)
Selection: ci.yml with Linux test job + native windows-latest build-windows job; retire windows-cross.yml -> D-16-04
User flagged: unsure push works from here; asked to review GitHub integrations needed.
Finding: origin reachable (read OK); origin/main stale at f83093e; ~59 unpushed commits; gh not installed; push unverified.
HARDEN-02 final sign-off gated on USER pushing + a green Actions run (human-verification item). -> D-16-04b

## --predict flag (QOL-04 + goal)
Selection: --predict adaptive|always|never (default adaptive) + --status (SRTT in title) -> D-16-05
