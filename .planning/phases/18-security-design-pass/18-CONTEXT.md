# Phase 18: Security Design Pass - Context

**Gathered:** 2026-06-02
**Status:** Ready for planning

<domain>
## Phase Boundary

Two deliverables: (1) a **security design document** (`docs/security.md`) that formally writes up
the threat model already implemented across M1–M4; and (2) **closing the one implementable gap it
names — silent TOFU** — by adding SSH-style host-key fingerprint confirmation before pinning an
unknown key to `known_hosts`, with a test proving rejection declines the connection.
Requirements: SEC-01, SEC-02. Depends on Phase 16 (the doc must accurately describe
noecho-suppression and the reattach two-factor, which must be in place first).

Out of scope: new transport/auth mechanisms; PQ crypto; anything beyond documenting the existing
model and closing silent TOFU.

</domain>

<decisions>
## Implementation Decisions

### Silent-TOFU close — interactive behavior (D-18-01)
- **D-18-01:** On first contact with an unknown host key, the client prints the key fingerprint
  SSH-style (`SHA256:…  Accept? [y/N]`) and pins to `known_hosts` only on explicit `y`. Anything
  else declines and aborts the connection. (Success criterion #2; format locked by roadmap.)

### Silent-TOFU close — non-interactive policy (D-18-02)
- **D-18-02:** **Fail closed + `--accept-new`, mirroring OpenSSH `StrictHostKeyChecking`.** When
  the client cannot prompt (no interactive terminal / piped input / automation) and the host key
  is unknown: **refuse to connect** by default. Provide an opt-in **`--accept-new`** flag that
  auto-pins a first-seen key without prompting (OpenSSH `accept-new` semantics) for automation.
  A key that is *known but changed* always hard-fails regardless of flags (MITM signal).

### Security doc depth & audience (D-18-03)
- **D-18-03:** **Concise maintainer threat model (~1–3 pages).** Audience: maintainers + security
  reviewers. Cover exactly the five mandated topics honestly:
  1. **TOFU first-contact gap** — named honestly, with the mitigation path (the D-18-01/02 prompt;
     out-of-band pre-pinning / `nosh-keyscan`-style pre-distribution as the residual gap).
  2. **Privilege model** — server runs as the authenticated user, **no privilege separation**;
     contrasted explicitly with sshd's privsep.
  3. **Datagram authentication + replay/staleness** — QUIC TLS 1.3 per-packet auth + the
     monotonic `epoch` staleness guard.
  4. **noecho-suppression** as a security requirement of prediction (Phase 15 D-15-01c).
  5. **Reattach two-factor** — the mint→send→commit token rotation that any future refactor MUST
     preserve.
  Not a full STRIDE/LINDDUN formal spec (too heavy for spike stage).

### Locked by REQUIREMENTS/roadmap (NOT relitigated)
- Doc lives at `docs/security.md`. A test confirms an unknown/declined key declines the connection
  (SEC-02). The doc describes already-implemented behavior — it does not introduce new mechanisms.

### Claude's Discretion
- Where the fingerprint prompt reads input (e.g. `/dev/tty` vs stdin) — must happen during connect,
  before the PTY session enters raw mode.
- Exact `known_hosts` line format details (already via `ssh-key`); doc section ordering.
- Whether `--accept-new` is a bare flag or part of a `--strict-host-key-checking <yes|accept-new|no>`
  surface — keep it OpenSSH-faithful.

</decisions>

<specifics>
## Specific Ideas

- Be honest about the residual TOFU gap rather than overclaiming — the doc's value is an accurate
  threat model a reviewer can trust. Mirror SSH's mental model throughout (the project's premise
  is "reuse existing SSH keys / mirror SSH semantics").

</specifics>

<canonical_refs>
## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### Requirements & success criteria
- `.planning/REQUIREMENTS.md` — **SEC-01, SEC-02**.
- `.planning/ROADMAP.md` — Phase 18 section (2 success criteria; 5 mandated doc topics).

### Existing implementation the doc describes / the TOFU code to change
- Client host-key verification / `known_hosts` TOFU pinning — the current silent-pin path is the
  gap to close (custom `ServerCertVerifier` + `ssh-key` known_hosts; client `--known_hosts`).
- Server privilege model (runs as authenticated user; env sanitization) — M2/M3 session core.
- Reattach two-factor token rotation (mint→send→commit) — M3 cold-reattach protocol.
- Datagram epoch staleness guard — `crates/nosh-proto/src/datagram.rs` + Phase 13/14 apply path.
- noecho-suppression — Phase 15 `15-CONTEXT.md` D-15-01c.

### Architecture
- `CLAUDE.md` — auth model (SSH-key mutual, TOFU/known_hosts, RPK vs cert-pinning), security
  invariants (env sanitization, never forward SSH_AUTH_SOCK, DoS caps). `INIT.md` risk register.
- `.planning/research/PITFALLS.md` — Pitfall 8 (TOFU pin forgery window) names this gap.

</canonical_refs>

<code_context>
## Existing Code Insights

### Reusable Assets
- `ssh-key` `known_hosts` parsing/writing already in use; fingerprint formatting (`SHA256:…`) is a
  standard `ssh-key` capability.
- Existing client TOFU pin path is the single call-site to wrap with the prompt + policy.

### Established Patterns
- The project deliberately mirrors OpenSSH semantics — `--accept-new` should match SSH's behavior
  exactly so the mental model transfers.

### Integration Points
- Prompt happens in the connect/verify path, before the interactive PTY session / raw mode.
- `--accept-new` (and a known-but-changed hard-fail) thread through the host-key verifier.

</code_context>

<deferred>
## Deferred Ideas

- `nosh-keyscan`-style host-key pre-distribution to fully close the first-contact window — name as
  residual mitigation in the doc; implementation deferred beyond M4.
- Formal STRIDE/LINDDUN threat model — out of scope; concise doc only (D-18-03).
- Privilege separation / sandboxing the server — documented as a known non-feature, not built here.

</deferred>

---

*Phase: 18-security-design-pass*
*Context gathered: 2026-06-02*
