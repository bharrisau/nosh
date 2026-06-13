---
phase: 27-security-hardening-pass
verified: 2026-06-14T00:00:00Z
status: passed
score: 4/4 must-haves verified
overrides_applied: 0
human_verification:
  - test: "Run the OSC-OOM fuzz target at raised max_len and confirm no OOM/crash"
    expected: "cargo +nightly fuzz run osc_accumulation -- -max_len=2097152 -max_total_time=120 completes with zero crash artifacts and bounded memory"
    why_human: "Requires nightly toolchain + cargo-fuzz; long-running; cannot be executed in a fast verification pass. SUMMARY claims it was run clean (27-01-SUMMARY.md line 41); the deterministic in-harness 10 MiB path + unit test + CI gate are verified in code, but the adversarial fuzz run itself is operator-confirmable."
notes_test_quality:
  - guard: "resize rate-limit (SC#3)"
    runtime: "REAL and correct (main.rs:2094-2114)"
    test: "resize_rate_limit_enforced_in_main — SOURCE-GREP test; passes even when the < comparison is inverted to > (logic broken, text present). Inadequate as a regression guard, but the runtime logic was independently read and confirmed correct."
  - guard: "PtyData cap (SC#3)"
    runtime: "REAL and correct (main.rs:2072-2079, TransportDrop on >1 MiB)"
    test: "ptydata_cap_enforced_in_main — SOURCE-GREP test. Acceptable pragmatic floor: enforcement lives in the binary-crate select! loop (hard to unit-test); runtime confirmed by reading."
  - guard: "channel-ID parity (SC#3)"
    runtime: "REAL and correct (client.rs:795-803, pre-loop bail; testable function)"
    test: "channel_id_parity_enforced_in_await_accept — SOURCE-GREP test and CIRCULAR: the grepped strings ('expected_id % 2 != 0', 'is odd (client-initiated IDs must be even)') appear in the test's own body and error messages, so read_to_string('src/client.rs') matches itself. PROVEN: removing BOTH runtime parity bails leaves the test passing. INADEQUATE — await_channel_accept is directly callable; a real test should invoke it with an odd/zero/u32::MAX id and assert the bail. Logged as residual risk (not a blocker — runtime guard is real and correct)."
---

# Phase 27: Security Hardening Pass Verification Report

**Phase Goal:** The server is safe to expose to the internet — OSC OOM bounds adversarially confirmed, client hardened against a malicious server, threat model documented.
**Verified:** 2026-06-14
**Status:** passed (4/4 success criteria enforced in code; one stale doc note + three weak tests recorded as residual risk for human awareness)
**Re-verification:** No — initial verification (post code-review CR-01 fix)

## Goal Achievement

### Observable Truths (the 4 ROADMAP Success Criteria)

| # | Truth | Status | Evidence |
|---|-------|--------|----------|
| 1 | SEC-05: bound test passes; fuzz re-runs at raised max_len without OOM; prefilter bounds ALL OSC categories; CI gate prevents regression | ✓ VERIFIED | `osc_prefilter` (terminal.rs:346-458) UNCHANGED this phase (phase diff is +46 additive OSC8 lines only — prefilter core untouched). `oversized_multi_chunk_osc_is_bounded_then_resyncs` passes. CI job `osc-bound-regression` (ci.yml:64-77) runs the named test on every push. Fuzz header documents `-max_len=2097152` after `--` and warns LIBFUZZER_MAX_LEN is ignored. Category-agnostic: prefilter fires at byte level before vte dispatch (all OSC codes). Fuzz *execution* → human. |
| 2 | SEC-04: client per-OSC byte gate, OSC52-read reject, DCS/PM/APC no-op, title escape strip, clipboard selection whitelist | ✓ VERIFIED | Runtime: title CR/LF/ESC/BEL strip (main.rs:2187-2190); clipboard whitelist {c,p,s,q,0-9} + drop (2154-2164); OSC52 `?` read drop (2148-2152); DCS/PM/APC no-op (vte default impls, terminal.rs scope fence). OSC8 scheme whitelist http/https/mailto/file, case-insensitive, ESC-stripped (server terminal.rs:1159-1180 + client 2196-2216). Postcard wire-compat: append-only `Hyperlink`=2 (discriminant test codec.rs). PROBE: reverting the title/OSC8 algorithm fails the tests (title_with_cr_is_filtered, hyperlink_with_dangerous_scheme_is_dropped). |
| 3 | SEC-04: client enforces server-issued resize rate-limit, PtyData recv cap, channel-ID range validation | ✓ VERIFIED | **Resize rate-limit (the fixed BLOCKER) is REAL:** runtime `Message::Resize` handler (main.rs:2094-2114) with `last_server_resize: Option<Instant>` (declared 1847), `duration_since(last) < MIN_RESIZE_INTERVAL_MS → continue` (flood within 300ms skipped; latest still applied; resets predictor). PtyData cap → TransportDrop on >1 MiB (2072-2079). Channel-ID: even/non-zero/not-u32::MAX bail pre-loop (client.rs:795-803). All three runtime guards read and confirmed correct. (Their *tests* are source-grep — see test-quality findings.) |
| 4 | docs/SECURITY.md covers assets/boundaries, attacker caps, proxy trust model, Mode A/B, mandatory-inner-auth, residual risks | ✓ VERIFIED | docs/SECURITY.md (415 lines) covers all D-08 items: §2 assets+boundaries, §3 proxy trust model + D-09 outer-CA/inner-auth-authoritative composition + RFC 9266 EKM, §1 Mode A vs B, §4 attacker capabilities, §5 STRIDE table, §7 residual risks, §8 operator checklist. **Caveat:** the resize STRIDE row (line 139 "⚠️ Partial") + §5 note (143) + §6 (249-258) are STALE post-CR-01-fix — see Anti-Patterns. Non-blocking (all 6 D-08 topics present and substantive). |

**Score:** 4/4 truths verified

### Probe Evidence (revert guard → confirm → restore)

| # | Probe | Before (guard reverted) | After (restored) | Verdict |
|---|-------|------------------------|------------------|---------|
| SC#3 resize | Removed runtime `duration_since < MIN_RESIZE_INTERVAL_MS` block | `resize_rate_limit_enforced_in_main` FAILED ("must be used in Duration::from_millis") | PASS | grep test trips on text removal |
| SC#3 resize logic | Inverted `<` to `>` (logic broken, all grep tokens kept) | `resize_rate_limit_enforced_in_main` **PASSED** despite broken logic | n/a | **Grep test gives zero logic protection** — runtime logic independently confirmed correct by reading |
| SC#3 channel-ID | Removed BOTH runtime parity bails (expected_id + channel_id), kept test | `channel_id_parity_enforced_in_await_accept` **PASSED** with all runtime enforcement gone | n/a | **Test is CIRCULAR** — greps strings in its own body. Runtime guard (client.rs:795-803) is real & correct. |
| SC#2 title (runtime) | Reverted runtime title filter, kept test helper | all 3 title tests PASSED (test exercises helper copy, not runtime) | n/a | Test validates the algorithm, not the inline select! path |
| SC#2 title (algorithm) | Reverted the test-helper filter | `title_with_cr_is_filtered`/lf/esc FAILED | PASS | Test correctly pins the strip algorithm |
| SC#2 OSC8 | Disabled OSC8 helper whitelist | `hyperlink_with_dangerous_scheme_is_dropped` FAILED (javascript: not dropped) | PASS | Test correctly pins the whitelist algorithm |
| SC#1 prefilter | Disabled OSC overflow truncation in prefilter | `oversized_multi_chunk_osc_is_bounded_then_resyncs` **PASSED** (downstream MAX_TITLE_BYTES masks 10 MiB; no process OOM at 10 MiB) | n/a | Unit test is NOT a direct prefilter-removal falsifier; OOM protection is the fuzz target's job (→ human) + the prefilter code is intact & correct |

### Required Artifacts

| Artifact | Expected | Status | Details |
|----------|----------|--------|---------|
| crates/nosh-server/src/terminal.rs `osc_prefilter` | OSC OOM bound, unaltered | ✓ VERIFIED | Phase diff additive only (OSC8 whitelist +46 lines); prefilter core untouched |
| crates/nosh-server/src/terminal.rs OSC8 dispatch | scheme whitelist http/https/mailto/file | ✓ VERIFIED | terminal.rs:1159-1180; non-whitelisted dropped; cleared on RIS |
| crates/nosh-client/src/main.rs Resize handler | runtime rate-limit | ✓ VERIFIED | 2094-2114; last_server_resize tracker (1847) |
| crates/nosh-client/src/main.rs PtyData cap | TransportDrop on >1 MiB | ✓ VERIFIED | 2072-2079 |
| crates/nosh-client/src/main.rs title/clipboard/OSC52/OSC8 | strip + whitelist + read-reject | ✓ VERIFIED | 2128-2216 |
| crates/nosh-client/src/client.rs `await_channel_accept` | channel-ID validation | ✓ VERIFIED (runtime) | 795-803 pre-loop bail; in-loop check 811-816 |
| crates/nosh-proto messages.rs/codec.rs Hyperlink | append-only, discriminant-stable | ✓ VERIFIED | Hyperlink=2 append-only; discriminant test |
| .github/workflows/ci.yml `osc-bound-regression` | required CI gate | ✓ VERIFIED | 64-77; runs named bound test |
| fuzz/fuzz_targets/osc_accumulation.rs | correct -max_len header + deterministic 10 MiB path | ✓ VERIFIED | header documents `-max_len=2097152`; in-harness 10 MiB drive every iteration |
| docs/SECURITY.md | STRIDE threat model, all D-08/D-09 | ✓ VERIFIED (with stale resize note) | 415 lines; see truth #4 |

### Anti-Patterns Found

| File | Line | Pattern | Severity | Impact |
|------|------|---------|----------|--------|
| docs/SECURITY.md | 139, 143, 249-258 | Stale doc: resize row marked "⚠️ Partial" and §5/§6 claim "no explicit rate-limit on how many Resize messages a malicious server can send on the control stream" / "bounds client-initiated resizes" | ⚠️ Warning | CONTRADICTS the shipped CR-01 fix (main.rs:2094-2114 enforces exactly the server→client control-stream resize rate-limit the doc says is absent). Should be updated to "✅ Mitigated" / "enforced". Doc must "honestly reflect shipped code" (§8 self-statement, line 408). Non-blocking — SC#4 only requires the topic be covered, and it is, but the note is now inaccurate. |
| crates/nosh-client/src/client.rs | 913-932 | `channel_id_parity_enforced_in_await_accept` is a CIRCULAR source-grep test (greps strings present in its own assertion/error text); passes with all runtime enforcement removed | ⚠️ Warning | Zero regression protection for the channel-ID guard. Function is directly testable; a real test should call it with odd/0/u32::MAX id and assert bail. Runtime guard itself is real & correct, so not a goal blocker. |
| crates/nosh-client/src/main.rs / client.rs | 953-1009 | `resize_rate_limit_enforced_in_main`, `ptydata_cap_enforced_in_main` are source-grep tests; resize one passes even with inverted comparison logic | ⚠️ Warning | Weak floor for binary-crate select!-loop guards. Acceptable as a pragmatic floor for PtyData/resize (genuinely hard to unit-test inline select! arms) ONLY because the runtime logic was independently confirmed correct here. Future regressions to the logic (not the text) would go undetected. |

### Requirements Coverage

| Requirement | Source Plan | Description | Status | Evidence |
|-------------|-------------|-------------|--------|----------|
| SEC-05 | 27-01 | OSC-OOM re-verify + CI gate | ✓ SATISFIED | SC#1 evidence |
| SEC-04 | 27-02 | Client trust-boundary hardening | ✓ SATISFIED | SC#2 + SC#3 evidence |
| SEC-01 | 27-03 | docs/SECURITY.md threat model | ✓ SATISFIED | SC#4 evidence |

### Human Verification Required

#### 1. OSC-OOM fuzz run at raised max_len

**Test:** `cargo +nightly fuzz run osc_accumulation -- -max_len=2097152 -max_total_time=120`
**Expected:** Completes with zero crash artifacts and bounded memory (no OOM).
**Why human:** Requires nightly + cargo-fuzz; long-running; out of scope for a fast verification pass. The deterministic in-harness 10 MiB path, the unit bound test, the CI gate, and the unaltered prefilter are all verified in code; only the adversarial fuzz execution itself is operator-confirmable. SUMMARY (27-01) claims it ran clean.

### Gaps Summary

No blocking gaps. All four ROADMAP success criteria are enforced by real, correct runtime code that I independently read and (where falsifiable) probed.

**Is the resize fix real?** YES. The CR-01 fix is a genuine runtime `Message::Resize` handler at main.rs:2094-2114 with a backing `last_server_resize: Option<Instant>` state field (1847). A flood of server Resize frames within `MIN_RESIZE_INTERVAL_MS` (300ms) is skipped via `continue`; the latest valid frame still applies (resizes screen + predictor). This is correct runtime logic — not dead code, not a stub. The prior "MIN_RESIZE_INTERVAL_MS is dead code" BLOCKER is genuinely closed.

**Are the source-grep tests acceptable, or a residual finding?** They are a RESIDUAL FINDING (recorded as Warnings, not a blocker):
- `channel_id_parity_enforced_in_await_accept` is INADEQUATE and circular — proven to pass with all runtime enforcement removed. `await_channel_accept` is directly callable; this should be a real call-the-function test.
- `resize_rate_limit_enforced_in_main` is INADEQUATE for logic — proven to pass with the comparison inverted.
- `ptydata_cap_enforced_in_main` is a tolerable pragmatic floor (binary-crate select! arm).
None of these block the phase goal because the underlying runtime guards were independently read and confirmed correct in this verification pass. They are flagged so the weak tests are not mistaken for behavioural regression protection.

Two non-blocking improvements recommended (route to backlog, operator's choice):
1. Update docs/SECURITY.md resize note (STRIDE row 139, §5 note, §6 250-258) to reflect the now-enforced server→client resize rate-limit (currently stale "Partial"/"no rate-limit" claim contradicts shipped code).
2. Replace the three source-grep "enforced" tests with real behavioural tests (at minimum the channel-ID one, which is trivially callable).

---

_Verified: 2026-06-14_
_Verifier: Claude (gsd-verifier, opus, adversarial pass)_
