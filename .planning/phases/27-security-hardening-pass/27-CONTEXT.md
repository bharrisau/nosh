# Phase 27: Security Hardening Pass - Context

**Gathered:** 2026-06-13 (batched all-phase discussion v1.4)
**Status:** Ready for planning

<domain>
## Phase Boundary

Make the server safe to expose to the internet: adversarially re-verify the OSC-OOM bound (999.7) with a CI regression gate, harden the client against a malicious server (SEC-04), and write the threat-model document (SEC-01, `docs/SECURITY.md`). Must be code-complete before the Phase 28 UAT that validates it.
</domain>

<decisions>
## Implementation Decisions

### SEC-05 — OSC-OOM re-verification (999.7), research-reconciled
- **D-01:** This is **re-verification + regression-gating, not a net-new fix.** The Phase-19 `osc_prefilter` in `TerminalState::advance` is already in place and is the correct fix. Tasks: re-run `oversized_multi_chunk_osc_is_bounded_then_resyncs`; re-run the fuzz target at `LIBFUZZER_MAX_LEN=2097152` (the default 4096 is too short to surface multi-MiB OSC); audit the prefilter against **all** OSC categories nosh handles (not just OSC 0/2/52); add a **required CI gate** so any future change to `TerminalState::advance` re-runs the bound test. If re-verification surfaces a real gap, create a tracked gap-closure plan mid-phase.

### SEC-04 — client trust-boundary hardening (full list locked)
- **D-02:** Per-OSC byte-count gate applied **before** `vte::Parser::advance()` (the prefilter is the right layer; vte buffers internally before dispatch).
- **D-03:** Reject **OSC 52 clipboard-read** requests (write already shipped in v1.2 — keep write, reject read; a malicious server must not exfiltrate the clipboard).
- **D-04:** No-op `dcs_hook` / PM / APC passthrough.
- **D-05:** Strip escape bytes from title (`TerminalControl(Title)`) before re-emission; validate the `TerminalControl(Clipboard)` selection field against known values.
- **D-06:** Enforce a server-issued **resize rate-limit**, a `PtyData` **receive cap**, and **channel-ID range validation**.
- **D-07 (OSC 8 hyperlinks — user decision):** **Whitelist safe schemes** — pass through `http`/`https`/`mailto`/`file`; strip everything else (e.g. `javascript:`, `data:`). Keeps useful clickable links, blocks dangerous schemes.

### SEC-01 — threat-model document
- **D-08:** `docs/SECURITY.md` must cover: assets + trust boundaries; attacker capabilities in an internet-exposed deployment; the **proxy trust model**; the **Mode A vs Mode B** distinction; the **mandatory-inner-auth** rationale; and residual risks.
- **D-09:** Explicitly document the **outer-CA-cert-but-inner-auth-is-authoritative** composition from Phase 24 (D-05): the WebTransport outer TLS (Let's Encrypt cert from a sidecar) provides HTTPS identity/proxy-friendliness, but a terminating/compromised proxy is contained because the inner SSH-key handshake (RFC 9266-bound) is the real end-to-end trust anchor.

### Claude's Discretion
- **SEC-01 methodology/audience:** recommend a **STRIDE**-structured doc aimed at operators + security reviewers (assets → boundaries → STRIDE threats → mitigations → residual risks). Planner/author picks the exact structure; it should reference the specific code mitigations landed in this phase (D-02..D-07) and the channel-binding design (Phase 25).
- Exact resize rate-limit threshold, `PtyData` recv cap value, and channel-ID valid range — planner picks defensible bounds (cite the existing pre-auth caps from `docs/999.1-SECURITY.md` for consistency).
</decisions>

<specifics>
## Specific Ideas

- nosh's structured datagram path (StateDiff carries cell structs, not raw bytes) is already a security advantage — a malicious server cannot inject arbitrary VT escapes via datagrams. SEC-04 extends that protection to the remaining raw-byte paths (OSC accumulation, DCS/PM/APC, title, clipboard). Frame the hardening as "close the raw-byte gaps", not "build from scratch".
- 999.7/SEC-05 must land before the server is declared internet-ready — it is a post-auth availability DoS that becomes far more dangerous once the server is publicly reachable.
</specifics>

<canonical_refs>
## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### Phase scope & decisions
- `.planning/ROADMAP.md` §"Phase 27" — goal + 4 success criteria
- `.planning/REQUIREMENTS.md` — SEC-01, SEC-04, SEC-05
- `.planning/research/SUMMARY.md` §"Phase 5: Security Hardening Pass" + §"Critical Pitfalls" (SEC-1/2/3) + §"Gaps to Address" (999.7 reconciliation)

### Security analysis (read before hardening)
- `docs/999.7-SECURITY.md` — the definitive OSC-OOM analysis; the Phase-16-mitigation-was-wrong finding; the prefilter design and the named regression test
- `docs/999.1-SECURITY.md` — pre-auth caps, anti-amplification, residual risks (cite for SEC-04 bound consistency and SEC-01)
- `.planning/research/PITFALLS.md` — SEC-1 (OSC OOM regression), SEC-2 (terminal escape injection), SEC-3 (TOFU fatigue)
- `.planning/research/FEATURES.md` — SEC-04 client-hardening table (CVE-grounded attack categories)

### Source to read
- `crates/nosh-server/src/terminal.rs` — `TerminalState::advance`, `osc_prefilter`, OSC category handling
- `crates/nosh-client/src/screen.rs` (and the client apply path) — where the per-OSC gate, OSC 8 whitelist, title strip, clipboard validation, resize rate-limit, PtyData cap, channel-ID validation are applied
- `fuzz/` crate — the OSC accumulation fuzz target (re-run at raised `max_len`)
</canonical_refs>

<code_context>
## Existing Code Insights

### Reusable Assets
- The Phase-19 `osc_prefilter` already exists and is correct (per research) — SEC-05 re-verifies and gates it, does not rebuild it.
- The `fuzz/` cargo-fuzz harnesses (999.1) — re-run the OSC target at `LIBFUZZER_MAX_LEN=2097152`.
- Existing pre-auth caps + `cargo audit` + `deny.toml` CI (999.1) — extend the CI with the OSC-bound regression gate.

### Established Patterns
- Structured-datagram architecture limits VT injection to raw-byte paths — SEC-04 targets exactly those paths.
- CI-gate-as-required-check pattern (v1.3 noecho gate) — apply to the OSC-bound test.

### Integration Points
- Client-side hardening sits on the apply path that consumes server output over the (now possibly WebTransport) transport; SEC-01 doc references Phase 24/25 topology + binding decisions.
</code_context>

<deferred>
## Deferred Ideas

None new — SEC-02 (interactive TOFU) lives in Phase 25; SEC-03 (OSC OOM bound) shipped in Phase 19 and is re-verified here as SEC-05.

## Pending Decisions — Re-Ask Before Planning

None blocking. All SEC-04/05 items and the OSC 8 policy are decided above. SEC-01 is authored against the as-built topology — it should be written/finalised **after** the Phase 24–26 mitigations are concrete so it documents what actually shipped (note for the planner: draft early, finalise late in the phase).
</deferred>

---

*Phase: 27-security-hardening-pass*
*Context gathered: 2026-06-13*
