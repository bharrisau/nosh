#![no_main]
use libfuzzer_sys::fuzz_target;
use nosh_server::terminal::{TerminalState, OSC_52_MAX_BYTES, MAX_TITLE_BYTES};

fuzz_target!(|data: &[u8]| {
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
});
