---
phase: 07-connection-migration-validation
reviewed: 2026-05-30T00:00:00Z
depth: standard
files_reviewed: 7
files_reviewed_list:
  - crates/nosh-server/src/server.rs
  - crates/nosh-proto/src/transport.rs
  - crates/nosh-client/src/client.rs
  - crates/nosh-client/Cargo.toml
  - crates/nosh-client/tests/common/mod.rs
  - crates/nosh-client/tests/migration.rs
  - docs/migration-live-check.md
findings:
  critical: 1
  warning: 3
  info: 1
  total: 5
status: issues_found
---

# Phase 7: Code Review Report

**Reviewed:** 2026-05-30
**Depth:** standard
**Files Reviewed:** 7
**Status:** issues_found

## Summary

This phase adds `server_config.migration(true)` (explicit, correct), a
`make_endpoint_with_transport` helper for qlog injection, and the
`migration_survives_path_change` integration test. The transport config and
server changes are sound. The primary concerns are in the migration test itself:
one dropped-data bug that can silently corrupt the sequence check (BLOCKER), one
dead `break` that defeats the DONE-detection intent, a stall measurement
race that understates the real stall, and an unused production dependency.

---

## Critical Issues

### CR-01: Silently dropped PtyData frame when first frame is not `SessionOpened`

**File:** `crates/nosh-client/tests/migration.rs:103-111`

**Issue:** The test reads the first frame and pattern-matches
`Message::SessionOpened { .. }` to discard it. When the first frame is NOT
`SessionOpened` (the code's own comment acknowledges this can happen: "PtyData
arrived before SessionOpened"), the frame is consumed from the stream and then
discarded — only an `eprintln!` fires. If that PtyData chunk contains
`LINE:0`, it is permanently lost. The sequence validation at line 268
(`assert_eq!(n, 0, "sequence must start at LINE:0")`) will then fail spuriously,
OR if the discard path is taken silently and `LINE:0` is the only line in the
chunk, the entire test's "no-gap" guarantee is defeated without any signal.

The comment states "we cannot un-read it, but the parser will skip non-LINE
lines" — this is incorrect: the parser cannot skip data it never receives. The
data was consumed from `recv` and then thrown away.

**Fix:** Store the frame and re-process it rather than discarding it. One
approach: collect the frame into a local variable and feed it into the same
parse path as the main loop:

```rust
let mut pending_first_frame: Option<Message> = None;
{
    let first = tokio::time::timeout(
        Duration::from_secs(10),
        nosh_proto::read_message(&mut recv),
    )
    .await
    .expect("no hang waiting for first frame")
    .expect("read first frame");
    if let Message::SessionOpened { .. } = first {
        // Discarded as expected.
    } else {
        // Re-inject into the main loop so its data is not lost.
        pending_first_frame = Some(first);
    }
}

// Then at the top of the main loop, drain pending_first_frame first:
loop {
    let frame = if let Some(f) = pending_first_frame.take() {
        f
    } else {
        tokio::time::timeout(
            Duration::from_secs(10),
            nosh_proto::read_message(&mut recv),
        )
        .await
        .expect("no hang waiting for frame")
        .expect("read frame (no ConnectionError)")
    };
    // ... rest of loop
}
```

---

## Warnings

### WR-01: `DONE` detection is a no-op — the comment says "break out" but no `break` exists

**File:** `crates/nosh-client/tests/migration.rs:193-195`

**Issue:** The `else if trimmed == "DONE"` branch contains only a comment
("Shell finished the sequence. Break out.") and no code. The inner `for` loop
over `text.lines()` continues; the outer frame loop continues. Termination
relies entirely on `sequence.len() >= 80` or `SessionClose`. This is
functionally tolerable under the happy path, but it means:

1. If the shell emits `DONE` because it crashed early (e.g., command not found,
   `$i` arithmetic error) before producing 80 lines, the test loops for up to 30
   seconds hitting the outer timeout and then panics with "migration test timed
   out" rather than cleanly detecting the shell failure.
2. The misleading comment actively misdirects future readers into believing an
   exit condition is implemented when it is not.

**Fix:** Add a `break` to the inner loop and set a flag to break the outer loop,
or restructure to use a labeled break:

```rust
} else if trimmed == "DONE" {
    // Shell finished the sequence; signal outer loop to exit.
    done_received = true;
    break; // break inner for-loop
}
// ...
if done_received {
    break; // break outer frame loop
}
```

### WR-02: Stall measurement understates actual anti-amplification stall when multiple lines arrive in one chunk

**File:** `crates/nosh-client/tests/migration.rs:160-200`

**Issue:** `t_first_post` is set by comparing `rebind_done` WITHIN the
`for line in text.lines()` inner loop. When the rebind fires on `LINE:10` (line
171), `rebind_done` is set to `true` (line 190). If the SAME `PtyData` chunk
also contains `LINE:11` (a common occurrence given PTY batching), the next
iteration of the inner loop hits `rebind_done && t_first_post.is_none()` (line
164) and sets `t_first_post = Instant::now()`. But this data was already
buffered and received BEFORE the rebind happened — the "stall" measured is
effectively 0 (or a few nanoseconds) regardless of the actual network stall. The
metric logged by `D-04` is then misleading and the `ratio > 3.0` soft-warning
never fires even on a genuinely stalled migration.

This does not make the test pass trivially (the `path_challenge` FrameStats
assertion is the binding proof), but the D-04 stall metric — which the code
treats as informative — is unreliable.

**Fix:** Track `t_first_post` only from data in frames received AFTER the rebind
call, not from data within the same PtyData chunk. One approach: capture the
entire `PtyData` chunk before the inner for-loop, and set `t_first_post` based
on the chunk's frame arrival time relative to `t_rebind`, using `t_rebind` as
the lower bound:

```rust
// After rebind_done is set, only count t_first_post from the NEXT frame
// (not the current one that triggered the rebind).
// Move t_first_post capture to the outer PtyData arm, not the inner loop:
if rebind_done && t_first_post.is_none() {
    // Only set on frames that arrived AFTER the rebind completed.
    // Skip this frame if it's the same one that triggered rebind.
    if !just_rebound {
        t_first_post = Some(Instant::now());
    }
}
```

where `just_rebound` is a flag cleared at the end of each outer loop iteration.

### WR-03: `rcgen` is an unused production dependency in `nosh-client`

**File:** `crates/nosh-client/Cargo.toml:20`

**Issue:** `rcgen = { workspace = true }` appears in the `[dependencies]`
section (production, not `[dev-dependencies]`). No source file under
`crates/nosh-client/src/` imports or uses `rcgen`. This adds a non-trivial crate
to the production binary's dependency tree (rcgen pulls in `ring`, `pem`, and
`x509-cert` transitively) without benefit.

**Fix:** Remove `rcgen` from `[dependencies]` in `crates/nosh-client/Cargo.toml`.
If cert generation is needed for tests, move it to `[dev-dependencies]` (it is
already available transitively via `nosh-server` which is a dev-dep, so it may
not even need an explicit entry).

---

## Info

### IN-01: `pre_stats` initialised before session open is a dead write

**File:** `crates/nosh-client/tests/migration.rs:126`

**Issue:** `let mut pre_stats = conn.stats()` at line 126 is assigned before the
session is opened and before any shell output arrives. It is unconditionally
overwritten at line 172 (inside the rebind trigger block). The first assignment
at line 126 is never read. This is harmless dead code that adds visual noise.

**Fix:** Declare `pre_stats` where it is first meaningfully assigned, inside the
rebind block, or use a placeholder that makes clear it must be overwritten:

```rust
// Inside the rebind block at line 171:
let pre_stats = conn.stats();
// (remove the `let mut pre_stats = conn.stats()` at line 126)
```

---

_Reviewed: 2026-05-30_
_Reviewer: Claude (gsd-code-reviewer)_
_Depth: standard_
