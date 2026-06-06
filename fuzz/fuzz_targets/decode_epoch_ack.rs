#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // decode_epoch_ack checks:
    //   (1) non-empty input (no tag byte → Err),
    //   (2) tag byte == 0x02 (TAG_CLIENT_EPOCH; 0x01 TAG_STATE_DIFF and all others → Err),
    //   (3) valid postcard u64 varint body.
    // Security invariant (T-13-01): a StateDiff payload (tag 0x01) is explicitly
    // rejected — it cannot be misread as an epoch-ack.
    // Invariant: any input → Ok(u64) or Err(ProtoError), never panic.
    let _ = nosh_proto::datagram::decode_epoch_ack(data);
});
