---
phase: 11-datagram-wire-protocol
verified: 2026-06-01T00:00:00Z
status: human_needed
score: 4/4 success criteria verified (with 2 robustness warnings)
re_verification:
  previous_status: passed
  previous_score: 4/4
  gaps_closed: []
  gaps_remaining: []
  regressions: []
overrides_applied: 0
human_verification:
  - test: "Decide whether the encode_datagram totality guarantee must hold for ALL cap values (including cap < ~8 bytes), or whether a documented minimum-cap precondition is acceptable. Currently caps below the fixed ~7-byte header return an oversized payload in release builds (debug_assert is compiled out)."
    expected: "Either (a) accept that cap is always max_datagram_size()-100 (>1000) at the real callsite — document a minimum-cap precondition and harden the debug_assert to a real assert! or saturating return; or (b) treat 'for ANY input' (D-11-01b) literally and require the function to never emit payload >= cap. This is a design-intent call, not a code fact."
    why_human: "D-11-01b says 'for ANY input' but the only documented caller passes a QUIC-derived cap that can never be < 8. Whether the literal totality wording or the practical-callsite reading governs is a maintainer decision."
  - test: "Replace the shipped `heterogeneous_continue_past_rejection` regression test with one that actually fails under a break-on-first-rejection bug. The current test uses cap=120 where the cursor-priority large run FITS, so the rejection-then-continue path with a subsequently-fitting run is never exercised — injecting `break` at the rejection site leaves the test green."
    expected: "A regression test that turns red if the fill loop breaks on first rejection (e.g. a wholly-rejected early run followed by a fitting later run with budget remaining)."
    why_human: "The production code IS correct (verified adversarially below), but the named guard test does not protect the property it claims to. This is a test-quality gap a maintainer should close so a future refactor can't silently reintroduce the bug."
---

# Phase 11: Datagram Wire Protocol — Re-Verification Report (Adversarial, opus)

**Phase Goal:** A sparse, size-bounded terminal-diff wire format exists in `nosh-proto` — the shared interface that every subsequent server and client component builds on.

**Verified:** 2026-06-01 (independent adversarial re-verification on opus)
**Status:** human_needed (4/4 success criteria met in code; 2 robustness/test-quality warnings need a maintainer decision)
**Re-verification:** Yes — re-checking the executor's self-reported `passed`.

## Bottom line

The load-bearing properties hold **in the code**. `encode_datagram` is total and emits `payload.len() < cap` strictly for every realistic input, the cursor-priority fill **does continue past a rejected oversize run** (verified by injecting the bug and by a 4000-char split probe), round-trips are exact including multibyte UTF-8 and max dimensions, decode is hardened, `chars` is `String`, `epoch` is documented monotonic + distinct from `seq`, the large-repaint decision (with rejected alternatives) is a doc comment at the `encode_datagram` definition, and no new dependencies were added. All 22 production tests pass.

Two things prevent a clean rubber-stamp `passed`, both surfaced for a maintainer decision rather than asserted as code bugs:

1. **Totality edge below the header floor (WARNING).** For `cap <= 7` (smaller than the fixed ~7-byte header), `encode_datagram` panics in debug (`debug_assert`) and returns an **oversized payload in release** (e.g. payload=7 at cap=2..7). D-11-01b says "for ANY input … `< cap`" — literally false below the header floor. The only documented caller passes `max_datagram_size() - 100` (always > 1000), so this is unreachable in practice — but the guard is a `debug_assert`, which is the exact "silent in release" failure mode.

2. **The continue-past-rejection regression test does not catch the bug it names (WARNING).** Injecting `break;` at the rejection site leaves `heterogeneous_continue_past_rejection` green, because at its cap=120 the cursor-priority large run *fits* and is never the rejected run. Production behavior is correct; the guard test is not load-bearing.

## Observable Truths

| # | Truth | Status | Evidence |
|---|-------|--------|----------|
| 1 | `StateDiff` carries sparse runs, monotonic `epoch:u64`, dims (`cols`/`rows`), cursor | ✓ VERIFIED | `datagram.rs:36-62`; epoch doc lines 38-47 (monotonic, never resets, DISTINCT from `seq`) |
| 2 | `encode/decode` round-trip exact for all valid inputs | ✓ VERIFIED | Probe 3 (empty, `你好🌍café`, `u16::MAX` dims, `u64::MAX` epoch) all `decoded == original`; 5 shipped round-trip tests pass |
| 3 | Payload provably STRICTLY `< cap` for the realistic case | ✓ VERIFIED | Probe 5 swept caps 50..1200 on a full 80x24 repaint — `len < cap` held for every cap. Shipped `size_cap_full_80x24_repaint` asserts `< 1100` strict + non-empty deferred |
| 3b | Totality for ALL caps (incl. < header floor) | ⚠️ WARNING | Probe 1b/boundary: cap ≤ 7 → release returns payload=7 (≥ cap); debug panics. Unreachable from real callsite; see human item 1 |
| 4 | Large-repaint decision documented at `encode_datagram` definition, alternatives rejected | ✓ VERIFIED | `datagram.rs:121-152` doc block: cursor-priority chosen; Skip-frame + Reliable-stream fallback explicitly rejected with rationale |
| 5 | Fill CONTINUES past a rejected oversize run | ✓ VERIFIED (code) / ⚠️ WARNING (guard test) | Probe 1 split a 5000-char run at cap 200 (kept 180 + deferred 4820 = 5000, no loss, payload 195<200). Injected `break` measurably changed the genuine probe (chars conservation 3970 vs 3971 → FAIL). BUT shipped `heterogeneous_continue_past_rejection` stays green under injected break — does not guard the property |

**Score:** 4/4 ROADMAP success criteria verified in code.

## Adversarial Probes Run (all throwaway, removed)

| Probe | Result |
|-------|--------|
| 1. 5000-char single run @ cap 200 | SPLIT not whole-emit; payload 195 < 200; chars conserved 5000; no panic — ✓ |
| 1b. tiny-cap sweep (2,5,10,20,50,100) | Triggered `debug_assert` panic at cap=2 → led to boundary analysis — ⚠️ |
| boundary. cap 0..15 debug+release | Release returns payload=7 (≥ cap) for cap 0..7; `< cap` from cap 8 — ⚠️ |
| 2. heterogeneous continue (cap 120) | small `x` run present — ✓ (but see note: large run fits here, so not a true distinguisher) |
| 3. round-trip empty / multibyte / max dims | exact — ✓ |
| 4. decode empty / unknown tag / truncated | all `Err`, no panic — ✓ |
| 5. strict boundary sweep 50..1200 | `len < cap` every cap; never `== cap` — ✓ |
| genuine continue (huge run sorts first, tiny after) | correct impl conserves chars; injected `break` → FAIL (real distinguisher) — ✓ proves production code correct |
| break-injection vs shipped `heterogeneous` test | shipped test stays GREEN under the bug — ⚠️ guard not load-bearing |

## Required Artifacts

| Artifact | Expected | Status | Details |
|----------|----------|--------|---------|
| `crates/nosh-proto/src/datagram.rs` | StateDiff/DiffRun/CellStyle, total encode/decode, docs, tests | ✓ VERIFIED | 796 lines; `chars: String` (line 94, not `Vec<char>`); `pub fn encode_datagram` line 164 |
| `crates/nosh-proto/src/lib.rs` | re-exports | ✓ VERIFIED | line 15 `pub use datagram::{...}` |
| `crates/nosh-proto/Cargo.toml` | NO new deps | ✓ VERIFIED | serde/postcard/bytes/thiserror/quinn/tokio only; no termwiz/prost/bincode/etc. |

## Requirements Coverage

| Requirement | Status | Evidence |
|-------------|--------|----------|
| SYNC-01 (sparse size-bounded datagram wire format, postcard/serde, round-trip + size-cap, no new deps) | ✓ SATISFIED | All four fields present; postcard via existing dep; round-trip + size-cap tests pass; zero new crates |

## Anti-Patterns Found

| File | Pattern | Severity | Impact |
|------|---------|----------|--------|
| `crates/nosh-proto/src/datagram.rs:303` | `debug_assert!` guards the totality invariant — compiled out in release | ⚠️ Warning | Sub-header caps return oversized payload silently in release; unreachable from documented callsite |
| `crates/nosh-proto/src/datagram.rs` (test) `heterogeneous_continue_past_rejection` | regression test does not fail under the bug it names | ⚠️ Warning | False confidence; future refactor could reintroduce break-on-first-rejection undetected |
| `crates/nosh-proto/src/datagram.rs.bak` (0 bytes, untracked) | Editor leftover from the executor | ℹ️ Info | Untracked (won't be committed); pollutes working tree — recommend `rm` |

## Conclusion

The phase goal is **achieved in code**: the datagram wire-format contract is complete, correct, and — for the only inputs the documented caller can produce — total with a strict `< cap` guarantee and a genuinely continue-past-rejection fill. I verified the load-bearing continue-past-rejection property adversarially (bug-injection + 5000-char split) rather than trusting the summary.

Status is `human_needed` rather than `passed` because two robustness/test-quality matters require a maintainer's intent decision, not because any success criterion fails:
- the literal "for ANY input" totality wording vs. the practical QUIC-derived cap (sub-header caps), and
- the named continue-past-rejection guard test not actually guarding the property.

Neither blocks Phase 12+ from building on the type, but both should be closed before this module is treated as a frozen foundation.

---

_Verified: 2026-06-01_
_Verifier: Claude (gsd-verifier, opus, adversarial re-verification)_
_All throwaway probe files removed; production `datagram.rs` confirmed byte-identical to HEAD (`git diff` empty)._
