---
gsd_state_version: 1.0
milestone: v1.1
milestone_name: M3 Roaming + Windows Client
status: planning
stopped_at: Phase 4 context gathered
last_updated: "2026-05-30T05:31:22.924Z"
last_activity: 2026-05-29 -- Phase 05 planning complete
progress:
  total_phases: 5
  completed_phases: 3
  total_plans: 11
  completed_plans: 9
  percent: 60
---

# Project State

## Project Reference

See: .planning/PROJECT.md (updated 2026-05-29)

**Core value:** A single QUIC connection on UDP/443 can carry a live interactive shell, authenticated entirely from the user's existing SSH-key identity — and that session survives network changes without re-authenticating.
**Current focus:** Phase 5 — session persistence

## Current Position

Phase: 5
Plan: Not started
Status: Ready to plan
Last activity: 2026-05-29 -- Phase 05 planning complete

```
Progress: [████████░░] 82%
```

## Performance Metrics

**Velocity:**

- Total plans completed: 2 (v1.1)
- Average duration: - (see v1.0 archive for historical baseline)
- Total execution time: 0 hours (v1.1)

**By Phase:**

| Phase | Plans | Total | Avg/Plan |
|-------|-------|-------|----------|
| 4. Identity Threading | 0/? | - | - |
| 5. Session Persistence | 0/? | - | - |
| 6. Cold Reattach Protocol | 0/? | - | - |
| 7. Connection Migration Validation | 0/? | - | - |
| 8. Windows Client | 0/? | - | - |
| 4 | 2 | - | - |

**Recent Trend:**

- Last 5 plans: -
- Trend: -

*Updated after each plan completion*

## Accumulated Context

### Decisions

Decisions are logged in PROJECT.md Key Decisions table.
Recent decisions affecting current work:

- v1.0: Cert-pinning path for M1 (not RFC 7250 RPK) — rustls issue #2257 resolution unconfirmed in 0.23.40
- v1.0: Ed25519-first for auth; RSA must be tested before Phase 2 closes
- v1.0: `spawn_blocking` bridge for PTY I/O (not AsyncFd)
- v1.0: Session keyed on SSH identity fingerprint (not QUIC connection ID) — M3 reattach seam
- v1.1 roadmap: Identity threading is Phase 4 (IDENT-01 only) — prerequisite seam; tiny surface, blocks everything else
- v1.1 roadmap: `MasterPty` must move into `SessionSlot` and stay open for entire orphan lifetime (SIGHUP prevention — critical correctness requirement for Phase 5)
- v1.1 roadmap: Reattach token is a session selector, not a credential; full TLS handshake re-runs on every reconnect (two-factor design baked in from Phase 6 first implementation, not retrofitted)
- v1.1 roadmap: `ServerConfig::migration(true)` set explicitly even though it is the QUIC default — documents intent, guards against future default changes (Phase 7)
- v1.1 roadmap: Windows client (Phase 8) isolated behind `#[cfg]` gates in nosh-client only; nosh-proto, nosh-auth, nosh-server unchanged

### Pending Todos

- At Phase 5 start: verify `uuid` crate is already in nosh-server lockfile (research indicates yes — confirm)
- At Phase 6 start: review PITFALLS.md 13-item reattach checklist before planning; state machine spec is the risk
- At Phase 8 start: confirm `ring` 0.17.14 precompiled x86_64-windows assembly objects are present (no NASM/CMake needed)

**From 2026-05-30 live Windows→Linux validation (native Windows client authenticated + opened PTY session OK):**

- `authorized_keys` must IGNORE unsupported/unparseable entries (warn + skip, like sshd) — currently `load_authorized_keys` (`crates/nosh-auth/src/keys.rs:118`) propagates the first parse error via `?`, so a single RSA/ECDSA/malformed line rejects the entire file. Real-world `authorized_keys` files routinely contain non-Ed25519 keys.
- Client needs a **connection timeout** when no server responds — `connect(...).await` (`crates/nosh-client/src/client.rs:188-190`) has no timeout and hangs; wrap in `tokio::time::timeout` with a clear error.
- Unused `PathBuf` import warning on Windows builds — `crates/nosh-auth/src/signer.rs:15` is only used by the `#[cfg(unix)]` `AgentSigner`; gate the import.
- Investigate Windows `quinn_udp` `WSAEMSGSIZE` (Os code 10040) sendmsg warning (len 1389, ECN Ect0) — connection still succeeded, likely benign GSO/segmentation-offload fallback, but confirm it doesn't degrade Windows reliability or spam logs.
- **[SIGNIFICANT — Phase 8 gap] Windows console not put into virtual-terminal INPUT mode.** Root cause: `run_pump` (`crates/nosh-client/src/main.rs:415`) forwards raw `tokio::io::stdin()` bytes (Unix VT model), and `client.rs:275` calls `crossterm::terminal::enable_raw_mode()` which on Windows only clears line/echo/processed-input — it does NOT set `ENABLE_VIRTUAL_TERMINAL_INPUT`. Symptoms (live test): vim opens in REPLACE mode (Insert key), arrow/up-down keys don't work, `less` uncontrollable (special keys not encoded as ANSI escape seqs); Ctrl-C terminates nosh-client.exe (exit 130) instead of being forwarded to the remote shell as 0x03. Fix (consolidated pass, `#[cfg(windows)]`): after enable_raw_mode, `GetConsoleMode`/`SetConsoleMode` on the stdin handle to add `ENABLE_VIRTUAL_TERMINAL_INPUT` and ensure `ENABLE_PROCESSED_INPUT` is cleared (Ctrl-C delivered as 0x03 byte, forwarded to remote like Unix); set stdout `ENABLE_VIRTUAL_TERMINAL_PROCESSING`; restore on exit. Reconsider `quit_signal()`/`tokio::signal::ctrl_c` on Windows so Ctrl-C interrupts the REMOTE command (Unix semantics) rather than quitting the client — pick a different client-quit mechanism. Requires Windows-host re-test (vim, less, arrows, Ctrl-C). This means Phase 8 D-02 is currently PARTIAL: line shell + auth + resize work; raw-key/TUI input does not.

### Blockers/Concerns

- Phase 6 (Cold Reattach): state machine correctness and two-factor authorization design are the principal risk; must be correct from first implementation (cannot retrofit identity check after token-only build without a protocol change)
- Phase 8 (Windows): Windows ACL permission check gap — `std::fs::Permissions` cannot read ACLs; best-effort warning + documented limitation is the agreed approach
- Phase 8 (Windows): crossterm `use-dev-tty` feature must NOT be enabled (Unix-only; breaks Windows build with `event-stream` per issue #935)

## Deferred Items

Items acknowledged and carried forward:

| Category | Item | Status | Deferred At |
|----------|------|--------|-------------|
| v2 (M4) | Predictive local echo / datagram state sync (ECHO-01) | Deferred to M4 | Init |
| v2 (M5) | Channel multiplexing, forwarding, OSC 52 (FEAT-01..06) | Deferred to M5 | Init |
| v2 (M6) | Windows ConPTY / native server (PLAT-01) | Deferred to M6 | Init |
| v2 (M7) | WebTransport / NAT topologies (TOPO-01..03) | Deferred to M7 | Init |
| v2 (M3+) | Connection status / latency indicator (ROAM-03) | Deferred to M4+ | v1.1 scoping |
| v2 (M3+) | Windows ssh-agent / Pageant (WIN-05) | Deferred post-v1.1 | v1.1 scoping |
| v2 (M3+) | Encrypted key passphrase prompt (WIN-06) | Deferred post-v1.1 | v1.1 scoping |
| v2 (M5+) | Named/numbered session selection | Deferred to M5+ | v1.1 scoping |

## Session Continuity

Last session: 2026-05-30T05:31:22.885Z
Stopped at: Phase 4 context gathered
Resume file: .planning/phases/04-identity-threading/04-CONTEXT.md

## Operator Next Steps

- Start Phase 4: `/gsd:plan-phase 4`
