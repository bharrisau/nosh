#![no_main]
use libfuzzer_sys::fuzz_target;
use nosh_server::terminal::{TerminalState, OSC_52_MAX_BYTES, MAX_TITLE_BYTES, OSC_ACCUMULATION_MAX};

// CORRECT INVOCATION (SEC-05 / 27-01):
// cargo +nightly fuzz run osc_accumulation -- -max_len=2097152 -max_total_time=120
//
// NOTE: The LIBFUZZER_MAX_LEN environment variable is SILENTLY IGNORED by cargo-fuzz.
// You must pass -max_len=2097152 AFTER the -- separator (directly to libFuzzer).
//
// This bound exercises OSC_ACCUMULATION_MAX (1 MiB) from 999.7-SECURITY.md:
// the deterministic in-harness 10 MiB multi-chunk test verifies the prefilter
// truncates at 1 MiB and resyncs, while libFuzzer mutation explores adjacent paths.

fuzz_target!(|data: &[u8]| {
    // ── Original single-chunk test (unchanged) ────────────────────────────────
    //
    // Drives the existing OSC 52 / title storage-cap assertions with arbitrary
    // libFuzzer-generated data (max_len default 4096 — bounded by libFuzzer's
    // max_len setting, insufficient to reach OSC_ACCUMULATION_MAX alone).
    let mut state = TerminalState::new(80, 24);
    // Invariant: arbitrary VT bytes → OSC caps hold, no panic, no unbounded alloc.
    state.advance(data);

    // Assert OSC 52 cap: stored payload must never exceed OSC_52_MAX_BYTES (65_536).
    // A cap-assert firing IS the fuzzer signal — it means the truncation guard failed.
    if let Some((_sel, payload)) = state.osc52_pending() {
        assert!(
            payload.len() <= OSC_52_MAX_BYTES,
            "OSC 52 payload exceeded cap: {} bytes (max {})",
            payload.len(),
            OSC_52_MAX_BYTES
        );
    }

    // Assert title cap: stored title must never exceed MAX_TITLE_BYTES (1_024).
    // Titles over the cap are discarded (not truncated), so None is the expected result.
    if let Some(title) = state.title() {
        assert!(
            title.len() <= MAX_TITLE_BYTES,
            "title exceeded cap: {} bytes (max {})",
            title.len(),
            MAX_TITLE_BYTES
        );
    }

    // ── SEC-03 multi-chunk accumulation regression (D-19-01 / T-19-06 / T-19-07) ──
    //
    // libFuzzer's default max_len (4096) is too small to reach OSC_ACCUMULATION_MAX
    // via the `data` input alone. We construct the oversized OSC deterministically
    // and drive it in chunks to exercise the pre-bound across many advance() calls.
    //
    // OSC 2 title: ESC ] 2 ; <10 MiB of 'A'> BEL
    // After the fix: accumulation is capped at OSC_ACCUMULATION_MAX (1 MiB), the
    // oversized sequence is discarded, and the parser resyncs to ground state so
    // subsequent OSC sequences still parse correctly.
    let mut state2 = TerminalState::new(80, 24);

    const CHUNK: usize = 4096;
    const TOTAL: usize = OSC_ACCUMULATION_MAX * 10; // 10 MiB

    state2.advance(b"\x1b]2;");
    let chunk_data = vec![b'A'; CHUNK];
    let chunks = TOTAL / CHUNK;
    for _ in 0..chunks {
        state2.advance(&chunk_data); // must not OOM or panic
    }
    state2.advance(b"\x07"); // BEL terminator

    // After truncation + resync, title must be bounded.
    if let Some(title) = state2.title() {
        assert!(
            title.len() <= MAX_TITLE_BYTES,
            "title after multi-chunk 10 MiB OSC must be bounded: {} bytes (max {})",
            title.len(),
            MAX_TITLE_BYTES
        );
    }

    // D-19-02 / T-19-07: after overflow resync, a normal OSC 2 title must parse.
    state2.advance(b"\x1b]2;OK\x07");
    if let Some(title) = state2.title() {
        assert_eq!(title, "OK", "normal OSC title must work after multi-chunk overflow resync");
    }

    // D-19-03: after resync, OSC 52 must still dispatch and respect its storage cap.
    state2.advance(b"\x1b]52;c;SGVsbG8=\x07");
    if let Some((_sel, payload)) = state2.osc52_pending() {
        assert!(
            payload.len() <= OSC_52_MAX_BYTES,
            "OSC 52 cap must hold after multi-chunk overflow resync"
        );
    }
});
