---
gsd_state_version: 1.0
milestone: v1.4
milestone_name: M7 Remote Access over HTTP/3 + Security Hardening
status: Roadmap created; ready for Phase 23
stopped_at: Phases 23-28 context gathered (batched discussion)
last_updated: "2026-06-13T04:29:37.752Z"
last_activity: 2026-06-13 — v1.4 roadmap written (Phases 23-28)
progress:
  total_phases: 6
  completed_phases: 0
  total_plans: 2
  completed_plans: 0
  percent: 0
---

# Project State

## Project Reference

See: .planning/PROJECT.md (updated 2026-06-13)

**Core value:** A single QUIC connection on UDP/443 can carry a live interactive shell, authenticated entirely from the user's existing SSH-key identity — and that session survives network changes without re-authenticating.
**Current focus:** Phase 23 — Transport Abstraction Seam

## Current Position

Phase: 23 — Transport Abstraction Seam
Plan: —
Status: Roadmap created; ready for Phase 23
Last activity: 2026-06-13 — v1.4 roadmap written (Phases 23-28)

```
Progress: [░░░░░░░░░░░░░░░░░░░░] 0% (0/6 phases)
```

## Performance Metrics

**Velocity:**

- v1.0: 3 phases, 11 plans (single day, 2026-05-29)
- v1.1: 6 phases (2026-05-30)
- v1.2: 8 phases complete (10-17), 1 deferred (18); ~29 plans
- v1.3: 4 phases (19-22), 16 plans; shipped 2026-06-12

**By Phase (v1.4):**

| Phase | Plans | Total | Avg/Plan |
|-------|-------|-------|----------|
| 23. Transport Abstraction Seam | 0/? | - | - |
| 24. WebTransport Endpoint + Mode A | 0/? | - | - |
| 25. Inner SSH-Key Handshake + TOFU Prompt | 0/? | - | - |
| 26. Migration Handover over WebTransport | 0/? | - | - |
| 27. Security Hardening Pass | 0/? | - | - |
| 28. Interactive UAT Clearing | 0/? | - | - |

**Recent Trend:**

- Last 5 plans: -
- Trend: -

*Updated after each plan completion*

## Accumulated Context

### Decisions

Decisions are logged in PROJECT.md Key Decisions table.
Recent decisions affecting current work:

- v1.4 roadmap: `NoshTransport`/`NoshSendStream`/`NoshRecvStream` trait seam in `nosh-proto` is the mandatory prerequisite (Phase 23) — no WebTransport code can share the session pump without it; the Quinn wrapper must be a pure pass-through with all existing tests green
- v1.4 roadmap: `wtransport` 0.7.1 added with `default-features = false` and `ring` provider pinned explicitly to avoid the `aws-lc-rs` dual-activation panic (Critical Pitfall WT-1); add with `features = ["runtime-tokio", "ring"]`; verify no `aws-lc-rs` in `cargo tree` immediately
- v1.4 roadmap: Mode A (nosh binds its own WebTransport listener on UDP/443) is the committed deliverable; Mode B (Envoy as proxy) is a stretch goal only — nginx and HAProxy do not proxy WebTransport; Envoy's support is experimental and not covered by its security team
- v1.4 roadmap: Inner-auth channel binding (RFC 9266 `tls-exporter`) is a wire-format decision that cannot be retrofitted — must be resolved before any inner-auth code is written in Phase 25; fallback is CSPRNG-nonce pair (client + server each contribute 32 bytes), which must be explicitly documented in SEC-01 if used
- v1.4 roadmap: New `Message` discriminants for inner auth appended at 18-21 (after `ScrollbackCredit` = 17); `message_discriminant_order_is_stable` test updated in the same commit; inserting before 17 silently corrupts deployed sessions (Critical Pitfall WF-1)
- v1.4 roadmap: `InnerAuthFail` is fieldless — no reason code, no oracle for key existence or signature validity (Critical Pitfall MH-1)
- v1.4 roadmap: State machine in WebTransport path enforces strict `Unauthenticated -> ChallengeExchanged -> Authenticated` transition; `SessionOpen` and `Reattach` only accepted in `Authenticated` state
- v1.4 roadmap: Migration handover reuses `run_reattach_session` unchanged (made generic in Phase 23); client reconnect loop detects WT session loss, reconnects, re-runs inner auth, then sends `Reattach`; `SessionRegistry::try_reattach` uses atomic `HashMap::remove` (not get-then-remove) to prevent double-attach race (Pitfall MH-2)
- v1.4 roadmap: 999.7 OSC OOM — the Phase-19 prefilter (`osc_prefilter` in `TerminalState::advance`) is the correct fix and is in place; v1.4 task is adversarial re-verification and regression gating, not net-new implementation, unless re-verification reveals a gap
- v1.4 roadmap: SEC-04 client hardening lands in Phase 27 (before UAT); must ship before the server is internet-exposed
- v1.4 roadmap: WT-STRETCH-01 (Mode B Envoy proxy) is not a committed phase; it is future scope only
- v1.4 roadmap: Datagram MTU in WebTransport mode uses `wtransport::Connection::max_datagram_payload_size()` not `conn.max_datagram_size()` — Quarter Stream ID capsule overhead is ~8-20 bytes smaller than raw QUIC MTU (Pitfall WT-2)

### Pending Todos

- At Phase 23 start: confirm which functions in `server.rs` and `client.rs` hold `quinn::Connection` by concrete type (read `crates/nosh-server/src/server.rs` and `crates/nosh-client/`) before drafting the trait surface; `channel.rs` ChannelEvent::Stream variant is the key seam
- At Phase 24 start: verify wtransport issue #311 (time crate build failure, filed 2026-06-12) is resolved before pulling 0.7.1 into the workspace; check crates.io release notes or the issue tracker
- At Phase 24 start: verify exact API name for `max_datagram_payload_size()` on `wtransport::Connection` 0.7.1 against docs.rs before using it
- At Phase 25 start: decide RFC 9266 vs CSPRNG-nonce fallback — verify `ConnectionCommon::export_keying_material` accessibility through the quinn/wtransport handshake data path before writing a single line of inner-auth code; this is the phase-research gate
- At Phase 25 start: decide and lock the inner-auth wire format (field layout of `InnerAuthChallenge`/`InnerAuthResponse`/`InnerAuthComplete`) before implementation begins; document in a design note
- At Phase 26 start: confirm `SessionRegistry::try_reattach` uses atomic `HashMap::remove` (not get-then-remove); add the concurrent same-token test before wiring the client reconnect loop
- At Phase 27 start: re-run `oversized_multi_chunk_osc_is_bounded_then_resyncs` as the first act; run fuzz target with `LIBFUZZER_MAX_LEN=2097152`; audit `osc_prefilter` against all OSC categories nosh handles before writing any SEC-04 hardening code
- At Phase 28: UAT is human-driven, one item at a time — do not dump a checklist; confirm each item before moving to the next; if a gap is found, create a gap-closure plan and track it before the phase closes

### Blockers/Concerns

None at roadmap time. Concerns to watch during execution:

- Phase 23: Quinn wrapper must be a pure pass-through — any behaviour change risks breaking the session invariants that later phases rely on; gate on full `cargo test --workspace` green
- Phase 24: Crypto-provider feature unification is a day-one blocker if not handled; verify `cargo tree` for `aws-lc-rs` immediately after adding `wtransport`
- Phase 25: Inner-auth channel binding design is the highest-severity architectural risk in the milestone — a missing `tls-exporter` binding makes a trusted proxy a silent MITM; cannot be retrofitted after the wire format is deployed
- Phase 25: `InnerAuthFail` must be fieldless from the first commit — adding a reason code later is a wire-breaking change
- Phase 26: Double-attach race (MH-2) must be confirmed atomic before the reconnect loop ships
- Phase 27: 999.7 re-verification must precede any SEC-04 hardening work — if re-verification reveals the prefilter has a gap, a gap-closure plan takes priority

## Deferred Items

Items acknowledged and carried forward from previous milestones:

| Category | Item | Status | Deferred At |
|----------|------|--------|-------------|
| Phase 18 | Security Design Pass (SEC-01 threat-model doc) | Absorbed into Phase 27 (v1.4) | v1.2 user decision |
| Phase 18 | SEC-02 interactive TOFU fingerprint prompt | Absorbed into Phase 25 (v1.4) | v1.2 user decision |
| Future milestone | SEC-04 client trust-boundary hardening (999.2) | Absorbed into Phase 27 (v1.4) | v1.2 backlog |
| Future milestone | 999.7 OSC OOM re-verification | Absorbed into Phase 27 (v1.4) | v1.3 close |
| v1.4+ | Port forwarding (FWD-01), Agent forwarding (FWD-02) | Declared in mux registry; rejected by v1.3 peers | v1.3 scoping |
| v1.4+ | File transfer (XFER-01) | Deferred | v1.3 scoping |
| v1.4+ | Mode 2027 grapheme clustering | Deferred; wcwidth-per-codepoint is safe v1.3 baseline | v1.3 scoping |
| v1.4+ | Scrollback search / copy-mode text selection | Deferred | v1.3 scoping |
| v1.4+ | Scrollback reflow on resize (per-line width metadata) | Deferred | v1.3 scoping |
| v2 (M6) | Windows ConPTY / native server (PLAT-01) | Deferred to M6 | Init |
| v2 (M6) | Windows ssh-agent / Pageant signing (PLAT-02) | Deferred post-v1.1 | v1.1 scoping |
| Future | WT-STRETCH-01 — Mode B Envoy proxy-fronted mode | Experimental stretch; not a committed phase | v1.4 scoping |
| Future | WT-UX-01 — URL-scheme dispatch (`https://` vs raw host) | Should-have; deferred | v1.4 scoping |
| Future | WT-UX-02 — `--trust-key` / `--strict-host-key-checking` flags | Should-have; deferred | v1.4 scoping |
| Phase 19 | Windows visual re-test of full-screen TUI rendering (4 scenarios) | Open — UAT-01 covers this in Phase 28 | v1.3 close (2026-06-12) |
| Backlog 999.3 | Client terminal rendering correctness pack | Open — UAT-01 covers this in Phase 28 | v1.3 close (2026-06-12) |
| Backlog 999.4 | `read -s` / predictive-echo fix on Windows client | Open — UAT-01 covers this in Phase 28 | v1.3 close (2026-06-12) |

## Session Continuity

Last session: 2026-06-13T03:36:58.433Z
Stopped at: Phases 23-28 context gathered (batched discussion)
Resume file: .planning/phases/23-transport-abstraction-seam/23-CONTEXT.md

## Operator Next Steps

- Start Phase 23 with `/gsd:plan-phase 23`
