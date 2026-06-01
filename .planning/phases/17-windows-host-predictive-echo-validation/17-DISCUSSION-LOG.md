# Phase 17: Windows-Host Predictive Echo Validation - Discussion Log

> **Audit trail only.** Decisions captured in CONTEXT.md.

**Date:** 2026-06-02
**Phase:** 17-windows-host-predictive-echo-validation
**Areas discussed:** Migration test method, Evidence standard, App coverage
**Mode:** discuss (interactive, via /gsd:autonomous --interactive)

---

## Migration test method

| Option | Description | Selected |
|--------|-------------|----------|
| Wi-Fi ↔ phone hotspot | Switch laptop Wi-Fi to tethered hotspot mid-session. | |
| Toggle two NICs | Disable active adapter, fail over. | |
| Operator's choice on the day | Whatever's available. | |

**User's choice:** WireGuard tunnel — bring up WG, connect nosh through it, then close the tunnel to force the path change. → D-17-01.

## Evidence standard

| Option | Description | Selected |
|--------|-------------|----------|
| Operator observation + recording | Human attestation + screen recording. | |
| Measured timing | Predicted-vs-confirmed timestamps, real numbers. | ✓ |
| Both | Attestation + measurement. | |

**User's choice:** Measured timing. → D-17-02. Note: requires client latency instrumentation built on Linux in Phase 15/16 (D-17-02a) since Phase 17 is Windows-only.

## App coverage

| Option | Description | Selected |
|--------|-------------|----------|
| Mirror Phase 15 matrix | Full adversarial set on Windows. | |
| Mandated minimum + Windows-native | vim + noecho + Windows editor + PowerShell/cmd. | ✓ |
| Mandated minimum only | vim + noecho. | |

**User's choice:** Mandated minimum + Windows-native. → D-17-03.

## Claude's Discretion
- Doc section ordering (mirror v1.1); measured-timing log format.

## Deferred Ideas
- Prediction-latency instrumentation → build in Phase 15/16; fold into Phase 16 planning.
- Full Phase 15 matrix on Windows — reduced to mandated + Windows-native.
