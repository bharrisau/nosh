# Phase 18: Security Design Pass - Discussion Log

> **Audit trail only.** Decisions captured in CONTEXT.md.

**Date:** 2026-06-02
**Phase:** 18-security-design-pass
**Areas discussed:** TOFU non-interactive policy, Security doc depth
**Mode:** discuss (interactive, via /gsd:autonomous --interactive)

---

## TOFU non-interactive policy

| Option | Description | Selected |
|--------|-------------|----------|
| Fail closed + --accept-new | Refuse when can't prompt; opt-in --accept-new auto-pins first-seen (OpenSSH semantics). | ✓ |
| Fail closed only | Refuse unless already in known_hosts. | |
| Prompt, fail if impossible | Always try /dev/tty; refuse if no terminal. | |

**User's choice:** Fail closed + --accept-new. → D-18-02. Known-but-changed key always hard-fails.

## Security doc depth

| Option | Description | Selected |
|--------|-------------|----------|
| Concise maintainer threat model | 1-3 pages, 5 mandated topics, maintainer/reviewer audience. | ✓ |
| Comprehensive security spec | Full STRIDE/LINDDUN formal model. | |

**User's choice:** Concise maintainer threat model. → D-18-03.

## Claude's Discretion
- Prompt input source (/dev/tty vs stdin, pre-raw-mode); known_hosts line format; --accept-new flag surface (OpenSSH-faithful).

## Deferred Ideas
- nosh-keyscan host-key pre-distribution — residual mitigation, deferred beyond M4.
- Formal STRIDE/LINDDUN model — out of scope.
- Server privilege separation — documented non-feature.
