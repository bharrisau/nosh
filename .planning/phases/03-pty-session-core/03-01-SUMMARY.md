# Plan 03-01 Summary: Message protocol extension

**Status:** Complete

## What was built
- Extended `nosh-proto` `Message` enum with `SessionOpen { term, cols, rows, env: Vec<(String,String)> }`,
  `PtyData { data: Vec<u8> }`, and `Resize { cols, rows }`. `SessionClose { exit_code, reason }` kept.
- `env` is an ordered `Vec<(String,String)>` (not a map) for deterministic postcard encoding and
  stable test assertions.
- No codec wire-format change — postcard handles the new variants; added a `session_variants_round_trip`
  test asserting each variant survives the length-delimited codec and that `SessionOpen.env` ordering
  is preserved.

## Files
- `crates/nosh-proto/src/messages.rs`
- `crates/nosh-proto/src/codec.rs`

## Verification
- `cargo test -p nosh-proto` → 4 passed.

## Requirements
Foundation for SESS-01/04/05/07/08 (the session frames).
</content>
