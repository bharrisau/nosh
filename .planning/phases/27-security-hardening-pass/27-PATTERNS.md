# Phase 27: Security Hardening Pass - Pattern Map

**Mapped:** 2026-06-14
**Files analyzed:** 12
**Analogs found:** 10 / 12

## File Classification

| New/Modified File | Role | Data Flow | Closest Analog | Match Quality |
|-------------------|------|-----------|----------------|---------------|
| `crates/nosh-server/src/terminal.rs` (SEC-05 re-verify) | model | event-driven | `crates/nosh-server/src/terminal.rs` (lines 293-455) | exact |
| `crates/nosh-client/src/screen.rs` (SEC-04 apply path) | model | request-response | `crates/nosh-client/src/screen.rs` (lines 207-300) | exact |
| `crates/nosh-client/src/main.rs` (SEC-04 message handling) | controller | event-driven | `crates/nosh-client/src/main.rs` (lines 2075-2120) | exact |
| `.github/workflows/ci.yml` (SEC-05 CI gate) | config | batch | `.github/workflows/ci.yml` (lines 10-23) | role-match |
| `fuzz/fuzz_targets/osc_accumulation.rs` (SEC-05 fuzz re-run) | test | event-driven | `fuzz/fuzz_targets/osc_accumulation.rs` (lines 37-84) | exact |
| `docs/SECURITY.md` (SEC-01 threat model) | docs | N/A | `docs/999.1-SECURITY.md` + `docs/999.7-SECURITY.md` | role-match |
| `crates/nosh-client/src/main.rs` (D-02 per-OSC gate) | controller | event-driven | `crates/nosh-server/src/terminal.rs` (lines 341-455) | pattern-match |
| `crates/nosh-client/src/main.rs` (D-03 OSC 52 read reject) | controller | event-driven | `crates/nosh-client/src/main.rs` (lines 2077-2098) | exact |
| `crates/nosh-client/src/main.rs` (D-04 DCS/PM/APC no-op) | controller | event-driven | `crates/nosh-server/src/terminal.rs` (lines 1157-1177) | pattern-match |
| `crates/nosh-client/src/main.rs` (D-05 title strip) | controller | event-driven | `crates/nosh-client/src/main.rs` (lines 2100-2118) | exact |
| `crates/nosh-client/src/main.rs` (D-06 resize rate-limit) | controller | event-driven | `crates/nosh-client/src/main.rs` (lines 45-52) | pattern-match |
| `crates/nosh-client/src/main.rs` (D-07 OSC 8 scheme whitelist) | controller | event-driven | N/A | new-feature |

## Pattern Assignments

### 1. SEC-05: OSC-OOM Re-verification + CI Regression Gate

#### `crates/nosh-server/src/terminal.rs` (model, event-driven)

**Analog:** `crates/nosh-server/src/terminal.rs` (existing osc_prefilter, lines 341-455)

**OSC prefilter pattern** (lines 341-455):
```rust
fn osc_prefilter<'a>(&mut self, bytes: &'a [u8]) -> &'a [u8] {
    let mut in_osc = self.in_osc;
    let mut osc_byte_count = self.osc_byte_count;
    
    // OSC framing detection
    // - Start: 0x9D or ESC ] (0x1B 0x5D)
    // - End: BEL (0x07) or ST (ESC \ = 0x1B 0x5C)
    
    // Count bytes while in_osc, truncate at OSC_ACCUMULATION_MAX (1 MiB)
    // Return safe prefix or truncated slice
}
```

**Regression test pattern** (lines 650-750 of terminal.rs test module):
```rust
#[test]
fn oversized_multi_chunk_osc_is_bounded_then_resyncs() {
    // Feed 10 MiB OSC sequence in chunks
    // Assert title is bounded
    // Assert normal OSC still works after resync
}
```

#### `.github/workflows/ci.yml` (config, batch)

**Analog:** `.github/workflows/ci.yml` (existing linux test job, lines 10-23)

**CI gate pattern**:
```yaml
# Add new job after linux/build-windows/audit jobs
osc-bound-regression:
  name: OSC OOM bound regression test
  runs-on: ubuntu-latest
  steps:
    - uses: actions/checkout@v4
    - uses: dtolnay/rust-toolchain@stable
    - name: Run OSC bound regression test
      run: cargo test -p nosh-server --lib terminal::oversized_multi_chunk_osc_is_bounded_then_resyncs
```

#### `fuzz/fuzz_targets/osc_accumulation.rs` (test, event-driven)

**Analog:** `fuzz/fuzz_targets/osc_accumulation.rs` (existing deterministic multi-chunk OSC, lines 37-84)

**Re-run pattern**:
```bash
# Re-run at raised max_len to surface multi-MiB OSC
LIBFUZZER_MAX_LEN=2097152 cargo +nightly fuzz run osc_accumulation
```

### 2. SEC-04: Client Trust-Boundary Hardening

#### `crates/nosh-client/src/main.rs` (D-02: Per-OSC byte-count gate)

**Analog:** `crates/nosh-server/src/terminal.rs` (osc_prefilter pattern, lines 341-455)

**Pattern:** Apply a pre-filter before any VT interpretation on the client side, similar to the server-side osc_prefilter. The client does not currently use vte, so this is a new gate that counts OSC bytes before processing TerminalControl messages.

**Implementation site:** In the TerminalControl message handler (around line 2075), before processing the payload.

#### `crates/nosh-client/src/main.rs` (D-03: OSC 52 clipboard read reject)

**Analog:** `crates/nosh-client/src/main.rs` (existing Clipboard handling, lines 2077-2098)

**Current pattern** (lines 2077-2098):
```rust
TerminalControlPayload::Clipboard { selection, data } => {
    // Re-emit OSC 52 clipboard WRITE to the local terminal.
    // Write-only by construction (T-16-05): the read/query form
    // was dropped server-side in Plan 16-01, D-16-01a.
    let sel: String = String::from_utf8_lossy(&selection)
        .chars()
        .filter(|&c| c != '\x07' && c != '\x1b')
        .collect();
    let b64: String = String::from_utf8_lossy(&data)
        .chars()
        .filter(|&c| c != '\x07' && c != '\x1b')
        .collect();
    let osc52 = format!("\x1b]52;{sel};{b64}\x07");
    let _ = stdout.write_all(osc52.as_bytes()).await;
}
```

**D-03 hardening:** Add explicit check: if `data == b"?"` (read form), reject entirely (no re-emission).

#### `crates/nosh-client/src/main.rs` (D-04: DCS/PM/APC no-op)

**Analog:** `crates/nosh-server/src/terminal.rs` (scope fence pattern, lines 1157-1177)

**Scope fence pattern** (lines 1157-1177):
```rust
fn esc_dispatch(&mut self, _intermediates: &[u8], _ignore: bool, byte: u8) {
    match byte {
        b'c' => {
            // RIS: full reset
            // ...
        }
        _ => {
            // Scope fence: other ESC sequences are ignored.
        }
    }
}
```

**D-04 implementation:** The client does not currently handle DCS/PM/APC. This is a documentation requirement to confirm these are no-ops (not forwarded to stdout).

#### `crates/nosh-client/src/main.rs` (D-05: Title ESC strip + clipboard selection validate)

**Analog:** `crates/nosh-client/src/main.rs` (existing Title/Clipboard handling, lines 2077-2118)

**Title strip pattern** (lines 2100-2118):
```rust
TerminalControlPayload::Title { title } => {
    if !status {
        let clean_title: String = title
            .chars()
            .filter(|&c| c != '\x07' && c != '\x1b')
            .collect();
        let osc02 = format!("\x1b]0;{clean_title}\x07");
        let _ = stdout.write_all(osc02.as_bytes()).await;
    }
}
```

**Clipboard validation pattern** (lines 2088-2091):
```rust
let sel: String = String::from_utf8_lossy(&selection)
    .chars()
    .filter(|&c| c != '\x07' && c != '\x1b')
    .collect();
```

**D-05 hardening:** 
- Title: Already strips ESC/BEL (WR-03 fix shipped in v1.3).
- Clipboard: Add validation that `selection` matches known values: `b"c"`, `b"p"`, `b"q"`, `b"s"`, `b"a` (PRIMARY, SECONDARY, CLIPBOARD, SAVE, TARGET). Reject unknown selections.

#### `crates/nosh-client/src/main.rs` (D-06: Resize rate-limit + PtyData recv cap + channel-ID range)

**Analog:** `crates/nosh-client/src/main.rs` (existing resize debounce, lines 45-52) + `docs/999.1-SECURITY.md` (pre-auth caps)

**Resize debounce pattern** (lines 45-52):
```rust
const RESIZE_DEBOUNCE: Duration = Duration::from_millis(40);
```

**Pre-auth cap pattern** (from `docs/999.1-SECURITY.md` lines 16-19):
```rust
// Half-open cap: 64 concurrent connections
// Auth timeout: 5s
// Datagram buffers: 1 MiB receive + 1 MiB send
```

**D-06 implementation:**
- Resize rate-limit: Add a token bucket or rate counter on the client side to limit Resize messages to at most 1 per 100ms (defensible bound).
- PtyData recv cap: Add a byte cap per session (e.g., 16 MiB/sec) to prevent a malicious server from flooding the client.
- Channel-ID range: Validate channel_id is within 0..=MAX_CHANNELS (e.g., 1024) before opening streams.

#### `crates/nosh-client/src/main.rs` (D-07: OSC 8 hyperlink scheme whitelist)

**Analog:** N/A (new feature for SEC-04)

**Pattern:** Parse OSC 8 hyperlinks from TerminalControl (if added), validate URI scheme against whitelist: `http`, `https`, `mailto`, `file`. Reject all other schemes (e.g., `javascript:`, `data:`).

**Note:** OSC 8 is not currently forwarded by the server. This is a preparatory hardening item for when OSC 8 support is added.

### 3. SEC-01: Threat-Model Document

#### `docs/SECURITY.md` (docs, static)

**Analog:** `docs/999.1-SECURITY.md` + `docs/999.7-SECURITY.md`

**Structure pattern** (from `docs/999.1-SECURITY.md`):
```markdown
# nosh — [topic] security review

**Phase:** [phase number/name]
**Date:** [date]
**Scope:** [what's covered]
**Verdict:** [high-level assessment]

## 1. [Threat area]

### Root cause
[Technical explanation]

### Attack vector
[How it's exploited]

### Trust boundary
[Who can reach this]

## 2. Mitigation

### Mechanism
[How it's fixed]

## 3. Residual risk and rationale
[What's left and why]

## Verify before relying on this
[How to re-verify]
```

**SEC-01 required sections** (from `CONTEXT.md` D-08):
- Assets + trust boundaries
- Attacker capabilities (internet-exposed deployment)
- Proxy trust model
- Mode A vs Mode B distinction
- Mandatory-inner-auth rationale
- Residual risks
- Outer-CA-cert-but-inner-auth-is-authoritative composition (Phase 24 D-05)

## Shared Patterns

### Input Validation (Caps and Bounds)

**Source:** `crates/nosh-server/src/terminal.rs` (OSC_ACCUMULATION_MAX, lines 73-77) + `crates/nosh-client/src/screen.rs` (MAX_TERMINAL_COLS/ROWS, lines 34-41)

**Apply to:** All SEC-04 hardening items

```rust
// Server-side: OSC allocation cap
pub const OSC_ACCUMULATION_MAX: usize = 1_048_576; // 1 MiB

// Client-side: Dimension bounds
const MAX_TERMINAL_COLS: u16 = 512;
const MAX_TERMINAL_ROWS: u16 = 256;
```

**Pattern:** Define explicit constants for all caps, document rationale, validate before allocation/processing.

### Escape Sequence Sanitization

**Source:** `crates/nosh-client/src/main.rs` (lines 2088-2095, 2111-2114)

**Apply to:** All TerminalControl payload re-emission paths

```rust
let clean: String = raw
    .chars()
    .filter(|&c| c != '\x07' && c != '\x1b') // Strip BEL and ESC
    .collect();
```

**Pattern:** Always strip terminator bytes (BEL, ESC) from server-controlled strings before interpolation into OSC sequences.

### Scope Fence (No-Op Unknown Sequences)

**Source:** `crates/nosh-server/src/terminal.rs` (lines 1176-1177)

**Apply to:** D-04 (DCS/PM/APC), any future terminal extensions

```rust
_ => {
    // Scope fence: other sequences are intentionally ignored.
}
```

**Pattern:** Unknown control sequences are no-ops by default; requires explicit decision to add support.

### Defense-in-Depth Validation

**Source:** `crates/nosh-client/src/main.rs` (lines 2088-2095) + `crates/nosh-server/src/terminal.rs` (lines 1132-1134)

**Apply to:** All cross-trust-boundary data flows

```rust
// Client-side strip even though server-side validation exists
// Server-side reject even though client-side strip exists
```

**Pattern:** Validate at both ends of a trust boundary; server-side validation is authoritative, client-side is defense-in-depth.

## No Analog Found

| File | Role | Data Flow | Reason |
|------|------|-----------|--------|
| `crates/nosh-client/src/main.rs` (D-07 OSC 8 scheme whitelist) | controller | event-driven | OSC 8 hyperlink parsing is not currently implemented; this is a preparatory pattern for when the feature is added |

## Metadata

**Analog search scope:** `/home/bharris/github.com/bharrisau/nosh/crates/nosh-server/src/terminal.rs`, `/home/bharris/github.com/bharrisau/nosh/crates/nosh-client/src/`, `/home/bharris/github.com/bharrisau/nosh/docs/`, `/home/bharris/github.com/bharrisau/nosh/fuzz/`, `/home/bharris/github.com/bharrisau/nosh/.github/workflows/`

**Files scanned:** 15

**Pattern extraction date:** 2026-06-14

**Key Insights:**
1. **SEC-05 re-verification** targets existing, shipped code (`osc_prefilter` in terminal.rs). The pattern is a regression test + CI gate, not new implementation.
2. **SEC-04 client hardening** applies the existing server-side validation patterns (caps, escape sanitization, scope fence) to the client message-handling path.
3. **SEC-01 threat model** follows the existing security doc structure (999.1 and 999.7), adapted to the internet-exposed topology.
4. **Shared pattern:** All security validations follow a defense-in-depth model — validate at both ends of a trust boundary, use explicit constants for caps, scope-fence unknown sequences.
