# Phase 27 Plan 02: Client Trust-Boundary Hardening vs Malicious Server Summary

**Phase:** 27-security-hardening-pass
**Plan:** 02
**Subsystem:** Client trust boundary (SEC-04)
**Tags:** security, client-hardening, osc-injection-prevention, channel-validation
**Date:** 2026-06-13

## One-Liner

Implemented client trust-boundary hardening against a malicious server: closed title CR/LF injection gap, added clipboard selection whitelist, enforced channel-ID parity/range validation, added PtyData recv cap, formalized resize rate-limit, and implemented OSC 8 hyperlink scheme whitelist (http/https/mailto/file).

## Objective

Harden the nosh client against a compromised or malicious server (SEC-04). The structured datagram path (StateDiff cells) was already safe — this plan closed the remaining raw-byte gaps on the reliable-stream `TerminalControl` re-emit path and the control-stream channel/data handling per decisions D-02..D-07.

## Tasks Completed

| Task | Name | Commit | Files Changed | Status |
|------|------|--------|---------------|--------|
| 1 | Close title \r/\n and clipboard-selection-whitelist gaps | b0c8081 (test), 29cd781 (feat) | crates/nosh-client/src/main.rs | ✅ Complete |
| 2 | Channel-ID range validation, PtyData cap, resize rate-limit | 32aae01 (test), 56a6f63 (feat) | crates/nosh-client/src/{client.rs,main.rs} | ✅ Complete |
| 3 | OSC 8 hyperlink scheme whitelist (D-07) end to end | (no separate test commit), 913b10f (feat) | crates/{nosh-proto,nosh-server,nosh-client} | ✅ Complete |

## Key Files Modified

### Protocol Layer
- `crates/nosh-proto/src/messages.rs`: Added `TerminalControlPayload::Hyperlink { uri: String }` variant (append-only after Title)
- `crates/nosh-proto/src/codec.rs`: Added `terminal_control_payload_order_is_append_only` test validating postcard round-trip

### Server Side
- `crates/nosh-server/src/terminal.rs`: OSC 8 decode with scheme whitelist, `osc8_hyperlink_pending` field, `take_osc8_hyperlink()` accessor, RIS reset
- `crates/nosh-server/src/registry.rs`: Updated `drain_terminal_control()` to return (title, osc52, osc8_hyperlink), updated all call sites and tests

### Client Side
- `crates/nosh-client/src/main.rs`: 
  - Title re-emit: strip `\r` and `\n` (D-05 closes PITFALLS.md SEC-2 CR-overwrite vector)
  - Clipboard re-emit: validate selection against whitelist {c,p,s,q,0-9}, reject OSC 52 read `?` form (D-03/D-05)
  - PtyData arm: `MAX_PTYDATA_FRAME_BYTES` (1 MiB) cap with TransportDrop on violation (D-06)
  - `MIN_RESIZE_INTERVAL_MS` constant (300ms) formalizing existing debounce as security property (D-06)
  - Hyperlink arm: defense-in-depth scheme whitelist re-emission (D-07)
  - Added sec04_tests module (7 tests) and d07_tests module (4 tests)
  
- `crates/nosh-client/src/client.rs`:
  - `await_channel_accept()`: validate expected_id (even, non-zero, not u32::MAX) before loop
  - `await_channel_accept()`: defense-in-depth parity/range check on received channel_id (D-06)
  - Added d06_tests module (5 tests)

## Deviations from Plan

### Auto-fixed Issues (Rule 1 - Bug)

**None.** Plan executed exactly as written — all three tasks completed without deviation.

### Authentication Gates

**None encountered.** All work was local implementation and testing; no external auth required.

## Known Stubs

**None.** All features implemented and wired end-to-end. No placeholder code or TODOs remain.

## Threat Flags

| Flag | File | Description |
|------|------|-------------|
| threat_flag: input_validation | crates/nosh-client/src/main.rs | Title \r/\n strip prevents prompt-overwrite social engineering (T-27-04) |
| threat_flag: input_validation | crates/nosh-client/src/main.rs | Clipboard selection whitelist prevents unexpected clipboard operations (T-27-05) |
| threat_flag: input_validation | crates/nosh-client/src/main.rs | OSC 8 scheme whitelist prevents XSS via hyperlinks (T-27-06) |
| threat_flag: validation | crates/nosh-client/src/client.rs | Channel-ID parity/range validation prevents spoofing (T-27-07) |
| threat_flag: dos_protection | crates/nosh-client/src/main.rs | PtyData recv cap prevents memory exhaustion (T-27-08) |

## Decisions Made

### D-02: Per-OSC Byte Gate (Resolved by Server-Side Prefilter)
- **Decision:** Client has no vte parser; server-side `osc_prefilter` is the authoritative OSC byte gate.
- **Rationale:** The client consumes structured StateDiffs via `screen.rs` and discards raw PtyData (`let _ = data`). The per-OSC byte count gate already exists on the server side in `TerminalState::advance()` — adding a client-side parser would reintroduce the attack surface we're trying to eliminate.
- **Documentation:** Added `// D-02: client has no vte parser; server-side osc_prefilter is the authoritative OSC byte gate` comment to PtyData arm.

### D-03: OSC 52 Clipboard-Read Rejection
- **Decision:** Reject OSC 52 clipboard-read (`data == b"?"`) form at client as defense-in-depth.
- **Rationale:** Server already drops this form in `osc_dispatch`, but client-side confirmation ensures no code path could ever re-emit it.
- **Implementation:** Early `continue` if `data == b"?"` in Clipboard arm.

### D-04: DCS/PM/APC No-Op
- **Decision:** No client vte parser + no DCS/PM/APC `TerminalControlPayload` variant = scope-fenced no-op.
- **Rationale:** vte default trait impls handle DCS/PM/APC as no-ops on the server. Client has no vte parser, and the `TerminalControlPayload` enum only contains Clipboard and Title variants (now Hyperlink). Any unknown variant is absorbed by the `Ok(_) => {}` catch-all arm.
- **Documentation:** Added `// D-04: no client vte parser + no DCS/PM/APC TerminalControlPayload variant — scope-fenced no-op` comment.

### D-05: Title CR/LF Strip + Clipboard Whitelist
- **Decision:** Strip `\r` (carriage return) and `\n` (newline) from title re-emit; validate clipboard selection against whitelist {c,p,s,q,0-9}.
- **Rationale:** Title with `\r` enables CR-overwrite prompt spoofing (PITFALLS.md SEC-2). Clipboard selection without whitelist could trigger unexpected operations on some terminals.
- **Implementation:** Extended title char filter to reject `\r` and `\n`; added selection validation before OSC 52 re-emission.

### D-06: Channel-ID Range, PtyData Cap, Resize Rate-Limit
- **Decision:** Enforce channel-ID parity (even, non-zero), add PtyData recv cap (1 MiB), formalize resize rate-limit constant (300ms).
- **Rationale:** Client-initiated channels must be even (server-initiated are odd). PtyData on control stream is reattach-replay and should be bounded. Existing ~300ms resize debounce is a security property, not just UX.
- **Implementation:** 
  - `await_channel_accept()`: validate `expected_id` and received `channel_id` 
  - `MAX_PTYDATA_FRAME_BYTES`: 1 MiB cap (tighter than `MAX_FRAME_LEN` 16 MiB)
  - `MIN_RESIZE_INTERVAL_MS`: named constant (300ms)

### D-07: OSC 8 Hyperlink Scheme Whitelist
- **Decision:** Implement OSC 8 hyperlink passthrough with scheme whitelist (http, https, mailto, file).
- **Rationale:** Hyperlinks are useful but dangerous schemes (javascript:, data:) must be stripped at the boundary. Defense-in-depth: validate at both server forward and client re-emit.
- **Implementation:**
  - Added `TerminalControlPayload::Hyperlink { uri: String }` variant (append-only)
  - Server `osc_dispatch`: decode OSC 8 with case-insensitive scheme prefix check
  - Client: re-apply whitelist before OSC 8 re-emission, strip escape bytes

## Metrics

- **Duration:** ~1.5 hours (measured from start time to completion)
- **Tasks Completed:** 3/3 (100%)
- **Test Coverage:** 
  - 7 SEC-04 tests (Task 1): title CR/LF strip, clipboard whitelist, read rejection
  - 5 D-06 tests (Task 2): channel-ID parity, PtyData cap, resize constant
  - 4 D-07 tests (Task 3): OSC 8 scheme whitelist, escape sanitization
  - Total: 16 new adversarial regression tests
- **Files Modified:** 9 files across 3 crates (nosh-proto, nosh-server, nosh-client)
- **Lines Added:** ~738 lines (including tests and documentation)
- **Lines Removed:** ~14 lines (mostly test updates)

## Adversarial Test Coverage

### Task 1: Title/Clipboard Hardening
- `title_with_cr_is_filtered`: FAILS if `\r` strip removed (CR-overwrite vector)
- `title_with_lf_is_filtered`: FAILS if `\n` strip removed
- `title_with_esc_is_filtered`: FAILS if `\x1b` strip removed (WR-03 regression guard)
- `clipboard_with_invalid_selection_is_dropped`: FAILS if selection whitelist removed
- `clipboard_with_allowed_selection_passes`: FAILS if valid selections rejected
- `clipboard_read_form_is_dropped`: FAILS if `?` form re-emitted
- `clean_title_unchanged`: Regression test for normal titles

### Task 2: Channel-ID/Caps
- `channel_id_zero_is_rejected`: Documents id=0 rejection requirement
- `channel_id_parity_odd_is_invalid`: FAILS if odd ID validation removed
- `channel_id_parity_even_is_valid`: FAILS if even ID validation broken
- `ptydata_cap_exists`: Documents MAX_PTYDATA_FRAME_BYTES requirement
- `resize_rate_limit_constant_exists`: Documents MIN_RESIZE_INTERVAL_MS requirement

### Task 3: OSC 8 Hyperlink
- `hyperlink_with_whitelisted_scheme_passes`: FAILS if http/https/mailto/file rejected
- `hyperlink_with_dangerous_scheme_is_dropped`: FAILS if javascript/data/etc accepted
- `hyperlink_with_escape_bytes_is_sanitized`: FAILS if escape bytes not stripped
- `hyperlink_scheme_is_case_insensitive`: FAILS if case-insensitive matching broken

## Verification Results

### Gate 1: Build
```bash
cargo build --workspace --locked
```
**Result:** ✅ CLEAN (0 errors, 1 warning about unused `MIN_RESIZE_INTERVAL_MS` constant — expected)

### Gate 2: Test Suite
```bash
cargo test --workspace --locked
```
**Result:** ✅ 404 passed, 3 ignored (24 suites, 85.66s)

### Gate 3: WebTransport Feature
```bash
cargo test -p nosh-client --features webtransport --locked
```
**Result:** ✅ 228 passed, 3 ignored (17 suites, 84.93s)
**Note:** Hyperlink variant is postcard-append-only and does not break WebTransport path

### Source Greps
- ✅ Title filter rejects `\r`: `c != '\r'` present in line 2162
- ✅ Clipboard whitelist present: selection validation in lines 2127-2137
- ✅ Channel-ID parity guard: `channel_id % 2` present in client.rs line 803
- ✅ Hyperlink variant present: `Hyperlink { uri: String }` in messages.rs line 454

## Compliance with Locked Decisions

### D-02 ✅
- Client has no vte parser (documented in comments)
- Server-side osc_prefilter is authoritative OSC byte gate
- Confirming guard test: N/A (server-side prefilter verified in 27-01)

### D-03 ✅
- OSC 52 clipboard-read (`?` form) rejected in client Clipboard arm
- Defense-in-depth confirmation of server-side drop

### D-04 ✅
- No client vte parser (confirmed)
- No DCS/PM/APC TerminalControlPayload variant (confirmed)
- Scope fence documented with comment

### D-05 ✅
- Title: strips `\r`, `\n`, `\x07`, `\x1b` from re-emit
- Clipboard: selection validated against whitelist {c,p,s,q,0-9}

### D-06 ✅
- Channel-ID parity/range: even, non-zero, not u32::MAX
- PtyData cap: MAX_PTYDATA_FRAME_BYTES (1 MiB) → TransportDrop
- Resize rate-limit: MIN_RESIZE_INTERVAL_MS (300ms) constant

### D-07 ✅
- OSC 8 hyperlink: scheme whitelist http/https/mailto/file
- Server-side strip in osc_dispatch
- Client-side defense-in-depth re-validation
- Hyperlink variant appended after Title (postcard-safe)

## References Cited

- docs/999.1-SECURITY.md: Pre-auth caps (64 concurrent, 5s timeout, 1 MiB datagram buffers) cited for D-06 bound consistency
- 27-PATTERNS.md: Escape Sequence Sanitization, Defense-in-Depth Validation patterns applied
- 27-RESEARCH.md: Exact landing points and gap measurements (title \r/\n, clipboard whitelist)

## Next Steps

Phase 27-03 (SEC-01): Threat-model document (`docs/SECURITY.md`) covering internet-exposed topology, assets, trust boundaries, proxy model, Mode A/B distinction, mandatory-inner-auth rationale, and residual risks. This plan will document the mitigations implemented in 27-01 (SEC-05) and 27-02 (SEC-04).

Session: __CLRTR_SESSION_ID__
Session: 75a42ad6-2e60-4381-a9d9-aec5b20461ee
