# Phase 8: Windows Client - Discussion Log

> **Audit trail only.** Not consumed by downstream agents. Decisions are in CONTEXT.md.

**Date:** 2026-05-30
**Phase:** 08-windows-client
**Mode:** discuss (interactive, via /gsd:autonomous --interactive)
**Areas presented:** Verification scope, Identity-file scope, Encrypted-key handling, Key-permission strictness

## Area selection
Presented 4 gray areas (multiSelect). User submitted with NO selections → interpreted as "accept all recommended defaults for Phase 8." All four locked at their recommended option; the user was given an explicit chance to correct before context was written.

## Decisions (all = recommended defaults)

### Verification scope
- `cargo check --target x86_64-pc-windows-gnu` from Linux CI = automated gate; documented human Windows interactive test, non-blocking (phase human_needed). → D-01/D-02

### Identity-file scope
- `--identity-file` (FileSigner, on-disk Ed25519) cross-platform/opt-in; ssh-agent stays Linux default; only auth path on Windows. → D-03/D-04

### Encrypted-key handling
- Detect encrypted key, error with clear guidance; interactive prompt deferred to P2. → D-06

### Key-permission strictness
- Best-effort non-fatal warning + documented ACL gap; do not hard-refuse. → D-10

## Locked by research (not re-discussed)
- crossterm 0.28→0.29 (no use-dev-tty); EventStream resize on Windows / SIGWINCH on unix via cfg; ~40ms coalescing preserved (D-07/D-08).
- TERM=xterm-256color + LANG=en_US.UTF-8 defaults (D-09).
- ZeroizeOnDrop on the on-disk key, narrowest scope, never logged (D-05).
- All platform gates confined to nosh-client.

## Scope creep redirected
None. Native Windows server/ConPTY, Pageant, passphrase prompt, proper ACL check, macOS all explicitly deferred.
