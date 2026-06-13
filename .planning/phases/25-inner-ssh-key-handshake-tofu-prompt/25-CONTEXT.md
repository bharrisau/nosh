# Phase 25: Inner SSH-Key Handshake + TOFU Prompt - Context

**Gathered:** 2026-06-13 (batched all-phase discussion v1.4)
**Status:** Ready for planning

<domain>
## Phase Boundary

The WebTransport session performs full **mutual SSH-key authentication as an application-level handshake inside the tunnel**, before any `SessionOpen` or `Reattach` frame is processed, bound to the outer TLS session so a terminating proxy cannot relay it. Plus an interactive **blocking TOFU fingerprint prompt** on first contact (SEC-02). This is the most security-critical phase of the milestone.
</domain>

<decisions>
## Implementation Decisions

### Channel binding — RESEARCH-RESOLVED, locked
- **D-01:** Use **RFC 9266 `tls-exporter` channel binding**. `export_keying_material(&mut out, label, context)` is confirmed reachable on **both** `quinn::Connection` and `wtransport::Connection` (verified this session). The server's `InnerAuthChallenge` includes exported keying material from the outer TLS session; both sides derive it identically and it is folded into the signed transcript. **No CSPRNG-nonce fallback is needed** — the highest-severity pitfall (WT-3, transparent-proxy MITM) is fully mitigated. (If, at impl, the API surprises us, the documented fallback is a 32-byte CSPRNG nonce pair from each side — but research says this won't be necessary.)

### Inner-auth scope — user decision
- **D-02:** Inner auth runs **over WebTransport only.** Native QUIC keeps its existing in-TLS-handshake mutual auth (proven since v1.0, AUTH-01..04) — no inner handshake there, no double-auth, no change to the proven native path.

### Wire format
- **D-03:** Four-step mutual challenge-response on the control stream: `InnerAuthChallenge` (discriminant 18) → `InnerAuthResponse` (19) → `InnerAuthComplete` (20) → `InnerAuthFail` (21). Appended **after** `ScrollbackCredit` (17) in strict append-only order. The `message_discriminant_order_is_stable` test is updated in the **same commit** as the new variants (WF-1 silent-corruption guard).
- **D-04:** `InnerAuthFail` is **fieldless** — reveals neither whether the key exists nor whether the signature was valid (no-oracle invariant, same as `ReattachErr`).
- **D-05:** State machine strictly enforces `Unauthenticated → ChallengeExchanged → Authenticated`; `SessionOpen`/`Reattach` are accepted **only** in `Authenticated` (MH-1 guard — no token before auth).
- **D-06:** Nonces are 32-byte CSPRNG, single-use server-side (WT-4 replay guard).
- **D-07:** Reuse existing `nosh-auth` crypto — `RawEd25519Signer`/`AgentSigner` for signing, `lookup_known_host`/`record_known_host` for server TOFU, and a `check_authorized_key` helper extracted from `AuthorizedKeysVerifier`. No new auth crates.

### TOFU prompt (SEC-02)
- **D-08:** On first contact with an unknown server host key, the client shows a **blocking** prompt: SHA-256 hex fingerprint, requires typing `yes`, produces **no PTY output until resolved**. Declining disconnects cleanly. (Replaces the v1.3 silent-record anti-feature.)
- **D-09 (native-QUIC path — user decision):** The TOFU prompt on the native-QUIC path is a **pre-connect known_hosts check**, NOT a block inside the rustls verifier. Before dialling: check known_hosts; if unknown, prompt, record on `yes`, then `connect()` with the key already pinned. Keeps interactive I/O off the TLS handshake callback thread. (On the WebTransport path the prompt sits naturally in the app-level inner handshake.)
- **D-10 (no-TTY — user decision):** In a non-interactive context (no TTY), an unknown host key **fails closed** — refuse to connect, print the fingerprint, point the user to interactive confirmation or the future `--trust-key` flag. No silent trust.

### Claude's Discretion
- Exact byte layout of the signed transcript (which fields + the exported-keying-material bytes are concatenated/hashed before signing) — planner designs; must include the channel-binding material (D-01) and both nonces (D-06). Keep it a single canonical transcript both sides reconstruct identically.
- Whether inner auth lives in a shared `inner_auth.rs` used by both client and server, or split per crate — planner's call (ARCHITECTURE.md sketches `run_inner_auth_server`/`run_inner_auth_client`).
</decisions>

<specifics>
## Specific Ideas

- The TOFU prompt format should match OpenSSH's familiar wording closely (SHA-256 fingerprint, `Are you sure you want to continue connecting (yes/no)?`-style) — users already trust that flow; don't reinvent it.
- The pre-connect TOFU check (D-09) and the future `--trust-key`/`--strict-host-key-checking` flags (WT-UX-02, deferred) share the same known_hosts code path — design the pre-connect check so the flags slot in later without rework.
</specifics>

<canonical_refs>
## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### Phase scope & decisions
- `.planning/ROADMAP.md` §"Phase 25" — goal + 5 success criteria
- `.planning/REQUIREMENTS.md` — WT-04, SEC-02
- `.planning/research/SUMMARY.md` §"Phase 3: Inner SSH-Key Handshake + TOFU Prompt" + §"Critical Pitfalls" (WT-3, WT-4, WF-1, MH-1)

### Security design (read before designing the wire format)
- `.planning/research/PITFALLS.md` — WT-3 channel binding (highest severity), WF-1 discriminant stability, MH-1 token-before-auth, SEC-3 TOFU fatigue
- `.planning/research/ARCHITECTURE.md` §"inner SSH-key handshake" — 4-step sequence, message variants, `nosh-auth` reuse
- Verified API fact (this session): `Connection::export_keying_material(&mut out, label, context)` exists on both quinn 0.11.x and wtransport 0.7.x → RFC 9266 binding is achievable; no fallback needed
- RFC 9266 "Channel Bindings for TLS 1.3" — `tls-exporter` semantics

### Source to read
- `crates/nosh-proto/src/messages.rs` — `Message` enum, current discriminants 0–17, append-only invariant, `message_discriminant_order_is_stable` test
- `crates/nosh-auth/src/verifier.rs` — `HostKeyVerifier` (silent-record path to replace), `AuthorizedKeysVerifier` (extract `check_authorized_key`)
- `crates/nosh-auth/src/keys.rs` + signer traits — `RawEd25519Signer`/`AgentSigner` reuse
</canonical_refs>

<code_context>
## Existing Code Insights

### Reusable Assets
- `nosh-auth` provides every crypto primitive the inner handshake needs — signing, SPKI extraction, authorized_keys/known_hosts lookup. Inner auth is new *wiring*, not new crypto.
- Phase 23 transport trait — the inner handshake runs over `NoshSendStream`/`NoshRecvStream` (the control stream), so it is transport-agnostic by construction (though only invoked on the WebTransport path per D-02).

### Established Patterns
- Append-only `Message` discriminants with a stability test as the gating first commit (v1.3 Phase 21 precedent) — follow it exactly for variants 18–21.
- Opaque failure messages (`ReattachErr` precedent) → `InnerAuthFail` fieldless.

### Integration Points
- Inner auth gates the WebTransport `handle_connection` path before `SessionOpen`/`Reattach`; the pre-connect TOFU check sits in the client connect path before `connect()`.
</code_context>

<deferred>
## Deferred Ideas

- **`--trust-key <fingerprint>` / `--strict-host-key-checking` CLI flags** — Future (WT-UX-02). D-09's pre-connect check is designed to accommodate them later. D-10's no-TTY failure message should reference `--trust-key` as the (future) escape hatch.

## Pending Decisions — Re-Ask Before Planning

1. **Channel-binding API confirmation** — *Dependency: Phase 24 (outer TLS wiring must exist to know exactly how to reach `export_keying_material` on the live WebTransport connection).*
   RE-ASK TRIGGER (research, autonomous): after Phase 24 completes, before Phase 25 planning — confirm `export_keying_material` returns identical bytes on both ends through the actual wtransport connection handle as wired in Phase 24. Research says YES; this is a verify-don't-trust gate, not a user question. Only escalate to the user if the API turns out unreachable (then the CSPRNG fallback decision becomes a real question).
</deferred>

---

*Phase: 25-inner-ssh-key-handshake-tofu-prompt*
*Context gathered: 2026-06-13*
