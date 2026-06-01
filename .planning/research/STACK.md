# Stack Research

**Domain:** QUIC-based roaming remote shell (Rust) — v1.2 M4 Predictive Echo + Daily-Driver Readiness
**Researched:** 2026-06-01
**Confidence:** HIGH (all version claims and APIs verified against docs.rs / GitHub live data)

---

## Existing Validated Stack (v1.0/v1.1 — DO NOT RE-RESEARCH)

These are locked-in and shipped. Do not revisit.

| Technology | Pinned Version | Role |
|------------|---------------|------|
| `quinn` | 0.11.9 | QUIC transport (latest on crates.io, confirmed) |
| `rustls` | 0.23.x | TLS 1.3 via quinn |
| `tokio` | 1.52.3 | Async runtime (latest) |
| `portable-pty` | 0.9.0 | PTY (Linux) |
| `ssh-key` | 0.6.7 | Key parsing / authorized_keys / known_hosts |
| `ssh-agent-client-rs` | 1.1.x | Agent signing (Linux) |
| `ed25519-dalek` | 2.2.0 | Ed25519 material |
| `vte` | 0.15.0 | VT parser (server-side, existing) |
| `rcgen` | 0.14.x | Ephemeral self-signed certs |
| `crossterm` | 0.29.0 | Client terminal raw mode + event reading |
| `postcard` + `serde` | 1.x / 1.x | Frame serialization |
| `bytes`, `tracing`, `anyhow`, `thiserror`, `clap` | — | Shared utilities |
| `uuid` | 1.x (v4) | Session IDs (server) |
| `nix` | 0.29 | Signal handling (server, Linux) |
| `dirs` | 5.x | Platform path resolution |
| `x509-parser` | 0.18 | SPKI extraction from TLS certs |

---

## v1.2 Stack Additions and Changes

### 1. Terminal Grid/Screen Abstraction for Predictive Echo

**Decision: `termwiz` 0.23.3 for client-side screen model; keep `vte` 0.15.0 for server-side VT parsing.**

This is the most consequential new dependency in M4. Here is the full rationale.

#### What M4 needs and does NOT have

The existing `vte` 0.15.0 is a VT/ANSI _parser_ — it fires a `Perform` callback for each escape sequence. It does NOT maintain a screen grid, does NOT track cell content, and does NOT produce diffs. For predictive echo to work, the _client_ needs:

1. A full terminal screen grid (rows × columns of cells with attributes)
2. The ability to apply incoming server VT data to that grid
3. A way to overlay speculative "prediction" cells on top of the confirmed grid
4. A diff mechanism for the datagram payload (what changed from frame N to frame N+1)

Neither `vte` nor `alacritty_terminal` provide (3) or (4) in a usable form:

- **`alacritty_terminal` 0.26.0**: Has a `Grid<Cell>` and `Term` but no change-tracking or diff API. It is also clearly an internal component of the Alacritty terminal emulator — not designed as a general library dep. The API is unstable (sub-1.0), not documented for external use, and diffs would require hand-rolling a full grid comparison.

- **`termwiz` 0.23.3**: Specifically designed as a reusable library. Its `Surface` type is the correct primitive:
  - Maintains a terminal grid (rows × cols of `Cell`s with full attribute state)
  - `add_change(Change)` / VT parsing via `termwiz::terminal::parser` → `Surface`
  - `get_changes(seq: SequenceNo) → (SequenceNo, Cow<[Change]>)` — returns incremental change log since a sequence number
  - `flush_changes_older_than(seq)` — prunes the log (exactly the "trim acked" pattern already used by the replay buffer)
  - `diff_screens(&other_surface) → Vec<Change>` — computes the change vector between two screen states
  - `Change` enum is `Serialize + Deserialize` with `use_serde` feature — postcard can encode `Vec<Change>` directly
  - `SequenceNo` is a monotonic counter — directly usable as the epoch tracker for confirming predictions

This maps Mosh's Framebuffer+diff model onto a Rust type the project does not need to own.

**Confidence: HIGH** — `get_changes`, `flush_changes_older_than`, `diff_screens`, `Surface`, `SequenceNo` all verified in docs.rs/termwiz 0.23.3. `Change` serde confirmed. Version 0.23.3 released 2026-03-20.

**Dependency footprint caveat**: termwiz 0.23.3 has ~30 direct deps and ~14–21 MB dependency tree (lib.rs confirmed). It pulls in `wezterm-bidi`, `wezterm-color-types`, `vtparse`, `pest`, `unicode-segmentation`, `terminfo`, and others. This is moderate but not negligible. Evaluate whether the `widgets` feature is needed (it is not for nosh — omit it). Use `default-features = false, features = ["use_serde"]` to reduce build surface.

**Wire format for datagram state sync**: `Vec<Change>` serialized with `postcard` (already in tree). Do NOT add `prost`/protobuf. Mosh uses protobuf for its C++ wire format, but nosh already has a serde/postcard discipline and `Change` is serde-serializable. Postcard encodes smaller and faster than prost for this type of structured payload (confirmed by djkoloski/rust_serialization_benchmark). A bespoke datagram envelope:

```rust
// In nosh-proto/src/datagram.rs (new file)
#[derive(Serialize, Deserialize)]
pub struct ScreenFrame {
    /// Monotonic epoch. Client uses this to confirm predictions.
    pub epoch: u64,
    /// Sequence number of the last input frame the server has processed.
    /// Client uses this to retire predictions older than echo_ack.
    pub echo_ack: u64,
    /// Incremental change stream since the last acknowledged frame.
    pub changes: Vec<termwiz::surface::change::Change>,
}
```

The `epoch` field replaces Mosh's `confirmed_epoch`/`prediction_epoch` pair; `echo_ack` is Mosh's "server has processed input up to frame N" signal that drives prediction retirement.

#### Prediction Engine (bespoke — no crate exists)

The prediction/local echo logic is bespoke, as in Mosh. No Rust crate implements Mosh-style SSP prediction. The Mosh C++ reference is `src/frontend/terminaloverlay.cc`. The key data structures to port:

```rust
// In nosh-client/src/prediction.rs (new file)
struct ConditionalCell {
    replacement: termwiz::surface::change::Change,
    tentative_until_epoch: u64,   // confirmed when server epoch >= this
    original: CellContents,       // what was there before prediction
    active: bool,
}

struct PredictionEngine {
    overlays: Vec<Vec<ConditionalCell>>,  // indexed [row][col]
    cursors: Vec<PredictedCursor>,
    current_epoch: u64,
    confirmed_epoch: u64,  // from last ScreenFrame.echo_ack
}
```

The `cull()` method compares the prediction overlay against the latest confirmed `Surface` state and retires predictions whose epoch is <= `confirmed_epoch`. This is ~200–300 lines of logic with no external dep.

**Server-side**: Keep `vte` 0.15.0. The server feeds raw PTY output through `vte` into a `termwiz::surface::Surface` (replacing the existing custom `Perform` handler), then calls `surface.get_changes(last_sent_epoch)` and encodes a `ScreenFrame` datagram.

---

### 2. Cargo.toml Changes for Predictive Echo

```toml
# workspace Cargo.toml — add to [workspace.dependencies]
termwiz = { version = "0.23", default-features = false, features = ["use_serde"] }

# nosh-proto/Cargo.toml — add
termwiz = { workspace = true }

# nosh-server/Cargo.toml — add
termwiz = { workspace = true }

# nosh-client/Cargo.toml — add
termwiz = { workspace = true }
```

---

### 3. OSC 52 Clipboard (QoL UX)

**Use `crossterm` 0.29.0 `CopyToClipboard` — already in tree, zero new dep.**

crossterm 0.29.0 (already in `nosh-client`) added OSC 52 clipboard support via `crossterm::clipboard::CopyToClipboard`. It requires the `osc52` feature flag (verified: module documented as `requires feature = "osc52"`).

```toml
# nosh-client/Cargo.toml
crossterm = { version = "0.29", features = ["events", "osc52"] }
```

```rust
use crossterm::clipboard::CopyToClipboard;
// Write clipboard via OSC 52 escape sequence:
execute!(std::io::stdout(),
    CopyToClipboard::to_clipboard_from(text_content))?;
```

**Limitation**: OSC 52 requires the terminal emulator and any multiplexer to pass through the escape sequence. Works in most modern terminals (iTerm2, WezTerm, modern xterm, Windows Terminal). Kitty and tmux may require config. This is a best-effort UX feature, not a reliability requirement. Server must detect OSC 52 in the PTY output stream and forward it as a dedicated `Message::Osc52` control frame (not raw PTY bytes) because the client terminal, not the server's PTY, is the clipboard target.

**Confidence: HIGH** — crossterm 0.29.0 `CopyToClipboard` API verified on docs.rs; `osc52` feature requirement confirmed.

---

### 4. Connection-Loss Notifications (QoL UX)

**No new crate required.** This is pure application logic over the existing `tokio` + `quinn` stack.

Pattern: wrap the QUIC `Connection` in a state machine with states `{ Connected, Reconnecting { since: Instant, attempt: u32 }, Failed }`. On `Connection::closed()` or a stream I/O error, transition to `Reconnecting` and write a status line to the terminal via `crossterm::style::Print`. The reconnect loop already exists from M3 reattach — this feature is a display wrapper around it.

```rust
// No new deps. crossterm already writes status lines.
// Display: "[nosh] connection lost — reconnecting (attempt 3)…"
// Abort instruction: "Press ~. to quit"
```

The `~.` quit escape is already implemented in the client (M3 phase 9). No stack changes needed.

---

### 5. PTY Reader-Zombie Race Fix

**Pattern: replace `spawn_blocking` + `read()` with `tokio::io::unix::AsyncFd` + `O_NONBLOCK`.**

The current issue: `portable-pty` returns a `Box<dyn Read>` (the master PTY reader). The code wraps this in `tokio::task::spawn_blocking`, which cannot be interrupted by `abort()` once the blocking `read()` syscall is in flight. This is the "zombie race" — on reconnect, the server loop cannot cleanly stop the reader task.

**The fix** (verified via tokio issue #4488 and AsyncFd docs):

1. After `openpty()`, extract the raw file descriptor from the master PTY via `AsRawFd` (already available because `nix` 0.29 is in tree).
2. Set the fd to non-blocking: `fcntl(fd, FcntlArg::F_SETFL(OFlag::O_NONBLOCK))` (nix already in tree, no new dep).
3. Wrap in `tokio::io::unix::AsyncFd::new(fd)`.
4. Use `async_fd.readable().await` + `try_io(|inner| unistd::read(inner.get_ref(), buf))` in the session loop — this is now a proper tokio future that respects cancellation/select.

```rust
// nosh-server/src/session.rs (modified, no new deps)
use tokio::io::unix::AsyncFd;
use nix::fcntl::{fcntl, FcntlArg, OFlag};
use nix::unistd;

let raw_fd = pty_master.as_raw_fd();
fcntl(raw_fd, FcntlArg::F_SETFL(OFlag::O_NONBLOCK))?;
let async_fd = AsyncFd::new(raw_fd)?;

loop {
    let mut guard = async_fd.readable().await?;
    guard.try_io(|inner| {
        let mut buf = [0u8; 4096];
        match unistd::read(*inner.get_ref(), &mut buf) {
            Ok(n) => Ok(n),
            Err(nix::errno::Errno::EAGAIN) => {
                Err(std::io::Error::from(std::io::ErrorKind::WouldBlock))
            }
            Err(e) => Err(std::io::Error::from_raw_os_error(e as i32)),
        }
    })?;
}
```

This loop is a proper tokio future; `tokio::select!` with a cancellation channel can abort it cleanly. The `JoinHandle::abort()` issue goes away because there is no `spawn_blocking`.

**Caveats**: The `AsyncFd` approach requires bypassing `portable-pty`'s `Box<dyn Read>` abstraction and accessing the raw fd directly. This is Linux-specific (Unix fd). It does not affect the Windows server path (M6, out of scope). Gate with `#[cfg(unix)]`.

**No new crates needed.** `tokio::io::unix::AsyncFd` is in the existing tokio dep (it is part of the `io-util` feature already enabled). `nix` 0.29 is already in the server's deps.

**Confidence: HIGH** — `AsyncFd` approach confirmed working for PTY in tokio issue #4488; `try_io` / `readable` API verified on docs.rs/tokio 1.52.3.

---

### 6. WSAEMSGSIZE quinn-udp Warning on Windows

**Current status: this is a known, open quinn-rs issue (#2041) with no upstream fix. The connection functions correctly despite the log warning.**

Root cause (verified from quinn-rs/quinn#2041 and source inspection): quinn-udp's Windows receive path uses `WSARecvMsg()` with a 128-byte control buffer. When Windows GRO (Generic Receive Offload) is active, it tries to append `UDP_COALESCED_INFO` to the control buffer. If the buffer is too small, the coalesced packet metadata is not delivered and quinn-udp receives an oversized datagram. This is logged as a warning. The actual UDP data arrives intact; the packet is not lost.

**Resolution options (in priority order):**

1. **Suppress the warning with a tracing filter** (simplest, M4-appropriate): Add a `tracing_subscriber` filter that drops quinn-udp's `WARN` log level for the `quinn_udp` target on Windows. No code change to quinn itself required.

   ```rust
   // nosh-client/src/main.rs, Windows initialization
   #[cfg(target_os = "windows")]
   fn build_subscriber() -> impl tracing::Subscriber {
       tracing_subscriber::fmt()
           .with_env_filter(
               tracing_subscriber::EnvFilter::from_default_env()
                   .add_directive("quinn_udp=error".parse().unwrap())
           )
           .finish()
   }
   ```

   This silences the warning without suppressing genuine quinn errors.

2. **Wait for upstream fix**: quinn-udp 0.6.1 (March 2025) added "disable GSO after probing" and "reuse existing socket for probing GRO/GSO support." These changes partially address Windows GRO/GSO reliability. Monitor quinn 0.11.x changelog for a full WSAEMSGSIZE resolution. Update `quinn` in the workspace when a fix ships.

3. **Do NOT disable GRO globally** via quinn transport config — there is no public API to do so in quinn 0.11.9, and doing so would harm performance on Linux where GRO works correctly.

**Confidence: MEDIUM** — issue #2041 confirmed open; suppression via tracing filter confirmed viable as a workaround; no upstream fix in 0.11.9.

---

### 7. Windows Cross-Compile CI Gate

**No new crate required. Pure CI/GitHub Actions configuration.**

The existing Windows client code compiles natively (confirmed in v1.1). The CI gate was wired but never ran because there is no git remote. Once a remote is configured, add this job to `.github/workflows/ci.yml`:

```yaml
# .github/workflows/ci.yml
jobs:
  build-windows:
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

Key notes confirmed from research:
- `windows-latest` runner already has MSVC Build Tools installed — no extra setup for `ring` (ring 0.17.14 ships precompiled asm objects for `x86_64-windows`, no NASM needed).
- Build is native (not cross-compile) — the runner IS Windows x86_64, so `--target x86_64-pc-windows-msvc` does not require cross toolchain or emulation.
- Only `nosh-client` (-p nosh-client) builds on Windows. Do NOT build `nosh-server` (has `portable-pty`, `nix`, Unix-only) or `nosh-auth` in the Windows target without `--no-default-features` (ssh-agent-client-rs is already `cfg(unix)`-gated).
- Do NOT add `cargo-xwin` or `cross` — they are for cross-compiling FROM Linux TO Windows. Here we run natively ON Windows.

**Confidence: HIGH** — windows-latest + x86_64-pc-windows-msvc + ring pattern verified in multiple 2024/2025 CI guides; ring precompiled objects confirmed in ring/BUILDING.md.

---

## Summary of Cargo.toml Changes for v1.2

### Workspace `Cargo.toml` — add to `[workspace.dependencies]`

```toml
termwiz = { version = "0.23", default-features = false, features = ["use_serde"] }
```

### `nosh-proto/Cargo.toml` — add

```toml
termwiz = { workspace = true }
```

(Needed because `ScreenFrame` in `nosh-proto` references `termwiz::surface::change::Change`.)

### `nosh-server/Cargo.toml` — add

```toml
termwiz = { workspace = true }
```

(Server maintains a `Surface` and calls `get_changes()` to build datagram payloads.)

### `nosh-client/Cargo.toml` — changes

```toml
termwiz = { workspace = true }
# Add osc52 feature to existing crossterm dep:
crossterm = { version = "0.29", features = ["events", "osc52"] }
```

### `nosh-server/Cargo.toml` — no new deps for PTY fix

`tokio::io::unix::AsyncFd` is already available via the existing tokio dep (`io-util` feature already enabled). `nix` 0.29 is already present.

---

## Alternatives Considered

| Category | Recommended | Alternative | Why Not |
|----------|-------------|-------------|---------|
| Client terminal grid | `termwiz` 0.23.3 | `alacritty_terminal` 0.26.0 | alacritty_terminal has no diff/change-tracking API; not designed as an external library dep; unstable sub-1.0 API |
| Client terminal grid | `termwiz` 0.23.3 | Roll bespoke grid from scratch | ~1,500–2,000 lines to get correct cell+attribute semantics, Unicode width, scrollback — termwiz is battle-tested in wezterm |
| Datagram serialization | `postcard` (existing) + `termwiz::Change` serde | `prost` / protobuf | prost adds a build-time `protoc` dependency; postcard is already in the tree, smaller payloads, same or better perf; `Change` is already serde-serializable |
| PTY async read | `AsyncFd` + `O_NONBLOCK` (tokio stdlib) | `tokio_pty_process` crate | `tokio_pty_process` is unmaintained (last release 2019); `AsyncFd` is the current tokio-idiomatic approach |
| OSC 52 clipboard | `crossterm` 0.29.0 (existing) | `copypasta-ext` crate | `crossterm` already in tree and supports OSC 52 write; `copypasta-ext` adds a dep for the same functionality |
| WSAEMSGSIZE | tracing filter workaround | Patch quinn-udp | Patching quinn-udp is out of scope for M4; the connection works correctly; upstream fix is the right path |
| Windows CI | Native `windows-latest` job | `cargo-xwin` from Linux | `cargo-xwin` is for cross-compile from Linux; `windows-latest` runner has MSVC natively — simpler and more faithful |

---

## What NOT to Add

| Avoid | Why | Use Instead |
|-------|-----|-------------|
| `prost` / protobuf | Adds `protoc` build-time dep; `termwiz::Change` is already serde-serializable; postcard is already in tree and produces smaller payloads | `postcard` + `termwiz::Change` serde |
| `alacritty_terminal` | Sub-1.0 unstable API, no diff/change-tracking, designed as internal Alacritty component | `termwiz::surface::Surface` |
| `tokio_pty_process` | Last released 2019, unmaintained | `tokio::io::unix::AsyncFd` + `O_NONBLOCK` directly |
| `copypasta` or `clipboard` crate | Native clipboard requires platform-specific APIs (X11, Wayland, Win32); for a remote shell the only meaningful clipboard path is OSC 52 (terminal-mediated) | `crossterm` 0.29.0 `CopyToClipboard` |
| `cargo-xwin` or `cross` | These are Linux→Windows cross-compile tools; the nosh Windows CI runs natively on `windows-latest` | Native cargo on `windows-latest` runner |
| `termwiz::widgets` feature | Provides TUI widget layout — not needed for nosh; adds build weight | `default-features = false, features = ["use_serde"]` |
| Any 0-RTT reattach crate | 0-RTT is deliberately deferred (INIT.md, PROJECT.md); 1-RTT cold reattach is already implemented | Nothing |
| `rkyv` or other zero-copy serde | overkill for datagram-sized terminal diffs; introduces unsafe; no serde compatibility with existing `Change` type | `postcard` |

---

## Version Compatibility (v1.2 additions)

| Package | Compatible With | Notes |
|---------|-----------------|-------|
| `termwiz` 0.23.3 | `serde` 1.x | `use_serde` feature enables `#[derive(Serialize, Deserialize)]` on `Change`; serde 1.x already in workspace |
| `termwiz` 0.23.3 | `tokio` 1.x | No direct tokio dep in termwiz; integration is purely at the application layer |
| `termwiz` 0.23.3 | `postcard` 1.x | `Change` serializes fine with postcard; no postcard-incompatible types in the variants used |
| `crossterm` 0.29.0 + `osc52` | `x86_64-pc-windows-msvc` | OSC 52 writes a plain escape sequence to stdout; works cross-platform |
| `tokio::io::unix::AsyncFd` | `tokio` 1.52.3 | Part of tokio's Unix-specific I/O; `io-util` feature (already enabled in workspace) is required |
| `nix` 0.29 `fcntl` | Linux only | Already gated `cfg(unix)` in nosh-server; no Windows impact |

---

## Sources

- https://docs.rs/termwiz/0.23.3/termwiz/surface/struct.Surface.html — `get_changes`, `flush_changes_older_than`, `diff_screens`, `SequenceNo` API verified
- https://docs.rs/termwiz/0.23.3/termwiz/surface/change/enum.Change.html — all 15 variants confirmed; `Serialize + Deserialize` confirmed
- https://lib.rs/crates/termwiz — version 0.23.3 (released 2026-03-20); ~416K SLoC dep tree confirmed
- https://docs.rs/alacritty_terminal/0.26.0/alacritty_terminal/ — no diff/change-tracking confirmed; sub-1.0 unstable API confirmed
- https://github.com/mobile-shell/mosh/blob/master/src/frontend/terminaloverlay.cc — prediction engine structure (ConditionalOverlayCell, PredictionEngine, epoch model, cull() pattern) verified
- https://deepwiki.com/mobile-shell/mosh/3.2-state-synchronization-protocol — SSP diff model, echo_ack mechanism, protobuf serialization confirmed
- https://docs.rs/crossterm/0.29.0/crossterm/index.html — `clipboard` module, `CopyToClipboard`, `osc52` feature requirement confirmed
- https://github.com/crossterm-rs/crossterm/blob/master/CHANGELOG.md — OSC 52 added in 0.29.0 ("Copy to clipboard using OSC52 #974") confirmed
- https://docs.rs/crate/quinn/latest — quinn 0.11.9 confirmed as latest (released 2025-08-27)
- https://docs.rs/crate/tokio/latest — tokio 1.52.3 confirmed as latest (released 2026-05-08)
- https://github.com/quinn-rs/quinn/issues/2041 — Windows GRO/WSAEMSGSIZE root cause confirmed (open, no upstream fix); 128-byte control buffer insufficient for UDP_COALESCED_INFO
- https://github.com/quinn-rs/quinn/releases — quinn-udp 0.6.1 (2025-03-27) "disable GSO after probing" partial Windows fix confirmed; no full WSAEMSGSIZE resolution
- https://github.com/tokio-rs/tokio/issues/4488 — `AsyncFd` as correct pattern for PTY master fd nonblocking async read confirmed
- https://docs.rs/tokio/latest/tokio/io/unix/struct.AsyncFd.html — `readable()`, `try_io()`, nonblocking fd requirements confirmed
- https://github.com/djkoloski/rust_serialization_benchmark — postcard outperforms prost on payload size and encode speed confirmed
- https://reemus.dev/tldr/rust-cross-compilation-github-actions — `windows-latest` + `x86_64-pc-windows-msvc` native build pattern confirmed (no cross-compile toolchain needed)

---
*Stack research for: nosh QUIC remote shell — v1.2 M4 Predictive Echo + Daily-Driver Readiness*
*Researched: 2026-06-01*
