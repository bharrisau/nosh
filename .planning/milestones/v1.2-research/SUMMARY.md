# Project Research Summary

**Project:** nosh — QUIC roaming remote shell
**Domain:** v1.2 M4 Predictive Echo + Daily-Driver Readiness
**Researched:** 2026-06-01
**Confidence:** HIGH

## Executive Summary

nosh v1.2 adds the headline capability that makes a roaming shell worth using daily: Mosh-style
speculative local echo over QUIC datagrams. The four research files converge on a clear build
order: fix the PTY reader zombie race first (it is already a latent server reliability bug and
must not accumulate further under M4 orphan-session load), then design the datagram state-sync
wire format before writing a single line of prediction code (the format gates everything else),
then build the server terminal state model and diff encoder, then the client predictor, then the
QoL features on top. All four researchers independently flag the PTY race fix and the datagram
format as blocking prerequisites — this is the strongest cross-cutting signal in the research.

The recommended stack addition is `termwiz 0.23.3` (client- and server-side terminal grid and
`get_changes` diff API) replacing a bespoke grid that would need to be written from scratch. The
existing `postcard` + `serde` discipline is extended to carry `termwiz::surface::change::Change`
payloads in QUIC datagrams — no protobuf, no new serialization crate. `crossterm 0.29.0`'s
`osc52` feature handles clipboard passthrough with zero new dependencies. All other QoL features
(connection-loss banner, terminal title propagation, `--predict` flags) are application logic on
the existing stack.

The hardest single design decision is the prediction epoch model: epoch-reset-on-cursor-move is a
day-one gate, not a refinement. An engine that predicts in cursor-addressing apps (vim, htop,
less) produces visible screen corruption that is worse than no prediction. Conservative fallback
(suppress prediction after any CSI cursor-move or non-printing control key; never display on a
fresh terminal row until the server confirms the first character) must be baked into the initial
design. Likewise, noecho-suppression (do not predict during `stty -echo` prompts) is a security
requirement of the prediction feature itself, not a later hardening step. The security design
document is the last deliverable of the milestone but TOFU prompt UX and noecho-suppression must
be tracked as requirements during implementation, not deferred.

## Key Findings

### Recommended Stack

The v1.1 stack is locked and ships unchanged. The single consequential new dependency is
`termwiz 0.23.3` added to `nosh-proto`, `nosh-server`, and `nosh-client` with
`default-features = false, features = ["use_serde"]`. termwiz provides `Surface` (full terminal
grid), `get_changes(seq)` / `flush_changes_older_than(seq)` (incremental diff log), and
`diff_screens` — mapping Mosh's framebuffer+diff model onto a maintained Rust type rather than
owning ~2000 lines of grid logic. The `Change` enum serializes with postcard directly; no
protobuf and no new serialization crate. All other v1.2 features are zero-new-crate changes:
OSC 52 from the existing `crossterm 0.29.0` `osc52` feature, the PTY race fix from existing
`nix 0.29` + `tokio::io::unix::AsyncFd`, and connection-loss notification from existing
`tokio` + `quinn::Connection::stats()`.

**Core v1.2 additions:**
- `termwiz 0.23.3`: terminal grid + incremental diff API — replaces bespoke grid; `get_changes` is the critical API
- `crossterm 0.29.0` `osc52` feature: OSC 52 clipboard write — already in tree, feature flag only
- `tokio::io::unix::AsyncFd` + `nix` `O_NONBLOCK` + `fcntl`: PTY async read — eliminates zombie race, no new deps
- Bespoke `PredictionEngine` (~250-350 lines): client-side speculative overlay — no Rust crate exists for Mosh SSP

**Do NOT add:**
- `prost`/protobuf — `termwiz::Change` is serde-serializable; postcard is smaller and faster
- `alacritty_terminal` — no diff API, sub-1.0 unstable, not designed for external use
- `tokio_pty_process` — unmaintained since 2019; `AsyncFd` is the current idiomatic approach
- Any 0-RTT reattach mechanism — deliberately deferred per INIT.md; 1-RTT cold reattach already ships

### Expected Features

**Must have (table stakes — M4 done when these pass):**
- Datagram state sync: server sends sparse cell diffs over RFC 9221 datagrams; latest-state-wins
- Predictive echo: printable characters, backspace, left/right arrow cursor motion
- Unconfirmed rendering: underline on speculative cells when RTT > FLAG_TRIGGER_HIGH (~80 ms)
- Prediction epochs: control chars (ESC, Enter, Ctrl-C, up/down arrows) reset epoch; no display until server confirms from new epoch
- Conservative fallback: no prediction on fresh terminal row; no prediction in cursor-addressing apps; no prediction during noecho prompts
- Adaptive mode (default): prediction activates above ~30 ms SRTT, suppressed below ~20 ms; `--predict always/adaptive/never` flags
- Connection-loss banner: overlay row 0 when no datagram for >5 s; elapsed counter; "Press ~. to disconnect" after threshold
- OSC 52 clipboard passthrough: detect in server PTY output, forward on reliable stream, emit at client; write-only
- Terminal title propagation: OSC 0/2 sequences pass through unstripped (policy check, likely no new code)

**Should have (competitive / daily-driver polish):**
- `--predict always/adaptive/never` CLI flags — power-user override; low cost once engine exists
- Terminal title with RTT indicator (`--status`) — OSC 0/2 + SRTT already measured; P3 stretch

**Defer (post-v1.2):**
- Windows client predictive echo — Linux client must be validated first; scoped as stretch goal
- Full native scrollback sync — M5; OSC 52 passthrough covers the main "copy from terminal" case
- Named/numbered session listing — M5; v1.1 auto-reattach covers the solo use case
- Bell/notification passthrough (OSC 9) — M5+ point release; low daily-driver value

**Anti-features (explicitly excluded):**
- Predictive echo for all control sequences / vim commands — epoch reset suppresses naturally; predicting CSI sequences produces screen corruption
- OSC 52 clipboard read (paste remote to local) — security hole; most terminals disable it
- tmux integration — excluded per PROJECT.md; conflicts with native scrollback story

### Architecture Approach

v1.2 adds two layers to the v1.1 session substrate without replacing any existing paths. A new
parallel display path carries server-authoritative terminal state diffs over QUIC datagrams to
the client; the existing reliable stream continues to carry raw `PtyData` chunks for the
`SequencedOutputBuffer` and cold-reattach replay (these are not replaced). On the server,
`TerminalState` (new module, `vte::Perform` impl) tracks the rendered screen; `DiffEncoder`
emits `StateDiff` datagrams from the `run_session` pump's `select!` loop. On the client,
`Predictor` maintains a confirmed `ScreenGrid` plus a speculative overlay; `ClientScreen`
composes them into a single render pass; `ConnectionLossOverlay` injects a banner when datagrams
go silent, as a post-process over the same render path — never as a direct `stdout.write_all`.

**Major components (dependency-ordered):**
1. `nosh-proto/src/datagram.rs` (new): `StateDiff` + `ClientEpoch` wire types; encode/decode using postcard — gates all other work
2. `nosh-server/src/terminal.rs` (new): `TerminalState` implementing `vte::Perform`; add `term_state: Mutex<TerminalState>` to `SessionSlot`; extend `push_output` to also feed vte
3. `run_session` / `run_reattach_session` pump (modify): new `diff_interval.tick()` arm — `encode_diff()` + `conn.send_datagram()`
4. `nosh-client/src/predictor.rs` (new): `Predictor`, `ClientScreen`, `ConnectionLossOverlay`; confirmed rendering first, then speculative overlay
5. `run_pump` client (modify): add `conn.read_datagram()` arm routing through predictor; suppress direct `stdout.write_all` for display when datagrams are active (still advance `highest_applied`)
6. PTY reader fix: `Session` gets pipe-based shutdown fd; `output_reader` uses `nix::poll` on `[PTY fd, shutdown pipe]` so `abort()` actually works

**Key architectural invariants:**
- `PtyData` on the reliable stream MUST continue to advance `highest_applied` — the `Ack` mechanism and `SequencedOutputBuffer` trim depend on it
- Keystrokes go on the reliable stream only — never as datagrams; keystroke loss is never acceptable
- All output to the local terminal goes through `ClientScreen.render_to_stdout()` — never direct `stdout.write_all` once the predictor exists
- Datagrams are suppressed on the client during the reattach replay window; a `ResumeComplete` signal gates fresh datagrams post-replay

### Critical Pitfalls

All twelve PITFALLS.md entries are real. The top five that must be resolved at design time, not
retrofitted:

1. **Epoch-reset-on-cursor-move is the design gate** — predicting in cursor-addressing apps (vim, less, htop) produces screen corruption that is visually worse than no prediction. Any CSI cursor-move, ED/EL erase, or alternate-screen sequence must reset the epoch. Conservative fallback (no display on fresh terminal row, no display in confirmed-control-char window) must be built in from the initial commit, not added later. Adversarial test required: vim session, `iHello<Esc>`, zero corrupt cells.

2. **PTY reader zombie race is a prerequisite** — `output_reader.abort()` on a `spawn_blocking` task has no effect while the blocking `read()` syscall is in flight. Under M4 orphan-session load this fills the tokio blocking pool. Fix (self-pipe / `nix::poll`) must be the first task of M4 before any load-testing. Every integration test that creates and drops sessions accumulates stuck threads until this is fixed.

3. **Datagram MTU / sparse diff must be designed before prediction code** — a full 80x24 terminal grid is ~7.8 KB; QUIC datagram limit is ~1200 bytes. The `StateDiff` wire format must be sparse (changed cells only) and capped at `max_datagram_size() - 100` bytes before any prediction code references it. This also resolves the v1.1 `WSAEMSGSIZE` log warning on Windows.

4. **Noecho-suppression is a security requirement of prediction** — a prediction engine that echoes characters during `stty -echo` prompts leaks passwords visually at the client terminal. Track server echo state from the confirmed terminal model; suppress prediction when the server is not echoing the last confirmed character. Must be in the initial design, with a `read -s` test.

5. **Reattach / datagram sync race** — on cold reattach the client replays `PtyData` from `SequencedOutputBuffer`; a datagram arriving during replay applies to a partial-replay screen, creating a torn view. Client must ignore datagrams between `ReattachOk` and replay-complete; a `ResumeComplete` signal gates fresh datagrams post-replay.

**Also critical but scoped to the security doc pass:**
- TOFU first-contact gap must be named explicitly in the security doc with a mitigation path
- Privilege model (no privsep) must have its own section in the security doc
- Datagram replay/staleness analysis must be in the security doc (QUIC TLS 1.3 authenticates datagrams; application-layer monotonic epoch handles stale delivery)

## Implications for Roadmap

All four research files agree on the phase ordering below. The dependency chain is strict enough
that deviating from it produces a blocked phase.

### Phase 1: PTY Reader Race Fix (prerequisite, must be first)

**Rationale:** The `output_reader.abort()` + `spawn_blocking` zombie race is already latent in
v1.1. Every M4 integration test that creates and orphans sessions accumulates stuck blocking
threads. Fixing this first means all subsequent development and load testing is not poisoned by
a pre-existing reliability hole. This is the only fix that has no dependencies on any other M4
work; every other phase depends on a functioning server session lifecycle.

**Delivers:** A server that cleanly terminates PTY reader threads when sessions are orphaned or
cleaned up. Blocking thread count stays bounded under orphan load.

**Addresses:** PITFALLS.md Pitfall 6 (PTY reader zombie race)

**Implementation:** Replace `spawn_blocking` + `reader.read` loop with `nix::poll` on
`[PTY master fd, shutdown pipe read end]`. Add `shutdown_pipe: RawFd` to `Session`. On orphan /
abort, write to the pipe write end. Gate with `#[cfg(unix)]`. No new crates.

**Avoids:** Blocking pool exhaustion before any M4 load test is run.

**Research flag:** Standard pattern (nix::poll self-pipe trick is well-documented; tokio AsyncFd
alternative also documented). Skip research-phase for this phase.

---

### Phase 2: Datagram Wire Protocol (nosh-proto, blocks all prediction work)

**Rationale:** Every other M4 component — server diff encoder, client predictor, reattach
integration — references the `StateDiff` and `ClientEpoch` wire types. This must be defined and
tested in isolation before any other new module is written. Getting the format wrong (full grid
instead of sparse diff, no monotonic epoch, no size cap) requires retrofitting all consumers.

**Delivers:** `nosh-proto/src/datagram.rs` with `StateDiff` (sparse `DiffCell` list, `epoch: u64`,
`cols`/`rows`, `cursor`), `ClientEpoch` (`confirmed: u64`), `encode_datagram` / `decode_datagram`
using postcard. Unit tests: encode/decode round-trip; datagram size assertion
(payload `<= max_datagram_size() - 100`).

**Addresses:** Predictive echo P1 (foundation); PITFALLS.md Pitfalls 4 (desync), 5 (MTU), 10 (stale)

**Uses:** `postcard` + `serde` (existing); `termwiz::surface::change::Change` (new dep, use_serde feature)

**Open question (must be resolved in this phase):** Sparse diff encoding strategy — how to
represent changed-only cells in a size-bounded payload when a large refresh (vim file open, `clear`)
changes the entire screen. Options: (a) send cells up to size limit, accept partial update this
frame; (b) prioritize cells near the cursor; (c) fall back to reliable stream for full-screen
repaints. Must be decided before implementation.

**Research flag:** Needs design decision on sparse-diff encoding strategy. Flag for
`--research-phase` during roadmap.

---

### Phase 3: Server Terminal State Model (nosh-server)

**Rationale:** With the wire protocol defined, the server-side `TerminalState` implementing
`vte::Perform` can be built and unit-tested against known VT sequences before the QUIC plumbing
is touched. This isolation keeps the vte integration testable independently.

**Delivers:** `nosh-server/src/terminal.rs` — `TerminalState` (grid of `Cell`, cursor position,
epoch counter), `vte::Perform` impl (`print`, `execute`, `csi_dispatch`, `osc_dispatch` at
minimum), `push_output_and_parse` on `SessionSlot` that feeds both `SequencedOutputBuffer` and
`TerminalState`. Unit tests: feed known VT sequences, assert cell contents and cursor position.

**Addresses:** PITFALLS.md Pitfall 1 (cursor-move epoch gate requires accurate server cursor tracking)

**Uses:** `vte 0.15.0` (add to `nosh-server/Cargo.toml` — NOT currently present); `termwiz 0.23.3`

**Open question:** vte vs termwiz parser on the server. ARCHITECTURE.md and STACK.md converge on:
use `vte::Perform` for parsing, `termwiz::Surface` as the storage model. Verify the `vte` 0.15.0
`Perform` trait surface (specifically `osc_dispatch` for OSC 52 detection) before committing.

**Research flag:** MEDIUM confidence on vte 0.15.0 `Perform` API surface — verify `osc_dispatch`
parameters before phase planning.

---

### Phase 4: Server Datagram Sender (run_session pump extension)

**Rationale:** With protocol types and terminal state model both working, wire them together into
the server's `run_session` `select!` loop. This phase produces end-to-end datagrams from server to
client — without any prediction yet, just confirmed state delivery.

**Delivers:** New `diff_interval.tick()` arm in `run_session` and `run_reattach_session` calling
`encode_diff()` + `conn.send_datagram()`. Integration test: connect client and server, type
characters, assert `conn.read_datagram()` on client receives non-empty `StateDiff` frames. Add
`ResumeComplete` handshake to gate datagrams after reattach replay.

**Addresses:** PITFALLS.md Pitfall 7 (reattach/datagram race)

**Uses:** `quinn::Connection::send_datagram` (already enabled in `nosh-proto/src/transport.rs:8-9`)

**Note:** Coalesce PTY output — drain `out_rx`, build one diff, send one datagram per 16 ms tick.
Do NOT send one datagram per `PtyData` chunk.

**Research flag:** Standard pattern. Skip research-phase.

---

### Phase 5: Client Predictor — Confirmed Rendering, Then Speculative Overlay

**Rationale:** Split into two sub-phases to contain complexity. First confirm the datagram path
works end-to-end (5a); then add speculative overlay (5b). Keeps bisection tractable when
predictions go wrong.

**Sub-phase 5a — Confirmed rendering:**
`nosh-client/src/predictor.rs` with `Predictor` (confirmed `ScreenGrid` only), `ClientScreen`
(render confirmed state to stdout), `ConnectionLossOverlay` (stub). Adds `conn.read_datagram()` arm
to `run_pump`; suppresses direct `stdout.write_all` for display (still advances `highest_applied`).
End-to-end test: screen rendered from datagrams matches raw PTY output.

**Sub-phase 5b — Speculative overlay:**
Adds `PendingPrediction`, epoch tracking, `add_prediction` / `confirm_up_to` / `cull`, epoch-reset
on control chars, conservative fallback (fresh row, no-confirm window), underline rendering,
adaptive RTT threshold, `unicode-width` column advance for CJK/wide chars, bracketed paste
suppression. This is the hardest UX step — budget 2-3 phases per INIT.md §10.

**Addresses:** All P1 features in FEATURES.md; PITFALLS.md Pitfalls 1, 2, 3, 4, 9

**Critical tests before marking done:**
- vim session: `iHello<Esc>` — zero corrupt cells
- `read -s` noecho: zero predicted characters
- `你好` CJK: cursor column correct after each wide character
- Bracketed paste: zero predicted cells between `\x1b[200~` and `\x1b[201~`
- 30% datagram loss simulation: self-corrects within 2 RTTs

**Research flag:** Sub-phase 5b needs `--research-phase` during planning — highest-complexity area
of M4. Mosh `terminaloverlay.cc` cull() logic, epoch model, `Validity` enum all need careful
translation to Rust.

---

### Phase 6: QoL Feature Pack

**Rationale:** Connection-loss banner, OSC 52 passthrough, and terminal title propagation are
mostly independent of the prediction engine and share the "detect escape sequence in PTY output"
mechanism on the server side. Batch them in one phase.

**Delivers:**
- `ConnectionLossOverlay` activated: `last_datagram_received > 5s` triggers overlay at row 0 with counter + abort instructions
- OSC 52 passthrough: server detects in `osc_dispatch`; forwards as `Message::Osc52` on reliable stream; client emits via `crossterm::clipboard::CopyToClipboard`
- Terminal title propagation: verify OSC 0/2 is not stripped; un-filter if needed
- `--predict always/adaptive/never` CLI flag: wire to `PredictDisplayMode` enum

**Addresses:** FEATURES.md P1 (connection-loss banner, OSC 52) and P2 (terminal title); QoL ranking items 2, 3, 4, 5

**Uses:** `crossterm 0.29.0` with `osc52` feature (add feature flag to `nosh-client/Cargo.toml`); no new crates

**Research flag:** Standard pattern. Skip research-phase.

---

### Phase 7: Windows CI Gate

**Rationale:** The Windows CI gate has existed since v1.1 but has never run (no git remote
configured). Now that M4 adds new client code, running CI on Windows is required before the
milestone is complete.

**Delivers:** `.github/workflows/ci.yml` `build-windows` job on `windows-latest` runner running
`cargo build --locked --target x86_64-pc-windows-msvc -p nosh-client`. Also adds
`quinn_udp=error` tracing filter on Windows to silence the WSAEMSGSIZE log warning (workaround
pending upstream quinn fix in a future 0.11.x release).

**Addresses:** PITFALLS.md Pitfall 12 (Windows CI false-green); STACK.md Windows CI section

**Research flag:** Standard pattern. Skip research-phase.

---

### Phase 8: Security Design Document

**Rationale:** The security doc is the last deliverable but TOFU prompt UX and noecho-suppression
must be tracked as requirements during Phases 5-6, not deferred. The security doc formalizes what
must already be implemented.

**Delivers:** Security design document covering:
- TOFU first-contact gap: threat named explicitly; mitigation = first-connection fingerprint prompt + `nosh-keyscan` for pre-distribution
- Privilege model: server runs as authenticated user; no privsep; contrast with sshd; mitigations listed
- Datagram replay/staleness: QUIC TLS 1.3 authenticates datagrams; per-packet replay protection; application-layer monotonic epoch handles stale delivery
- Noecho-suppression: documented as security requirement of prediction; verified via `read -s` test
- Reattach two-factor (W1 pattern): mint-send-commit token rotation must survive any M4 refactor

**Addresses:** PITFALLS.md Pitfalls 8, 9, 10, 11

**Research flag:** Standard documentation pass. Use PITFALLS.md "Looks Done But Isn't" checklist
for sign-off criteria.

---

### Phase Ordering Rationale

- **Phase 1 before all others:** PTY zombie race is the only fix with no M4 dependencies; all four research files flag it independently as a prerequisite. Every integration test that creates and drops sessions is poisoned until this is fixed.
- **Phase 2 before Phases 3-5:** Wire protocol is the shared interface between server and client components. Format-churn retrofitting all consumers is the failure mode of getting this wrong.
- **Phase 3 before Phase 4:** Server terminal state model must be unit-tested in isolation before async QUIC plumbing is added.
- **Phase 4 before Phase 5:** Confirmed datagram delivery must be end-to-end validated before speculative overlay is built on top.
- **Phase 5a before Phase 5b:** Confirmed rendering validates the datagram path without speculative complexity; makes bisection tractable.
- **Phases 6 and 7 after Phase 5a:** QoL features and CI are independent of speculative prediction but should land after the core datagram path is stable.
- **Phase 8 last:** The security doc formalizes what must already be implemented; cannot be written meaningfully before implementation is stable.

### Research Flags

Phases needing deeper research during planning:
- **Phase 2 (datagram wire protocol):** Sparse diff encoding strategy is an open design question — how to handle large terminal refreshes within QUIC datagram size limits. Must be resolved before implementation can be planned.
- **Phase 3 (server terminal state):** vte 0.15.0 `Perform` trait `osc_dispatch` signature needs verification before phase planning. MEDIUM confidence.
- **Phase 5b (speculative overlay):** Highest-complexity area of M4. Mosh `terminaloverlay.cc` cull() logic, epoch model, `Validity` state machine all need careful translation. Budget 2-3 phases. Flag for `--research-phase`.

Phases with standard patterns (skip research-phase):
- **Phase 1** — nix::poll self-pipe trick; well-documented
- **Phase 4** — `conn.send_datagram()` already enabled; interval-based coalescing is standard
- **Phase 6** — crossterm OSC 52 API and connection-loss timer are standard async patterns
- **Phase 7** — windows-latest + x86_64-pc-windows-msvc native build is well-documented
- **Phase 8** — security doc; checklist-driven

## Confidence Assessment

| Area | Confidence | Notes |
|------|------------|-------|
| Stack | HIGH | All crate versions and APIs verified against docs.rs / GitHub live data; termwiz `get_changes` / `flush_changes_older_than` / `Change` serde confirmed; crossterm OSC 52 confirmed; AsyncFd PTY pattern confirmed via tokio issue #4488 |
| Features | HIGH | Mosh paper + source-level research; SRTT thresholds from deepwiki Mosh analysis; all claims attributed to primary sources |
| Architecture | HIGH | Every component boundary grounded in actual nosh codebase file:line citations (registry.rs:235, server.rs:409, main.rs:604); Mosh SSP design verified against published paper |
| Pitfalls | HIGH | 12 pitfalls sourced to primary references (Mosh paper, tokio docs, quinn issues, QUIC RFCs); nosh codebase v1.1 inspection confirms zombie race location |

**Overall confidence:** HIGH

### Gaps to Address

- **Sparse diff encoding (Phase 2):** Open design decision, not a research gap. Must be resolved as the first task of Phase 2 before any wire format is committed. Options: partial update (cursor context first), skip frame and wait for next tick, fall back to reliable stream for large repaints.

- **vte 0.15.0 `osc_dispatch` parameters:** MEDIUM confidence on exact `Perform` trait API for OSC dispatch. Verify `fn osc_dispatch(&mut self, params: &[&[u8]], bell_terminated: bool)` signature and alternate-screen handling before Phase 3 planning.

- **termwiz parser vs vte for server-side:** Both work; the integration path (vte parses into termwiz Surface, vs termwiz's own parser) needs a decision before Phase 3 implementation. Low risk either way.

- **portable-pty 0.9.0 `AsRawFd` API:** STACK.md states `AsRawFd` is available; verify the exact method name and whether it requires bypassing the `Box<dyn Read>` abstraction (acknowledged as Linux-specific; gate `#[cfg(unix)]`).

- **Emoji / ZWJ sequence width in prediction:** `unicode-width` handles CJK; ZWJ emoji sequences (family emoji, flag sequences) are not handled by `UnicodeWidthChar` alone. Policy decision needed: epoch-reset for ambiguous-width inputs is the recommended default.

## Sources

### Primary (HIGH confidence)

- https://docs.rs/termwiz/0.23.3/termwiz/surface/struct.Surface.html — `get_changes`, `flush_changes_older_than`, `diff_screens`, `SequenceNo` API verified
- https://docs.rs/termwiz/0.23.3/termwiz/surface/change/enum.Change.html — 15 variants; `Serialize + Deserialize` confirmed
- https://docs.rs/crossterm/0.29.0/crossterm/index.html — `clipboard::CopyToClipboard`, `osc52` feature confirmed
- https://docs.rs/tokio/latest/tokio/io/unix/struct.AsyncFd.html — `readable()`, `try_io()` API confirmed
- https://github.com/tokio-rs/tokio/issues/4488 — `AsyncFd` as PTY master nonblocking pattern confirmed
- https://mosh.org/mosh-paper.pdf — SSP, epoch-based prediction, conservative mode, underline rendering, 0.9% error rate
- https://deepwiki.com/mobile-shell/mosh/4.2-predictive-overlay-system — SRTT thresholds, `Validity` enum, `cull()` pattern
- https://docs.rs/quinn/latest/quinn/struct.Connection.html — `send_datagram`, `read_datagram`, `max_datagram_size()` API
- `crates/nosh-server/src/registry.rs:235` — `SessionSlot` struct; `push_output` at line 334 (codebase verified)
- `crates/nosh-server/src/server.rs:356-373` — `output_reader` spawn_blocking location (codebase verified)
- `crates/nosh-proto/src/transport.rs:8-9` — datagram buffer sizes already enabled (codebase verified)

### Secondary (MEDIUM confidence)

- https://deepwiki.com/mobile-shell/mosh/3.2-state-synchronization-protocol — SSP diff model, echo_ack mechanism
- https://github.com/mobile-shell/mosh/blob/master/src/frontend/terminaloverlay.cc — prediction engine structure (C++ reference for Rust port)
- https://github.com/quinn-rs/quinn/issues/2041 — Windows GRO/WSAEMSGSIZE root cause (open issue, no upstream fix)
- https://lib.rs/crates/termwiz — dep tree size (~416K SLoC) confirmed
- RFC 9221 — QUIC datagram extension (unreliable delivery guarantee)
- RFC 9001 — QUIC-TLS (TLS 1.3 per-packet authentication applies to datagrams)

### Tertiary (LOW confidence / needs validation at implementation time)

- vte 0.15.0 `Perform` trait `osc_dispatch` signature — verify parameters before Phase 3
- `portable-pty 0.9.0` `MasterPty::as_raw_fd()` exact method name — verify before Phase 1 implementation
- termwiz internal VT parser (`termwiz::terminal::parser`) as alternative to vte — mentioned as option; not verified against 0.23.3 API
- unicode ZWJ emoji handling via `unicode-width` — `UnicodeWidthChar` handles CJK; ZWJ sequences need separate policy decision

---
*Research completed: 2026-06-01*
*Ready for roadmap: yes*
