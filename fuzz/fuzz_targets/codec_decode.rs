#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // codec::decode takes the postcard body (no length prefix).
    // Invariant: arbitrary input → Ok or Err(ProtoError), NEVER panic.
    let _ = nosh_proto::codec::decode(data);
});
