---
status: partial
phase: 19-full-screen-tui-rendering-correctness
source: [19-VERIFICATION.md, 19-05-PLAN.md]
started: 2026-06-07
updated: 2026-06-07
---

## Current Test

[awaiting human testing — live Linux nosh client↔server]

## Tests

### 1. vim alternate-screen round-trip (TUI-01)
expected: `vim --noplugin` over a Linux nosh client↔server opens to a blank canvas (no shell text bleeding through); after `:q`, the primary buffer and cursor are exactly as they were before vim launched.
result: [pending]

### 2. htop rendering vs reference terminal (TUI-04)
expected: `htop` renders columns, bars, and CPU meters aligned and correct when compared side-by-side against the same htop in a reference terminal — no garbled output, no missing spaces.
result: [pending]

### 3. Full-screen TUI / Claude Code — no predictor overlay (TUI-05)
expected: running Claude Code (or another full-screen TUI) over nosh shows no speculative-echo overlay flicker while typing inside the alternate screen; rendering is correct.
result: [pending]

### 4. CJK + emoji column accuracy (TUI-03)
expected: pasting `中文流语` at the prompt causes no column drift; a ZWJ emoji sequence advances the cursor correctly (renders as one cluster).
result: [pending]

## Summary

total: 4
passed: 0
issues: 0
pending: 4
skipped: 0
blocked: 0

## Gaps

(none recorded — automated criteria all verified; these four items require a live client↔server visual pass per D-19-07/08, deferred to UAT by operator decision on 2026-06-07.)
