# Phase 16: QoL Feature Pack + Windows CI Gate - Context

**Gathered:** 2026-06-01
**Status:** Ready for planning

<domain>
## Phase Boundary

Day-to-day ergonomics: connection-loss banner, OSC 52 clipboard passthrough (write-only),
terminal-title propagation, `--predict` mode flag + `--status` RTT surfacing — and a native
Windows CI gate that runs on every push. Requirements: QOL-01..04, HARDEN-02, HARDEN-03.
Depends on Phase 14 (the confirmed datagram path + compositor + ConnectionLossOverlay stub).

</domain>

<decisions>
## Implementation Decisions

### OSC 52 clipboard passthrough (D-16-01, QOL-02)
- **D-16-01:** **Re-emit OSC 52 to local stdout** — the client writes the OSC 52 set-sequence
  to its own stdout and the user's real terminal emulator applies it to the local clipboard.
  No new dependency, no OS clipboard code, naturally write-only, cross-platform. It is an
  out-of-band control sequence (paints no cells) so it is emitted directly to stdout WITHOUT
  going through the datagram compositor (the "single composition path" rule is about display
  cells, not terminal control sequences).
- **D-16-01a (security, write-only):** The SERVER (Phase 12 detects OSC 52 at osc_dispatch)
  forwards only clipboard-WRITE payloads over the RELIABLE STREAM (no MTU limit — clipboard
  data can exceed a datagram). It MUST NEVER forward the OSC 52 read/query form (`OSC 52 ; c ; ?`)
  — honoring a read would let the remote exfiltrate the LOCAL clipboard. Read is never honored
  (REQUIREMENTS.md non-goal). Carrier: reliable stream control message (new Message variant).
- **D-16-01b (caveat documented):** Relies on the local terminal supporting OSC 52
  (iTerm2/kitty/wezterm/recent tmux do; some disable by default). Acceptable per the standard
  tmux approach. arboard-crate direct-OS-clipboard was rejected (extra dep + headless/Wayland
  edge cases).

### Terminal-title propagation (D-16-02, QOL-03)
- **D-16-02:** **Re-emit OSC 0/2 to local stdout** (same passthrough mechanism as D-16-01).
  Server does not strip OSC 0/2; the title reaches the client and is emitted to stdout so the
  local tab reflects the remote context. Out-of-band control (no compositor involvement).
  Carrier: reliable stream control message (consistent with OSC 52). Rejected: carrying title
  in the datagram StateDiff (bloats the wire type for a rarely-changing ephemeral value).

### Connection-loss overlay (D-16-03, QOL-01)
- **D-16-03:** Activate the Phase 14 `ConnectionLossOverlay` (no-op stub → live). When no
  datagram is received for >5 s, the compositor renders an unobtrusive overlay at ROW 0 with an
  elapsed "last contact" counter and `Press ~. to disconnect`; it clears automatically when
  datagram traffic resumes. Rendered as an OVERLAY LAYER through the Phase 14 compositor
  (D-14-01a) — not a direct stdout write — so it composes over the confirmed screen and is
  removed cleanly on resume.
- **D-16-03a:** The `~.` escape (ssh-style disconnect) handler is wired in the client input
  path if not already present; the overlay text advertises it. Exact escape-state machine is
  Claude's discretion (mirror ssh's newline-`~`-`.` recognition).

### Windows CI gate (D-16-04, HARDEN-02)
- **D-16-04:** New `.github/workflows/ci.yml` with TWO jobs on every push: (1) a Linux job
  (`cargo build` + `cargo test` — the primary gate) and (2) a `build-windows` job on
  `windows-latest` building `nosh-client` for `x86_64-pc-windows-msvc`. **Retire**
  `windows-cross.yml` (native windows-latest catches MSVC/winapi issues the Linux cross-compile
  misses). Not false-green: the windows job must actually compile, failing the run on error.
- **D-16-04a (HARDEN-03, WSAEMSGSIZE):** Resolve or deliberately suppress the `quinn_udp`
  `WSAEMSGSIZE` warning on Windows (e.g. a `quinn_udp=error` tracing filter on Windows) with
  the rationale + upstream issue reference recorded in a code comment.
- **D-16-04b (PREREQUISITE — verification gating):** The `origin` remote exists
  (github.com/bharrisau/nosh) and read access works, BUT `origin/main` is stale (`f83093e`) with
  ~59 unpushed local commits and `gh` is not installed / push not verified from the build
  sandbox. HARDEN-02's "CI runs on every push, not false-green" is VERIFIABLE ONLY after the
  USER pushes the branch to GitHub and Actions runs green. Phase 16 authors `ci.yml`; final
  HARDEN-02 sign-off is a human-verification item pending the user's push + a green Actions run.
  Setup the user must do: push commits, ensure Actions enabled (Settings→Actions), note repo
  visibility (public=free Windows minutes; private=2× rate). No repo secrets needed (build-only).

### --predict flag + --status (D-16-05, QOL-04 + goal)
- **D-16-05:** Add `--predict <adaptive|always|never>` (default **adaptive**) Mosh-style flag:
  adaptive = RTT-gated (Phase 15 default), always = predict regardless of RTT (testing/high
  latency), never = disable. The flag is WIRED now; the Phase 15 predictor reads it. Add
  `--status` to surface the measured SRTT in the terminal title (QOL-04), reusing the SRTT
  already tracked for adaptive prediction. clap is already the arg parser (main.rs).

### Claude's Discretion
- New reliable-stream Message variant(s) for clipboard/title forwarding (or one combined
  "terminal control" passthrough message); exact overlay rendering (row-0 layer) and elapsed
  formatting; `~.` escape state machine; where `--status` writes the RTT (title vs status line).

</decisions>

<specifics>
## Specific Ideas

- OSC 52 + OSC 0/2 are both handled by the SAME pattern: server detects at osc_dispatch
  (Phase 12), forwards over the reliable stream, client re-emits to stdout. One mechanism.
- The connection-loss overlay is the first real consumer of the Phase 14 compositor overlay
  layer — validates that the compositor seam (D-14-01a) works before Phase 15's prediction
  overlay piles on.

</specifics>

<canonical_refs>
## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### Requirements & success criteria
- `.planning/REQUIREMENTS.md` — QOL-01, QOL-02, QOL-03, QOL-04, HARDEN-02, HARDEN-03 (+ the
  non-goals table: OSC 52 read never honored; full scrollback sync deferred to M5).
- `.planning/ROADMAP.md` Phase 16 section (5 success criteria).

### Code to build on
- Phase 12 `crates/nosh-server/src/terminal.rs` — OSC 52 / OSC 0/2 detection at osc_dispatch
  (the source of the sequences to forward; ensure read-form is distinguishable from write-form).
- Phase 14 `crates/nosh-client/src/...ClientScreen` — the compositor + `ConnectionLossOverlay`
  stub to activate; the single render path.
- `crates/nosh-proto/src/messages.rs` — add the clipboard/title forwarding Message variant(s).
- `crates/nosh-client/src/main.rs` — clap `Args` (~271-300) for `--predict`/`--status`.
- `crates/nosh-client/src/client.rs` — datagram silence timer (>5s) feeding the overlay; SRTT
  source for `--status`; `~.` escape handling in the input path.
- `.github/workflows/windows-cross.yml` (existing — to be replaced by `ci.yml`).

### Architecture / prior art
- `CLAUDE.md` — OSC 52 + agent/forwarding security notes; datagram vs reliable split.
- `.planning/research/FEATURES.md` (QoL features), `PITFALLS.md` (WSAEMSGSIZE / quinn_udp on
  Windows; OSC 52 read exfiltration), `STACK.md`.

</canonical_refs>

<code_context>
## Existing Code Insights

### Reusable Assets
- Phase 12 osc_dispatch detection; Phase 14 compositor overlay layer + ConnectionLossOverlay stub.
- SRTT already tracked (for adaptive prediction / migration) — reused for --status.
- clap Args struct in main.rs — extend with --predict/--status.
- Existing windows-cross.yml as a reference for the cargo/toolchain setup in the new ci.yml.

### Established Patterns
- Reliable-stream Message enum + codec (postcard) for the new forwarding variant(s).
- crossterm raw-mode/stdout setup already configured for ANSI passthrough.

### Integration Points
- Server: forward OSC 52 (write-only) + OSC 0/2 over reliable stream.
- Client: re-emit to stdout; datagram-silence timer → overlay layer; --predict wired to Phase 15.
- CI: .github/workflows/ci.yml (linux + windows-latest jobs).

</code_context>

<deferred>
## Deferred Ideas

- Speculative echo (the predictor that --predict actually controls) — Phase 15.
- OSC 52 clipboard READ (paste remote→local) — explicit NON-GOAL (security; never implement).
- Full native scrollback sync — M5.
- Live Windows predictive-echo validation on a physical machine — Phase 17.

</deferred>

---

*Phase: 16-qol-feature-pack-windows-ci-gate*
*Context gathered: 2026-06-01*
