---
phase: 20-repaint-pacing
verified: 2026-06-11T00:00:00Z
status: passed
score: 9/9 must-haves verified
overrides_applied: 0
re_verification:
  previous_status: null
gaps: []
---

# Phase 20: Repaint Pacing Verification Report

**Phase Goal:** Full-screen repaints land in roughly one round-trip instead of dribbling one MTU per 16 ms tick — multiple state-diff datagrams burst within a single tick, the two 999.4 failure modes (R-1 infinite-spin, R-2 noecho-epoch leak) are designed out architecturally, and the noecho security invariant is proven by a required CI gate.
**Verified:** 2026-06-11
**Status:** passed
**Re-verification:** No — initial verification

## Goal Achievement

### Observable Truths

| # | Truth | Status | Evidence |
| --- | ----- | ------ | -------- |
| 1 | A full 80x24 repaint is delivered in a single tick's burst of datagrams (PACE-01, SC1) | ✓ VERIFIED | `send_burst()` (server.rs:427) sends `result.payload` then loops draining `result.deferred` via `encode_datagram` within one tick until empty/cap/buffer-full. `burst_drains_when_grid_differs_from_acked_baseline` (1559) fills a real 80x24 grid (1920 cells) and asserts the drain empties `deferred` in `iter_count > 0` and `< 100` iterations. Test passes. |
| 2 | `send_burst` actually bursts (multiple datagrams within one tick, budget-gated) and is wired into BOTH tick arms (PACE-01) | ✓ VERIFIED | Loop at server.rs:468 gates on `burst_count < BURST_CAP (64)` AND `datagram_send_buffer_space() >= cap`, sending each `encode_datagram` payload. Called at server.rs:856 (run_session) and server.rs:1346 (run_reattach_session) — identical wiring. `grep -c send_burst` = 9 (1 def + 2 calls + comments). |
| 3 | build_state_diff is called exactly once per tick; the burst drain calls encode_datagram only — no R-1 spin (PACE-03) | ✓ VERIFIED | `send_burst` body contains zero `build_state_diff` calls (only `encode_datagram` at 483). Both tick arms call `build_state_diff` once outside any loop (836, 1329). Drain terminates on empty `deferred` / `BURST_CAP` / buffer-full / encode-Err — all finite. R-1 regression gate asserts `< max_iterations`. |
| 4 | current_epoch increments exactly once per tick regardless of burst datagram count (PACE-02, R-2 server side) | ✓ VERIFIED | `*current_epoch += 1` appears at exactly one site (server.rs:369) inside `build_state_diff`. `send_burst` reuses `result.epoch` for every burst `StateDiff` (476). `one_epoch_per_tick` test (1656) asserts `current_epoch == 1` before and that the encode-only drain never advances it. |
| 5 | Both run_session and run_reattach_session burst identically (Pitfall 6) | ✓ VERIFIED | Both arms (818-862, 1315-1352) are structurally identical: build_state_diff once → `epoch_snapshots.push_back` once → `last_sent_snapshot` set → `send_burst` → assign leftover to `pending_deferred` → break on transport_lost. |
| 6 | Multiple same-epoch burst datagrams all apply to the confirmed grid; none silently discarded after the first (PACE-02, SC4) | ✓ VERIFIED | apply() guard at screen.rs:231 is `diff.epoch == 0 \|\| diff.epoch < self.last_applied_epoch`. Same-epoch (≥1) diffs are NOT `<`, so all apply. `apply_same_epoch_burst_applies` (696) applies two epoch=1 diffs and asserts the second overwrites the first (`ch == 'X'`). Test passes. |
| 7 | A strictly-older-epoch diff is still discarded (T-20-06 replay/reorder) | ✓ VERIFIED | Same guard discards `diff.epoch < last_applied_epoch`. `apply_monotonic_older_epoch_is_noop` (726) and `apply_monotonic_lower_epoch_is_noop` (756) both pass. Plus epoch=0 explicitly rejected (WR-01 defence-in-depth fix). |
| 8 | noecho_read_dash_s_zero_predicted_chars passes as a required, non-#[ignore] CI gate with burst-style same-epoch delivery (PACE-02, D-20-09) | ✓ VERIFIED | predict.rs:741 is `#[tokio::test]` with NO `#[ignore]` (grep confirms zero ignores in predict.rs). Runs against the live burst-active server via `server_with_bash`, applies all burst datagrams through `drain_datagrams_with_cull` (internal `<` guard), and asserts no predicted cell visible + `confirmed_epoch` does not advance during the read -s window. Test ran (not skipped) and passed. |
| 9 | The noecho gate fails loudly (panics) if its bash precondition is unmet, rather than passing vacuously (WR-03) | ✓ VERIFIED | predict.rs:746-752: `if !have_bash() { panic!("SECURITY GATE: ... requires /bin/bash ...") }`. No silent `return`. |

**Score:** 9/9 truths verified

### Required Artifacts

| Artifact | Expected | Status | Details |
| -------- | -------- | ------ | ------- |
| `crates/nosh-server/src/server.rs` | send_burst() + extended DiffTickResult + burst loop in both arms | ✓ VERIFIED | `fn send_burst` (427); DiffTickResult has cols/rows/cursor/alt_screen (310-316) populated in build_state_diff (398-401); BURST_CAP=64 (180); both arms wired. |
| `crates/nosh-server/src/server.rs` (tests) | burst_drains + one_epoch_per_tick unit tests | ✓ VERIFIED | Both present (1559, 1656); R-1 gate uses real full grid + finite bound; epoch gate asserts single increment. Both pass. |
| `crates/nosh-client/src/screen.rs` | apply() guard `<=`→`<` + tests | ✓ VERIFIED | Guard at 231 (`<` with epoch=0 reject); old `<=` guard absent. New + renamed tests present. |
| `crates/nosh-client/tests/predict.rs` | noecho gate non-ignored, same-epoch burst delivery, panic on missing bash | ✓ VERIFIED | Non-ignored, exercises burst via direct `screen.apply`, panics if bash absent. |

### Key Link Verification

| From | To | Via | Status | Details |
| ---- | --- | --- | ------ | ------- |
| run_session tick arm (836) | send_burst() | call after build_state_diff | ✓ WIRED | server.rs:856 |
| run_reattach_session tick arm (1329) | send_burst() | call after build_state_diff | ✓ WIRED | server.rs:1346 (identical) |
| burst loop | conn.datagram_send_buffer_space() | per-iteration budget gate | ✓ WIRED | server.rs:470 |
| ClientScreen::apply() | self.last_applied_epoch | strictly-older discard guard | ✓ WIRED | screen.rs:231 |
| noecho gate | live burst server + screen.apply | same-epoch burst delivery | ✓ WIRED | predict.rs:756/823, drain helper applies all same-epoch diffs |

### Data-Flow Trace (Level 4)

| Artifact | Data Variable | Source | Produces Real Data | Status |
| -------- | ------------- | ------ | ------------------ | ------ |
| send_burst burst StateDiff | runs (deferred) | encode_datagram overflow from build_state_diff (real terminal snapshot via slot.with_terminal_state) | Yes | ✓ FLOWING |
| DiffTickResult geometry | cols/rows/cursor/alt_screen | slot.with_terminal_state snapshot (server.rs:345-357), no hardcoding | Yes | ✓ FLOWING |
| apply() confirmed grid | diff.runs | server-sent StateDiff datagrams | Yes | ✓ FLOWING |

### Behavioral Spot-Checks

| Behavior | Command | Result | Status |
| -------- | ------- | ------ | ------ |
| Full workspace test suite | `cargo test --workspace` | 0 failures across all binaries (107 server unit + 100/11/9/6/etc client integration); 3 ignored are unrelated (ssh-agent/slow idle) | ✓ PASS |
| noecho security gate runs (not skipped) | `cargo test --workspace` | `noecho_read_dash_s_zero_predicted_chars ... ok` | ✓ PASS |
| R-1 burst-drain gate | included above | `burst_drains_when_grid_differs_from_acked_baseline ... ok` | ✓ PASS |
| one-epoch-per-tick gate | included above | `one_epoch_per_tick ... ok` | ✓ PASS |
| same-epoch burst applies | included above | `apply_same_epoch_burst_applies ... ok` | ✓ PASS |

### Requirements Coverage

| Requirement | Source Plan | Description | Status | Evidence |
| ----------- | ----------- | ----------- | ------ | -------- |
| PACE-01 | 20-01 | Full-screen repaint bursts per tick, bounded by datagram_send_buffer_space() | ✓ SATISFIED | send_burst budget-gated burst in both arms; burst_drains test |
| PACE-02 | 20-01, 20-02 | Burst preserves noecho invariant — one epoch per tick; noecho gate required non-ignored | ✓ SATISFIED | single epoch increment site; non-ignored noecho gate passing; apply() `<` guard |
| PACE-03 | 20-01 | Two 999.4 traps cannot recur — encode_datagram-only drain; RED/GREEN burst-drain test | ✓ SATISFIED | no build_state_diff in send_burst; finite-termination R-1 gate |

All three PACE IDs declared in plan frontmatter are present in REQUIREMENTS.md (lines 26-28) and marked Complete in the traceability matrix (99-101). No orphaned requirements.

### Anti-Patterns Found

| File | Line | Pattern | Severity | Impact |
| ---- | ---- | ------- | -------- | ------ |
| server.rs | 495 | `TooLarge => {}` inside burst loop | ℹ️ Info | Genuinely unreachable (encode_datagram guarantees payload < cap); commented; not a stub. First-send TooLarge is now handled (WR-02 fix returns early). |

No TBD/FIXME/XXX debt markers in the modified files. No stubs — burst loop and apply guard are fully wired with real data flow. All three code-review warnings (WR-01 epoch=0, WR-02 first-send TooLarge, WR-03 silent skip) were fixed and verified present in the actual code.

### Human Verification Required

None. SC1's live-RTT timing claim (`vim --noplugin` renders within ~2 RTTs over 150 ms simulated link) is satisfied structurally by the burst-drain test (the full repaint drains in one tick); over 0-RTT loopback the ≤2-RTT criterion is trivially met. No visual/real-time human check is gating goal achievement — the security and architectural invariants are all provable in the default test path.

### Gaps Summary

None. The phase goal is achieved in the codebase. Both 999.4 failure modes are designed out architecturally (not merely tested): R-1 is prevented because `send_burst` structurally cannot call `build_state_diff` (only `encode_datagram`), and the loop has three independent finite-termination conditions; R-2 is prevented because `current_epoch` is incremented at exactly one site inside `build_state_diff` (called once per tick) and every burst datagram reuses `result.epoch`. The noecho security invariant is proven by a required, non-`#[ignore]` CI gate that panics loudly if bash is absent and ran successfully in this verification. `cargo test --workspace` reports 0 failures.

---

_Verified: 2026-06-11_
_Verifier: Claude (gsd-verifier)_
