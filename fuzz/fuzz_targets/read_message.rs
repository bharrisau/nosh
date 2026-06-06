#![no_main]
use libfuzzer_sys::fuzz_target;
use std::sync::OnceLock;
use tokio::runtime::Runtime;

fuzz_target!(|data: &[u8]| {
    // OnceLock ensures we construct the runtime ONCE per fuzz process, not per
    // iteration. Per-iteration Runtime::new() exhausts file descriptors (Pitfall 4).
    static RT: OnceLock<Runtime> = OnceLock::new();
    let rt = RT.get_or_init(|| {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
    });
    rt.block_on(async {
        // std::io::Cursor<&[u8]> implements AsyncRead through tokio's compat layer.
        let mut cursor = tokio::io::BufReader::new(std::io::Cursor::new(data));
        // Invariant: MAX_FRAME_LEN guard fires BEFORE vec![0u8; len] allocation.
        // Arbitrary input must return Ok(Message) or Err(ProtoError::FrameTooLarge /
        // ProtoError::Io / ProtoError::Postcard), never OOM or panic.
        let _ = nosh_proto::codec::read_message(&mut cursor).await;
    });
});
