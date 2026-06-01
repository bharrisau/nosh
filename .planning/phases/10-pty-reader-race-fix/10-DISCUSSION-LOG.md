# Phase 10: PTY Reader Race Fix - Discussion Log

> **Audit trail only.** Do not use as input to planning, research, or execution agents.
> Decisions captured in CONTEXT.md — this log preserves the discussion.

**Date:** 2026-06-01
**Phase:** 10-pty-reader-race-fix
**Mode:** discuss (interactive, via /gsd:autonomous --interactive)
**Areas discussed:** Interrupt mechanism, Thread-count verification, Fix scope, Portability stance

## Area: Interrupt mechanism

**Options presented:**
- Signal-fd + poll (self-pipe/eventfd + poll/select on [pty_fd, signal_fd])
- AsyncFd + O_NONBLOCK (no blocking thread)
- Let research pick A vs B based on portable-pty's API

**Caveat surfaced:** a bare `libc::close()` on the fd does NOT reliably wake a thread already
blocked in `read()` on Linux (the blocked read holds a reference to the open file
description), so the "close-the-fd stopgap" from PITFALLS §6 is unsafe as a primary fix.

**User selection:** "Make sure the solution is Windows compatible — so maybe the AsyncFd option?"

**Clarification given:** `tokio::io::unix::AsyncFd` is Unix-only and will not compile on
Windows. Windows compatibility therefore comes from the trait boundary (see Portability),
not from AsyncFd. Captured as: AsyncFd + O_NONBLOCK for the Unix impl, behind a trait so the
Windows ConPTY impl slots in at M6; signal-fd + poll as the researcher-confirmed fallback if
portable-pty won't expose the master fd. → D-01, D-01a, D-02a

## Area: Thread-count verification

**Options presented:** Completion barrier / tokio RuntimeMetrics (unstable) / timing probe
**User selection:** Completion barrier (Recommended) → D-04, D-04a

## Area: Fix scope

**Options presented:** Reader + reattach clone / Output reader only
**User selection:** Reader + reattach clone (Recommended). Input writer already interruptible
— no change. → D-03, D-03a

## Area: Portability stance

**Options presented:** Trait boundary + Unix impl now / Linux-only revisit at M6
**User selection:** Trait boundary, Unix impl now (Recommended) → D-02

## Deferred Ideas
- Native Windows/ConPTY interruptible-reader implementation — Phase 17 / M6.
- Datagram/state-sync emission from the PTY output callsite — Phase 12/13.
