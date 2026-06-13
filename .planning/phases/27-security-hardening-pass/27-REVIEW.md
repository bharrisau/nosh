---
phase: 27-security-hardening-pass
reviewed: 2026-06-14T00:00:00Z
depth: deep
files_reviewed: 11
files_reviewed_list:
  - crates/nosh-client/src/main.rs
  - crates/nosh-client/src/client.rs
  - crates/nosh-server/src/server.rs
  - crates/nosh-server/src/terminal.rs
  - crates/nosh-server/src/registry.rs
  - crates/nosh-proto/src/messages.rs
  - crates/nosh-proto/src/codec.rs
  - .github/workflows/ci.yml
  - fuzz/fuzz_targets/osc_accumulation.rs
  - fuzz/Cargo.lock
  - docs/SECURITY.md
findings:
  critical: 1
  warning: 3
  info: 5
  total: 9
status: issues_found
---

# Phase 27: Code Review Report — Security Hardening Pass

**Reviewed:** 2026-06-14
**Depth:** deep (cross-file call-chain tracing, server + client + protocol boundary analysis)
**Files Reviewed:** 11
**Status:** issues_found — 1 BLOCKER, 3 WARNING, 5 INFO

## Summary

Phase 27 is a security-hardening pass carrying three security-change requests (SEC-01 doc, SEC-04 client trust-boundary hardening, SEC-05 OSC-OOM CI gate). Nine of the ten security mitigation items in SEC-04 (D-03 through D-07) are correctly implemented with pinned regression tests. The fuzz invocation and CI gate for SEC-05 are correct.

**One critical gap:** the server-issued resize rate-limit (SC#3, D-06 item) is NOT implemented. The constant `MIN_RESIZE_INTERVAL_MS` (main.rs:53) exists but is dead code — never consumed by any runtime path. Three of the twelve Sec-04 tests are false-confidence "constant exists" assertions that do not verify runtime enforcement. The resize rate-limit gap is the most significant finding; the other mitigations (title strip, clipboard whitelist, OSC52 read reject, PtyData cap, OSC 8 scheme whitelist) are correctly wired.

---

## Critical Issues

### CR-01: Server-issued Resize rate-limit missing (SC#3 unmet) — constant `MIN_RESIZE_INTERVAL_MS` is dead code

**File:** `crates/nosh-client/src/main.rs:53`
**Issue:** The constant `MIN_RESIZE_INTERVAL_MS` (300ms) is defined in the `run_pump` scope but NEVER consumed by any runtime code path. Search confirms zero references to `MIN_RESIZE_INTERVAL_MS` outside its definition (line 53) and the false-confidence test in `client.rs:935`.

The existing `RESIZE_DEBOUNCE` (40ms, line 48) and `ResizeWatcher` ONLY throttle CLIENT-initiated resizes (triggered by local SIGWINCH / terminal::size() polling — see the `resize.next_resize()` arm at main.rs:2751-2752 and the `resize_sleep` arm at main.rs:2756-2761). These arms send `Message::Resize` from client to server — they regulate OUTBOUND resize traffic.

**There is NO rate-limit on INBOUND Resize messages.** The control-stream `read_message_ns` arm in `run_pump` (main.rs:2058-2199) dispatches `Message::TerminalControl` variants but has ZERO handling for `Message::Resize` — the `Ok(_) => {}` catch-all at main.rs:2195 silently swallows any server-issued Resize frame. On the server side, the `Message::Resize` handler (server.rs:1022-1029 and 1924-1928) is the RECEIPT side for client-initiated resizes — it does not SEND Resize frames (the server never issues Resize messages today).

However, the architecture document SC#3 (D-06) requires: *"the client enforces a server-issued resize rate-limit"* — meaning the client must rate-limit ANY Resize frames received from the server on the control stream, regardless of current server behaviour. A malicious or compromised server COULD send Resize frames, and there is zero defense. The test `resize_rate_limit_constant_exists` (client.rs:932-938) is a **false-confidence test**: it creates a local constant `MIN_RESIZE_INTERVAL_MS: u64 = 300` inside the test function and asserts `300 >= 100` — this proves only that the constant can be defined, not that it is used in runtime enforcement.

**Exploitation scenario:** A malicious server floods the client with Resize frames on the control stream. Each frame would cause `screen.set_size()` / grid reallocation (if Resize were handled; currently silently dropped, but once wired, unbounded allocation). Even benign Resize frames without a rate-limit could cause excessive terminal re-renders.

**Fix:** Two parts required.

**Part 1 — client-side enforcement:** Add a `Message::Resize` arm to the control-stream select! block in `run_pump` that enforces `MIN_RESIZE_INTERVAL_MS` before accepting a server-issued resize:

```rust
// In the control-stream select! arm (main.rs:2058+), add a dedicated Resize arm:
Ok(Message::Resize { cols, rows }) => {
    // D-06: enforce MIN_RESIZE_INTERVAL_MS on server-issued Resize frames
    // (SC#3: client rate-limits inbound resize from a malicious server)
    let now = Instant::now();
    if let Some(last_resize) = last_server_resize {
        if now.duration_since(last_resize) < Duration::from_millis(MIN_RESIZE_INTERVAL_MS) {
            tracing::warn!(
                cols, rows,
                "server-issued Resize rate-limited; dropping frame within MIN_RESIZE_INTERVAL_MS"
            );
            continue;
        }
    }
    last_server_resize = Some(now);
    // Apply the resize to the local screen
    screen.set_size(cols, rows);
    // ... re-render as needed
}
```

This requires tracking `last_server_resize: Option<Instant>` in `run_pump`'s local state (alongside `resize_deadline`).

**Part 2 — fix the false-confidence test:** Replace the existing `resize_rate_limit_constant_exists` test (client.rs:932-938) with a test that verifies runtime enforcement — e.g., send two Resize frames within 300ms and assert the second is dropped; send after 301ms and assert it is applied.

**Part 3 — confirm `Message::Resize` is not silently ignored:** The current `Ok(_) => {}` catch-all at main.rs:2195 silently drops Resize frames. This needs to become either the rate-limited handler above or an explicit ignore-with-trace arm. If Resize from server is intentionally out-of-scope, document why the client does not handle it and ensure the catch-all explicitly traces unexpected frame variants rather than silently discarding them.

---

## Warnings

### WR-01: `ptydata_cap_exists` test is false-confidence (constant exists but runtime enforcement not verified)

**File:** `crates/nosh-client/src/client.rs:922-929`
**Issue:** The test `ptydata_cap_exists` defines a local constant `MAX_PTYDATA_FRAME_BYTES: usize = 1_048_576` and asserts `MAX_PTYDATA_FRAME_BYTES <= 1_048_576`. This only proves the constant can be defined — it does NOT verify that the runtime enforcement path (main.rs:2066-2072) actually triggers when a frame exceeds the cap. A future refactor that removes or weakens the `if data.len() > MAX_PTYDATA_FRAME_BYTES` check would NOT cause this test to fail.

**Fix:** Replace with a test that constructs a `Message::PtyData` with a `data` field exceeding the cap and verifies the `TransportDrop` outcome (or an integration-level test that sends an oversized PtyData frame and asserts the connection is dropped with the correct tracing event). The test must exercise the enforcement path, not just the constant's existence.

### WR-02: `channel_id_parity_odd_is_invalid` test verifies constant values, not runtime validation

**File:** `crates/nosh-client/src/client.rs:900-911`
**Issue:** The test asserts that hardcoded odd channel IDs (1, 3, 5, 999, 1_000_000_001) are indeed odd (`assert_ne!(id % 2, 0, ...)`). This verifies Rust arithmetic on literals, not that `await_channel_accept` (client.rs:795-802) actually rejects odd IDs passed as `expected_id`. The runtime validation IS present in the function body — but the test never calls the function, so a future edit that removes the parity check would not fail this test.

**Fix:** Replace with a test that calls `await_channel_accept` with an odd `expected_id` and asserts it returns an error (bailing with the "odd" message). This requires a mock `NoshRecvStream` or an in-memory channel pair. If that is impractical at the unit-test layer, add a comment noting that the runtime enforcement is verified by the integration test `channel_simultaneous_open` (which exercises odd-ID rejection via the mux layer).

### WR-03: PtyData 1 MiB inner cap enforced POST-allocation — defense-in-depth mispositioned

**File:** `crates/nosh-client/src/main.rs:2066-2072`
**Issue:** The check `if data.len() > MAX_PTYDATA_FRAME_BYTES` runs AFTER `read_message_ns` has already allocated a full `Vec<u8>` containing the frame body (up to 16 MiB via `MAX_FRAME_LEN` in codec.rs:14). The inner cap prevents the client from *processing* an oversized PtyData frame but does NOT prevent memory allocation of up to 16 MiB per malicious frame. A server flooding oversized PtyData frames can cause the client to allocate 16 MiB repeatedly (bounded by `read_message_ns`'s `MAX_FRAME_LEN` enforcement, which is the true outer bound).

This is defense-in-depth rather than a primary cap — the 16 MiB `MAX_FRAME_LEN` is the true allocation bound. The 1 MiB cap triggers a `TransportDrop` which tears down the connection, so sustained flooding is limited to one 16 MiB allocation per reconnect. The risk is low but the documentation (main.rs:55-60) implies the inner cap prevents buffering, when in fact it prevents processing AFTER buffering.

**Fix:** Either (a) accept this as defense-in-depth and update the doc comment to clarify that allocation is bounded by `MAX_FRAME_LEN` (16 MiB) and the inner cap is a transport-integrity check that drops the connection on violation, or (b) add a streaming length check in the `read_message_ns` code path that rejects frames exceeding a tighter bound BEFORE allocating the full body. Option (a) is sufficient given `MAX_FRAME_LEN` already provides the hard allocation cap.

---

## Info

### IN-01: D-05 title CR/LF strip correctly implemented with pinned tests

**Files:**
- `crates/nosh-client/src/main.rs:2160-2163` (runtime enforcement — strips `\x07`, `\x1b`, `\r`, `\n`)
- `crates/nosh-client/src/main.rs:2872-2890` (`title_with_cr_is_filtered` — asserts `\r` stripped)
- `crates/nosh-client/src/main.rs:2892-2902` (`title_with_lf_is_filtered` — asserts `\n` stripped)
- `crates/nosh-client/src/main.rs:2904-2915` (`title_with_esc_is_filtered` — asserts `\x1b`/ANSI stripped)

All four dangerous bytes are stripped. Tests would fail if any filter were removed. The tests pin the exact stripping behavior: they construct a title with the dangerous byte, run through `emit_title_sequence`, extract the OSC sequence, and assert the dangerous byte is absent. Would fail if reverted.

### IN-02: D-05 clipboard selection whitelist {c,p,s,q,0-9} correctly enforced with pinned tests

**Files:**
- `crates/nosh-client/src/main.rs:2129-2133` (runtime whitelist check)
- `crates/nosh-client/src/main.rs:2917-2932` (`clipboard_with_invalid_selection_is_dropped` — asserts X/invalid/a/z/abc dropped)
- `crates/nosh-client/src/main.rs:2936-2953` (`clipboard_with_allowed_selection_passes` — asserts c/p/s/q/0/9 pass)

Whitelist enforced BEFORE re-emit (`continue` at line 2136 drops non-matching selections). Tests would fail if the whitelist were removed. The `is_allowed_selection` helper (line 2864-2869) is a faithful copy of the runtime logic — verified. 

### IN-03: D-03 OSC52 read reject correctly drops `?` form (both server and client)

**Files:**
- `crates/nosh-server/src/terminal.rs:1141-1143` (server-side: `if data == b"?" { return; }`)
- `crates/nosh-client/src/main.rs:2121-2124` (client-side defense-in-depth: `if data == b"?" { continue; }`)
- `crates/nosh-client/src/main.rs:2955-2965` (`clipboard_read_form_is_dropped` test)

Both server and client drop the `?` read form. The test sends `b"?"` as data for all three valid selection prefixes (c, p, s) and asserts `None` (dropped). Would fail if the check were removed.

### IN-04: D-04 DCS/PM/APC sequences genuinely no-op — no unbounded accumulation possible

**File:** `crates/nosh-server/src/terminal.rs:1226-1227`
**Finding:** The vte `Perform` trait's `hook`/`put`/`unhook` methods are the trait defaults (no-ops). The `TerminalState` struct has no custom fields for DCS accumulation. DCS sequences (device control strings, such as sixel, PM, APC) are parsed by vte but the content is never stored — the parser transitions through DCS states and silently discards all bytes. This is a permanent scope fence (D-12-02b). No unbounded accumulation is possible. The comment at line 1226-1227 explicitly documents this as intentional.

**Verdict:** Correct. No test needed — the Rust type system enforces this at compile time (the trait provides default no-op impls for `hook`/`put`/`unhook`).

### IN-05: D-07 OSC 8 Hyperlink scheme whitelist correctly implemented at both boundaries

**Files:**
- `crates/nosh-server/src/terminal.rs:1168-1174` (server boundary: whitelist http://, https://, mailto:, file:)
- `crates/nosh-client/src/main.rs:2179-2184` (client defense-in-depth: same whitelist)
- `crates/nosh-proto/src/messages.rs:458-461` (Hyperlink variant is append-only after Title)
- `crates/nosh-proto/src/codec.rs:348-367` (discriminant stability test — Hyperlink = 2)
- `crates/nosh-server/src/server.rs:872,1804` (both session pump paths drain and forward Hyperlink)
- `crates/nosh-server/src/registry.rs:562-564` (`drain_terminal_control` returns 3-tuple including Hyperlink)

**Whitelist enforcement at server:** `uri.to_lowercase()` ensures case-insensitive matching. `starts_with` on each allowed scheme handles both `file:` (no `//`) and `file:///` (with `//`) since `file:` is a prefix of `file:///`. Non-whitelisted schemes are silently dropped — never stored in `osc8_hyperlink_pending`.

**Whitelist enforcement at client:** Identical whitelist, also case-insensitive. Escape bytes (`\x07`, `\x1b`, `\r`, `\n`) are stripped before scheme check.

**Tests:**
- `hyperlink_with_whitelisted_scheme_passes` (main.rs:3008) — http, https, mailto, file pass
- `hyperlink_with_dangerous_scheme_is_dropped` (main.rs:3026) — javascript:, data:, vbscript: dropped
- `hyperlink_with_escape_bytes_is_sanitized` (main.rs:3049) — ESC, BEL, CR, LF stripped
- `hyperlink_scheme_is_case_insensitive` (main.rs:3079) — HTTP://, HtTp:/, etc. pass

All tests would fail if any filter were reverted. Postcard wire compatibility is verified via the discriminant stability test in codec.rs.

### IN-06: SEC-05 CI gate is a real job running the bound test

**File:** `.github/workflows/ci.yml:64-77`
**Finding:** The `osc-bound-regression` job runs `cargo test --locked -p nosh-server oversized_multi_chunk_osc_is_bounded_then_resyncs`. This is a correctly named, real test that exercises the OSC accumulation pre-bound. Runs on every push/PR. Would fail if the OSC accumulation bound were weakened or removed.

**File:** `fuzz/fuzz_targets/osc_accumulation.rs:5-9`
**Finding:** The fuzz header documents the correct invocation: `cargo +nightly fuzz run osc_accumulation -- -max_len=2097152 -max_total_time=120`. The `-max_len=` flag is correctly placed AFTER `--` (directly to libFuzzer). The comment warns that `LIBFUZZER_MAX_LEN` env var is silently ignored by cargo-fuzz — this is correct guidance.

### IN-07: Security invariants intact — no SSH_AUTH_SOCK in env, env sanitization enforced, no secrets logged

**Files:**
- `crates/nosh-server/src/session.rs:31-40` (ENV_DENY_DOC includes SSH_AUTH_SOCK, LD_*, BASH_ENV, etc.)
- `crates/nosh-server/src/session.rs:82` (whitelist gate: exact TERM/LANG/TZ + LC_* prefix only)
- `crates/nosh-server/src/session.rs:88-113` (child env built deny-by-default from minimal baseline)
- `crates/nosh-client/src/client.rs:506-537` (collect_client_env only sends TERM, LANG, TZ, LC_*)
- `crates/nosh-server/src/channel.rs:1097` (PortForward/AgentForward rejected: "SSH_AUTH_SOCK never reachable")

**Verdict:** SSH_AUTH_SOCK is excluded from client env collection and denied at the server boundary. No token or secret bytes appear in `{:?}` / `format!` calls outside test assertions. The `variant_name()` pattern is consistently used at all log/dispatch sites for token-bearing variants (SessionOpened, Reattach, ReattachOk, InnerAuthChallenge, InnerAuthResponse, InnerAuthComplete). Confirmed via grep: zero token-leaking format strings in production code.

---

_Reviewed: 2026-06-14_
_Reviewer: Claude (gsd-code-reviewer)_
_Depth: deep_
