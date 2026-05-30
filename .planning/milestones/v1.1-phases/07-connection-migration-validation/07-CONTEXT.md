# Phase 7: Connection Migration Validation - Context

**Gathered:** 2026-05-30
**Status:** Ready for planning

<domain>
## Phase Boundary

Confirm that a live nosh session survives a client IP/path change by continuing the SAME QUIC connection (connection migration) with no re-handshake and no application-visible interruption. The production change is minimal (`ServerConfig::migration(true)` set explicitly); the deliverable is the validation: a headless integration test that forces a path change via `Endpoint::rebind()`, plus a documented human Wi-Fi→cellular live check. This is DISTINCT from cold reattach (Phase 6) — migration keeps the same connection; no token, no replay. NOT in scope: cold reattach, the Windows client, NAT-traversal/relay topologies (M7).
</domain>

<decisions>
## Implementation Decisions

### Migration enabled explicitly
- **D-01:** Set `ServerConfig::migration(true)` explicitly in the server config (build_server_config / transport layer), even though it is the quinn default — with a code comment documenting intent, so a future default change can't silently disable roaming.

### Path-change method (headless test)
- **D-02:** The headless test forces a path change by having the client call `Endpoint::rebind()` onto a FRESH local UDP socket (same host, new port) mid-session. This triggers QUIC path validation (PATH_CHALLENGE/PATH_RESPONSE) without requiring real multi-homing, so it runs in any CI. (Not network-namespaces / dual-interface — that fidelity is unnecessary for v1.1 validation.)

### Acceptance thresholds
- **D-03:** The headless test HARD-FAILS on: any byte loss, out-of-order data, or `ConnectionError` on the active reliable stream across the rebind. The session's interactive stream must continue uninterrupted (same connection — `remote_address` may change, but no new handshake).
- **D-04:** The post-migration anti-amplification stall (RFC 9000 §9.4, ~1-2 RTT while the new path is validated) is MEASURED and logged, with a SOFT warning if it exceeds ~3× RTT — NOT a hard failure (CI scheduling jitter makes a hard latency bound flaky). Record the measured stall in the test output for visibility.

### CID-rotation verification
- **D-05:** Enable quinn's qlog in the headless test and PARSE it to confirm connection-ID rotation / PATH_CHALLENGE on the path change (RFC 9000 §9.5 privacy requirement). This is the primary verification of CID rotation, directly satisfying the success criterion.

### Human live check
- **D-06:** Write a short DOCUMENTED manual procedure + checklist for the real Wi-Fi→cellular check (start a session, switch networks on the client device, confirm the session continues with no re-auth and no data loss). The live check is NON-BLOCKING for autonomous completion: Phase 7 is marked `human_needed`, the autonomous run continues, and the operator runs the check when convenient and records it as PASSED in the phase completion notes.

### Claude's Discretion
- Reuse of the existing in-process test harness (`crates/nosh-client/tests/common/`) for the migration test.
- How RTT / stall is measured (Connection stats `path.rtt`, timing around the rebind).
- qlog output location, format, and which parser/approach is used to detect the CID change.
- Exact wording/format of the human-check procedure doc (where it lives in the repo).
</decisions>

<specifics>
## Specific Ideas

- Migration ≠ reattach: the test must assert the SAME connection survives (no new TLS handshake, no Reattach message) — that is the whole point versus Phase 6. Use a long-running interactive stream and verify continuity through the rebind.
- The post-migration stall is EXPECTED behavior, not a bug — measure and surface it rather than trying to eliminate it.
</specifics>

<canonical_refs>
## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### Requirements & roadmap
- `.planning/REQUIREMENTS.md` — ROAM-01 (session survives IP/path change via migration, no re-handshake, validated headless + human live check)
- `.planning/ROADMAP.md` §"Phase 7: Connection Migration Validation" — goal + 4 success criteria
- `.planning/research/SUMMARY.md` §"Phase 4: Connection Migration Validation" + §"Gaps to Address" (anti-amplification stall measurement)
- `.planning/research/PITFALLS.md` — #1 (migration flag not set), #2 (anti-amplification stall), #3 (CID linkability), #4 (keep-alive/idle-timeout interaction during migration)
- `.planning/research/STACK.md` — quinn 0.11.9 migration API surface: `Endpoint::rebind`, `Connection::stats().path.rtt`, `remote_address`; NOTE `Connection::migrate()`/`set_path()` do NOT exist

### Code touchpoints
- `crates/nosh-proto/src/transport.rs` — shared TransportConfig (idle timeout 300s, keep-alive 15s already set — confirm these don't fight migration path validation, Pitfall #4)
- `crates/nosh-server/src/server.rs:47-81` — `build_server_config` (where `ServerConfig::migration(true)` is set explicitly, D-01)
- `crates/nosh-client/tests/common/`, `crates/nosh-client/tests/session.rs` — existing in-process test harness to extend with the rebind/migration test
</canonical_refs>

<code_context>
## Existing Code Insights

### Reusable Assets
- Existing in-process client/server test harness (`tests/common/`) drives a real session over loopback — extend it with a mid-session `Endpoint::rebind()`.
- `transport_config()` (transport.rs) already sets a finite idle timeout + keep-alive; migration validation must confirm these survive a path change (keep-alive keeps the new path warm).
- quinn `Connection::stats()` exposes `path.rtt` and migration is observable via `remote_address()` change.

### Established Patterns
- Tests bind loopback sockets and run the server in-process (Phase 1-3 pattern). `Endpoint::rebind` on the client endpoint fits this model directly.
- `#[ignore]`-gated slow/live tests already exist (e.g. the 60s idle test) — the human live check is documentation, not an automated test; the headless rebind test is a normal CI test.

### Integration Points
- Server config: one-line explicit `migration(true)` + comment.
- New headless test: open session → exchange data → `rebind()` → assert continuity + parse qlog for CID rotation + measure stall.
- New doc: manual Wi-Fi→cellular procedure/checklist.
</code_context>

<deferred>
## Deferred Ideas

- Cold reattach (new connection after the old one fully died) — Phase 6; migration is the same-connection path.
- NAT hole-punch / relay / WebTransport topologies — M7.
- Connection-status / migration-state UI indicator — M4 (ROAM-03 deferred).
- Hard latency SLA on the migration stall — explicitly NOT a hard gate in v1.1 (D-04); revisit if profiling warrants.

None implemented in Phase 7 beyond the decisions above.
</deferred>

---

*Phase: 07-connection-migration-validation*
*Context gathered: 2026-05-30*
