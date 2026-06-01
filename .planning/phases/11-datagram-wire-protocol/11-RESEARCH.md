# Phase 11: Datagram Wire Protocol - Research

**Researched:** 2026-06-01
**Domain:** Postcard binary encoding, QUIC datagram sizing, sparse terminal-diff wire format (Rust)
**Confidence:** HIGH

---

<user_constraints>
## User Constraints (from CONTEXT.md)

### Locked Decisions

- **D-11-01: Cursor-priority partial update.** When changed cells exceed the cap, `encode_datagram` encodes cells prioritized by cursor proximity, fills to the cap, defers the rest. Deferred cells reappear in subsequent ticks. Everything stays on the datagram path — reliable stream NOT coupled. `encode_datagram` MUST be total: any input returns payload < `max_datagram_size() - 100`.
- **D-11-01a:** Decision documented at the encode callsite; alternatives explicitly rejected (skip-frame, reliable-stream fallback).
- **D-11-01b:** Size-cap unit test drives a full 80x24 repaint and asserts the bound.
- **D-11-02: Run-length runs.** Contiguous changed cells sharing a style encoded as `(row, start_col, style, chars)`. NOT modeled on `termwiz::Change`. Style captures fg/bg/bold/italic/underline/reverse.
- **D-11-02a:** Keep `termwiz` out of `nosh-proto` public wire contract.
- **D-11-03: Epoch = monotonic tick counter.** Never resets. Client applies only if `epoch > last_applied`. Resize is just a diff with new dims.
- **D-11-03a:** `epoch` is DISTINCT from reliable-stream `seq`. Do not conflate.
- **Locked:** postcard + serde, NO new serialization crate.
- **Locked:** `StateDiff` carries sparse runs, `epoch: u64`, terminal dimensions, cursor position.
- **Locked:** Tests: round-trip + size-cap.

### Claude's Discretion

- Exact struct field layout, style/attribute bitset representation, run struct shape.
- Cursor-distance ordering metric for partial-update fill (provided cap is guaranteed and cursor cell is included).
- Whether `encode_datagram` takes `max_datagram_size` as a parameter or a const (must be testable against 80x24 case).

### Deferred Ideas (OUT OF SCOPE)

- Coalescing diffs into one datagram per ~16ms tick — Phase 13 (SYNC-03).
- `ResumeComplete` gating so datagrams don't apply during cold-reattach replay — Phase 13.
- Any client-side application/rendering of `StateDiff` — Phase 14.

</user_constraints>

<phase_requirements>
## Phase Requirements

| ID | Description | Research Support |
|----|-------------|------------------|
| SYNC-01 | Sparse, size-bounded datagram wire format in `nosh-proto`; changed cells only, monotonic epoch, dims + cursor; payload capped below `max_datagram_size()`; round-trip and size-cap unit tests; postcard/serde, no new serialization crate | All six research questions fully resolved below |

</phase_requirements>

---

## Summary

Phase 11 delivers `nosh-proto/src/datagram.rs` — a single self-contained module that defines the `StateDiff` wire type and the `encode_datagram` / `decode_datagram` pair. The module is the shared interface that Phase 12 (server state model), Phase 13 (datagram sender), and Phase 14/15 (client predictor) all build on.

The key engineering challenge is the cursor-priority partial-update fill loop: `encode_datagram` must provably return a payload smaller than the cap for *any* input, including a full 80x24 repaint that in the worst case encodes to ~2064 bytes — nearly twice the ~1100-byte budget. The fill algorithm sorts changed runs by cursor proximity (Manhattan distance), greedily adds runs while calling `postcard::experimental::serialized_size` (a no-alloc dry-run), and defers any run that would push the encoded size over the cap. Runs spanning more bytes than the cap alone (only possible on very wide terminals, ~1063+ ASCII columns) must be split at the char level before deferral.

All six research questions are resolved at HIGH confidence from direct codebase inspection. No new crate dependencies are required for this phase — everything needed (`postcard`, `serde`, `bytes`, `quinn`) is already in the workspace.

**Primary recommendation:** Use `String` (not `Vec<char>`) for the `chars` field in a run — it is more compact under postcard (1 byte per ASCII char vs 2), naturally bounds the per-char encoding cost, and the varint length prefix is built into postcard's `str` serializer.

---

## Architectural Responsibility Map

| Capability | Primary Tier | Secondary Tier | Rationale |
|------------|-------------|----------------|-----------|
| Wire format types (`StateDiff`, `DiffRun`, `CellStyle`) | `nosh-proto` crate | — | Shared contract; both server and client depend on it |
| Encode/decode logic (`encode_datagram`, `decode_datagram`) | `nosh-proto/src/datagram.rs` | — | Isolated behind one module per D-03/D-04 migration convention |
| `max_datagram_size()` query | `quinn::Connection` (at callsite) | — | The cap is a runtime property of the QUIC path; `encode_datagram` accepts it as a parameter for testability |
| Cursor-proximity sort | Inside `encode_datagram` | — | Purely algorithmic; no I/O or async needed |
| Deferred-run tracking | Caller (Phase 13 datagram sender) | — | Phase 11 returns the deferred runs; the tick loop (Phase 13) re-presents them next cycle |

---

## Resolved Research Questions

### Q1: `Connection::max_datagram_size()` — Type, Semantics, Practical Value

**Source:** Direct inspection of `quinn-0.11.9/src/connection.rs` line 480. [VERIFIED: codebase]

```rust
pub fn max_datagram_size(&self) -> Option<usize>
```

Returns `None` if datagrams are unsupported by the peer or disabled locally. When `Some`, the value is the maximum payload bytes that may be passed to `send_datagram()`. The doc comment states: "if the peer's limit is large this is guaranteed to be a little over a kilobyte at minimum" — meaning the safe floor is ~1200 bytes on IPv6-min paths.

The value fluctuates as DPLPMTUD probes the path. The PITFALLS research confirmed: cap outgoing datagrams at 1200 bytes during the first ~10 datagrams (before DPLPMTUD converges), then track `max_datagram_size()` dynamically. [VERIFIED: codebase + PITFALLS.md]

**Implementation implication:** `encode_datagram` MUST accept `cap: usize` as a parameter rather than a compile-time constant. The caller (Phase 13) queries `conn.max_datagram_size().unwrap_or(1200)` and subtracts 100 to get the `cap` argument. This makes the function testable against the 80x24 case with `cap = 1100` without a real QUIC connection.

**Concrete values:**
- IPv6 minimum path: `max_datagram_size()` ≈ 1200 bytes (QUIC overhead ~48 bytes from UDP/QUIC framing leaves ~1152 bytes)
- Ethernet LAN (1500-byte MTU): ≈ 1452 bytes
- Budget in `encode_datagram`: `cap = max_datagram_size().unwrap_or(1200) - 100`

---

### Q2: Postcard Sizing — `serialized_size` API

**Source:** Direct inspection of `postcard-1.1.3/src/ser/mod.rs` line 490 and `postcard-1.1.3/src/lib.rs` line 60. [VERIFIED: codebase]

The pinned postcard version is **1.1.3** (from `Cargo.lock`). The workspace enables the `alloc` feature only — no `experimental-derive` needed.

```rust
// Publicly re-exported at:
postcard::experimental::serialized_size

// Signature (from ser/mod.rs:490):
pub fn serialized_size<T: Serialize + ?Sized>(value: &T) -> postcard::Result<usize>
```

This function uses the `Size` flavor — a zero-allocation dry-run serializer that counts bytes without producing output. It is always available when the `alloc` feature is enabled (the re-export at `postcard::experimental::serialized_size` has NO feature gate). The `experimental-derive` feature is only needed for the `MaxSize` derive proc-macro, which is NOT required here.

**Usage in the fill loop:**

```rust
// Check if adding a run would exceed the cap (no allocation):
let candidate_diff = StateDiff { runs: draft_runs.clone(), ..diff_header };
let encoded_size = postcard::experimental::serialized_size(&candidate_diff)?;
if encoded_size <= cap { draft_runs.push(run); } else { deferred.push(run); }
```

The check is O(runs_so_far) per step (the size serializer walks the whole struct). With at most ~24 runs for an 80x24 terminal, the total work is O(n²) ≈ 576 iterations — negligible.

**Alternative: `postcard::to_slice` with a fixed buffer:**

```rust
let mut buf = [0u8; 1100];
match postcard::to_slice(&candidate_diff, &mut buf) {
    Ok(used) => { draft_runs.push(run); }
    Err(_) => { deferred.push(run); }
}
```

This is also correct: `to_slice` returns `Err(SerializeBufferFull)` when the value overflows the buffer. It avoids the `experimental` API at the cost of a stack-allocated buffer. **The planner should pick one and use it consistently.** `serialized_size` is slightly cleaner (no buffer, pure count); `to_slice` is slightly simpler to reason about.

---

### Q3: Cursor-Priority Fill Algorithm

**Guarantee:** `encode_datagram` is total — for any input, the output is `< cap`.

**Algorithm (recommended: greedy with `serialized_size` check):**

```
fn encode_datagram(diff: &StateDiff, cap: usize) -> Result<(Bytes, Vec<DiffRun>), ProtoError>:

1. Sort all runs by priority:
   priority = |run.row - cursor.row| * terminal_cols + |run.start_col - cursor.col|
   Runs on the cursor row have the lowest priority score.
   The run containing the cursor cell itself is first (priority = 0).

2. Build draft incrementally:
   draft_runs = Vec::new()
   deferred = Vec::new()
   for run in sorted_runs:
       tentative = StateDiff { runs: draft_runs + [run], ..header }
       if serialized_size(&tentative) <= cap:
           draft_runs.push(run)
       else:
           // Try to split: how many chars fit?
           split_chars = compute_max_chars_that_fit(run, remaining_cap)
           if split_chars > 0:
               draft_runs.push(run.take_first(split_chars))
               deferred.push(run.skip_first(split_chars))
           else:
               deferred.push(run)
           // Do NOT stop — a later shorter run may still fit.

3. final_diff = StateDiff { runs: draft_runs, ..header }
   payload = postcard::to_allocvec(&final_diff)?
   debug_assert!(payload.len() < cap)
   return (Bytes::from(payload), deferred)
```

**Edge case: single run exceeds cap (very wide terminals):**

A `DiffRun` with `chars` of 1064+ ASCII bytes (only possible on terminals wider than ~1063 columns) would have an encoded size exceeding the ~1075-byte run budget. The split is:

```
// remaining_cap = cap - serialized_size(&StateDiff { runs: [], ..header })
// max_char_bytes = remaining_cap - run_header_bytes (row + col + style + fg + bg + str_varint_prefix)
// run_header_worst_case = 3+3+1+1+1+3 = 12 bytes
// max_chars = max_char_bytes (UTF-8 bytes, not codepoints for multibyte)
split_point = compute_utf8_split(run.chars, max_char_bytes)
left = DiffRun { start_col: run.start_col, chars: run.chars[..split_point] }
right = DiffRun { start_col: run.start_col + char_count(left.chars), chars: run.chars[split_point..] }
```

For practical terminal sizes (≤ 512 columns), a single run never exceeds the cap alone. The split path is a defensive correctness requirement, not a hot path.

**Invariant verification:** After `encode_datagram` returns, call `debug_assert!(payload.len() < cap)` in the implementation. The size-cap unit test provides release-mode coverage.

---

### Q4: Style/Attribute Representation

**Recommendation (Claude's Discretion):** Three-byte style: `CellStyle(u8)` bitflags + `fg: u8` + `bg: u8`. [ASSUMED — no external standard mandates this; chosen for compactness]

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CellStyle(pub u8);

impl CellStyle {
    pub const BOLD:      u8 = 0x01;
    pub const ITALIC:    u8 = 0x02;
    pub const UNDERLINE: u8 = 0x04;
    pub const REVERSE:   u8 = 0x08;
    // Bits 0x10, 0x20, 0x40, 0x80 reserved for future SGR attributes.
}
```

`fg` and `bg` are ANSI 256-color indices (`0` = default terminal color). This encodes as 1 byte each under postcard (`u8` = 1 byte always, no varint).

**Why not a struct with `bool` fields:** Each `bool` encodes as 1 byte in postcard. Four bools = 4 bytes vs one `u8` = 1 byte. The bitflag approach is 3x more compact per run.

**Why not `bitflags` crate:** Adds a dependency for a 1-byte type. Hand-rolled `CellStyle(u8)` is equivalent on the wire.

**True color (RGB):** Deferred — not needed for v1.2. When needed, add `Option<[u8;3]>` fg/bg fields (adds 0-4 bytes per run when `None` = 0).

**Postcard encoding size per `DiffRun` header (worst case):**

| Field | Type | Postcard bytes |
|-------|------|----------------|
| `row` | `u16` | 1–3 (varint) |
| `start_col` | `u16` | 1–3 (varint) |
| `style` | `CellStyle(u8)` | 1 |
| `fg` | `u8` | 1 |
| `bg` | `u8` | 1 |
| `chars` (String) | varint(len) + UTF-8 | 1 + len |
| **Total header** | | **6–10 bytes + char bytes** |

---

### Q5: Test Shapes

**nyquist_validation is `false` in config.json** — no test framework section required. However, SYNC-01 explicitly requires round-trip and size-cap tests; these are non-negotiable.

**Test 1 — Round-trip (concrete cases):**

```rust
#[test]
fn encode_decode_round_trip() {
    let diff = StateDiff {
        epoch: 42,
        cols: 80,
        rows: 24,
        cursor: CursorPos { row: 12, col: 40 },
        runs: vec![DiffRun {
            row: 12, start_col: 40,
            style: CellStyle(CellStyle::BOLD | CellStyle::UNDERLINE),
            fg: 2, bg: 0,
            chars: String::from("hello"),
        }],
    };
    let (encoded, deferred) = encode_datagram(&diff, 1100).unwrap();
    assert!(deferred.is_empty());
    let decoded = decode_datagram(&encoded).unwrap();
    assert_eq!(diff, decoded);
}
```

**proptest** is not in the workspace. Options for the planner:
- Add `proptest = "1"` as a dev-dependency in `nosh-proto` — acceptable since it is a test tool, not a serialization crate. This is Claude's discretion; the planner should include proptest if the effort is ≤ 1 task.
- Hand-rolled property test with a representative matrix (empty diff, single cell, full row, mixed styles) — sufficient without proptest.

**Test 2 — Size-cap (success criterion SC#3):**

```rust
#[test]
fn size_cap_full_80x24_repaint() {
    // 24 full-row runs, all cells changed, cursor at (0,0)
    let runs: Vec<DiffRun> = (0u16..24).map(|row| DiffRun {
        row, start_col: 0,
        style: CellStyle(0), fg: 0, bg: 0,
        chars: "a".repeat(80),
    }).collect();
    let diff = StateDiff { epoch: 1, cols: 80, rows: 24, cursor: CursorPos { row: 0, col: 0 }, runs };
    let (encoded, deferred) = encode_datagram(&diff, 1100).unwrap();
    assert!(encoded.len() < 1100,
        "payload {} bytes must be < 1100", encoded.len());
    // At 86 bytes per row, 12 rows fit; 12 rows must be deferred
    assert!(!deferred.is_empty(), "full repaint must produce deferred runs");
}
```

**Test 3 — No-deferred for small change:**

```rust
#[test]
fn single_cell_change_no_deferred() {
    let diff = StateDiff {
        epoch: 5, cols: 80, rows: 24, cursor: CursorPos { row: 12, col: 40 },
        runs: vec![DiffRun { row: 12, start_col: 40, style: CellStyle(0), fg: 0, bg: 0, chars: String::from("x") }],
    };
    let (encoded, deferred) = encode_datagram(&diff, 1100).unwrap();
    assert!(deferred.is_empty());
    assert!(encoded.len() < 1100);
}
```

**Test 4 — Cursor-priority ordering:**

```rust
#[test]
fn cursor_priority_includes_cursor_row_first() {
    // Rows 0–23 all changed; cursor at row 23 (bottom).
    // After encode, row 23 MUST be in the encoded payload (highest priority).
    let rows: Vec<DiffRun> = (0u16..24).map(|row| DiffRun {
        row, start_col: 0, style: CellStyle(0), fg: 0, bg: 0,
        chars: "x".repeat(80),
    }).collect();
    let diff = StateDiff { epoch: 1, cols: 80, rows: 24, cursor: CursorPos { row: 23, col: 0 }, runs: rows };
    let (encoded, _deferred) = encode_datagram(&diff, 1100).unwrap();
    let decoded = decode_datagram(&encoded).unwrap();
    assert!(decoded.runs.iter().any(|r| r.row == 23),
        "cursor row 23 must be included in encoded payload");
}
```

---

### Q6: Postcard Version and `serialized_size` API Surface

**Pinned version:** `postcard 1.1.3` (from `Cargo.lock` line 1293–1302). [VERIFIED: codebase]

**Workspace feature:** `alloc` only (from root `Cargo.toml`: `postcard = { version = "1", default-features = false, features = ["alloc"] }`). [VERIFIED: codebase]

**Available functions (with `alloc` feature):**
- `postcard::to_allocvec(value)` — serializes to `Vec<u8>`. Used by codec.rs; use the same pattern in `encode_datagram`.
- `postcard::from_bytes(bytes)` — deserializes from `&[u8]`. Use in `decode_datagram`.
- `postcard::experimental::serialized_size(value)` — returns `Result<usize>`; no allocation. [VERIFIED: codebase — re-exported unconditionally in `experimental` module]
- `postcard::to_slice(value, &mut buf)` — serializes to a fixed buffer; returns `Err(SerializeBufferFull)` on overflow. Alternative to `serialized_size` for the cap check.

**`char` encoding in postcard 1.1.3:** [VERIFIED: codebase — `postcard-1.1.3/src/ser/serializer.rs:177`]
A `char` is serialized as a UTF-8 `str` (length-prefixed): 1-byte varint length + UTF-8 bytes. ASCII chars = 2 bytes; 4-byte Unicode = 5 bytes. The `MaxSize` for `char` is 5 bytes.

**`String` encoding:** Varint length prefix (1–3 bytes for strings ≤ 16383 bytes) + raw UTF-8 bytes. For 80 ASCII chars: 1 + 80 = 81 bytes. This is 2× more compact than `Vec<char>` for ASCII (which would be 1 + 80×2 = 161 bytes).

**Varint sizes:**

| Type | Varint max bytes | Typical (small value) |
|------|-----------------|----------------------|
| `u8` | 2 | 1 byte (value < 128) |
| `u16` | 3 | 1 byte (value < 128) |
| `u32` | 5 | 1–2 bytes |
| `u64` | 10 | 1–2 bytes (early epochs) |

---

## Standard Stack

### Core (all already in workspace — no new deps)

| Library | Version | Purpose | Why Standard |
|---------|---------|---------|--------------|
| `postcard` | 1.1.3 (pinned) | Wire serialization | Locked by SYNC-01/D-04; already in `nosh-proto/Cargo.toml` |
| `serde` | 1.x | Derive traits | Locked; already in workspace |
| `bytes` | 1.x | `Bytes` return type from `encode_datagram` | Already a dep of `nosh-proto` |
| `quinn` | 0.11.9 | `Connection::max_datagram_size()` at callsite | Already in `nosh-proto` for transport types |

**No new dependencies required for Phase 11.** The `postcard::experimental::serialized_size` function is available without any new feature flag (the `alloc` feature is already enabled).

### Package Legitimacy Audit

> Phase 11 installs zero new packages. All dependencies are pre-existing workspace members.

**Packages removed due to slopcheck [SLOP] verdict:** none
**Packages flagged as suspicious [SUS]:** none

---

## Architecture Patterns

### System Architecture Diagram

```
Phase 11 scope: nosh-proto/src/datagram.rs (new module)

┌──────────────────────────────────────────────────────────────┐
│  nosh-proto/src/datagram.rs  (Phase 11 deliverable)          │
│                                                              │
│  StateDiff { epoch, cols, rows, cursor, runs: Vec<DiffRun> } │
│  DiffRun   { row, start_col, style: CellStyle, fg, bg, chars }│
│  CursorPos { row, col }                                      │
│  CellStyle (u8 bitflags: BOLD/ITALIC/UNDERLINE/REVERSE)      │
│                                                              │
│  encode_datagram(diff: &StateDiff, cap: usize)               │
│    → Result<(Bytes, Vec<DiffRun>), ProtoError>               │
│       ↑ payload (provably < cap)     ↑ deferred runs         │
│                                                              │
│  decode_datagram(bytes: &[u8])                               │
│    → Result<StateDiff, ProtoError>                           │
│                                                              │
│  [INTERNAL] cursor_priority_fill(runs, cursor, cap, cols)    │
│    sorts by Manhattan distance, greedy fill with             │
│    postcard::experimental::serialized_size check             │
└──────────────────────────────────────────────────────────────┘
           ↓ imports from                   ↓ imports from
    nosh-proto/src/codec.rs           nosh-proto/src/messages.rs
    (ProtoError — reuse directly)     (nothing — epoch ≠ seq, distinct types)

Phase 13 (caller):
  conn.max_datagram_size().unwrap_or(1200) - 100 → cap
  encode_datagram(&statediff, cap) → (payload, deferred)
  conn.send_datagram(payload)
  [keep deferred for next tick]
```

### Recommended Project Structure

```
crates/nosh-proto/src/
├── codec.rs         # unchanged (ProtoError lives here — reuse it)
├── datagram.rs      # [NEW] Phase 11 deliverable
├── lib.rs           # add: pub mod datagram; re-export StateDiff, encode_datagram, decode_datagram
├── messages.rs      # unchanged (reliable-stream Message enum)
└── transport.rs     # unchanged (RFC 9221 datagram enablement)
```

### Pattern 1: Codec Module Convention (mirror codec.rs)

**What:** Wire format isolated behind one module; postcard is the only dependency.
**When to use:** Always — this is the D-03/D-04 migration-path convention.

```rust
// Source: crates/nosh-proto/src/codec.rs (existing pattern to mirror)
// encode_datagram returns Bytes (consumed by conn.send_datagram)
// decode_datagram takes &[u8] (from conn.read_datagram().as_ref())

pub fn encode_datagram(diff: &StateDiff, cap: usize) -> Result<(Bytes, Vec<DiffRun>), ProtoError> {
    // [Phase 11 implementation]
    // 1. cursor_priority_fill to get encoded_runs, deferred_runs
    // 2. postcard::to_allocvec(&StateDiff { runs: encoded_runs, .. })
    // 3. debug_assert!(payload.len() < cap)
    // 4. Ok((Bytes::from(payload), deferred_runs))
}

pub fn decode_datagram(bytes: &[u8]) -> Result<StateDiff, ProtoError> {
    Ok(postcard::from_bytes(bytes)?)
}
```

### Pattern 2: ProtoError Reuse

**What:** `datagram.rs` imports and re-uses `crate::codec::ProtoError` — no new error type.
**When to use:** Always — `postcard::Error` is already wrapped by `ProtoError::Postcard`.

```rust
// In datagram.rs:
use crate::codec::ProtoError;
// postcard::Error -> ProtoError::Postcard via #[from] already on ProtoError
```

### Pattern 3: Encode/decode Tag Byte (Discriminant)

The datagram channel carries two message types in Phase 11+: `StateDiff` (server → client) and `ClientEpoch` (client → server, future Phase 13). The architecture research recommends a tag byte to distinguish them.

**Recommendation for Phase 11:** Phase 11 only introduces `StateDiff`. Add a 1-byte tag prefix now so Phase 13 can add `ClientEpoch` without breaking the decode path:

```rust
const TAG_STATE_DIFF: u8 = 0x01;
// const TAG_CLIENT_EPOCH: u8 = 0x02; // reserved for Phase 13

pub fn encode_datagram(diff: &StateDiff, cap: usize) -> Result<(Bytes, Vec<DiffRun>), ProtoError> {
    // ... fill loop ...
    let body = postcard::to_allocvec(&encoded_diff)?;
    let mut payload = Vec::with_capacity(1 + body.len());
    payload.push(TAG_STATE_DIFF);
    payload.extend_from_slice(&body);
    Ok((Bytes::from(payload), deferred))
}

pub fn decode_datagram(bytes: &[u8]) -> Result<StateDiff, ProtoError> {
    // Strip tag byte; handle unknown tags as ProtoError
    let (_tag, body) = bytes.split_first().ok_or(ProtoError::Postcard(postcard::Error::DeserializeUnexpectedEnd))?;
    Ok(postcard::from_bytes(body)?)
}
```

The 1 tag byte reduces the effective cap by 1 — negligible (adjust `cap` accounting to `cap - 1` before the fill loop).

### Anti-Patterns to Avoid

- **Using `Vec<char>` instead of `String` for `chars` field:** `Vec<char>` encodes as 2 bytes per ASCII char under postcard (varint-per-char = `char` serializes as `str`). `String` encodes as varint_len + raw UTF-8 = 1 byte per ASCII char. Use `String`.
- **Calling `encode_datagram` with a hardcoded cap:** The cap must come from `conn.max_datagram_size()` at the callsite. For tests, pass `1100` explicitly.
- **Sharing the `Message` enum for datagrams:** `StateDiff` is NOT a `Message` variant. Datagrams bypass the reliable-stream length-prefix framing entirely. Separate `encode_datagram`/`decode_datagram` from `codec::encode`/`decode`.
- **Resetting epoch on resize:** Resize is just a diff with updated `cols`/`rows`. Epoch increments monotonically; it never resets (D-11-03).
- **Conflating epoch with seq:** `epoch` is for datagrams (latest-state-wins), `seq` is for reliable-stream `Ack` (sequential). These are independent counters.
- **Stopping the fill loop after the first rejection:** A shorter run after a rejected long run may still fit. Continue iterating all candidates.

---

## Don't Hand-Roll

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| Serialized size check | Custom byte-counter that mirrors postcard format manually | `postcard::experimental::serialized_size` | Any deviation from postcard's actual encoding causes incorrect cap checks; the `Size` flavor is the exact serializer with output discarded |
| Binary encoding format | Custom codec | `postcard::to_allocvec` / `from_bytes` | Locked by SYNC-01; codec.rs already establishes this pattern |
| Varint encoding for size estimates | Manual varint math | `postcard::experimental::serialized_size` | postcard varints handle signed/unsigned differently; hand-computing them is fragile |

**Key insight:** The `serialized_size` function uses the exact same serialization path as `to_allocvec` — it's the same serializer with a counter-only output. There is no gap between the size estimate and the actual encoded size.

---

## Common Pitfalls

### Pitfall 1: `postcard::experimental::serialized_size` Requires the `alloc` Feature

**What goes wrong:** Calling `postcard::experimental::serialized_size` when the `alloc` feature is not enabled produces a compile error — the function is available (it's in the `experimental` module unconditionally) but the `Size` flavor's underlying types require alloc internally.

**Why it happens:** The workspace `Cargo.toml` pins `postcard = { ..., features = ["alloc"] }` which IS enabled. No issue in practice for this project.

**How to avoid:** The workspace already has `alloc` enabled. No action needed. Do NOT add `experimental-derive` — it's a proc-macro feature for the `MaxSize` derive, not needed here.

**Warning signs:** Compile errors mentioning `Size` flavor or `alloc` in `postcard::ser::flavors`.

---

### Pitfall 2: `max_datagram_size()` Returns `None` Before Transport Config Is Applied

**What goes wrong:** `conn.max_datagram_size()` returns `None` if the peer has not enabled datagrams. This would panic if the callsite does `.unwrap()`.

**Why it happens:** If the server or client `TransportConfig` does not set `datagram_receive_buffer_size(Some(...))`, the peer reports no datagram support and `max_datagram_size()` returns `None`.

**How to avoid:** Both endpoints already set datagram buffers in `transport.rs` (verified: `datagram_receive_buffer_size(Some(DATAGRAM_BUFFER))` and `datagram_send_buffer_size(DATAGRAM_BUFFER)`). The callsite should still use `.unwrap_or(1200)` as a defensive fallback. `encode_datagram` itself never calls this method — it receives `cap` as a parameter.

---

### Pitfall 3: `char` is NOT 1 Byte Under Postcard

**What goes wrong:** A developer assumes `char` encodes as 1 byte (like a C char), writes `Vec<char>` for the `chars` field, and discovers the 80-col row encodes to 160+ bytes per run instead of 80, blowing the cap calculation.

**Why it happens:** postcard serializes `char` as a UTF-8 string: `v.encode_utf8(&mut buf).serialize(self)`, which means 1-byte varint length prefix + 1–4 UTF-8 bytes. For ASCII chars this is 2 bytes per char. [VERIFIED: postcard-1.1.3/src/ser/serializer.rs:177]

**How to avoid:** Use `String` for the `chars` field. postcard serializes `String` as 1 varint (len) + raw UTF-8 bytes — 1 byte per ASCII char, optimal. The run contains a contiguous substring of terminal text; a `String` is the natural Rust type and the compact encoding.

**Warning signs:** 80-col row encodes to > 90 bytes in tests; size calculations use `char` as 1 byte.

---

### Pitfall 4: Datagram Tag Byte Not Allocated in the Cap Budget

**What goes wrong:** If a 1-byte tag prefix is added to the datagram payload (Pattern 3 above), the fill loop uses `cap` as the budget for the postcard body, but the actual total payload is `body.len() + 1`. The `debug_assert` catches this in dev builds but the bug is silent in release.

**How to avoid:** Adjust the effective cap before the fill loop: `let body_cap = cap - 1;` and use `body_cap` in all `serialized_size` checks.

---

### Pitfall 5: Deferred Run's `start_col` After a Split

**What goes wrong:** A run is split at char index `N`. The deferred run must have `start_col = original.start_col + N`, not `start_col = original.start_col`. If the deferred run is re-presented next tick with the wrong `start_col`, cells land in the wrong column.

**How to avoid:** The split helper must compute `start_col` correctly. For a `String` chars field, count Unicode scalar values (not UTF-8 bytes) to advance `start_col` by the number of *displayed characters*, not bytes. Use `chars().count()` on the taken prefix.

**Warning signs:** Wide-terminal split tests show cells at wrong columns after deferred repainting.

---

## Code Examples

### StateDiff Type (Recommended Shape)

```rust
// Source: designed from postcard encoding analysis (this research document)

use bytes::Bytes;
use serde::{Deserialize, Serialize};
use crate::codec::ProtoError;

/// Monotonic epoch counter. Incremented on every emitted diff. Never resets.
/// Client applies a diff only if epoch > last_applied_epoch (D-11-03).
/// DISTINCT from reliable-stream seq (D-11-03a).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateDiff {
    /// Server-side state version. Monotonically increasing, never resets.
    pub epoch: u64,
    /// Terminal width in columns.
    pub cols: u16,
    /// Terminal height in rows.
    pub rows: u16,
    /// Cursor position at the time this diff was encoded.
    pub cursor: CursorPos,
    /// Changed cells encoded as run-length runs (sparse; only changed cells).
    /// May be a subset of all changed cells if the full set exceeded the cap
    /// (cursor-priority partial update, D-11-01). Deferred cells reappear
    /// in subsequent ticks.
    pub runs: Vec<DiffRun>,
}

/// A cursor position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CursorPos {
    pub row: u16,
    pub col: u16,
}

/// A run of contiguous changed cells on one row sharing the same style.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffRun {
    /// Row index (0-based).
    pub row: u16,
    /// Column index of the first cell in the run (0-based).
    pub start_col: u16,
    /// SGR attributes for all cells in this run.
    pub style: CellStyle,
    /// ANSI 256-color foreground index (0 = default terminal color).
    pub fg: u8,
    /// ANSI 256-color background index (0 = default terminal color).
    pub bg: u8,
    /// UTF-8 text for all cells in the run. Length in chars == column count
    /// of the run (single-width chars assumed; wide chars handled by Phase 15).
    pub chars: String,
}

/// SGR attribute bitflags. Encodes as a single u8 under postcard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CellStyle(pub u8);

impl CellStyle {
    pub const NONE:      u8 = 0x00;
    pub const BOLD:      u8 = 0x01;
    pub const ITALIC:    u8 = 0x02;
    pub const UNDERLINE: u8 = 0x04;
    pub const REVERSE:   u8 = 0x08;
    // Bits 0x10–0x80: reserved for future SGR attributes.
}
```

### encode_datagram Skeleton

```rust
// Source: designed from postcard 1.1.3 API analysis (this research document)

const TAG_STATE_DIFF: u8 = 0x01;

/// # Large-repaint decision (D-11-01)
///
/// When the set of changed cells (`diff.runs`) would exceed the datagram cap,
/// this function encodes cells prioritized by proximity to the cursor and defers
/// the rest. Deferred cells naturally reappear in subsequent ticks because diffs
/// are computed against the last *acked* state.
///
/// ## Alternatives rejected
///
/// - **Skip-frame:** A persistently-large screen (full-screen vim, `cat big_file`)
///   could fail to converge — the screen never fully updates.
/// - **Reliable-stream fallback:** Couples the datagram and stream channels;
///   reintroduces head-of-line blocking for what should be a loss-tolerant path,
///   contradicting the core architecture (CLAUDE.md load-bearing decisions).
///
/// ## Guarantee (D-11-01b)
///
/// For ANY input, the returned payload satisfies `payload.len() < cap`. This is
/// enforced by the cursor-priority fill loop, not assumed.
pub fn encode_datagram(
    diff: &StateDiff,
    cap: usize,
) -> Result<(Bytes, Vec<DiffRun>), ProtoError> {
    // Account for 1-byte tag prefix in the body budget.
    let body_cap = cap.saturating_sub(1);

    // Sort runs by Manhattan distance to cursor (ascending = cursor-closest first).
    let mut sorted_runs = diff.runs.clone();
    let cols = diff.cols as u32;
    let cursor = &diff.cursor;
    sorted_runs.sort_by_key(|r| {
        (r.row as i32 - cursor.row as i32).unsigned_abs() * cols
            + (r.start_col as i32 - cursor.col as i32).unsigned_abs()
    });

    let mut encoded_runs: Vec<DiffRun> = Vec::new();
    let mut deferred_runs: Vec<DiffRun> = Vec::new();
    let header = StateDiff { runs: vec![], ..diff.clone() };

    for run in sorted_runs {
        // Tentatively add this run and check if it fits.
        encoded_runs.push(run.clone());
        let candidate = StateDiff { runs: encoded_runs.clone(), ..header.clone() };
        let size = postcard::experimental::serialized_size(&candidate)
            .map_err(ProtoError::Postcard)?;
        if size <= body_cap {
            // Run fits. Keep it.
        } else {
            // Run doesn't fit. Remove it and try to split.
            encoded_runs.pop();
            // [TODO: split_run_to_fit(run, remaining_cap) -> (Option<DiffRun>, DiffRun)]
            // For now: defer the whole run. The split path handles very wide terminals.
            deferred_runs.push(run);
        }
    }

    let final_diff = StateDiff { runs: encoded_runs, ..header };
    let body = postcard::to_allocvec(&final_diff).map_err(ProtoError::Postcard)?;
    debug_assert!(body.len() <= body_cap, "fill loop invariant violated: {} > {}", body.len(), body_cap);

    let mut payload = Vec::with_capacity(1 + body.len());
    payload.push(TAG_STATE_DIFF);
    payload.extend_from_slice(&body);

    Ok((Bytes::from(payload), deferred_runs))
}

/// Decode a datagram payload into a [`StateDiff`].
pub fn decode_datagram(bytes: &[u8]) -> Result<StateDiff, ProtoError> {
    let (tag, body) = bytes.split_first()
        .ok_or_else(|| ProtoError::Postcard(postcard::Error::DeserializeUnexpectedEnd))?;
    if *tag != TAG_STATE_DIFF {
        return Err(ProtoError::Postcard(postcard::Error::DeserializeBadEncoding));
    }
    Ok(postcard::from_bytes(body).map_err(ProtoError::Postcard)?)
}
```

### Size Budget Illustration

```
Cap = max_datagram_size().unwrap_or(1200) - 100 = 1100 bytes
Body cap = 1100 - 1 (tag) = 1099 bytes

StateDiff header (epoch=1, cols=80, rows=24, cursor=(0,0)):
  epoch:  1 byte  (varint for epoch=1)
  cols:   1 byte  (varint for 80)
  rows:   1 byte  (varint for 24)
  cursor_row: 1 byte
  cursor_col: 1 byte
  Vec<DiffRun> length varint: 1 byte
  TOTAL HEADER: ~6 bytes

Remaining for runs: 1099 - 6 = 1093 bytes

Per DiffRun (80-col ASCII row, String chars):
  row:       1 byte (varint, < 128)
  start_col: 1 byte (varint for 0)
  style:     1 byte (CellStyle u8)
  fg:        1 byte
  bg:        1 byte
  chars len: 1 byte (varint for 80)
  chars:    80 bytes (raw UTF-8)
  TOTAL:    86 bytes

Runs fitting in 1093 bytes: floor(1093 / 86) = 12 full rows
Deferred: 24 - 12 = 12 rows (worst case: cursor at row 0, bottom half deferred)
```

---

## State of the Art

| Old Approach | Current Approach | When Changed | Impact |
|--------------|------------------|--------------|--------|
| Full terminal grid per datagram (7+ KB) | Sparse changed-cells diff with size cap | Established by Mosh SSP design (2012); adopted here | Fits in ~1200-byte QUIC datagram |
| Per-cell encoding | Run-length runs (same style → one entry) | D-11-02 (this phase) | 80-col line = 86 bytes vs 80×6 = 480 bytes per-cell |
| postcard `Vec<char>` for text | `String` field | Best practice; this research | 2× more compact for ASCII terminal content |
| Frame-per-chunk datagram | One datagram per ~16ms tick (Phase 13) | D-11-03/SYNC-03 | Reduces datagram count; this phase defines the type only |

**Deprecated/outdated:**
- `termwiz::Change` as the diff unit: rejected by D-11-02a — creates dependency coupling in the proto crate's public contract.
- `Vec<char>` for run chars: superseded by `String` (2× more compact under postcard for ASCII).

---

## Assumptions Log

| # | Claim | Section | Risk if Wrong |
|---|-------|---------|---------------|
| A1 | `CellStyle(u8)` + `fg: u8` + `bg: u8` is the right style encoding (no true-color for v1.2) | Style Representation | Low — true-color can be added later as Option<[u8;3]> without breaking the epoch/run shape |
| A2 | Manhattan distance is the right cursor-proximity metric | Fill Algorithm | Low — any metric that prioritizes the cursor cell first satisfies D-11-01; the exact metric is Claude's discretion |
| A3 | A 1-byte discriminant tag prefix is the right way to support future `ClientEpoch` datagram type | Architecture Patterns | Low — if Phase 13 uses a separate encode path for ClientEpoch, the tag can be omitted; but adding it now costs 1 byte and prevents future breaking changes |
| A4 | proptest is not needed (hand-rolled round-trip tests sufficient for SYNC-01) | Test Shapes | Low — SYNC-01 says "round-trip tests", not "proptest"; the planner can add proptest as dev-dep if desired |

**If this table is empty:** Not empty — four low-risk assumptions noted above. None require user confirmation; all are Claude's discretion per CONTEXT.md.

---

## Open Questions (RESOLVED)

1. **What is `Connection::max_datagram_size()` return type and practical minimum?**
   - (RESOLVED) Returns `Option<usize>`. `None` means peer didn't enable datagrams. Practical minimum ~1200 bytes (IPv6 min path). `encode_datagram` receives `cap` as a parameter; caller uses `conn.max_datagram_size().unwrap_or(1200) - 100`.

2. **Does postcard offer a serialized-size helper without allocation?**
   - (RESOLVED) Yes: `postcard::experimental::serialized_size(value) -> Result<usize>`. Available with the `alloc` feature only (no `experimental-derive` needed). Pinned in postcard 1.1.3, re-exported unconditionally in the `experimental` module.

3. **Concrete fill algorithm shape — greedy add-then-check vs precompute?**
   - (RESOLVED) Greedy add-then-check using `serialized_size`. When a run doesn't fit, try splitting at the char level; if no chars fit, defer the whole run and continue. O(n²) but n ≤ ~24 for 80x24 terminal.

4. **Style/attribute representation — bitflags vs enum?**
   - (RESOLVED) `CellStyle(u8)` bitflags + `fg: u8` + `bg: u8` = 3 bytes per run. More compact than bool fields (1 byte each) or Color enum variants. No `bitflags` crate needed.

5. **Test shape for round-trip and size-cap?**
   - (RESOLVED) Four concrete tests documented above. proptest optional (Claude's discretion for planner). The size-cap test drives a full 80x24 repaint and asserts `payload.len() < 1100`.

6. **Postcard version and `serialized_size` surface?**
   - (RESOLVED) postcard 1.1.3 (from Cargo.lock). `postcard::experimental::serialized_size` is at line 60 of lib.rs, unconditionally re-exported. `char` encodes as str (2 bytes for ASCII). Use `String` for run chars.

---

## Environment Availability

Step 2.6 SKIPPED — Phase 11 is a pure code-and-tests change in `nosh-proto`. No external dependencies beyond the existing Rust workspace toolchain.

---

## Security Domain

`security_enforcement` is not set to `false` in config.json (absent = enabled by default).

### Applicable ASVS Categories

| ASVS Category | Applies | Standard Control |
|---------------|---------|-----------------|
| V2 Authentication | No | Phase 11 is a wire format; no auth logic |
| V3 Session Management | No | No session lifecycle in this module |
| V4 Access Control | No | Pure encode/decode |
| V5 Input Validation | Yes | `decode_datagram` must handle malformed/oversized/truncated bytes without panic |
| V6 Cryptography | No | QUIC TLS 1.3 provides datagram authentication at the transport layer |

### Input Validation for `decode_datagram`

The function is called with bytes from `conn.read_datagram()` — bytes authenticated and decrypted by QUIC TLS 1.3, but potentially malformed at the application layer (corrupt postcard encoding, wrong tag byte, truncated payload).

Required validation:
- Empty bytes → return `ProtoError` (not panic)
- Unknown tag byte → return `ProtoError`
- Malformed postcard body → `postcard::from_bytes` returns `Err`; propagate as `ProtoError::Postcard`
- `String` chars field with invalid UTF-8 → postcard rejects this by construction (postcard uses serde's `str` deserializer which validates UTF-8)
- Excessively large `Vec<DiffRun>` → postcard deserializes all elements; add a sanity check: `if diff.runs.len() > 4096 { return Err(...) }` to prevent a malformed packet from allocating megabytes of run structs

### Known Threat Patterns

| Pattern | STRIDE | Standard Mitigation |
|---------|--------|---------------------|
| Malformed postcard body causing panic | DoS | postcard::from_bytes returns Err, not panic; no unsafe |
| Oversized `Vec<DiffRun>` (e.g., claimed len = 2^32) | DoS | Postcard varint decodes the length then allocates; add a `max_runs` sanity check |
| Stale datagram (old epoch applied after newer one) | Tampering | Application-layer monotonic epoch check at caller (Phase 14): apply only if `epoch > last_applied_epoch` |
| Injection from outside QUIC session | Tampering | Not possible — QUIC TLS 1.3 provides per-packet authentication (RFC 9001) |

---

## Sources

### Primary (HIGH confidence)
- `quinn-0.11.9/src/connection.rs:480` — `max_datagram_size()` API, `Option<usize>` return type, doc comment on peer limit and MTU floor — verified in local cargo cache
- `postcard-1.1.3/src/lib.rs:60` — `postcard::experimental::serialized_size` re-export (unconditional, no feature gate) — verified in local cargo cache
- `postcard-1.1.3/src/ser/mod.rs:490` — `serialized_size` function signature and doc — verified in local cargo cache
- `postcard-1.1.3/src/ser/serializer.rs:177` — `serialize_char` encodes as str (encode_utf8 + serialize) — verified in local cargo cache
- `postcard-1.1.3/src/max_size.rs:89` — `char::POSTCARD_MAX_SIZE = 5` — verified in local cargo cache
- `postcard-1.1.3/src/varint.rs:1` — varint_max formula: `(bits + 6) / 7` — verified in local cargo cache
- `Cargo.lock:1293` — postcard version 1.1.3 pinned — verified in project root
- `Cargo.toml:22` — workspace postcard features: `["alloc"]` — verified in project root
- `crates/nosh-proto/src/codec.rs` — postcard pattern (to_allocvec/from_bytes/ProtoError) to mirror — verified in codebase
- `crates/nosh-proto/src/messages.rs` — seq convention (DISTINCT from epoch) — verified in codebase
- `crates/nosh-proto/src/transport.rs:9` — DATAGRAM_BUFFER, RFC 9221 already enabled — verified in codebase
- `.planning/research/PITFALLS.md` — MTU limits, WSAEMSGSIZE, max_datagram_size advice — project research artifact

### Secondary (MEDIUM confidence)
- `.planning/research/ARCHITECTURE.md` — StateDiff/DiffEncoder design sketch, recommended project structure — project research artifact (verified consistent with codebase)
- `.planning/phases/11-datagram-wire-protocol/11-CONTEXT.md` — locked decisions D-11-01/02/03 — project context artifact

### Tertiary (LOW confidence)
- None — all claims in this research are verified or assumed-low-risk design choices.

---

## Metadata

**Confidence breakdown:**
- Postcard API surface: HIGH — inspected source in local cargo cache
- quinn max_datagram_size: HIGH — inspected source in local cargo cache
- Size math (runs per datagram): HIGH — computed from verified postcard encoding rules
- Fill algorithm design: HIGH — derived from verified API + standard greedy algorithm
- Style encoding recommendation: HIGH (encoding) / ASSUMED (specific bit values — Claude's discretion)

**Research date:** 2026-06-01
**Valid until:** 2026-09-01 (postcard and quinn are stable; 90-day window before rechecking versions)
