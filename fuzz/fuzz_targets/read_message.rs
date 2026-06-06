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
        // Invariant: allocation is bounded by MAX_FRAME_LEN (16 MiB). The guard is
        // `len > MAX_FRAME_LEN` (strict), so a declared length of exactly 16 MiB is
        // accepted and pre-allocates 16 MiB before read_exact fails on short input —
        // bounded at the intended cap, not unbounded. Arbitrary input must return
        // Ok(Message) or Err(FrameTooLarge / Io / Postcard), never OOM or panic.
        let _ = nosh_proto::codec::read_message(&mut cursor).await;
    });
});
