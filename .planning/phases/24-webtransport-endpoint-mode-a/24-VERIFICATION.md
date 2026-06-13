---
phase: 24-webtransport-endpoint-mode-a
verified: 2026-06-13T16:10:00Z
status: passed
score: 5/5 must-haves verified
overrides_applied: 0
re_verification:
  previous_status: none
human_verification: []
---

# Phase 24: WebTransport Endpoint + Mode A Verification Report

**Phase Goal:** A nosh server listens on UDP/443 as a direct WebTransport-over-HTTP/3 endpoint (Mode A, no proxy) and carries a fully interactive shell (datagram state-sync, predictive echo, reliable control/scrollback) via the Phase 23 transport trait, with downgrade protection. Inner SSH-key auth is deferred to Phase 25; a test-only auth stub gated behind `test-support` stands in, and release builds reject unauthenticated connections.

**Verified:** 2026-06-13T16:10:00Z
**Status:** passed
**Re-verification:** No — initial verification

## Goal Achievement

### Observable Truths (ROADMAP Success Criteria)

| # | Truth | Status | Evidence |
| --- | --- | --- | --- |
| SC#1 | `--mode webtransport` server ↔ `--webtransport` client delivers a live interactive shell (keystrokes/output/resize) | ✓ VERIFIED | `wt01_live_shell_over_webtransport` PASSES; real assertions on `IS_TTY` (`test -t 0`), `hello-nosh`, and `40 132` (`stty size` resize round-trip), exit code 0. Test ran clean (`webtransport.rs:90-102`). Client `--webtransport` path (`nosh-client/src/main.rs:1354-1402`) feeds `connect_wt` into `fresh_session`/`reattach_session` (same generic pump). Server `make_wt_endpoint` + `run_wt_accept_loop` (`nosh-server/src/main.rs:184-185`). |
| SC#2 | Datagram state-sync + predictive echo + scrollback function identically over WT as native QUIC | ✓ VERIFIED | `wt02_datagram_sync_over_webtransport` PASSES; asserts `max_datagram_size().is_some()` (D-03) and loops `read_datagram()` until a non-empty `StateDiff` with `epoch >= 1` arrives (`webtransport.rs:139-176`). Pump is reused unchanged via the Phase 23 `NoshTransport` trait — same `fresh_session`/`run_session` code as native QUIC. |
| SC#3 | `--mode webtransport` server rejects raw-QUIC (downgrade protection) | ✓ VERIFIED | `wt03_raw_quic_downgrade_rejected` PASSES; native quinn client dialing the WT endpoint either errors or reaches no session; the test panics with "WT-05 VIOLATED" if a session is obtained (`webtransport.rs:248-253`). Structurally, `run_wt_accept_loop` only creates a `wtransport::Endpoint<Server>`; no quinn endpoint exists in WT mode (`nosh-server/src/main.rs:155-185`, one-transport-per-process D-07). |
| SC#4 | `wtransport` ring-only, no aws-lc-rs | ✓ VERIFIED | `cargo tree -p nosh-server --features webtransport -e features \| grep -i aws-lc` → EMPTY (exit 1). `ring v0.17.14` present. Workspace `Cargo.toml:35`: `wtransport = { version = "0.7.1", default-features = false, features = ["self-signed", "ring", "quinn"] }`. |
| SC#5 | Datagram MTU uses `wtransport::Connection::max_datagram_size()` (capsule-adjusted), NOT `quic_connection().max_datagram_size()` | ✓ VERIFIED | Both wrappers call `self.0.max_datagram_size()` directly (`nosh-server/src/wt_transport.rs:92-94`, `nosh-client/src/wt_transport.rs:87-91`). `quic_connection()` is used only for `datagram_send_buffer_space` and `rtt`/`close_reason` — never for datagram sizing. Confirmed by reading both files. |

**Score:** 5/5 truths verified

### Required Artifacts

| Artifact | Expected | Status | Details |
| --- | --- | --- | --- |
| `crates/nosh-server/src/wt_transport.rs` | Server WT wrapper + accept loop + outer TLS + auth gate | ✓ VERIFIED | All 6 API adaptations present; CR-01 fix landed (permit dropped before session); release auth-gate rejects. |
| `crates/nosh-client/src/wt_transport.rs` | Client WT wrapper + `connect_wt` | ✓ VERIFIED | Wrapper + `build_wt_client_config` (native certs, Mode A) + `connect_wt` returning `Box<dyn NoshTransport>`. |
| `crates/nosh-server/src/main.rs` | `--mode native\|webtransport` flag | ✓ VERIFIED | `TransportMode` enum; webtransport arm requires `--cert`/`--key`; native arm never touched in WT mode. |
| `crates/nosh-client/src/main.rs` | `--webtransport` flag → generic pump | ✓ VERIFIED | `--webtransport` connects via `connect_wt`, feeds `fresh_session`/`reattach_session` with `&*conn`. |
| `crates/nosh-proto/src/transport_trait.rs` | Phase 23 seam | ✓ VERIFIED | `NoshTransport`/`NoshSendStream`/`NoshRecvStream` traits (lines 67/155/196); WT wrappers implement them. |
| `crates/nosh-client/tests/webtransport.rs` | E2E win-condition tests | ✓ VERIFIED | 3 tests, all PASS, all with substantive (non-vacuous) assertions. |
| `Cargo.toml` (workspace) | wtransport + time pin (D-01/D-02) | ✓ VERIFIED | wtransport 0.7.1 ring-only; `time = "=0.3.47"` pinned; lockfile shows `time 0.3.47`. |

### Key Link Verification

| From | To | Via | Status | Details |
| --- | --- | --- | --- | --- |
| client `--webtransport` | session pump | `connect_wt` → `fresh_session`/`reattach_session` | WIRED | `main.rs:1359,1376,1390` |
| server `--mode webtransport` | WT listener | `make_wt_endpoint` → `run_wt_accept_loop` | WIRED | `main.rs:184-185` |
| WT connection | session pump | `handle_connection_wt` → `run_session`/`run_reattach_session` (test-support) | WIRED (gated) | `wt_transport.rs:431-455` |
| outer TLS | wtransport server config | `build_ca_cert_rustls_config` → `with_custom_tls` | WIRED | `wt_transport.rs:217-268` |

### Security: Auth-Bypass Containment (highest severity)

| Check | Status | Evidence |
| --- | --- | --- |
| Bypass gated `#[cfg(any(test, feature = "test-support"))]` | ✓ VERIFIED | `wt_transport.rs:401-404` — `skip_inner_auth` is a compile-time const, not a runtime branch. |
| Release build (no test-support) reaches NO session without inner auth | ✓ VERIFIED | `if !skip_inner_auth` (line 406) executes `conn.close(1, ...)` + `return Ok(())` BEFORE `accept_bi()` (line 423). `cargo build -p nosh-server --features webtransport` (no test-support) compiles clean — bypass block excluded. |
| test-support is opt-in, not a default feature | ✓ VERIFIED | No `default =` line in `nosh-server/Cargo.toml`; `test-support = []` is opt-in. Client pulls it only as a dev-dep (`nosh-client/Cargo.toml:60`). |

### CR-01 Fix (DoS semaphore)

| Check | Status | Evidence |
| --- | --- | --- |
| Permit dropped after handshake / before `handle_connection_wt` | ✓ VERIFIED | `wt_transport.rs:361` `drop(permit)` sits after `session_request.accept().await` (line 353) and before `handle_connection_wt` (line 366). No `let _permit = permit;` holding it for the session. Matches native `server.rs:541` `drop(permit)` inside `handle_connection` after auth. |

### Adapter Correctness

| Check | Status | Evidence |
| --- | --- | --- |
| `finish()` awaited in WT send wrapper | ✓ VERIFIED | `wt_transport.rs:159/168` `self.0.finish().await?` (both server & client). |
| `max_datagram_size()` not quinn raw passthrough | ✓ VERIFIED | `self.0.max_datagram_size()`, not `quic_connection().max_datagram_size()`. |
| `SendDatagramError` 3-variant map | ✓ VERIFIED | TooLarge / UnsupportedByPeer / NotConnected→ConnectionLost (server `:71-81`, client `:69-79`). |
| `datagram_send_buffer_space` via `quic_connection()` | ✓ VERIFIED | `self.0.quic_connection().datagram_send_buffer_space()` (server `:86`, client `:84`). |
| `open_bi` double-await | ✓ VERIFIED | `self.0.open_bi().await?.await?` (server `:113`, client `:115`). |

### Behavioral Spot-Checks / Probe Execution

| Behavior | Command | Result | Status |
| --- | --- | --- | --- |
| WT win-condition (3 tests) | `cargo test -p nosh-client --features webtransport --test webtransport` | 3 passed; 0 failed | ✓ PASS |
| No aws-lc-rs (SC#4) | `cargo tree -p nosh-server --features webtransport -e features \| grep -i aws-lc` | empty (exit 1) | ✓ PASS |
| Release WT build (no test-support) compiles | `cargo build -p nosh-server --features webtransport` | Finished, exit 0 | ✓ PASS |
| Default-feature workspace unaffected | `cargo test --workspace` | all suites pass; 0 failures | ✓ PASS |
| time pin honored | `grep time Cargo.lock` | `version = "0.3.47"` | ✓ PASS |

### Requirements Coverage

| Requirement | Source Plan | Description | Status | Evidence |
| --- | --- | --- | --- | --- |
| WT-02 | 24-03 | Server in direct WebTransport mode binds own listener on UDP/443, outer TLS from cert/key | ✓ SATISFIED | `make_wt_endpoint`/`run_wt_accept_loop`, `--cert`/`--key` required in WT mode; outer rustls cfg via `build_ca_cert_rustls_config`. (REQUIREMENTS.md still shows WT-02 "Pending"/unchecked — see Gaps note; implementation is present and verified.) |
| WT-03 | 24-04/24-05 | Client connects to WT endpoint → fully interactive shell with datagram sync, predictive echo, control/scrollback | ✓ SATISFIED | wt01 + wt02 PASS; generic pump reused. |
| WT-05 | 24-03/24-04 | Explicit CLI transport selection; WT-only server rejects raw-QUIC | ✓ SATISFIED | `--mode`/`--webtransport` flags; wt03 PASS; structural no-quinn-endpoint guarantee. |

### Anti-Patterns Found

| File | Line | Pattern | Severity | Impact |
| --- | --- | --- | --- | --- |
| `nosh-server/src/wt_transport.rs` | 409 | `tracing::warn!("...inner auth not yet implemented...")` | ℹ️ Info | Intentional release-build rejection log on the Phase-25 boundary (inner auth is explicitly out of scope this phase). References Phase 25 as formal follow-up. Not a `TBD`/`FIXME`/`XXX` debt marker; not a blocker. |

No `TBD`/`FIXME`/`XXX`/`HACK`/`PLACEHOLDER` markers in the WT source or test files.

### Human Verification Required

None. The win condition is fully covered by the automated in-process E2E test (live PTY shell, TTY detection, resize round-trip, datagram StateDiff, raw-QUIC downgrade rejection). A real cross-network/cross-device interactive confirmation is a natural fit for the Phase 28 interactive UAT bundle but is not required to confirm this phase's goal — the goal is observably met in the codebase and tests.

### Gaps Summary

No gaps blocking the phase goal. All 5 ROADMAP success criteria are verified against the codebase and passing tests; WT-02/03/05 are all satisfied; the test-support auth bypass is provably release-unreachable (compile-time `#[cfg]` gate, opt-in feature, dev-dep only); and CR-01 (the sole review blocker) is fixed and matches the native accept-loop permit-release contract.

Minor bookkeeping note (not a gap): `.planning/REQUIREMENTS.md` still lists WT-02 as unchecked/"Pending" in its status table, while WT-03/WT-05 are marked Complete. The WT-02 implementation is present and verified; the tracking checkbox/table should be updated to Complete as a documentation follow-up. This does not affect goal achievement.

---

_Verified: 2026-06-13T16:10:00Z_
_Verifier: Claude (gsd-verifier)_
