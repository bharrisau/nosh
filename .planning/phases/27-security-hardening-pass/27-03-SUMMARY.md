---
phase: 27-security-hardening-pass
plan: 03
subsystem: Threat model documentation (SEC-01)
tags: [security, documentation, threat-model, stride]
dependency_graph:
  provides:
    - "docs/SECURITY.md — auditable threat model for internet exposure"
  affects:
    - "Operator deployment decisions (Mode A vs Mode B understanding)"
    - "Security reviewer understanding of as-shipped mitigations"
  tech_stack:
    added: []
    patterns:
      - "STRIDE-structured threat model"
      - "Code-location citations for every mitigation claim"
key_files:
  created:
    - "docs/SECURITY.md"
    - ".planning/phases/27-security-hardening-pass/27-03-SUMMARY.md"
  modified: []
decisions: []
metrics:
  duration: "0:00:00 (instant: doc authoring based on shipped code)"
  completed_date: "2026-06-14"
---

# Phase 27 Plan 03: Threat-Model Document (SEC-01) Summary

**One-liner:** Authored `docs/SECURITY.md`, a STRIDE-structured threat model documenting the as-shipped internet-exposed trust model, SEC-04/SEC-05 mitigations, and the outer-CA-cert-but-inner-auth-is-authoritative composition from Phases 24/25.

**Objective achieved:** Created the written artifact a security reviewer reads before trusting an internet-exposed nosh server. The doc covers assets and trust boundaries, attacker capabilities, the proxy trust model, Mode A vs Mode B distinction, the mandatory-inner-auth rationale, and residual risks — explicitly documenting the RFC 9266 EKM-bound inner-handshake composition and citing the actual code that shipped in 27-01/27-02.

## Deviations from Plan

**Rule 1 — Honest documentation of dead code:** The plan claimed `MIN_RESIZE_INTERVAL_MS` "enforces a server-issued resize rate-limit". Upon verification, this constant is **defined but not actually enforced** — it is dead code. The resize coalescing behavior exists via `RESIZE_DEBOUNCE` (40ms) in `ResizeWatcher`, but `MIN_RESIZE_INTERVAL_MS` (300ms) is never used in rate-limit logic. This is documented honestly in the "Note on resize rate-limit (DoS)" row of the STRIDE threat table: the constant formalizes the intent as a security property, but the actual enforcement is the existing debounce, not an explicit 300ms rate-limit check.

**Otherwise:** Plan executed exactly as written — all 8 required sections present, all D-08/D-09 items covered, all SEC-04/SEC-05 mitigations cited against actual code locations.

## Tasks Completed

| Task | Name | Commit | Files Changed |
| ---- | ---- | ------ | ------------- |
| 1 | Write docs/SECURITY.md as a STRIDE-structured threat model | [pending commit] | docs/SECURITY.md |

## Task Details

### Task 1: Write docs/SECURITY.md as a STRIDE-structured threat model

**Verification ran:**
- File existence check: ✅ `docs/SECURITY.md` exists
- Line count: ✅ 415 lines (> 120 minimum)
- Keyword checks: ✅ STRIDE, inner, Mode A, RFC 9266 all present
- SEC-04/SEC-05 references: ✅ D-02..D-07 patterns present; `osc_prefilter`/`OSC_ACCUMULATION_MAX` present
- Build check: ✅ `cargo build --workspace` → 0 errors, 1 warning (expected: unused `MIN_RESIZE_INTERVAL_MS`)

**Content sections created:**

1. **Scope and Topology** — Mode A (committed, direct UDP/443) vs Mode B (WebTransport behind proxy, stretch-only). Trust boundary diagrams for each.
2. **Assets and Trust Boundaries** — SSH private key, server host key, session state, reattach token, clipboard contents, local terminal display. Three trust boundaries: client←→network, network←→server, proxy←→server (Mode B).
3. **The Proxy Trust Model** — Explicit outer-CA-cert-but-inner-auth-is-authoritative composition. What outer TLS provides (confidentiality, HTTPS identity). What inner SSH-key handshake provides (end-to-end identity, RFC 9266 EKM binding, CSPRNG-nonce fallback). What a compromised proxy can and cannot do (cannot MITM auth, can observe traffic/drop connection).
4. **Attacker Capabilities** — Five adversary profiles: passive network, active MITM pre-inner-auth, unauthenticated pre-auth DoS, authenticated malicious client, compromised malicious server. Clear "can" vs "cannot" breakdowns.
5. **STRIDE Threat Table** — 15 threat rows with statuses. Spoofing (EKM binding, fieldless InnerAuthFail). Tampering (SEC-04 D-02..D-07 citations). Repudiation (accepted out of scope). Information Disclosure (OSC 52 read dropped, reattach token leakage). Denial of Service (OSC OOM, pre-auth flood, PtyData cap, **resize rate-limit partial**). Elevation of Privilege (env sanitization, SSH_AUTH_SOCK protection).
6. **Shipped Mitigations** — Code-location citations for every mitigation:
   - Pre-auth caps: `server.rs AuthLimits` (max_concurrent=64, auth_timeout=5s)
   - OSC OOM bound: `terminal.rs OSC_ACCUMULATION_MAX` (1 MiB) + `osc_prefilter` + CI gate + fuzz target
   - Client hardening: `main.rs` title/clipboard/hyperlink filters; `client.rs` channel-ID validation
   - Inner auth: RFC 9266 EKM binding; CSPRNG fallback; fieldless `InnerAuthFail`
   - TOFU prompt: blocking fingerprint confirm (Phase 25)
   - Env sanitization: LD_*/BASH_ENV strips (v1.0)
7. **Residual Risks** — RUSTSEC-2023-0071 (accepted, parsing-only), ssh-agent compromise (out of scope), physical client access (out of scope), quinn per-connection allocation (accepted, bounded in count), no Retry/address-validation hardening (accepted, low priority).
8. **Operator Checklist** — Mode A deployment checklist (firewall, SSH keys, host key, known_hosts, resource caps, CI gate, dependencies, fuzzing, logging, monitoring, backup). Mode B deployment stretch steps (proxy cert, EKM support, proxy policy, network topology).

**Dead code finding (MIN_RESIZE_INTERVAL_MS):**
- Constant defined at `main.rs:53` with value 300ms.
- Comment claims "security property, not just UX" and formalizes existing debounce.
- **Actual enforcement:** `RESIZE_DEBOUNCE` (40ms) is used in the resize coalescing logic. `MIN_RESIZE_INTERVAL_MS` is never referenced in any rate-limit check.
- Build warning: `const MIN_RESIZE_INTERVAL_MS is never used` (confirmed in this session).
- **Disposition:** Documented honestly in STRIDE table with "Partial" status and explanatory note. The constant documents intent but does not enforce a 300ms rate-limit. The actual protection is the existing 40ms debounce (`ResizeWatcher`) and the indirect bounding via datagram transport caps.

**Citations grounded in shipped code:**
- Every SEC-04 mitigation (D-02..D-07) cites the actual file+line from the 27-02 SUMMARY (e.g., title filter at `main.rs` ~2162, clipboard validation at `main.rs` ~2127, hyperlink whitelist at `terminal.rs` ~1165, channel-ID parity at `client.rs` ~803).
- SEC-05 mitigations cite `terminal.rs` constants (`OSC_ACCUMULATION_MAX`, `OSC_52_MAX_BYTES`, `MAX_TITLE_BYTES`) and the CI gate job name (`osc-bound-regression`).
- Phase 24/25 topology decisions are cited accurately (RFC 9266 EKM, CSPRNG fallback, fieldless `InnerAuthFail`, state machine transitions).
- `docs/999.1-SECURITY.md` and `docs/999.7-SECURITY.md` are cited for the OSC-OOM analysis and pre-auth caps (not duplicated).

## Deviations from Plan

**None other than the honest dead-code documentation above.**

## Auth Gates

**None encountered.** All work was local documentation; no external auth required.

## Threat Surface Scan

**No new security surface introduced.** This plan is documentation-only.

The threat register from the plan (T-27-11, T-27-12, T-27-SC) is satisfied:
- **T-27-11 (Repudiation/false-assurance):** Mitigated by drafting after 27-01/27-02 and citing actual shipped code from the SUMMARYs — no aspirational claims.
- **T-27-12 (Information Disclosure):** Accepted — the doc describes mitigations and architecture, not secrets.
- **T-27-SC (Tampering):** Accepted — no packages installed (docs only).

## Known Stubs

**None.** The doc is complete; all 8 sections contain substantive content. No TODOs or placeholders remain.

## Success Criteria

SEC-01 ROADMAP success criterion #4 **SATISFIED**:
- ✅ `docs/SECURITY.md` exists (415 lines, > 120 minimum)
- ✅ Contains all 8 sections (scope/topology, assets/boundaries, proxy trust model, attacker capabilities, STRIDE table, shipped mitigations, residual risks, operator checklist)
- ✅ Outer-CA-cert-but-inner-auth-is-authoritative composition stated explicitly (Section 3)
- ✅ Mode A (committed) vs Mode B (stretch) distinction present (Section 1)
- ✅ SEC-04 mitigations cited by D-ID (D-02..D-07) with actual file locations from 27-02
- ✅ SEC-05 mitigations referenced (`osc_prefilter`, `OSC_ACCUMULATION_MAX`, CI gate)
- ✅ Residual RUSTSEC-2023-0071 documented as accepted (parsing-only)
- ✅ Mitigation references match shipped code in 27-01/27-02 SUMMARYs (no invented constants/file paths)
- ✅ Build still clean (`cargo build --workspace` → 0 errors)

## Session: __CLRTR_SESSION_ID__
Session: 75a42ad6-2e60-4381-a9d9-aec5b20461ee
