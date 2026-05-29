# Phase 3: PTY Session Core - Discussion Log

> **Audit trail only.** Not consumed by downstream agents. Decisions live in 03-CONTEXT.md.

**Date:** 2026-05-29
**Phase:** 03-pty-session-core
**Mode:** discuss (default, interactive — inline via /gsd:autonomous --interactive)
**Areas discussed:** Shell I/O stream layout, Shell selection & login, Client env/locale forwarding, Disconnect behavior

## Pre-discussion scouting
Read `crates/nosh-server/src/server.rs` `handle_connection` — confirmed the Phase 2 echo loops (stream_echo_loop/datagram_echo_loop) are what Phase 3 replaces, and the authenticated `quinn::Connection` + released pre-auth permit are already in hand.

## Questions & Answers

### Shell I/O stream layout
- Options: Single framed bidi stream / Raw data + control stream
- **Selected:** Single framed bidi stream — all session traffic via the postcard Message codec; datagrams not used for shell I/O (M4).

### Shell selection & login
- Options: Login shell from passwd / $SHELL non-login / Configurable default-login
- **Selected:** Login shell from /etc/passwd (argv[0] = "-shell").

### Client env / locale forwarding
- Options: Forward TERM + locale / TERM + size only
- **Selected:** Forward TERM + size + whitelisted LANG/LC_*/TZ (SSH SendEnv-style); deny-by-default sanitization unchanged.

### Disconnect behavior
- Options: Terminate + reap now / Keep alive briefly
- **Selected:** Terminate + reap now (SIGHUP, wait/reap, free PTY; no zombies). Session struct exists but process not kept running.

## Notes
- All four answered directly from options; no freeform follow-up needed.
- Privilege-drop/multi-user noted as out of scope for the spike (single-account server).

## Deferred Ideas
- Predictive echo (M4), scrollback/forwarding/multiplexing (M5), reattach (M3), Windows/ConPTY (M6), privilege drop.
