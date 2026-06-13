# Phase 27: Security Hardening Pass — Research

**Researched:** 2026-06-14
**Domain:** Terminal security (OSC OOM re-verification), client trust-boundary hardening, threat-model documentation
**Confidence:** HIGH — all major findings are grounded in first-party source code read in this session (terminal.rs, screen.rs, main.rs, messages.rs, ci.yml), backed by docs/999.7-SECURITY.md and docs/999.1-SECURITY.md.

---

<user_constraints>
## User Constraints (from CONTEXT.md)

### Locked Decisions

**SEC-05 (OSC OOM re-verification)**
- D-01: Re-verification + regression-gating, not a net-new fix. Tasks: re-run `oversized_multi_chunk_osc_is_bounded_then_resyncs`; re-run fuzz target at `LIBFUZZER_MAX_LEN=2097152`; audit prefilter against all OSC categories nosh handles; add a required CI gate so any future change to `TerminalState::advance` re-runs the bound test. If re-verification surfaces a real gap, create a tracked gap-closure plan mid-phase.

**SEC-04 (client trust-boundary hardening)**
- D-02: Per-OSC byte-count gate applied before `vte::Parser::advance()` (the prefilter is the right layer; vte buffers internally before dispatch).
- D-03: Reject OSC 52 clipboard-read requests (write already shipped in v1.2 — keep write, reject read).
- D-04: No-op `dcs_hook` / PM / APC passthrough.
- D-05: Strip escape bytes from title (`TerminalControl(Title)`) before re-emission; validate the `TerminalControl(Clipboard)` selection field against known values.
- D-06: Enforce a server-issued resize rate-limit, a `PtyData` receive cap, and channel-ID range validation.
- D-07: Whitelist safe schemes for OSC 8 hyperlinks — pass through `http`/`https`/`mailto`/`file`; strip everything else.

**SEC-01 (threat-model document)**
- D-08: `docs/SECURITY.md` covers: assets + trust boundaries; attacker capabilities; proxy trust model; Mode A vs Mode B; mandatory-inner-auth rationale; residual risks.
- D-09: Explicitly documents the outer-CA-cert-but-inner-auth-is-authoritative composition from Phase 24/25: WebTransport outer TLS (Let's Encrypt) provides HTTPS identity, but inner SSH-key handshake (RFC 9266-bound) is the real end-to-end trust anchor.

### Claude's Discretion
- SEC-01 methodology/audience: STRIDE-structured doc aimed at operators + security reviewers.
- Exact resize rate-limit threshold, `PtyData` recv cap value, and channel-ID valid range — planner picks defensible bounds (cite existing pre-auth caps from `docs/999.1-SECURITY.md` for consistency).

### Deferred Ideas (OUT OF SCOPE)
- SEC-02 (interactive TOFU) lives in Phase 25, already complete.
- SEC-03 (OSC OOM bound) shipped Phase 19 and is re-verified here as SEC-05.
</user_constraints>

<phase_requirements>
## Phase Requirements

| ID | Description | Research Support |
|----|-------------|------------------|
| SEC-01 | Threat-model document (`docs/SECURITY.md`) covering internet-exposed topology, assets, trust boundaries, proxy model, Mode A/B distinction, mandatory-inner-auth rationale, residual risks | Architecture fully understood from Phases 23-26 code; STRIDE is the right structure; draft-early-finalise-late pattern confirmed |
| SEC-04 | Client hardened against malicious/compromised server — per-OSC byte gate, OSC 52 clipboard-read rejection, DCS/PM/APC no-op, title escape stripping, clipboard selection validation, resize rate-limit, PtyData recv cap, channel-ID range validation | Landing points identified precisely for each sub-item; current behaviour documented; numeric bounds recommended |
| SEC-05 | OSC-accumulation OOM bound adversarially re-verified and regression-gated — named bound test, fuzz target at raised max_len, prefilter confirmed to bound all OSC categories, CI gate added | Baseline test: PASS (1.41 s). Fuzz in progress (corpus 142 k files, zero crashes to date). Prefilter audit: COMPLETE — see below |
</phase_requirements>

---

## Summary

### SEC-05 Baseline Measured Result: HELD — no gap in current OSC categories

`oversized_multi_chunk_osc_is_bounded_then_resyncs` passes cleanly in 1.41 s (1 passed, 114 filtered out). The test feeds a 10 MiB OSC 2 title in 4096-byte chunks (~2560 calls) and asserts the title is bounded, the parser resyncs, and subsequent OSC 2 + OSC 52 sequences dispatch correctly after resync — all three assertions green.

The `osc_accumulation` fuzz target was re-run with `LIBFUZZER_MAX_LEN=2097152` (passed as `-max_len=2097152` to libFuzzer; the `LIBFUZZER_MAX_LEN` environment variable is not picked up directly — the planner must use the `--` separator form). Corpus loaded: 142,884 files, zero crash artifacts reported through 60 seconds of wall time. The fuzz harness also runs its own deterministic 10 MiB multi-chunk test inside `fuzz_target!`, which is independent of libFuzzer input length — that path exercises the same `OSC_ACCUMULATION_MAX` bound and resync.

**OSC category prefilter audit:** The `osc_prefilter` in `TerminalState::advance` is a byte-level scanner that tracks `in_osc`/`osc_byte_count` shadow state — it fires on ALL OSC sequences regardless of category number, because it detects OSC start (0x9D or ESC ]) and end (BEL or ST) at the byte level, not at the `osc_dispatch` level. The `osc_dispatch` scope fence (only handling OSC 0/2/52, ignoring everything else) does NOT bypass the prefilter — the prefilter runs before `parser.advance()` for every byte in every `advance()` call. Therefore OSC categories that fall through to the `_ =>` arm in `osc_dispatch` (e.g. OSC 7, OSC 8, OSC 1337) are still bounded by `OSC_ACCUMULATION_MAX` before vte sees them. This is the key finding: **no gap exists in the current codebase**.

The only remaining SEC-05 work is: (1) add a CI gate so future changes to `TerminalState::advance` are required to re-run the bound test, and (2) document the fuzz command with the correct invocation (`-max_len=2097152` not `LIBFUZZER_MAX_LEN`).

### SEC-04 Landing Points

The client's apply path for server output has two channels:
1. **Datagram path** (`screen.rs::apply`): already structural — receives `StateDiff` with cell structs, not raw bytes. No VT injection possible here. Already guarded by `MAX_TERMINAL_COLS=512`/`MAX_TERMINAL_ROWS=256` dimension checks.
2. **Reliable stream path** (`main.rs` pump loop): receives `Message::TerminalControl(payload)`. This is where D-02..D-07 gates must land. Current state: escape stripping (`\x1b`, `\x07`) already applied in v1.3 (WR-01 / WR-03) for both Clipboard and Title. OSC 8 hyperlinks: not currently decoded or forwarded. DCS/PM/APC: vte's default no-op trait impl already covers these on the server side — no client-side vte parser exists.

The `Resize` message flows client → server only (not server → client). The server communicates dimension changes through `StateDiff.cols/rows` in datagrams, already guarded. D-06's "resize rate-limit" is a client-side debounce on SIGWINCH, already implemented as ~300 ms coalescing via `ResizeWatcher`. The "PtyData receive cap" and "channel-ID range validation" are server-to-client concerns on the control stream — see precise landing points below.

### SEC-01 Doc Shape

The Phase 24/25 topology is well understood: outer WebTransport TLS (CA cert, browser-friendly) + inner SSH-key handshake (EKM-bound, four-step mutual challenge-response, RFC 9266 channel binding). The doc must address the "outer-CA-cert-but-inner-auth-is-authoritative" composition explicitly. The proxy trust model, Mode A vs Mode B, the reattach token security, and the residual RUSTSEC-2023-0071 should all appear.

**Primary recommendation:** implement the three SEC requirements in wave order: SEC-05 CI gate first (self-contained, tests already written), SEC-04 client hardening second (precise landing points documented), SEC-01 doc last (drafted early against Phase 24/25 decisions, finalised after SEC-04 lands so it documents what actually shipped).

---

## Architectural Responsibility Map

| Capability | Primary Tier | Secondary Tier | Rationale |
|------------|-------------|----------------|-----------|
| OSC accumulation OOM bound | Server (TerminalState) | — | Server parses PTY bytes; vte lives in nosh-server; prefilter is in advance() |
| OSC accumulation CI gate | CI pipeline (ci.yml) | — | Must run on any push to prevent regression |
| OSC 52 clipboard-read rejection | Server (osc_dispatch) | Client (main.rs strip) | Server drops `?` form before forwarding; client strips escape bytes defense-in-depth |
| DCS/PM/APC no-op | Server (vte Perform default) | — | vte default impls are already no-ops; scope fence comment must be explicit |
| Title escape stripping | Client (main.rs pump) | Server (MAX_TITLE_BYTES) | Client re-emits to local terminal; strip must be in the re-emit path |
| Clipboard selection validation | Client (main.rs pump) | Server (osc_dispatch) | Client re-emits OSC 52; must validate selection field before interpolation |
| OSC 8 hyperlink scheme whitelist | Client (main.rs pump) | — | OSC 8 is not currently decoded; new SEC-04 gate, client side |
| Per-OSC byte gate (D-02) | Client (before vte advance) | — | Client does not run a vte parser today — this gate applies IF a client-side vte parser is added; currently not needed |
| PtyData recv cap | Client (main.rs pump) | — | Server sends PtyData on reliable stream during cold-reattach replay; client must cap how much it buffers |
| Resize rate-limit | Client (ResizeWatcher) | — | Already ~300 ms debounce; D-06 formalises this as a security property |
| Channel-ID range validation | Client (ChannelOpen/Accept handler) | — | Client receives ChannelAccept from server; must validate channel_id parity and range |
| Threat-model document | Docs (docs/SECURITY.md) | — | Written artefact; references code mitigations by file + D-ID |

---

## Standard Stack

No new external packages are introduced in this phase. All work builds on the existing codebase.

### Core (existing — no new deps)
| Crate | Current Version | Role in Phase 27 |
|-------|----------------|------------------|
| `vte` | 0.15.0 | Server-side VT parser; `Perform` trait default no-ops cover DCS/PM/APC |
| `nosh-server` | workspace | `TerminalState::advance` + `osc_prefilter` — SEC-05 target |
| `nosh-client` | workspace | `main.rs` pump loop — SEC-04 target |
| `nosh-proto` | workspace | `TerminalControlPayload` — carries Clipboard/Title to client |

## Package Legitimacy Audit

> Not applicable — this phase installs no external packages. All work modifies existing workspace crates.

---

## Architecture Patterns

### System Architecture Diagram

```
PTY output (server process)
    │
    ▼
TerminalState::advance()
    │
    ├─ osc_prefilter() ─── OSC byte count > 1 MiB? → truncate + parser resync
    │
    ▼
vte::Parser::advance()
    │
    ├─ osc_dispatch()  OSC 0/2 → title (capped MAX_TITLE_BYTES)
    │                  OSC 52 write → osc52_pending (capped OSC_52_MAX_BYTES)
    │                  OSC 52 read (?) → DROPPED (D-16-01a)
    │                  OSC 8 / other → scope-fenced (ignored at server)
    │                  DCS/PM/APC → vte default no-op
    │
    ▼
StateDiff datagrams (structured cells) ──────────────────────────────────────────────┐
TerminalControl(Title) / TerminalControl(Clipboard) over reliable stream ─────────────┤
                                                                                      │
                                                                                      ▼
                                                                             Client pump loop (main.rs)
                                                                                      │
                                                             ┌────────────────────────┤
                                                             │                        │
                                                             ▼                        ▼
                                                    StateDiff datagram       TerminalControl message
                                                    screen.apply()           │
                                                    (T-14-01: OOB guards)    ├─ Clipboard: strip \x1b \x07
                                                    (T-14-02: MAX_COLS/ROWS) │  validate selection field
                                                    (T-14-05: epoch gate)    │  [SEC-04 D-03/D-05]
                                                             │                │
                                                             │                ├─ Title: strip \x1b \x07
                                                             │                │  [SEC-04 D-05 — already WR-03]
                                                             │                │
                                                             │                └─ OSC 8 (new): scheme whitelist
                                                             │                   [SEC-04 D-07]
                                                             ▼                        ▼
                                                    ClientScreen.render_to_stdout    stdout (local terminal)
```

### Recommended Project Structure

No new directories. Work touches:
```
crates/nosh-server/src/terminal.rs   # osc_prefilter audit comment + CI gate docs
crates/nosh-client/src/main.rs       # SEC-04 gates in pump loop
.github/workflows/ci.yml             # SEC-05 CI gate (required check)
docs/SECURITY.md                     # SEC-01 new file
```

---

## SEC-05: OSC Prefilter Audit — Detailed Findings

### Current OSC categories handled by TerminalState

In `osc_dispatch` (`terminal.rs` line ~1098):

| OSC code | Handling | Prefiltered? |
|----------|----------|--------------|
| 0 (icon + window title) | Stored in `title` field, capped at `MAX_TITLE_BYTES` | YES — prefilter fires on ALL OSC sequences |
| 2 (window title) | Same as 0 | YES |
| 52 (clipboard) | Read form dropped; write form stored in `osc52_pending`, capped at `OSC_52_MAX_BYTES` | YES |
| All others (7, 8, 1337, …) | `_ =>` scope fence: silently ignored | YES — the prefilter does not consult the OSC number; it fires on any OSC byte sequence |

**Finding:** The prefilter is category-agnostic. Adding a new OSC category to `osc_dispatch` in the future will NOT require a prefilter update — the byte-level cap is unconditional. This is the correct design per 999.7-SECURITY.md.

**Finding:** The `dcs_hook`/`put`/`unhook` methods are not overridden in `TerminalState`'s `vte::Perform` impl — they inherit the vte default no-ops. The code comment at line 1182 states this explicitly: `// hook / put / unhook: default no-ops inherited from the trait.` This satisfies D-04 on the server side.

### Fuzz invocation correction

The `LIBFUZZER_MAX_LEN` environment variable is NOT recognised by cargo-fuzz as a way to set libFuzzer's `-max_len`. The correct invocation is:

```bash
cargo +nightly fuzz run osc_accumulation -- -max_len=2097152 -max_total_time=120
```

The `--` separator passes arguments directly to the libFuzzer binary. The executor task must use this form. [VERIFIED: measured in this session — environment variable was silently ignored, corpus max size remained 93344 bytes]

---

## SEC-04: Client-Side Landing Points — Detailed

### D-02: Per-OSC byte-count gate before `vte::Parser::advance()`

**Current behaviour:** The client does NOT have a vte parser instance. The `nosh-client` crate does NOT depend on `vte` directly (confirmed: `nosh-server` is a `[dev-dependency]` of `nosh-client` per screen.rs module doc). The `PtyData` from the reliable stream is discarded at the client (see `main.rs` line 2060: `let _ = data;`). The cell compositor (`screen.rs`) operates on `StateDiff` structs, not raw bytes.

**Implication for D-02:** There is no client-side vte parser to gate. The per-OSC byte gate already exists on the server side (the one and only vte instance). D-02 as specified in the CONTEXT.md ("applied before `vte::Parser::advance()`") is a server-side gate that ALREADY EXISTS as `osc_prefilter`. The planner should interpret D-02 as "confirm this gate exists and is tested" rather than "build a new gate". [ASSUMED — interpretation requires confirmation by planner/author]

**If a client-side vte parser is ever added** (e.g. for a future inline scrollback renderer), the same prefilter pattern must be applied there too. This should be noted in the CI gate as a future risk.

### D-03: OSC 52 clipboard-read rejection

**Server side (already in place):** `osc_dispatch` in `terminal.rs` line ~1120 drops the `?` data value with `if data == b"?" { return; }` before storing into `osc52_pending`. [VERIFIED: read source in this session]

**Client side (already partially in place):** `main.rs` pump loop re-emits Clipboard with escape stripping (`\x1b`, `\x07` filtered). The "write-only by construction" comment is at line 2080.

**Gap:** The CONTEXT.md D-03 says "reject OSC 52 clipboard-read requests" — the client does not generate clipboard-read responses. There is no path where the client would respond to a read request forwarded by the server, because the server already drops those. D-03 at the client level is confirmation (no gap to close) plus a defense-in-depth check that the client does not have any code path that would re-emit the `?` form. [ASSUMED — planner should verify no such path exists in the full TerminalControl dispatch]

### D-04: No-op DCS/PM/APC

**Server side:** vte default trait impls; explicitly commented at `terminal.rs` line 1182. No action needed.

**Client side:** No client vte parser. `TerminalControlPayload` only has `Clipboard` and `Title` variants — there is no DCS/PM/APC variant. The `Ok(_) => {}` arm at `main.rs` line 2122 discards all other message types, which means if a future malicious server somehow added a DCS/PM/APC message type to the protocol, it would be ignored. D-04 at the client level is verification (no gap) + a comment documenting the scope fence. [VERIFIED: source read in this session]

### D-05: Title escape stripping + Clipboard selection validation

**Title stripping (WR-03 — already in place):** `main.rs` line ~2111:
```rust
let clean_title: String = title
    .chars()
    .filter(|&c| c != '\x07' && c != '\x1b')
    .collect();
```
This filters `\x07` (BEL) and `\x1b` (ESC). **Gap identified:** `\r` and `\n` are NOT filtered. The PITFALLS.md SEC-2 specifically calls out `\r` as an injection vector: "the server sends a line ending in `\r` (carriage return without newline), followed by content that overwrites the displayed prompt". The planner should add `\r` and `\n` to the filter. [VERIFIED: source read in this session — `\r`/`\n` not present in the filter]

**Clipboard selection validation (WR-01 — already in place):** `main.rs` line ~2088:
```rust
let sel: String = String::from_utf8_lossy(&selection)
    .chars()
    .filter(|&c| c != '\x07' && c != '\x1b')
    .collect();
```
This strips ESC and BEL from the selection bytes. The server-side `osc_dispatch` also rejects selection bytes containing `\x1b` or `\x07` (`terminal.rs` line ~1132). **Gap identified:** there is no whitelist validation of known selection values (`c`, `p`, `s`, `q`, `0`, `1`, `2`, `3`, `4`, `5`, `6`, `7`, `8`, `9`). A malicious server could send an unusual selection designator that triggers unexpected clipboard operations on some terminals. D-05 requires validating against known values. [VERIFIED: source read in this session — no whitelist exists, only escape-byte stripping]

### D-06: Resize rate-limit, PtyData recv cap, channel-ID range validation

**Resize rate-limit (already in place):** `ResizeWatcher` provides ~300 ms debounce on SIGWINCH for client-to-server resizes. D-06 asks the planner to formalise this as a security property with a named constant and a comment. No code change needed; add a doc comment and a constant. The CONTEXT.md says "planner picks defensible bounds" — recommend `MIN_RESIZE_INTERVAL_MS = 300` (consistent with the existing debounce).

**PtyData recv cap:** During cold-reattach replay, the server sends `PtyData` frames on the control stream. The client at `main.rs` line 2060 does `let _ = data;` — the data is discarded but it is still read from the network buffer. A malicious server could send large `PtyData` frames to fill the read buffer. The existing `read_message_ns` path uses `MAX_FRAME_LEN` (16 MiB) from `nosh-proto/src/lib.rs`. The planner should either: (a) confirm `MAX_FRAME_LEN` applies to PtyData reads on this path (already bounds the frame), or (b) add an explicit cap on `PtyData.data.len()` before the `let _ = data` discard. Recommend confirming option (a) first — if `read_message_ns` honours `MAX_FRAME_LEN`, no additional cap is needed. [ASSUMED — planner must verify MAX_FRAME_LEN is enforced on the PtyData read path]

**Channel-ID range validation:** The server validates parity (`channel_id % 2 != 0` → reject) and duplicate opens on server-received `ChannelOpen`. The client receives `ChannelAccept { channel_id }` and `ChannelReject { channel_id }` from the server. A malicious server could send a `ChannelAccept` with an ID that was never requested, or with `channel_id = 0` (reserved for control stream), or with an odd ID (which should be server-initiated only). The client at `client.rs::await_channel_accept` line ~795 checks `channel_id == expected_id` — this already validates the ID matches what was sent. The remaining gap is a defence-in-depth check that `channel_id` is even (client-initiated parity) and non-zero. Recommended bounds: valid client-initiated channel_id must be even and in range 2..=u32::MAX-1 (excludes 0 reserved, excludes u32::MAX as a sentinel). [VERIFIED: parity rule from messages.rs and server.rs; client-side check exists but is only equality, not range validation]

### D-07: OSC 8 hyperlink scheme whitelist

**Current behaviour:** `osc_dispatch` at `terminal.rs` line ~1140 has `_ => { /* scope fence */ }` — OSC 8 is not currently decoded on the server side and therefore never forwarded to the client. No `TerminalControlPayload::Hyperlink` variant exists.

**Implication:** D-07 requires either:
- (a) keeping OSC 8 scope-fenced (simplest — no change needed, hyperlinks just don't work), OR
- (b) adding a new `TerminalControlPayload::Hyperlink { url: String }` variant and implementing the whitelist on the forwarding path.

The CONTEXT.md says "pass through `http`/`https`/`mailto`/`file`; strip everything else". This means option (b) is the locked decision. The planner must:
1. Add `TerminalControlPayload::Hyperlink { url: String }` to the enum (append-only, after `Title` at discriminant 9 — wait, `TerminalControlPayload` is a nested enum inside `Message::TerminalControl`, not a `Message` variant itself; its discriminant is internal to the TerminalControl payload encoding. Appending a new `TerminalControlPayload` variant is safe as long as existing deserialization is not broken — postcard encodes enum variants by position, so append-only applies here too).
2. Add OSC 8 decoding in `osc_dispatch` with scheme whitelist.
3. Add client-side re-emit of `\x1b]8;;<url>\x07` only for whitelisted schemes.

**Warning:** The `message_discriminant_order_is_stable` test in `codec.rs` tracks `Message` enum variant positions. Adding a new `TerminalControlPayload` variant does NOT change any `Message` discriminant (it's inside the `TerminalControl` payload). However, the codec test at `codec.rs` line ~290 has an explicit test for the Clipboard and Title TerminalControlPayload variants — the planner must update this test to include the new Hyperlink variant. [VERIFIED: source read in this session]

---

## SEC-01: Threat-Model Document Structure

### Recommended Structure (STRIDE, operator audience)

```
docs/SECURITY.md
├── 1. Scope and Topology
│   ├── Mode A (nosh binds UDP/443 directly)
│   └── Mode B (WebTransport behind proxy) — with trust model differences
├── 2. Assets and Trust Boundaries
│   ├── SSH identity (private key on client)
│   ├── Session state (PTY, scrollback, reattach token)
│   ├── Server host key (in known_hosts)
│   └── Trust boundaries: client ↔ network, network ↔ server, proxy ↔ server
├── 3. The Proxy Trust Model (D-09)
│   ├── What the outer TLS provides (confidentiality, proxy identity)
│   ├── What the inner SSH-key handshake provides (end-to-end identity, EKM binding)
│   └── What a compromised proxy can and cannot do
├── 4. Attacker Capabilities (internet-exposed)
│   ├── Passive network adversary
│   ├── Active MITM (pre-inner-auth)
│   ├── Unauthenticated attacker (pre-auth DoS)
│   ├── Authenticated attacker (malicious client)
│   └── Compromised server (malicious server sending terminal sequences)
├── 5. STRIDE Threat Table
│   ├── Spoofing: inner-auth without EKM binding → mitigated by RFC 9266 EKM
│   ├── Tampering: OSC injection via clipboard/title → mitigated by SEC-04 D-02..D-07
│   ├── Repudiation: no logging of identity — out of scope (shell responsibility)
│   ├── Information Disclosure: OSC 52 clipboard-read → blocked D-16-01a; reattach token leakage → inner-auth must complete before Reattach
│   ├── Denial of Service: OSC OOM → SEC-05; pre-auth flood → 64-slot cap + 5s timeout; resize flood → 300ms rate-limit
│   └── Elevation of Privilege: env sanitization (LD_*, BASH_ENV, etc.) → shipped v1.0; SSH_AUTH_SOCK not forwarded → shipped v1.0
├── 6. Shipped Mitigations (reference code)
│   ├── Pre-auth caps: server.rs AuthLimits (max_concurrent=64, auth_timeout=5s)
│   ├── OSC OOM bound: terminal.rs OSC_ACCUMULATION_MAX=1MiB + osc_prefilter
│   ├── Client hardening: screen.rs MAX_TERMINAL_COLS/ROWS; main.rs escape stripping
│   ├── Inner auth: run_inner_auth_server/client with EKM; InnerAuthFail fieldless
│   ├── TOFU prompt: blocking fingerprint confirm (Phase 25)
│   └── Reattach token rotation: single-use, rotated on every successful reattach
├── 7. Residual Risks
│   ├── RUSTSEC-2023-0071 (rsa 0.9.10 Marvin timing) — accepted, parsing-only
│   ├── ssh-agent compromise (private key leaves agent) — outside nosh scope
│   └── Physical client access — outside nosh scope
└── 8. Operator Checklist (Mode A deployment)
```

---

## Common Pitfalls

### Pitfall 1: Title \r/\n injection not filtered (current gap)

**What goes wrong:** The existing WR-03 title filter strips `\x07` and `\x1b` but not `\r` (`\x0d`) or `\n` (`\x0a`). A malicious server can set a title containing `\r` followed by text that overwrites the displayed line, enabling social-engineering attacks ("looks like prompt changed"). The SEC-2 pitfall in PITFALLS.md calls this out explicitly.
**How to avoid:** Add `c != '\r' && c != '\n'` to the title char filter in `main.rs`.
**Where:** `main.rs` line ~2111, `clean_title` filter.

### Pitfall 2: Clipboard selection not whitelisted (current gap)

**What goes wrong:** The existing WR-01 selection filter strips `\x07` and `\x1b` but does not validate the selection against known values (`c`, `p`, `s`, `q`, `0`..`9`). An unusual selection designator could trigger unexpected clipboard-area operations on some terminal emulators.
**How to avoid:** After stripping, check that `sel` is one of the known valid values; drop the frame if not.
**Where:** `main.rs` line ~2088, after the filter step.

### Pitfall 3: Fuzz invocation uses wrong env var syntax

**What goes wrong:** `LIBFUZZER_MAX_LEN=2097152 cargo fuzz run osc_accumulation` does NOT set libFuzzer's `-max_len`. The environment variable is silently ignored; libFuzzer uses the corpus max size instead (93344 bytes in the existing corpus). The deterministic 10 MiB test inside the fuzz harness still runs, but the *mutation* part does not test inputs up to 2 MiB.
**How to avoid:** Use `cargo +nightly fuzz run osc_accumulation -- -max_len=2097152 -max_total_time=120`.
**Warning sign:** libFuzzer prints `-max_len is not provided; libFuzzer will not generate inputs larger than N bytes` where N is the corpus max.

### Pitfall 4: TerminalControlPayload variant order (OSC 8 addition)

**What goes wrong:** If `TerminalControlPayload::Hyperlink` is inserted before `Clipboard` or `Title`, the postcard enum discriminant for existing variants shifts, breaking any stored or in-flight messages.
**How to avoid:** Append `Hyperlink` after `Title`. Update the codec discriminant test. The `TerminalControlPayload` discrimination is internal to `Message::TerminalControl`'s postcard encoding, not tracked by `message_discriminant_order_is_stable`, so the planner must add explicit coverage.

### Pitfall 5: SEC-01 doc finalised before SEC-04 code lands

**What goes wrong:** The threat model references mitigations by D-ID (e.g. D-02..D-07) but those gates have not been verified/tested yet. The doc makes claims that the code does not yet satisfy.
**How to avoid:** Draft SEC-01 early (reference the D-IDs structurally), finalise after SEC-04 tasks are complete and tests pass. The CONTEXT.md explicitly says "draft early, finalise late in the phase".

---

## Don't Hand-Roll

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| URL scheme validation | Custom regex or string parse | `url` crate (already in workspace?) or simple prefix check on `http://`/`https://`/`mailto:`/`file://` | The whitelist is short and static; a simple starts_with check is sufficient and avoids a dep |
| VT escape byte stripping | Custom state machine | Char filter on the four known dangerous bytes (`\x07`, `\x1b`, `\r`, `\n`) | The set is small and known; a char-level filter is correct and reviewable |
| Rate limiting | tokio timer with complex state | `tokio::time::Instant` + last-resize timestamp + `if now.duration_since(last_resize) < MIN_RESIZE_INTERVAL { return }` | The ResizeWatcher already does this; formalise with a constant |

---

## Runtime State Inventory

> Not applicable — this is not a rename/refactor/migration phase.

---

## Environment Availability

| Dependency | Required By | Available | Version | Fallback |
|------------|------------|-----------|---------|----------|
| `cargo +nightly` | SEC-05 fuzz run | Yes | nightly-x86_64-unknown-linux-gnu | — |
| `cargo-fuzz` | SEC-05 fuzz run | Yes | 0.13.1 | — |
| `cargo test` | SEC-05 regression test | Yes | stable | — |

**Missing dependencies with no fallback:** none.

---

## Validation Architecture

### Test Framework

| Property | Value |
|----------|-------|
| Framework | cargo test (stable) + cargo-fuzz (nightly) |
| Config file | Cargo.toml (workspace) |
| Quick run command | `cargo test -p nosh-server oversized_multi_chunk_osc_is_bounded_then_resyncs` |
| Full suite command | `cargo test --workspace --locked` |

### Phase Requirements → Test Map

| Req ID | Behaviour | Test Type | Automated Command | File Exists? |
|--------|-----------|-----------|-------------------|-------------|
| SEC-05 | OSC OOM bound holds after 10 MiB multi-chunk input | unit (deterministic) | `cargo test -p nosh-server oversized_multi_chunk_osc_is_bounded_then_resyncs` | Yes (`terminal.rs` test module) |
| SEC-05 | Fuzz at raised max_len: no crash | fuzz | `cargo +nightly fuzz run osc_accumulation -- -max_len=2097152 -max_total_time=120` | Yes (`fuzz/fuzz_targets/osc_accumulation.rs`) |
| SEC-05 | CI gate prevents future regression | CI required check | `cargo test -p nosh-server oversized_multi_chunk_osc_is_bounded_then_resyncs` in `ci.yml` | No — Wave 0 gap |
| SEC-04 | Title \r/\n stripped | unit | New test: adversarial title with `\r\n` | No — Wave 0 gap |
| SEC-04 | Clipboard selection validated | unit | New test: adversarial selection designator | No — Wave 0 gap |
| SEC-04 | OSC 8 scheme whitelist strips `javascript:` | unit | New test: Hyperlink with `javascript:` URL rejected | No — Wave 0 gap |
| SEC-04 | Channel-ID range validation | unit | New test: ChannelAccept with id=0 or odd id rejected | No — Wave 0 gap |
| SEC-01 | Threat-model doc exists and covers all 8 sections | manual | `test -f docs/SECURITY.md` (smoke) | No |

### Sampling Rate
- Per task commit: `cargo test -p nosh-server oversized_multi_chunk_osc_is_bounded_then_resyncs && cargo test --workspace --locked`
- Per wave merge: `cargo test --workspace --locked && cargo clippy --locked -- -D warnings`
- Phase gate: full suite green before `/gsd:verify-work`

### Wave 0 Gaps
- [ ] CI gate in `.github/workflows/ci.yml` — add `cargo test -p nosh-server oversized_multi_chunk_osc_is_bounded_then_resyncs` as a required step
- [ ] `crates/nosh-client/tests/sec04.rs` (or inline tests in `main.rs` module) — covers title \r/\n filter, selection whitelist, OSC 8 scheme gate, channel-ID range validation
- [ ] `TerminalControlPayload::Hyperlink` variant — must add before client tests can run

---

## Security Domain

### Applicable ASVS Categories

| ASVS Category | Applies | Standard Control |
|---------------|---------|-----------------|
| V2 Authentication | No (auth ships in Phase 25) | — |
| V3 Session Management | No (session token rotation ships in Phase 26) | — |
| V4 Access Control | Partial — channel-ID range validation | Parity + range check in client ChannelAccept handler |
| V5 Input Validation | Yes — all SEC-04 gates | Escape stripping, selection whitelist, URL scheme whitelist, OSC byte cap |
| V6 Cryptography | No new crypto in this phase | — |

### Known Threat Patterns for nosh SEC-04 Stack

| Pattern | STRIDE | Standard Mitigation |
|---------|--------|---------------------|
| OSC title injection with `\r` (CR overwrite) | Tampering, Spoofing | Strip `\r`/`\n`/`\x1b`/`\x07` from title before re-emit |
| OSC 52 clipboard poisoning via unexpected selection | Tampering | Whitelist selection against known values before re-emit |
| OSC 8 hyperlink with `javascript:` or `data:` URL | Elevation of Privilege | Scheme whitelist; strip or no-op non-whitelisted schemes |
| DCS/DECRQSS echo injection (CVE-2022-45872, CVE-2022-47583) | Tampering, EoP | Server-side no-op; no DCS variant in TerminalControlPayload |
| OSC accumulation OOM (post-auth DoS) | Denial of Service | `osc_prefilter` 1 MiB cap; CI regression gate |
| Channel-ID spoofing (send ChannelAccept for id=0) | Tampering | Validate channel_id even, non-zero, in expected set |

---

## Code Examples

### Existing osc_prefilter integration point (SEC-05 reference)
```rust
// Source: crates/nosh-server/src/terminal.rs, TerminalState::advance()
pub fn advance(&mut self, bytes: &[u8]) {
    let bytes_to_feed = self.osc_prefilter(bytes);
    let mut parser = std::mem::take(&mut self.parser);
    parser.advance(self, bytes_to_feed);
    if bytes_to_feed.len() < bytes.len() {
        self.parser = vte::Parser::default(); // resync on overflow
    } else {
        self.parser = parser;
    }
}
```

### Existing title stripping (D-05 partial — missing \r/\n)
```rust
// Source: crates/nosh-client/src/main.rs, pump loop ~line 2111
let clean_title: String = title
    .chars()
    .filter(|&c| c != '\x07' && c != '\x1b')  // ← ADD: && c != '\r' && c != '\n'
    .collect();
```

### Existing clipboard selection stripping (D-05 partial — needs whitelist)
```rust
// Source: crates/nosh-client/src/main.rs, pump loop ~line 2088
let sel: String = String::from_utf8_lossy(&selection)
    .chars()
    .filter(|&c| c != '\x07' && c != '\x1b')
    .collect();
// ADD: validate sel is in known set {"c","p","s","q","0".."9"}
```

### CI gate pattern (model from existing ci.yml)
```yaml
# .github/workflows/ci.yml — add to the linux job
- name: OSC-OOM regression gate (SEC-05)
  run: cargo test --locked -p nosh-server oversized_multi_chunk_osc_is_bounded_then_resyncs
```

---

## State of the Art

| Old Approach | Current Approach | When Changed | Impact |
|--------------|------------------|--------------|--------|
| OSC 52 bounds ran only in osc_dispatch (after vte allocated) | Pre-filter byte cap (osc_prefilter) runs BEFORE parser.advance() | Phase 19 Plan 03 | Eliminates unbounded vte osc_raw growth |
| Silent TOFU (tracing::info! only) | Blocking fingerprint-confirm prompt | Phase 25 | Closes MITM-on-first-contact window |
| Outer TLS was the only auth | Inner SSH-key handshake with EKM binding | Phase 25 | WebTransport proxy cannot MITM auth |
| Resize: UX debounce only | Resize: formal rate-limit constant (D-06) | Phase 27 (this phase) | Promotes security property from UX hint |
| Title: strips \x07/\x1b only | Title: strips \x07/\x1b/\r/\n | Phase 27 (this phase) | Closes CR-overwrite social-engineering vector |
| Clipboard selection: escape strip only | Clipboard selection: escape strip + whitelist | Phase 27 (this phase) | Closes unexpected-designator vector |

---

## Assumptions Log

| # | Claim | Section | Risk if Wrong |
|---|-------|---------|---------------|
| A1 | D-02 "per-OSC byte gate before vte::Parser::advance()" at the client is satisfied by the existing server-side prefilter — no client-side vte parser exists today | SEC-04 D-02 | If a client vte parser exists somewhere not found in this audit, a new prefilter would be needed |
| A2 | D-03 client-side clipboard-read rejection is a no-op confirmation (server already drops `?` form) — no active path in client that would re-emit `?` | SEC-04 D-03 | If such a path exists, it must be found and blocked |
| A3 | MAX_FRAME_LEN (16 MiB in nosh-proto) is enforced on the PtyData read path, providing the PtyData recv cap without additional code | SEC-04 D-06 | If MAX_FRAME_LEN is not applied to the control-stream PtyData reads, an explicit cap in main.rs is required |
| A4 | `TerminalControlPayload::Hyperlink` is safe to append as a new variant without breaking existing codec tests (postcard append-only) | SEC-04 D-07 | The codec discriminant test may need updating even for the nested enum |

**If this table is empty:** not the case — four assumptions above require planner verification.

---

## Open Questions

1. **Is D-02 a client-side gate or a confirmation of the server-side prefilter?**
   - What we know: no client vte parser found in this audit; CONTEXT.md says "before vte::Parser::advance()"
   - What's unclear: whether the intent was to add a client-side parser+gate, or to confirm the server gate suffices
   - Recommendation: planner should confirm with the user — if client-side vte is intended for future scrollback rendering, add a placeholder gate now; if not, document the server-side gate as satisfying D-02

2. **OSC 8 scheme whitelist: forward as Hyperlink variant or emit raw OSC 8?**
   - What we know: no OSC 8 currently decoded; D-07 says whitelist and pass through
   - What's unclear: whether "pass through" means emit `\x1b]8;;<url>\x07` raw to local terminal or use a new `TerminalControlPayload::Hyperlink` variant
   - Recommendation: use a new variant — it gives the whitelist check a named type-level home and avoids raw byte emission for a security-sensitive feature

3. **Does the `TerminalControlPayload::Hyperlink` addition require a protocol version bump?**
   - What we know: postcard appends safely; existing `Ok(_) => {}` arm in client absorbs unknown variants gracefully
   - What's unclear: whether an old client connected to a new server receiving an unknown TerminalControl variant would crash or gracefully ignore it
   - Recommendation: the `Ok(_) => {}` arm handles this; no protocol bump needed

---

## Sources

### Primary (HIGH confidence — source code read in this session)
- `crates/nosh-server/src/terminal.rs` — `TerminalState::advance`, `osc_prefilter`, `osc_dispatch`, DCS no-op comment (line 1182)
- `crates/nosh-client/src/main.rs` — pump loop, `TerminalControl` dispatch, title stripping, clipboard stripping
- `crates/nosh-client/src/screen.rs` — `ClientScreen::apply`, `MAX_TERMINAL_COLS=512`, `MAX_TERMINAL_ROWS=256`
- `crates/nosh-proto/src/messages.rs` — `TerminalControlPayload` variants, channel-ID parity rules
- `crates/nosh-server/src/server.rs` — channel-ID parity validation (line ~1049)
- `fuzz/fuzz_targets/osc_accumulation.rs` — deterministic 10 MiB test inside fuzz harness
- `.github/workflows/ci.yml` — existing CI jobs, gate pattern

### Primary (HIGH confidence — first-party security docs)
- `docs/999.7-SECURITY.md` — prefilter design, regression test reference, constants table
- `docs/999.1-SECURITY.md` — pre-auth caps (AuthLimits max_concurrent=64, auth_timeout=5s), datagram buffers, residual risks

### Primary (HIGH confidence — measured in this session)
- `cargo test -p nosh-server oversized_multi_chunk_osc_is_bounded_then_resyncs` → PASS (1.41 s) [VERIFIED: measured]
- `cargo +nightly fuzz run osc_accumulation -- -max_len=2097152 -max_total_time=60` → zero crashes, 142,884 corpus files loaded, LIBFUZZER_MAX_LEN env var silently ignored [VERIFIED: measured]

### Secondary (HIGH confidence — planning docs)
- `.planning/phases/27-security-hardening-pass/27-CONTEXT.md` — all decisions D-01..D-09
- `.planning/REQUIREMENTS.md` — SEC-01, SEC-04, SEC-05 definitions
- `.planning/research/PITFALLS.md` — SEC-2 title/clipboard injection attack patterns

---

## Metadata

**Confidence breakdown:**
- OSC prefilter audit: HIGH — read source, ran test, confirmed category-agnostic design
- SEC-04 landing points: HIGH — read all relevant source; two gaps identified (title \r/\n, selection whitelist) with exact line numbers
- SEC-04 D-02/D-03 client-side: MEDIUM — no client vte parser found, but interpretation of D-02 has ambiguity (assumption A1)
- SEC-01 structure: HIGH — Phase 24/25 topology fully understood; STRIDE structure validated against existing docs
- Fuzz result: MEDIUM — corpus replay shows zero crashes; raised max_len mutation incomplete due to corpus-load time in 60s window; deterministic 10 MiB path confirmed via harness code

**Research date:** 2026-06-14
**Valid until:** 2026-07-14 (stable domain; vte API unlikely to change in 30 days)
