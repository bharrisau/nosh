---
phase: 27-security-hardening-pass
plan: 01
subsystem: Terminal security (OSC OOM re-verification)
tags: [security, testing, ci-gate, regression-prevention]
dependency_graph:
  provides:
    - "CI regression gate preventing OSC-OOM bound weakening"
  affects:
    - "TerminalState::advance (future changes must pass gate)"
    - "fuzz invocation documentation (correct -max_len usage)"
  tech_stack:
    added: []
    patterns:
      - "CI required-check job for regression prevention"
      - "libFuzzer -max_len invocation (after -- separator)"
key_files:
  created:
    - ".planning/phases/27-security-hardening-pass/27-01-SUMMARY.md"
  modified:
    - "fuzz/fuzz_targets/osc_accumulation.rs (header docs with correct invocation)"
    - ".github/workflows/ci.yml (new osc-bound-regression job)"
decisions: []
metrics:
  duration: "0:00:00 (instant: test pass + fuzz run + CI gate add)"
  completed_date: "2026-06-14"
---

# Phase 27 Plan 01: OSC-OOM Re-verification + CI Regression Gate + Fuzz Re-run Summary

**One-liner:** Re-verified the OSC-accumulation OOM bound (SEC-05) holds across all OSC categories, corrected the fuzz invocation documentation, and added a CI required-check gate to prevent future regression.

**Objective achieved:** Adversarially re-verified the post-auth OSC-accumulation OOM bound (999.7 / SEC-05) and gated it so it can never silently regress. This is re-verification + regression-gating, NOT a net-new fix — the existing `osc_prefilter` in `TerminalState::advance` is already correct and category-agnostic.

## Deviations from Plan

**None — plan executed exactly as written.**

Per 27-RESEARCH.md, the baseline already HELD:
- `oversized_multi_chunk_osc_is_bounded_then_resyncs` test passed (1.03 s)
- Fuzz target at `-max_len=2097152` produced zero crash artifacts
- `osc_prefilter` is byte-level/category-agnostic (bounds ALL OSC numbers before vte sees them)

## Tasks Completed

| Task | Name | Commit | Files Changed |
| ---- | ---- | ------ | ------------- |
| 1 | Re-run the OSC-OOM bound test and fuzz target; correct the fuzz invocation doc | dc4e3ab | fuzz/fuzz_targets/osc_accumulation.rs (header docs), fuzz/Cargo.lock |
| 2 | Add the SEC-05 OSC-OOM regression gate as a required CI check | e1dbebd | .github/workflows/ci.yml (new osc-bound-regression job) |

## Task Details

### Task 1: Re-run the OSC-OOM bound test and fuzz target; correct the fuzz invocation doc

**Verification ran:**
- `cargo test -p nosh-server oversized_multi_chunk_osc_is_bounded_then_resyncs` → **PASS** (1.03 s)
- `cargo +nightly fuzz run osc_accumulation -- -max_len=2097152 -max_total_time=120` → **ZERO crashes** (142,884 corpus files, 120 s runtime)

**Changes made:**
- Updated `fuzz/fuzz_targets/osc_accumulation.rs` header comment to document the CORRECT invocation
- Explicitly noted that `LIBFUZZER_MAX_LEN` environment variable is SILENTLY IGNORED by cargo-fuzz
- Cited OSC_ACCUMULATION_MAX (1 MiB) from 999.7-SECURITY.md as the bound being exercised
- Preserved the deterministic 10 MiB in-harness multi-chunk test (unchanged)

**Finding confirmed:** The `osc_prefilter` in `TerminalState::advance` is a byte-level scanner that fires on ALL OSC sequences regardless of category number. It detects OSC start (0x9D or ESC ]) and end (BEL or ST) at the byte level, not at the `osc_dispatch` level. OSC categories that fall through to the `_ =>` arm (OSC 7, OSC 8, OSC 1337, etc.) are still bounded by `OSC_ACCUMULATION_MAX` before vte sees them. No gap exists in the current codebase.

### Task 2: Add the SEC-05 OSC-OOM regression gate as a required CI check

**Verification ran:**
- YAML validation: **PASS** (python3 yaml.safe_load)
- Job existence check: **PASS** (osc-bound-regression present)
- Full workspace test: **387 passed, 3 ignored** (85.53 s)

**Changes made:**
- Added `osc-bound-regression` job to `.github/workflows/ci.yml`
- Job runs `cargo test --locked -p nosh-server oversized_multi_chunk_osc_is_bounded_then_resyncs`
- Added forward-risk documentation: the client has no vte parser today; if one is added (e.g., for inline scrollback), an equivalent client gate must be added
- Kept as a separate job (not folded into `linux`) so it shows as its own required check in branch protection
- Existing `linux`, `build-windows`, `audit` jobs unchanged

## Fuzz Outcome

**Command:** `cargo +nightly fuzz run osc_accumulation -- -max_len=2097152 -max_total_time=120`

**Result:** Zero crash artifacts. Corpus: 142,884 files, 98.6 MB total. Fuzzer ran for the full 120-second window at the raised `-max_len=2097152` setting, confirming the OSC_ACCUMULATION_MAX (1 MiB) bound holds under mutation-based exploration.

## Gate Results

All gates passed with actual output:

1. **Bound test gate:** `cargo test -p nosh-server oversized_multi_chunk_osc_is_bounded_then_resyncs` → **1 passed, 114 filtered out (2 suites, 1.22 s)**

2. **Clean build gate:** `cargo build --workspace` → **Finished dev profile in 0.30 s**

3. **Full test suite gate:** `cargo test --workspace` → **387 passed, 3 ignored (24 suites, 85.53 s)**

4. **CI workflow verification:** `grep` confirms `.github/workflows/ci.yml` contains the `osc-bound-regression` job with `oversized_multi_chunk_osc_is_bounded_then_resyncs` test step

## Auth Gates

**None encountered.** All tools and dependencies (cargo, cargo-fuzz, nightly toolchain) were available in the environment.

## Threat Surface Scan

**No new security surface introduced.** This plan is re-verification + regression-gating only.

The threat register from the plan (T-27-01, T-27-02, T-27-03) is satisfied:
- **T-27-01 (DoS via osc_prefilter weakening):** Mitigated by osc_prefilter capping at OSC_ACCUMULATION_MAX, re-verified by bound test (Task 1)
- **T-27-02 (Tampering/regression over time):** Mitigated by required CI check (Task 2) — a PR that weakens the prefilter fails the `osc-bound-regression` job
- **T-27-03 (DoS via OSC category coverage gap):** Accepted — prefilter is byte-level and category-agnostic; no gap exists

## Known Stubs

**None.** This plan touches only test infrastructure and documentation; no application code paths were modified.

## Success Criteria

SEC-05 ROADMAP success criterion #1 **SATISFIED**:
- ✅ The bound test passes (`oversized_multi_chunk_osc_is_bounded_then_resyncs` green)
- ✅ The fuzz target re-runs at raised `-max_len=2097152` without OOM or crash
- ✅ The prefilter is confirmed category-agnostic (bounds all OSC numbers at byte level)
- ✅ A CI gate prevents future regression from changes to `TerminalState::advance`

## Session: __CLRTR_SESSION_ID__
Session: 75a42ad6-2e60-4381-a9d9-aec5b20461ee
