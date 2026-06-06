#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // decode_datagram checks:
    //   (1) non-empty input (no tag byte → Err),
    //   (2) tag byte == 0x01 (TAG_STATE_DIFF; anything else → Err),
    //   (3) valid postcard body (decode StateDiff),
    //   (4) runs.len() <= MAX_RUNS (4096) — T-11-02 DoS guard.
    // Invariant: any input → Ok(StateDiff) or Err(ProtoError), never panic.
    let _ = nosh_proto::datagram::decode_datagram(data);
});
