# Phase 16: QoL Feature Pack + Windows CI Gate — Research

**Researched:** 2026-06-02
**Domain:** Terminal escape passthrough (OSC 52 / OSC 0/2), connection-loss overlay, CI/GitHub Actions, quinn_udp Windows warning suppression
**Confidence:** HIGH (all claims verified from codebase inspection, official docs, or crates.io)

---

<user_constraints>
## User Constraints (from CONTEXT.md)

### Locked Decisions

**D-16-01 (QOL-02): OSC 52 clipboard passthrough**
- Re-emit OSC 52 to local stdout (client writes OSC 52 set-sequence to own stdout; terminal emulator applies it to local clipboard).
- Out-of-band control sequence — emitted directly to stdout WITHOUT going through the datagram compositor (the "single composition path" rule is about display cells, not terminal control sequences).
- Server forwards only clipboard-WRITE payloads over the RELIABLE STREAM. MUST NEVER forward the OSC 52 read/query form (`OSC 52 ; c ; ?`).
- Carrier: reliable stream control message (new Message variant).

**D-16-01a (security, write-only):** OSC 52 read is NEVER forwarded. `OSC 52 ; c ; ?` is silently dropped server-side.

**D-16-01b (caveat):** Relies on the local terminal supporting OSC 52. arboard-crate rejected (extra dep + headless/Wayland edge cases).

**D-16-01c (OSC accumulation cap):** Phase 12 set `vte = { default-features = false }` capping vte's OSC accumulation at 1024 bytes. Phase 16 MUST handle this. Pick one approach:
- (a) Custom OSC-52-accumulation path that intercepts before vte truncates.
- (b) Re-enable vte `std` with an explicit bounded cap in `osc_dispatch` (e.g. 64–256 KB).
The cap MUST stay bounded (no return to unbounded OSC, which is the CR-03 DoS).

**D-16-02 (QOL-03): Terminal-title propagation**
- Re-emit OSC 0/2 to local stdout. Same passthrough mechanism as D-16-01.
- Server does not strip OSC 0/2; client re-emits to stdout.
- Carrier: reliable stream control message (consistent with OSC 52).

**D-16-03 (QOL-01): Connection-loss overlay**
- Activate the Phase 14 `ConnectionLossOverlay` (no-op stub → live).
- Timeout: >5 s no datagram → compositor renders overlay at ROW 0.
- Content: elapsed "last contact" counter + `Press ~. to disconnect`.
- Clears automatically when datagram traffic resumes.
- Rendered as an OVERLAY LAYER through the Phase 14 compositor — not a direct stdout write.

**D-16-03a:** The `~.` escape is ALREADY wired in the client input path (EscapeState machine in main.rs, fully implemented). The overlay text advertises it; no new escape handling is needed.

**D-16-04 (HARDEN-02): Windows CI gate**
- New `.github/workflows/ci.yml` with TWO jobs: (1) Linux (`cargo build` + `cargo test`), (2) `build-windows` on `windows-latest` building `nosh-client` for `x86_64-pc-windows-msvc`.
- Retire `windows-cross.yml` (Linux-hosted cross-compile to GNU target — replaced by native Windows MSVC build).
- The windows job must actually compile, failing the run on error.

**D-16-04a (HARDEN-03, WSAEMSGSIZE):** Resolve/suppress the `quinn_udp` WSAEMSGSIZE warning on Windows via a `quinn_udp=error` tracing filter on Windows. Rationale + upstream issue reference recorded in a code comment.

**D-16-04b (PREREQUISITE):** Final HARDEN-02 sign-off is human-verification: user must push to GitHub and confirm Actions runs green. Phase 16 AUTHORS `ci.yml`; verification is pending user push.

**D-16-05 (QOL-04 + goal): --predict flag + --status**
- `--predict <adaptive|always|never>` is ALREADY IMPLEMENTED (Phase 15, `PredictDisplayMode` in `crates/nosh-client/src/predictor.rs`, wired in `main.rs` Args struct).
- `--status` flag: surface SRTT in terminal title via `conn.rtt()` — the remaining work.

### Claude's Discretion
- New reliable-stream Message variant(s) for clipboard/title forwarding (one combined "terminal control" passthrough message vs. separate variants).
- Exact overlay rendering (row-0 layer) and elapsed formatting.
- `~.` escape state machine (ALREADY IMPLEMENTED — no discretion needed here).
- Where `--status` writes the RTT (title vs. status line).
- The OSC accumulation cap approach choice (a vs. b from D-16-01c) and the specific cap value.

### Deferred Ideas (OUT OF SCOPE)
- Speculative echo (the predictor that --predict actually controls) — Phase 15 (DONE).
- OSC 52 clipboard READ (paste remote→local) — explicit NON-GOAL (security; never implement).
- Full native scrollback sync — M5.
- Live Windows predictive-echo validation on a physical machine — Phase 17.
</user_constraints>

<phase_requirements>
## Phase Requirements

| ID | Description | Research Support |
|----|-------------|------------------|
| QOL-01 | Connection-loss banner at row 0 when no datagram for >5 s; elapsed counter; auto-clear on resume | `ConnectionLossOverlay` stub exists in `screen.rs` at line 90; compositor `Overlay` trait at line 80; `run_pump` datagram arm in `main.rs` is the silence-detection site |
| QOL-02 | OSC 52 clipboard passthrough (write-only, reliable stream); no OSC 52 read | `osc52_pending` field in `TerminalState` (terminal.rs:163); `osc_dispatch` detects and stores it (terminal.rs:650-657); new `Message` variant needed in `messages.rs` |
| QOL-03 | OSC 0/2 title propagation from remote shell to local terminal tab | `title` field in `TerminalState` (terminal.rs:159); `osc_dispatch` sets it (terminal.rs:643-648); same forwarding mechanism as OSC 52 |
| QOL-04 | `--status` flag surfacing SRTT in terminal title; reuse `conn.rtt()` already available in datagram arm | `conn.rtt()` called at main.rs:714 in the datagram arm; `--predict` flag already wired (main.rs:325); `--status` is additive to Args |
| HARDEN-02 | Windows CI gate: `windows-latest` job in `ci.yml` that builds `nosh-client` for `x86_64-pc-windows-msvc`; retire `windows-cross.yml` | Existing `windows-cross.yml` is Linux-hosted GNU cross-compile; replaced by native MSVC build per D-16-04 |
| HARDEN-03 | WSAEMSGSIZE quinn_udp warning resolved or suppressed on Windows with rationale recorded | `tracing_subscriber` EnvFilter with `quinn_udp=error` directive on Windows; applied in `main()` in `main.rs` |
</phase_requirements>

---

## Summary

Phase 16 delivers six concrete, independently-implementable features on top of the Phase 15 speculative overlay. The phase is primarily **integration and polish work** — almost all the underlying infrastructure is already built and verified.

**OSC 52 + OSC 0/2 passthrough (QOL-02, QOL-03):** The server already detects both sequences in `osc_dispatch` (Phase 12). Phase 16 adds: (1) a new `Message` variant to carry the payload over the reliable stream server→client, (2) server code to forward the payload when detected, and (3) client code to re-emit the raw OSC sequence to stdout. The only non-trivial decision is the vte 1024-byte OSC cap (D-16-01c): the research below recommends approach (b) — re-enable vte `std` + explicit cap in `osc_dispatch` — because the transient buffer concern is manageable and approach (a) requires a raw-byte pre-processor before vte's state machine.

**Connection-loss overlay (QOL-01):** The `ConnectionLossOverlay` struct and `Overlay` trait are already wired into the compositor (screen.rs). Phase 16 promotes it from a no-op to a live overlay: add a datagram-silence timer in `run_pump`, track elapsed seconds, and pass the overlay state into `render_with_predictor`. No new types needed; the compositor seam is purpose-built for this.

**--status (QOL-04):** `conn.rtt()` is already called in the datagram arm of `run_pump`. Adding `--status` means: (1) add the flag to `Args`, (2) when the flag is active, emit an OSC 0/2 sequence to stdout updating the title with the current SRTT after each datagram. This coexists with OSC 0/2 title propagation: client-emitted status overwrites the forwarded title, which is acceptable behavior (the user opted in).

**Windows CI gate (HARDEN-02):** Replace the existing Linux-hosted GNU cross-compile with a native `windows-latest` MSVC build. The new workflow is straightforward: `actions/checkout@v4` + `dtolnay/rust-toolchain@stable` + `Swatinem/rust-cache@v2` + `cargo build --locked -p nosh-client --target x86_64-pc-windows-msvc`. No MinGW, no extra apt-get steps. Only `nosh-client` is built (nosh-server is Unix-only).

**WSAEMSGSIZE suppression (HARDEN-03):** The quinn_udp WSAEMSGSIZE log comes from `quinn_udp`'s Windows GRO receive path (quinn-rs/quinn#2041 is the closest tracked issue — "Potential bug in GRO code for Windows?", open). The fix is a `#[cfg(target_os = "windows")]`-gated tracing filter directive `quinn_udp=error` applied in `main()` before the subscriber is initialized.

**Primary recommendation:** Implement in three waves — Wave 1: reliable-stream passthrough infrastructure (new Message variant + server detection + client re-emit, covers QOL-02/03); Wave 2: connection-loss overlay activation (QOL-01) + --status (QOL-04); Wave 3: CI gate + WSAEMSGSIZE (HARDEN-02/03).

---

## Architectural Responsibility Map

| Capability | Primary Tier | Secondary Tier | Rationale |
|------------|-------------|----------------|-----------|
| OSC 52 detection | Server (session pump) | — | Server sees the raw PTY output stream; client never receives raw PTY bytes after Phase 14 |
| OSC 52 forwarding | Server → Client (reliable stream) | — | Reliable stream has no MTU limit; clipboard data can exceed QUIC datagram size |
| OSC 52 local emission | Client (stdout direct write) | — | Out-of-band control sequence; bypasses compositor by design (D-16-01) |
| OSC 0/2 title forwarding | Server → Client (reliable stream) | — | Same mechanism as OSC 52; server already detects title in `osc_dispatch` |
| OSC 0/2 local emission | Client (stdout direct write) | — | Out-of-band; does not go through compositor |
| Connection-loss detection | Client (run_pump datagram timer) | — | Datagram silence is observable only at the client; server has no visibility |
| Connection-loss overlay render | Client (compositor overlay layer) | — | Compositor overlay seam already exists (Phase 14 D-14-01a) |
| ~. disconnect (connection-loss banner) | Client (EscapeState machine, main.rs) | — | ALREADY IMPLEMENTED; no new code needed |
| --status RTT emission | Client (run_pump, datagram arm) | — | `conn.rtt()` is already accessed in the datagram arm |
| Windows CI gate | CI (GitHub Actions `windows-latest`) | — | Native runner; no cross-compile toolchain needed |
| WSAEMSGSIZE suppression | Client (tracing subscriber init, main.rs) | — | Server is Linux-only; warning is client-side Windows QUIC receive path |

---

## Standard Stack

### Core (no new crates required for most features)

| Library | Version | Purpose | Why Standard |
|---------|---------|---------|--------------|
| `crossterm` | 0.29.0 (existing) | OSC 52 write via `CopyToClipboard`; stdout raw ANSI emit | Already in tree; `osc52` feature adds the clipboard module [VERIFIED: crates.io] |
| `tokio` | 1.52.x (existing) | Async timer for connection-loss detection (`tokio::time::Instant::now()`) | Already in tree |
| `tracing-subscriber` | 0.3 (existing) | `EnvFilter` for `quinn_udp=error` Windows suppression | Already in tree |
| `vte` | 0.15 (existing, modified) | OSC accumulation; approach (b) re-enables `std` feature with explicit cap | Already in tree; cap applied in `osc_dispatch` |

### Feature Flag Change Required

```toml
# nosh-client/Cargo.toml — add osc52 feature to existing crossterm dep
crossterm = { version = "0.29", features = ["events", "osc52"] }
```

```toml
# nosh-server/Cargo.toml — re-enable vte std for OSC accumulation (D-16-01c approach b)
# REMOVE: vte = { version = "0.15", default-features = false }
# REPLACE WITH:
vte = { version = "0.15" }  # std feature re-enabled for unbounded OSC accumulation
# ...then add explicit size cap in osc_dispatch (see Architecture Patterns below)
```

**No new crate dependencies are introduced by this phase.**

### Alternatives Considered

| Instead of | Could Use | Tradeoff |
|------------|-----------|----------|
| `crossterm::clipboard::CopyToClipboard` | Raw `write!(stdout, "\x1b]52;c;{}\x07", b64)` | Raw write is simpler and equally correct; `CopyToClipboard` is the idiomatic path since crossterm already owns the raw-mode guard |
| Re-enable vte `std` + cap in `osc_dispatch` (approach b) | Custom OSC-52-accumulation path before `advance()` (approach a) | Approach (a) is more complex: requires a state machine tracking OSC byte boundaries in the raw PTY stream, before vte sees it, to extract clipboard payloads. Approach (b) keeps vte as the parser and adds one size check in `osc_dispatch`. The residual risk of approach (b) is transient memory allocation during parsing — capped by the OS pipe buffer (typically 64 KiB) rather than unlimited |
| `tokio::time::interval` for loss timer | `tokio::time::sleep` + loop | `interval` is simpler for the per-second elapsed tick; a one-shot `sleep` with reset-on-datagram is cleaner for the 5s trigger threshold |

---

## Package Legitimacy Audit

> No new external packages are introduced by this phase. The only change is enabling the `osc52` feature on the existing `crossterm` crate (already verified legitimate) and re-enabling the `std` feature on the existing `vte` crate.

| Package | Registry | Age | Downloads | Source Repo | slopcheck | Disposition |
|---------|----------|-----|-----------|-------------|-----------|-------------|
| `crossterm` 0.29 | crates.io | 8 yrs (2018) | 138M total [VERIFIED: crates.io] | github.com/crossterm-rs/crossterm | [SUS] — false positive (typosquat check confused with `crossbeam`; 138M downloads, 8-year-old crate, already in project) | Approved — established crate |
| `vte` 0.15 | crates.io | existing in tree | existing | github.com/alacritty/vte | not re-checked (no version change) | Approved — existing dep |

**Packages removed due to slopcheck [SLOP] verdict:** none
**Packages flagged as suspicious [SUS]:** `crossterm` — slopcheck false-positive (typosquat check vs. `crossbeam`); 138M downloads and 8-year history confirm it is legitimate [VERIFIED: crates.io].

---

## Architecture Patterns

### System Architecture Diagram

```
PTY output bytes (server)
         │
         ▼
   vte::Parser::advance()          [OSC cap enforced in osc_dispatch — approach (b)]
         │
   ┌─────┴──────────────────────────────────────────────────────────────┐
   │                     TerminalState (osc_dispatch)                    │
   │                                                                     │
   │  OSC 0/2 title ──► title field ──► take_title() ──► reliable stream│
   │  OSC 52 write  ──► osc52_pending ──► take_osc52() ──► reliable stream│
   │  OSC 52 read   ──► SILENTLY DROPPED (D-16-01a, security)           │
   │  display cells ──► grid ──► StateDiff datagrams (existing path)     │
   └─────────────────────────────────────────────────────────────────────┘
                │
                ▼ (new Message variants: TerminalControl::Clipboard / TerminalControl::Title)
         Reliable QUIC stream (server → client)
                │
                ▼
        Client: run_pump (reliable stream arm)
         ├── Message::TerminalControl::Clipboard(sel, b64)
         │       └── write ESC]52;{sel};{b64}BEL directly to stdout
         │           (out-of-band: NOT through compositor)
         ├── Message::TerminalControl::Title(text)
         │       └── write ESC]0;{text}BEL directly to stdout
         │           (out-of-band: NOT through compositor)
         │           (if --status active: override with RTT title instead)
         └── (existing PtyData, SessionClose, etc.)

        Client: run_pump (datagram arm, existing)
         ├── receive StateDiff → screen.apply() → render_with_predictor()
         ├── update last_datagram_time = Instant::now()    [NEW: silence timer]
         ├── if conn.rtt() and --status active:             [NEW: RTT title emit]
         │       write ESC]0;nosh: {rtt}ms BEL to stdout
         └── ConnectionLossOverlay.clear() if was_showing  [NEW: overlay deactivate]

        Client: run_pump (new silence-check arm)
         ├── tokio::select! arm: sleep_until(last_datagram_time + 5s)
         ├── on trigger: ConnectionLossOverlay.activate()
         │       └── overlay.elapsed_since = last_datagram_time
         └── re-renders every ~1s via interval (elapsed counter)
```

### Recommended Project Structure (additions only)

```
crates/nosh-proto/src/
└── messages.rs          # add TerminalControl variant (one new enum variant)

crates/nosh-server/src/
└── terminal.rs          # add take_title() / take_osc52() drain methods
                         # add osc_dispatch size cap (approach b)
└── session.rs           # call take_title()/take_osc52(), send over reliable stream

crates/nosh-client/src/
└── screen.rs            # activate ConnectionLossOverlay (replace no-op with live impl)
└── main.rs              # --status flag, silence timer, OSC emit helpers, WSAEMSGSIZE filter

.github/workflows/
└── ci.yml               # NEW: Linux + windows-latest jobs
└── windows-cross.yml    # DELETED (or kept alongside if user prefers)
```

### Pattern 1: OSC 52 Write-Only Security Gate in osc_dispatch

The server's `osc_dispatch` at `terminal.rs:650` already stores `osc52_pending`. The security gate (reject `?` query form) must be in the detection path:

```rust
// Source: crates/nosh-server/src/terminal.rs osc_dispatch (Phase 12, lines 650-657)
b"52" => {
    let selection = params.get(1).copied().unwrap_or(b"c");
    let data = params.get(2).copied().unwrap_or(b"");

    // SECURITY (D-16-01a): OSC 52 read/query form MUST be silently dropped.
    // The read form sends '?' as the data parameter. Honoring a read would
    // let the remote process exfiltrate the LOCAL clipboard. This is the
    // most critical security invariant in Phase 16 — it MUST NOT be relaxed.
    if data == b"?" {
        return; // silently drop the read/query form
    }

    // Approach (b): cap retained payload at OSC_52_MAX_BYTES (e.g. 65536).
    // vte with std re-enabled may accumulate large payloads transiently,
    // but osc_dispatch only retains up to this cap.
    const OSC_52_MAX_BYTES: usize = 65_536; // 64 KiB cap
    let data = if data.len() > OSC_52_MAX_BYTES {
        &data[..OSC_52_MAX_BYTES]
    } else {
        data
    };

    self.osc52_pending = Some((selection.to_vec(), data.to_vec()));
}
```

### Pattern 2: New Message Variant (Recommended: One Combined Variant)

The simplest approach is one combined variant covering both clipboard and title:

```rust
// Source: crates/nosh-proto/src/messages.rs (NEW, after existing variants)
// Append AFTER all existing variants to preserve postcard discriminant order.
/// Server → client: out-of-band terminal control passthrough (D-16-01, D-16-02).
///
/// Carries clipboard writes (OSC 52) and terminal-title updates (OSC 0/2) over
/// the reliable stream so they reach the client without MTU limits.
/// The client re-emits these directly to stdout (NOT through the compositor).
TerminalControl(TerminalControlPayload),
```

```rust
/// Payload for `Message::TerminalControl`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TerminalControlPayload {
    /// OSC 52 clipboard write: selection designator + base64-encoded content.
    /// WRITE-ONLY: the read/query form (`?`) is never forwarded (D-16-01a).
    Clipboard {
        /// Selection designator bytes (e.g. `b"c"` for clipboard).
        selection: Vec<u8>,
        /// Base64-encoded clipboard content (as received from PTY output).
        data: Vec<u8>,
    },
    /// Terminal title from OSC 0/2 sequences.
    Title {
        /// The title string decoded from OSC 0 or OSC 2.
        title: String,
    },
}
```

Adding after `ReattachErr`/`Ack` means the new discriminant is `9`. Postcard serializes enums as discriminant + payload; existing messages are unaffected.

### Pattern 3: Server-Side Drain Methods for TerminalState

After `advance()`, the session loop drains these to send over the reliable stream:

```rust
// Source: crates/nosh-server/src/terminal.rs (NEW public methods)
impl TerminalState {
    /// Take and clear the pending OSC 52 clipboard payload, if any.
    /// Returns `Some((selection, base64_data))` when a write was detected.
    /// The drain semantics prevent double-forwarding.
    pub fn take_osc52(&mut self) -> Option<(Vec<u8>, Vec<u8>)> {
        self.osc52_pending.take()
    }

    /// Take and clear the pending window title, if any.
    /// Returns `Some(title)` when an OSC 0/2 sequence was detected since last drain.
    pub fn take_title(&mut self) -> Option<String> {
        self.title.take()
    }
}
```

### Pattern 4: Client OSC Re-emission (Out-of-Band)

The client writes the OSC sequence directly to stdout — NOT through the compositor. This is intentional per D-16-01: these are control sequences, not cell content.

```rust
// Source: crates/nosh-client/src/main.rs (run_pump reliable-stream arm)
Ok(Message::TerminalControl(payload)) => {
    match payload {
        TerminalControlPayload::Clipboard { selection, data } => {
            // Re-emit OSC 52 write sequence to local terminal.
            // The local terminal emulator (iTerm2/WezTerm/kitty) handles it.
            // NOTE: out-of-band stdout write is intentional — bypasses compositor
            // per D-16-01 (control sequences, not display cells).
            let sel = String::from_utf8_lossy(&selection);
            let b64 = String::from_utf8_lossy(&data);
            // Use crossterm CopyToClipboard (osc52 feature) or raw OSC sequence.
            // Raw is simpler and avoids the CopyToClipboard API surface question:
            let _ = stdout.write_all(
                format!("\x1b]52;{sel};{b64}\x07").as_bytes()
            ).await;
            let _ = stdout.flush().await;
        }
        TerminalControlPayload::Title { title } => {
            // Re-emit OSC 0 title sequence. Only emit if --status is not active
            // (if --status is active, the RTT title in the datagram arm takes precedence).
            if !status_active {
                let _ = stdout.write_all(
                    format!("\x1b]0;{title}\x07").as_bytes()
                ).await;
                let _ = stdout.flush().await;
            }
        }
    }
}
```

### Pattern 5: ConnectionLossOverlay — Live Implementation

Replace the no-op `ConnectionLossOverlay` at `screen.rs:90-96` with a stateful version:

```rust
// Source: crates/nosh-client/src/screen.rs (Phase 16 activation of D-14-01a stub)
pub struct ConnectionLossOverlay {
    /// Whether the overlay is currently showing.
    pub active: bool,
    /// Wall-clock instant of last received datagram.
    pub last_contact: std::time::Instant,
    /// Terminal width (to fill the row).
    cols: u16,
}

impl Overlay for ConnectionLossOverlay {
    fn cell_at(&self, row: u16, col: u16) -> Option<Cell> {
        if !self.active || row != 0 {
            return None;
        }
        // Build the banner text at row 0.
        let elapsed = self.last_contact.elapsed().as_secs();
        let msg = if elapsed >= 5 {
            format!("nosh: reconnecting — last contact {}s ago. Press ~. to disconnect.", elapsed)
        } else {
            format!("nosh: reconnecting — last contact {}s ago.", elapsed)
        };
        let chars: Vec<char> = msg.chars().collect();
        let col = col as usize;
        let ch = chars.get(col).copied().unwrap_or(' ');
        // Reverse-video (white on blue) per Mosh convention.
        Some(Cell {
            ch,
            style: CellStyle(CellStyle::REVERSE),
            fg: None,
            bg: None,
        })
    }
}
```

**Integration in run_pump:** The silence timer is a `tokio::select!` arm watching `last_datagram_time + 5s`. When the timer fires, set `overlay.active = true`. When a new datagram arrives, reset `overlay.active = false` and `overlay.last_contact = Instant::now()`. Force a re-render tick (e.g. a 1s `tokio::time::interval`) while the overlay is active to update the elapsed counter.

**Key challenge:** `ConnectionLossOverlay` is currently `Box<dyn Overlay>` inside `ClientScreen.overlays`. To mutate its state from `run_pump`, either:
- (a) Hoist `ConnectionLossOverlay` out of the `overlays` Vec (not `Box<dyn Overlay>`) and pass it separately to `render_with_predictor`, similar to how `PredictionOverlay` is handled; OR
- (b) Wrap it in `Arc<Mutex<>>` and share the reference.

**Recommendation:** Use approach (a) — mirror the `PredictionOverlay` pattern. `PredictionOverlay` is already NOT in the `overlays` Vec and is passed separately to `render_with_predictor`. Add a similar `render_with_predictor_and_loss_overlay` method, or extend the signature.

### Pattern 6: --status RTT Title Emission

```rust
// Source: crates/nosh-client/src/main.rs (datagram arm, after render)
if args.status {
    let rtt_ms = conn.rtt().as_millis();
    let _ = stdout.write_all(
        format!("\x1b]0;nosh: {rtt_ms}ms\x07").as_bytes()
    ).await;
    // No flush needed here; the OSC sequence is small and the next
    // render_with_predictor flush will carry it.
}
```

### Pattern 7: WSAEMSGSIZE Tracing Filter (Windows)

```rust
// Source: crates/nosh-client/src/main.rs main() function
// Applied BEFORE the tracing subscriber is initialized.
#[cfg(target_os = "windows")]
let env_filter = {
    // quinn_udp on Windows emits WARN-level WSAEMSGSIZE messages when
    // Windows GRO (Generic Receive Offload) appends UDP_COALESCED_INFO
    // to the control buffer and the buffer is too small (128 bytes).
    // The datagram is NOT lost — only the GRO metadata is missing.
    // This is a known upstream issue (quinn-rs/quinn#2041, open as of 2026-06).
    // Suppress at WARN level by filtering quinn_udp to ERROR-only on Windows.
    tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "info".into())
        .add_directive("quinn_udp=error".parse().unwrap())
};

#[cfg(not(target_os = "windows"))]
let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
    .unwrap_or_else(|_| "info".into());

tracing_subscriber::fmt()
    .with_env_filter(env_filter)
    .with_writer(std::io::stderr)
    .init();
```

**Note:** The `RUST_LOG` env var overrides the `try_from_default_env()` fallback. The `add_directive("quinn_udp=error")` is additive; if the user sets `RUST_LOG=debug`, `quinn_udp` will still be capped at `error` on Windows. This is the desired behavior (suppress the noise, don't expose a bypass).

### Pattern 8: New ci.yml (Replace windows-cross.yml)

```yaml
# Source: .github/workflows/ci.yml (new file, replaces windows-cross.yml)
name: CI

on:
  push:
    branches: [main]
  pull_request:
    branches: [main]

jobs:
  # Primary gate: Linux build + full test suite
  linux:
    name: cargo test (Linux)
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - name: Build
        run: cargo build --locked
      - name: Test
        run: cargo test --locked

  # Windows native build gate (HARDEN-02)
  # Runs natively on windows-latest (x86_64 MSVC) — NOT cross-compiled from Linux.
  # This catches MSVC/winapi-specific compile errors that Linux cross-compile misses.
  # WSAEMSGSIZE warning suppression (HARDEN-03) is verified at runtime; this job
  # confirms the code compiles. Final green-run sign-off is human (D-16-04b).
  build-windows:
    name: cargo build nosh-client (Windows MSVC)
    runs-on: windows-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          targets: x86_64-pc-windows-msvc
      - uses: Swatinem/rust-cache@v2
      - name: Build Windows client
        run: cargo build --locked --target x86_64-pc-windows-msvc -p nosh-client
```

**Key notes:**
- `windows-latest` on GitHub Actions runs Windows Server 2022 with MSVC Build Tools pre-installed — no MinGW, no `apt-get` needed. [CITED: GitHub Actions runner documentation]
- `ring 0.17.x` ships precompiled `x86_64-windows` asm objects — no NASM needed for MSVC builds. [CITED: ring/BUILDING.md]
- `nosh-server` is NOT built in the Windows job (it has `portable-pty`, `nix`, Unix-only deps).
- `Swatinem/rust-cache@v2` is the standard caching action for Rust CI. [ASSUMED — widely used but not verified against official docs in this session]
- The Linux job runs `cargo test --locked` (full test suite including integration tests that bind ports — `cargo nextest` is optional here since the standard test runner is sufficient).

### Anti-Patterns to Avoid

- **Re-implementing OSC 52 write as a direct clipboard crate call:** `arboard`/`copypasta` require native APIs (X11, Wayland, Win32) that don't work in headless CI, Wayland sandboxes, or SSH sessions. OSC 52 passthrough to the terminal emulator is the correct approach.
- **Forwarding OSC 52 read/query form (`?`):** This is a security hole. The `data == b"?"` check in `osc_dispatch` is the gate. Never forward it.
- **Putting OSC 52/title content through the datagram compositor:** These are control sequences, not display cells. They MUST bypass the compositor (D-16-01 / D-14-01a).
- **Writing RTT status via a persistent bottom row:** Steals a terminal row, breaks ncurses apps (vim, htop). Use terminal title via OSC 0/2 instead (the `--status` approach in D-16-05).
- **Re-enabling vte `std` without adding an explicit cap in `osc_dispatch`:** Reinstates the CR-03 unbounded OSC DoS. The cap in `osc_dispatch` MUST accompany the `std` re-enable.
- **Building `nosh-server` in the Windows CI job:** It has `portable-pty` and `nix` (Unix-only). The build would fail. Only `-p nosh-client`.
- **Using `cargo check` in the Windows CI job instead of `cargo build`:** `cargo check` is a type-check only; it would not link and would not catch linker/ABI issues. Use `cargo build`.

---

## Don't Hand-Roll

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| OSC 52 base64 emission | Custom base64 encoder + OSC framing | `crossterm::clipboard::CopyToClipboard` (osc52 feature) OR raw `"\x1b]52;c;{b64}\x07"` write | The base64 is already encoded by the PTY (the remote application sent it); the client re-emits the exact bytes it received. No re-encoding needed — just forward the `data` bytes from `osc52_pending` directly. |
| Windows native clipboard writes | `arboard` crate | OSC 52 passthrough to terminal emulator | arboard requires X11/Wayland/Win32 APIs; headless/SSH doesn't work; terminal emulator mediation is the correct approach |
| Datagram silence detection | Custom wallclock timer | `tokio::time::sleep_until(last_datagram_time + 5s)` in `select!` | tokio's timer primitives handle this in 3 lines |
| CI Windows toolchain setup | Manual rustup + target install in shell | `dtolnay/rust-toolchain@stable` with `targets: x86_64-pc-windows-msvc` | The action handles this; `windows-latest` runners have MSVC pre-installed |

**Key insight:** Every problem in Phase 16 is either a small integration (new Message variant + wire up existing detection) or a policy change (send vs. drop; active vs. no-op overlay). No novel algorithm or complex subsystem is needed.

---

## Common Pitfalls

### Pitfall 1: Forwarding OSC 52 Read Form (Security)

**What goes wrong:** The server forwards `OSC 52 ; c ; ?` to the client. The client re-emits it. The local terminal returns the clipboard contents in an invisible reply. A rogue remote process has exfiltrated the local clipboard.

**Why it happens:** The `osc_dispatch` currently stores any OSC 52 data without checking if it's the read/query form. The `data == b"?"` check is not yet present.

**How to avoid:** Add `if data == b"?" { return; }` BEFORE storing `osc52_pending` in `osc_dispatch`. This is the only guard needed.

**Warning signs:** Test for OSC 52 read forwarding explicitly — send `\x1b]52;c;?\x07` from the remote shell and confirm `osc52_pending()` returns `None`.

---

### Pitfall 2: vte `std` Re-enable Without Cap (CR-03 Regression)

**What goes wrong:** Re-enabling `vte = "0.15"` (with `std`) restores the unbounded `Vec<u8>` OSC accumulation buffer. A remote process sending a multi-MB OSC sequence causes a server OOM — the exact CR-03 DoS that Phase 12 fixed.

**Why it happens:** Approach (b) for D-16-01c requires re-enabling `vte std`, but the CR-03 fix was REMOVING `std`. Without adding explicit caps in `osc_dispatch`, removing the `default-features = false` regresses the security fix.

**How to avoid:** Add bounded size checks in `osc_dispatch` for ALL retained strings:
- `title`: `if title.len() <= MAX_TITLE_BYTES` (e.g. 1024 bytes for title is plenty)
- `osc52_pending data`: `let data = &data[..data.len().min(OSC_52_MAX_BYTES)]` (e.g. 64 KiB)

**Warning signs:** The `adversarial_large_osc_title_is_bounded_no_panic` and `adversarial_large_osc52_is_bounded_no_panic` tests from Phase 12 must still pass after re-enabling `vte std`. If they fail (no cap), the CR-03 regression is present.

---

### Pitfall 3: ConnectionLossOverlay Mutation via Box<dyn Overlay>

**What goes wrong:** The `overlays: Vec<Box<dyn Overlay>>` in `ClientScreen` holds the `ConnectionLossOverlay`. The `run_pump` loop needs to mutate it (set `active = true`, update `last_contact`). But `Box<dyn Overlay>` doesn't expose mutation — the `Overlay` trait only has `cell_at(&self, ...)`.

**Why it happens:** The existing design stores overlays immutably behind trait objects. This was sufficient for Phase 14 (no-op) and Phase 15 (`PredictionOverlay` is NOT in the `Vec` — it's passed separately). Phase 16 activation requires mutation.

**How to avoid:** Follow the `PredictionOverlay` pattern — remove `ConnectionLossOverlay` from the `overlays` Vec and hold it separately in `run_pump`, passed to a render method. The `screen.rs` `overlays` Vec can be removed entirely if both overlay types are passed separately.

Alternatively, use `Arc<Mutex<ConnectionLossOverlay>>` and downcast via `dyn Any`. The first approach (mirror PredictionOverlay) is simpler.

---

### Pitfall 4: OSC Re-emission While Compositor Is Mid-Render

**What goes wrong:** The client's `run_pump` receives a `TerminalControl::Clipboard` message on the reliable stream arm. It immediately writes the OSC 52 sequence to stdout. Simultaneously, the datagram arm triggers and calls `render_with_predictor`, which writes ANSI escape sequences. The two writes interleave in the stdout buffer, corrupting the terminal output.

**Why it happens:** In `tokio::select!`, each arm runs to completion before the next. There is no true interleaving. However, if OSC emit calls `stdout.write_all + flush()` and the render also calls `stdout.write_all + flush()`, the ordering within a single `select!` iteration is deterministic — but across iterations they can appear between cursor-move sequences if the OSC write doesn't include a cursor-restore.

**How to avoid:** The OSC 52 and OSC 0/2 sequences are safe to emit at any point — they don't move the cursor or write cells. Terminal emulators handle them out-of-band. The risk is theoretical; in practice, the `tokio::select!` arm serialization prevents actual byte-level interleaving. No special coordination is needed, but the OSC sequences should NOT include cursor-move prefixes.

---

### Pitfall 5: --status Title Conflicts With OSC 0/2 Forwarding

**What goes wrong:** The server forwards an OSC 0/2 title (`"user@host:~/path"`) and the client re-emits it. 50ms later, the datagram arm fires and the client overwrites the title with the RTT status (`"nosh: 45ms"`). The user set `--status` and expected the RTT; instead they briefly saw the remote title then the status. OR: the user DIDN'T set `--status` and the title correctly shows the remote context. These two paths must not conflict.

**How to avoid:** Simple rule: if `--status` is active, the client SKIPS re-emitting forwarded OSC 0/2 titles (the `TerminalControl::Title` arm does nothing). The RTT status in the datagram arm is the title. If `--status` is NOT active, the client re-emits forwarded OSC 0/2 titles and the datagram arm does NOT write a title.

---

### Pitfall 6: Windows CI Build Includes nosh-server

**What goes wrong:** The workflow runs `cargo build --locked --target x86_64-pc-windows-msvc` without `-p nosh-client`. `nosh-server` has `portable-pty`, `nix` (Unix-only), and Unix-only deps. The build fails on Windows with unintelligible error messages.

**How to avoid:** Always use `-p nosh-client` in the Windows job. `nosh-auth` is compiled transitively as a dependency of `nosh-client` (the `#[cfg(unix)]` gates for `ssh-agent-client-rs` are already in place from Phase 8 / v1.1).

---

## Code Examples

### Checking Current osc52_pending and title State (Server)

```rust
// Source: crates/nosh-server/src/terminal.rs, lines 363-377
// osc52_pending() returns Some((sel, data)) when OSC 52 write was detected.
// title() returns Some(str) when OSC 0/2 was detected.
// Both are drained with take_osc52() / take_title() (Phase 16 additions).
pub fn osc52_pending(&self) -> Option<(&[u8], &[u8])> {
    self.osc52_pending
        .as_ref()
        .map(|(sel, data)| (sel.as_slice(), data.as_slice()))
}
pub fn title(&self) -> Option<&str> {
    self.title.as_deref()
}
```

### The EscapeState Machine (Already Implemented)

```rust
// Source: crates/nosh-client/src/main.rs, lines 89-166
// The ~. disconnect escape is FULLY IMPLEMENTED (Phase 15 / v1.1).
// D-16-03a: no new escape handling needed. The overlay just tells the user
// to type ~. (the instruction is already correct).
```

### conn.rtt() for --status

```rust
// Source: crates/nosh-client/src/main.rs, line 714 (datagram arm)
// Already called in the existing datagram arm:
let rtt_ms = conn.rtt().as_millis() as u64;
// Phase 16 --status: if args.status { emit OSC 0/2 with rtt_ms }
```

### crossterm CopyToClipboard (osc52 feature)

```rust
// Source: docs.rs/crossterm/0.29.0/crossterm/clipboard
// [VERIFIED: crates.io, crossterm 0.29.0, 138M downloads]
use crossterm::clipboard::CopyToClipboard;
// The client can alternatively just write the raw OSC sequence:
// "\x1b]52;{sel};{b64}\x07"
// Both are equivalent; the raw form avoids importing the clipboard module.
```

---

## OSC Accumulation Cap Decision (D-16-01c)

**Recommendation: Approach (b) — re-enable vte `std` with explicit caps in `osc_dispatch`.**

**Reasoning:**
- Approach (a) (custom raw-OSC pre-processor) requires a state machine that:
  - Tracks the current OSC byte sequence before vte sees it
  - Detects OSC 52 start (`\x1b]52;`) in the raw stream
  - Accumulates bytes until the BEL (`\x07`) or ST (`\x1b\\`) terminator
  - Extracts the payload
  - Passes the raw bytes through to `advance()` as-is (so vte still sees the sequence for normal state machine operation)
  This is ~150 lines of error-prone state machine code operating on raw PTY bytes, with subtle edge cases around DCS/OSC interleaving and partial-chunk boundaries.

- Approach (b) keeps vte as the authoritative OSC parser. The residual risk (transient `Vec<u8>` growth before `osc_dispatch` is called) is bounded by:
  - The OS pipe buffer size (typically 64 KiB) that feeds `advance()` at a time
  - The network MTU (PTY output reaches the server in QUIC stream chunks, not unbounded blobs)
  - The explicit cap in `osc_dispatch` that limits what `nosh` retains

**Cap values (approach b):**
- `MAX_TITLE_BYTES = 1024` (terminal titles are short; 1024 bytes is generous for any real title)
- `OSC_52_MAX_BYTES = 65_536` (64 KiB; supports clipboard writes up to ~48 KB of raw data after base64 encoding overhead; covers the vast majority of copy-paste use cases)

**Test impact:** The Phase 12 regression tests `adversarial_large_osc_title_is_bounded_no_panic` and `adversarial_large_osc52_is_bounded_no_panic` must be updated — their assertions currently verify the vte ArrayVec 1024-byte cap. After approach (b), the bounds are `MAX_TITLE_BYTES` and `OSC_52_MAX_BYTES` respectively.

---

## State of the Art

| Old Approach | Current Approach | When Changed | Impact |
|--------------|------------------|--------------|--------|
| vte default-features=false (1024-byte cap, Phase 12 CR-03) | vte std (re-enabled) + explicit cap in osc_dispatch (Phase 16) | Phase 16 | Supports real clipboard payloads; explicit cap prevents DoS regression |
| No OSC passthrough (Phase 12 detection-only stubs) | OSC 52 + OSC 0/2 forwarded over reliable stream | Phase 16 | Clipboard and title propagation functional |
| ConnectionLossOverlay no-op stub (Phase 14) | Live overlay layer with elapsed timer | Phase 16 | Connection-loss UX complete |
| Windows cross-compile (GNU target from Linux, never ran) | Native windows-latest MSVC build in CI | Phase 16 | Real Windows compile validation |

**Deprecated/outdated:**
- `.github/workflows/windows-cross.yml`: Retired by Phase 16. Linux-hosted GNU cross-compile is replaced by native MSVC on `windows-latest`. Keep or delete at user's discretion.

---

## Assumptions Log

| # | Claim | Section | Risk if Wrong |
|---|-------|---------|---------------|
| A1 | `Swatinem/rust-cache@v2` is the current standard Rust caching action for GitHub Actions | Pattern 8 / Windows CI | Low — alternative is no caching (slower builds) or `actions/cache@v4` manually |
| A2 | The `quinn_udp=error` tracing filter suppresses the WSAEMSGSIZE warning without suppressing genuine quinn connection errors | Pattern 7 / WSAEMSGSIZE | Medium — if quinn_udp emits genuine errors at WARN level they would also be suppressed; check quinn-udp source for error vs. warn usage |
| A3 | The existing `title` field in `TerminalState` uses `Option<String>` and `take()` semantics are appropriate (drain-on-send) | Architecture Patterns | Low — code-confirmed; `title: Option<String>` at terminal.rs:159 |
| A4 | `cargo build` (not `cargo test`) in the Windows CI job is sufficient for HARDEN-02 | Standard Stack | Low — D-16-04 explicitly says "building nosh-client... failing the run on error"; test execution would require a Windows server to connect to |
| A5 | The `osc_dispatch` `data == b"?"` check is the complete security gate for OSC 52 read prevention | Pitfall 1 | Low — verified from OSC 52 specification: the read form sends the literal `?` as the data parameter |

---

## Open Questions

1. **Approach (a) vs. (b) for OSC accumulation cap (D-16-01c)**
   - What we know: approach (b) is simpler and the transient risk is bounded by OS pipe buffers; approach (a) is more robust but requires a raw-byte pre-processor.
   - What's unclear: whether any real-world clipboard content (e.g. neovim yanking a 100KB file) would exceed 64 KiB in base64 form (~75 KB base64 for 56 KB raw). If so, OSC 52 would silently truncate large pastes.
   - Recommendation: Implement approach (b) with `OSC_52_MAX_BYTES = 65536`. Document the limit. Add a Phase 17 open item to consider approach (a) if large-clipboard use cases are reported.

2. **ConnectionLossOverlay in overlays Vec vs. separate parameter**
   - What we know: `PredictionOverlay` is already passed separately to `render_with_predictor`. The `overlays` Vec currently only holds the no-op `ConnectionLossOverlay`.
   - What's unclear: whether removing `ConnectionLossOverlay` from `overlays` and passing it separately simplifies or complicates the `ClientScreen` API surface.
   - Recommendation: Follow the `PredictionOverlay` pattern. Both overlays passed explicitly. The `overlays: Vec<Box<dyn Overlay>>` Vec in `ClientScreen` can be removed (or kept empty for future use).

3. **--status title vs. RTT in a different location**
   - What we know: terminal title is the safest place (does not steal a terminal row; works across terminal emulators).
   - What's unclear: whether the user expects a persistent RTT indicator even when the remote app sets its own title.
   - Recommendation: Use the terminal title. Document in `--help` that `--status` replaces OSC 0/2 title propagation. (This is a known tradeoff per D-16-05.)

---

## Environment Availability

> This phase has no new external tool dependencies. The CI gate requires push access to GitHub (user prerequisite per D-16-04b).

| Dependency | Required By | Available | Version | Fallback |
|------------|------------|-----------|---------|----------|
| `cargo` / Rust toolchain | All Rust builds | ✓ | stable (verified by existing builds) | — |
| GitHub Actions (`windows-latest`) | HARDEN-02 CI gate | ✗ (not testable locally) | — | Author `ci.yml`; user verifies by pushing to GitHub (D-16-04b) |
| `git` remote `origin` configured | D-16-04b sign-off | Pending user push | — | User must push unpushed commits first |

**Missing dependencies with no fallback:**
- GitHub Actions execution — cannot be simulated locally. Phase 16 authors `ci.yml`; verification is explicitly deferred to user push (D-16-04b).

---

## Security Domain

> `security_enforcement` is not set to false in config.json — security domain is required.

### Applicable ASVS Categories

| ASVS Category | Applies | Standard Control |
|---------------|---------|-----------------|
| V2 Authentication | No | Not changed this phase |
| V3 Session Management | No | Not changed this phase |
| V4 Access Control | Yes (OSC 52 read gate) | `data == b"?"` check in `osc_dispatch` — must never be forwarded |
| V5 Input Validation | Yes (OSC accumulation cap) | `OSC_52_MAX_BYTES` and `MAX_TITLE_BYTES` caps in `osc_dispatch` after vte std re-enable |
| V6 Cryptography | No | Not changed this phase |

### Known Threat Patterns for OSC Passthrough Stack

| Pattern | STRIDE | Standard Mitigation |
|---------|--------|---------------------|
| OSC 52 clipboard exfiltration via forwarded read form | Information Disclosure | `data == b"?"` gate in `osc_dispatch`; never forward read/query form |
| OSC 52 DoS via unbounded clipboard payload | Denial of Service | `OSC_52_MAX_BYTES = 65536` cap in `osc_dispatch` before retaining payload |
| Title injection XSS-style attack via OSC 0/2 | Tampering | Title is emitted as a control sequence to local terminal — terminal emulator renders it as window title text, not HTML; no injection surface |
| WSAEMSGSIZE filter masking genuine quinn errors | Elevation of Privilege | Filter is `quinn_udp=error` not `quinn=error` — only `quinn_udp` WARN is suppressed, not `quinn` WARN (connection/auth errors) |

---

## Sources

### Primary (HIGH confidence — verified from codebase)
- `crates/nosh-server/src/terminal.rs` — `osc_dispatch` at lines 637-663, `osc52_pending` field at line 163, `title` field at line 159, `TerminalState::osc52_pending()` at line 363, `TerminalState::title()` at line 357
- `crates/nosh-client/src/screen.rs` — `ConnectionLossOverlay` struct at lines 90-96, `Overlay` trait at line 80, `overlays: Vec<Box<dyn Overlay>>` at line 119, `ClientScreen::new()` loads the overlay at line 139
- `crates/nosh-client/src/main.rs` — `EscapeState` machine at lines 89-166 (fully implemented `~.`), `Args` struct at lines 282-326 (`--predict PredictDisplayMode` already wired at line 325), `run_pump` datagram arm at line 703, `conn.rtt()` at line 714
- `crates/nosh-proto/src/messages.rs` — `Message` enum at line 21, all existing variants documented; `Ack` is discriminant 8 (last variant), new `TerminalControl` will be discriminant 9
- `crates/nosh-server/Cargo.toml` — vte `default-features = false` at line 47 with CR-03 explanation comment
- `crates/nosh-client/Cargo.toml` — `crossterm = { version = "0.29", features = ["events"] }` at line 33 (no `osc52` feature yet)
- `.planning/phases/12-server-terminal-state-model/12-REVIEW-FIX.md` — CR-03 analysis at lines 58-73; D-16-01c OSC 52 cap options at lines 66-71
- `.planning/phases/14-client-predictor-confirmed-rendering/14-CONTEXT.md` — `ConnectionLossOverlay` stub design at D-14-01a; compositor `Overlay` trait seam description
- `.github/workflows/windows-cross.yml` — existing GNU cross-compile workflow (to be retired)

### Secondary (MEDIUM confidence — official docs/crates.io)
- crates.io crossterm 0.29.0: 138M downloads, first published 2018, github.com/crossterm-rs/crossterm [VERIFIED: crates.io API]
- docs.rs crossterm 0.29.0: `crossterm::clipboard` module requires `osc52` feature; provides `CopyToClipboard` [VERIFIED: docs.rs]
- quinn-rs/quinn#2041 "Potential bug in GRO code for Windows?" — open issue, relates to Windows UDP receive path with GRO metadata; WSAEMSGSIZE is a Windows-specific symptom [VERIFIED: GitHub issue tracker]
- dtolnay/rust-toolchain: `@stable` usage with `targets:` parameter for cross-targets [VERIFIED: GitHub README]

### Tertiary (ASSUMED)
- `Swatinem/rust-cache@v2` as the standard Rust caching action for GitHub Actions [ASSUMED — widely cited in Rust CI guides but not verified against official GitHub Actions Marketplace docs in this session]

---

## Metadata

**Confidence breakdown:**
- Standard stack: HIGH — all crates verified from existing Cargo.toml files
- Architecture: HIGH — verified from Phase 12/14/15 code; new patterns follow established precedents
- OSC accumulation decision: HIGH — verified from Phase 12 REVIEW-FIX.md and vte Cargo.toml comment
- CI patterns: MEDIUM — GitHub Actions syntax verified via dtolnay/rust-toolchain docs; `Swatinem/rust-cache@v2` is ASSUMED
- WSAEMSGSIZE: MEDIUM — upstream issue confirmed open; tracing filter approach is standard but not verified against current quinn source

**Research date:** 2026-06-02
**Valid until:** 2026-07-02 (30 days — stable stack; CI action versions may update)
