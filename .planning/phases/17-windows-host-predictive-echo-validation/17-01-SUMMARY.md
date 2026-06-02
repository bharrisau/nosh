---
phase: 17-windows-host-predictive-echo-validation
plan: "01"
subsystem: validation-docs
tags: [validation, predictive-echo, windows, operator-run, PREDICT-07]
dependency_graph:
  requires: [15-client-predictor-speculative-overlay, 16-qol-feature-pack-windows-ci-gate]
  provides: [docs/windows-echo-test.md skeleton ready for operator]
  affects: [PREDICT-07, Phase 17 completion gate]
tech_stack:
  added: []
  patterns: [v1.1 operator sign-off document format (mirrors docs/windows-client-test.md)]
key_files:
  created:
    - docs/windows-echo-test.md
  modified: []
decisions: []
metrics:
  duration: 15
  completed_date: "2026-06-02"
---

# Phase 17 Plan 01: Windows Predictive Echo Validation Skeleton Summary

## One-liner

Operator-ready validation skeleton for PREDICT-07 with six checklist criteria, exact `RUST_LOG=nosh::predict=debug ... --predict always 2> predict.log` capture command, and WireGuard migration procedure — all Result/sign-off cells blank, awaiting operator execution.

## What Was Built

`docs/windows-echo-test.md` — a complete fill-in-the-blanks validation document mirroring the
`docs/windows-client-test.md` v1.1 sign-off format. Every section that does NOT require live
operator execution is fully authored:

- **Header / Status** — title, phase context, purpose (evidence-only phase, no feature work)
- **Prerequisites** — Windows Terminal, release build commands, unencrypted Ed25519 key,
  Linux server setup with `authorized_keys`, real (non-loopback) network requirement, WireGuard
  installation requirement
- **Run Commands** — baseline connect command, and the exact timing-capture command verbatim
  from D-17-02:
  `$env:RUST_LOG="nosh::predict=debug"; .\nosh-client.exe ... --predict always --status 2> predict.log`
  with explanation of why stderr is redirected
- **Validation Checklist** (6 rows, Result column blank):
  - C1: Auth over real (non-loopback) network
  - C2: Predicted echo measured via `predict.log` latency_ms (min/median/max)
  - C3: vim insert epoch reset, zero corrupt cells (D-17-03)
  - C4: noecho suppression via `read -s`, zero predicted chars (PREDICT-04)
  - C5: Windows-native editor/PowerShell quirks coverage (D-17-03)
  - C6: WireGuard-teardown roaming with active prediction, epoch reset, no screen corruption
- **Measured-latency capture instructions** — PowerShell one-liners to parse `predict.log`
  and compute min/median/max/count
- **WireGuard migration procedure** — 6-step numbered procedure with placeholder fenced
  blocks for the operator's actual WG config snippet and teardown command (D-17-01)
- **Expected Behavior Notes** — adaptive vs always mode, underline styling (PREDICT-05),
  epoch-reset-on-cursor-addressing, noecho suppression (PREDICT-04), anti-amplification stall
- **Known Limitations** — encrypted-key rejection, Windows ACL warning, no Pageant, use Windows
  Terminal, Linux-server-only, loopback invalidity, WireGuard user-installed
- **Operator Sign-off** — blank block with: date, host, terminal, server IP (non-loopback
  confirmation), server OS, key path, network/WG details, SRTT, latency_ms (count/min/median/max),
  per-criterion checkboxes, Overall PASSED/FAILED, notes, operator

No Result cells filled. No observed values invented. All sign-off fields are blank lines.

## Commits

| Task | Name | Commit | Files |
|------|------|--------|-------|
| 1 | Author docs/windows-echo-test.md skeleton | 0e506e4 | docs/windows-echo-test.md (created, 347 lines) |

## Task 2 Status: PENDING OPERATOR EXECUTION

Task 2 is a `checkpoint:human-verify` gate. The operator must:

1. Run the complete checklist (C1–C6) on a physical Windows host against a network-reachable
   Linux `nosh-server` over a real (non-loopback) network path.
2. Record measured `latency_ms` values from `predict.log` (min/median/max/count) and SRTT.
3. Execute the WireGuard migration procedure (C6) and paste the exact WG config / teardown
   command into the doc.
4. Complete and sign the Operator Sign-off block.
5. Signal "approved" once all six criteria are recorded and the sign-off is complete.

## Deviations from Plan

None — plan executed exactly as written. Task 1 is the only machine-doable task; Task 2 is the
operator-run live validation checkpoint.

## Known Stubs

None. The validation document is structurally complete. The blank Result cells and sign-off
fields are intentional (they are for the operator to fill; they are not stubs blocking the
document's purpose).

## Self-Check: PASSED

- [x] `docs/windows-echo-test.md` exists (347 lines)
- [x] Automated verification passed: all 10 required strings present, 6 C1-C6 rows confirmed
- [x] Commit 0e506e4 exists and staged only `docs/windows-echo-test.md`
- [x] No Result cells filled; no observed values invented
- [x] Task 2 explicitly NOT performed — checkpoint returned to operator
