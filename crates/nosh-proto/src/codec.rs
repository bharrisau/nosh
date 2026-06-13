//! The single, isolated message codec for nosh (decision D-03).
//!
//! Frames are length-delimited: a `u32` big-endian body length followed by the
//! postcard-serialized [`Message`] body. Keeping the wire format behind this one
//! module means the documented postcard -> protobuf (prost) migration (D-04) is
//! a one-file swap; cap'n proto is explicitly rejected (zero-copy is irrelevant
//! for small control frames).

use crate::messages::Message;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Maximum accepted frame body length (16 MiB). Guards against a malicious or
/// corrupt length prefix forcing an unbounded allocation.
pub const MAX_FRAME_LEN: usize = 16 * 1024 * 1024;

/// Errors produced by the codec.
#[derive(Debug, thiserror::Error)]
pub enum ProtoError {
    /// postcard failed to serialize or deserialize a message.
    #[error("postcard codec error: {0}")]
    Postcard(#[from] postcard::Error),
    /// The declared frame length exceeds [`MAX_FRAME_LEN`].
    #[error("frame too large: {0} bytes (max {MAX_FRAME_LEN})")]
    FrameTooLarge(usize),
    /// Underlying I/O error while reading or writing a framed message.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// `cap` argument to `encode_datagram` is below the minimum safe value
    /// ([`crate::datagram::MIN_CAP`]). The header-only (zero-run) payload is
    /// 7 bytes; any `cap <= 7` cannot satisfy the strict `payload.len() < cap`
    /// guarantee regardless of the diff content.
    #[error("datagram cap {0} is below minimum ({1})")]
    CapTooSmall(usize, usize),
}

/// Encode a [`Message`] into a length-delimited frame: 4-byte big-endian body
/// length prefix followed by the postcard body.
pub fn encode(msg: &Message) -> Result<Vec<u8>, ProtoError> {
    let body = postcard::to_allocvec(msg)?;
    if body.len() > MAX_FRAME_LEN {
        return Err(ProtoError::FrameTooLarge(body.len()));
    }
    let mut frame = Vec::with_capacity(4 + body.len());
    frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
    frame.extend_from_slice(&body);
    Ok(frame)
}

/// Decode a [`Message`] from a frame body (the postcard-serialized bytes,
/// without the length prefix).
pub fn decode(body: &[u8]) -> Result<Message, ProtoError> {
    Ok(postcard::from_bytes(body)?)
}

/// Write a length-delimited [`Message`] to an async writer.
pub async fn write_message<W: AsyncWrite + Unpin>(
    w: &mut W,
    msg: &Message,
) -> Result<(), ProtoError> {
    let frame = encode(msg)?;
    w.write_all(&frame).await?;
    w.flush().await?;
    Ok(())
}

/// Read a length-delimited [`Message`] from an async reader.
pub async fn read_message<R: AsyncRead + Unpin>(r: &mut R) -> Result<Message, ProtoError> {
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > MAX_FRAME_LEN {
        return Err(ProtoError::FrameTooLarge(len));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body).await?;
    decode(&body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_round_trip() {
        let msg = Message::SessionClose {
            exit_code: 42,
            reason: "bye".to_string(),
        };
        let frame = encode(&msg).expect("encode");
        // Strip the 4-byte length prefix before decoding the body.
        let body = &frame[4..];
        let decoded = decode(body).expect("decode");
        assert_eq!(msg, decoded);
    }

    #[test]
    fn length_prefix_is_big_endian_body_len() {
        let msg = Message::SessionClose {
            exit_code: 0,
            reason: String::new(),
        };
        let frame = encode(&msg).expect("encode");
        let declared = u32::from_be_bytes([frame[0], frame[1], frame[2], frame[3]]) as usize;
        assert_eq!(declared, frame.len() - 4);
    }

    #[tokio::test]
    async fn session_variants_round_trip() {
        let msgs = [
            Message::SessionOpen {
                term: "xterm-256color".to_string(),
                cols: 132,
                rows: 40,
                env: vec![
                    ("LC_ALL".to_string(), "C".to_string()),
                    ("TZ".to_string(), "UTC".to_string()),
                ],
            },
            Message::PtyData {
                data: vec![0x03, b'l', b's', b'\n'],
            },
            Message::Resize {
                cols: 100,
                rows: 50,
            },
        ];
        for msg in msgs {
            let mut buf: Vec<u8> = Vec::new();
            write_message(&mut buf, &msg).await.expect("write");
            let mut cursor = std::io::Cursor::new(buf);
            let got = read_message(&mut cursor).await.expect("read");
            assert_eq!(msg, got, "session variant must round-trip exactly");
        }

        // Explicitly assert env ordering is preserved (Vec, not a map).
        let open = Message::SessionOpen {
            term: "t".to_string(),
            cols: 1,
            rows: 1,
            env: vec![
                ("A".to_string(), "1".to_string()),
                ("B".to_string(), "2".to_string()),
            ],
        };
        let frame = encode(&open).expect("encode");
        if let Message::SessionOpen { env, .. } = decode(&frame[4..]).expect("decode") {
            assert_eq!(
                env,
                vec![
                    ("A".to_string(), "1".to_string()),
                    ("B".to_string(), "2".to_string())
                ]
            );
        } else {
            panic!("expected SessionOpen");
        }
    }

    #[tokio::test]
    async fn async_write_then_read_round_trip() {
        let msg = Message::SessionClose {
            exit_code: 7,
            reason: "shell exited".to_string(),
        };
        let mut buf: Vec<u8> = Vec::new();
        write_message(&mut buf, &msg).await.expect("write");
        let mut cursor = std::io::Cursor::new(buf);
        let got = read_message(&mut cursor).await.expect("read");
        assert_eq!(msg, got);
    }

    /// Phase 6 reattach variants: all five new variants must round-trip exactly
    /// through write_message / read_message, the no-oracle property must hold
    /// (ReattachErr encodes to a byte-identical frame every time), and appending
    /// the new variants must NOT shift the existing SessionClose discriminant.
    #[tokio::test]
    async fn reattach_variants_round_trip() {
        let token = [0xABu8; 16];

        // 1. SessionOpened
        let msg_opened = Message::SessionOpened { token };
        {
            let mut buf: Vec<u8> = Vec::new();
            write_message(&mut buf, &msg_opened).await.expect("write SessionOpened");
            let mut cursor = std::io::Cursor::new(buf);
            let got = read_message(&mut cursor).await.expect("read SessionOpened");
            assert_eq!(msg_opened, got, "SessionOpened must round-trip exactly");
        }

        // 2. Reattach — also verify the last_acked_seq convention is preserved.
        let msg_reattach = Message::Reattach {
            token,
            last_acked_seq: 12345,
        };
        {
            let mut buf: Vec<u8> = Vec::new();
            write_message(&mut buf, &msg_reattach).await.expect("write Reattach");
            let mut cursor = std::io::Cursor::new(buf);
            let got = read_message(&mut cursor).await.expect("read Reattach");
            assert_eq!(msg_reattach, got, "Reattach must round-trip exactly");
            // Verify the decoded last_acked_seq is what we encoded.
            if let Message::Reattach { last_acked_seq, .. } = got {
                assert_eq!(last_acked_seq, 12345, "last_acked_seq must survive codec");
            }
        }

        // 3. ReattachOk with truncated=true
        let msg_reattach_ok = Message::ReattachOk {
            new_token: token,
            replaying_from_seq: 42,
            truncated: true,
        };
        {
            let mut buf: Vec<u8> = Vec::new();
            write_message(&mut buf, &msg_reattach_ok).await.expect("write ReattachOk");
            let mut cursor = std::io::Cursor::new(buf);
            let got = read_message(&mut cursor).await.expect("read ReattachOk");
            assert_eq!(msg_reattach_ok, got, "ReattachOk must round-trip exactly");
        }

        // 4. ReattachErr — NO-ORACLE PROPERTY: encoding twice must produce
        //    byte-identical frames regardless of context. There is no reason
        //    field or discriminating data of any kind.
        {
            let mut buf1: Vec<u8> = Vec::new();
            write_message(&mut buf1, &Message::ReattachErr).await.expect("write ReattachErr #1");
            let mut buf2: Vec<u8> = Vec::new();
            write_message(&mut buf2, &Message::ReattachErr).await.expect("write ReattachErr #2");
            assert_eq!(buf1, buf2, "ReattachErr must encode byte-identically (no oracle)");
            // Also verify round-trip.
            let mut cursor = std::io::Cursor::new(buf1);
            let got = read_message(&mut cursor).await.expect("read ReattachErr");
            assert_eq!(Message::ReattachErr, got, "ReattachErr must round-trip exactly");
        }

        // 5. Ack
        let msg_ack = Message::Ack { seq: 99999 };
        {
            let mut buf: Vec<u8> = Vec::new();
            write_message(&mut buf, &msg_ack).await.expect("write Ack");
            let mut cursor = std::io::Cursor::new(buf);
            let got = read_message(&mut cursor).await.expect("read Ack");
            assert_eq!(msg_ack, got, "Ack must round-trip exactly");
        }

        // 6. DISCRIMINANT STABILITY: encode a SessionClose (existing variant,
        //    discriminant 3 in the original enum) and verify it still decodes as
        //    SessionClose after the five new variants were appended. Appending to
        //    the END must not shift existing discriminants.
        {
            let sc = Message::SessionClose {
                exit_code: 99,
                reason: "discriminant-stability-check".to_string(),
            };
            let mut buf: Vec<u8> = Vec::new();
            write_message(&mut buf, &sc).await.expect("write SessionClose");
            let mut cursor = std::io::Cursor::new(buf);
            let got = read_message(&mut cursor).await.expect("read SessionClose after extension");
            assert_eq!(sc, got, "SessionClose discriminant must not shift after appending new variants");
        }
    }

    /// MUX-06 / Phase 21: every `Message` variant must encode with its EXACT
    /// expected postcard discriminant byte. This test is the first commit of
    /// Phase 21 — any reordering of the enum is caught here before merge.
    ///
    /// postcard encodes enum variants as a leading varint equal to the variant's
    /// 0-based source-order index. For discriminants 0..127 this is a single byte.
    ///
    /// Count (0-based):
    ///   SessionOpen=0, PtyData=1, Resize=2, SessionClose=3,
    ///   SessionOpened=4, Reattach=5, ReattachOk=6, ReattachErr=7,
    ///   Ack=8, TerminalControl=9.
    ///   First Phase-21 mux variant = 10 (ChannelOpen).
    #[test]
    fn message_discriminant_order_is_stable() {
        use crate::messages::{ChannelType, TerminalControlPayload};
        use postcard::to_allocvec;

        let cases: &[(u8, Message)] = &[
            (0, Message::SessionOpen { term: "xterm".into(), cols: 80, rows: 24, env: vec![] }),
            (1, Message::PtyData { data: vec![0x41] }),
            (2, Message::Resize { cols: 80, rows: 24 }),
            (3, Message::SessionClose { exit_code: 0, reason: String::new() }),
            (4, Message::SessionOpened { token: [0u8; 16] }),
            (5, Message::Reattach { token: [0u8; 16], last_acked_seq: 0 }),
            (6, Message::ReattachOk { new_token: [0u8; 16], replaying_from_seq: 0, truncated: false }),
            (7, Message::ReattachErr),
            (8, Message::Ack { seq: 0 }),
            (9, Message::TerminalControl(TerminalControlPayload::Title { title: String::new() })),
            // Phase 21 mux variants — discriminants 10–14 (append-only after TerminalControl):
            (10, Message::ChannelOpen { channel_id: 2, channel_type: ChannelType::Echo }),
            (11, Message::ChannelAccept { channel_id: 2 }),
            (12, Message::ChannelReject { channel_id: 2 }),
            (13, Message::ChannelCredit { channel_id: 2, bytes: 256 * 1024 }),
            (14, Message::ChannelClose { channel_id: 2 }),
            // Phase 22: Scrollback Sync — discriminants 15–17 (append-only after ChannelClose):
            (15, Message::ScrollbackRequest { channel_id: 2, from_line: 0, count: 256 }),
            (16, Message::ScrollbackPage {
                channel_id: 2, from_line: 0, total_available: 0,
                epoch_at_snapshot: 0, lines: vec![] }),
            (17, Message::ScrollbackCredit { channel_id: 2, bytes: 0 }),
            // Phase 25: Inner SSH-key handshake — discriminants 18–21 (append-only after ScrollbackCredit):
            (18, Message::InnerAuthChallenge {
                server_nonce: [0u8; 32], server_spki: vec![], ekm: [0u8; 32] }),
            (19, Message::InnerAuthResponse {
                client_nonce: [0u8; 32], client_spki: vec![], client_sig: vec![0u8; 64] }),
            (20, Message::InnerAuthComplete { server_sig: vec![0u8; 64] }),
            (21, Message::InnerAuthFail),
        ];
        for (expected_disc, msg) in cases {
            let encoded = to_allocvec(msg).expect("encode");
            assert_eq!(
                encoded[0], *expected_disc,
                "Message::{} must encode with discriminant {}; encoded[0] = {}",
                msg.variant_name(), expected_disc, encoded[0]
            );
        }
    }

    /// Phase 27 / D-07: `TerminalControlPayload` variant order is append-only.
    ///
    /// postcard encodes enum variants by source-order position. Adding Hyperlink
    /// after Title preserves Clipboard and Title encodings. This test validates:
    /// 1. Clipboard and Title encode to the same discriminants as before.
    /// 2. Hyperlink encodes to the next discriminant (2).
    /// 3. Round-trip encoding/decoding works for all three variants.
    #[test]
    fn terminal_control_payload_order_is_append_only() {
        use crate::messages::TerminalControlPayload;
        use postcard::to_allocvec;

        // Clipboard should be discriminant 0
        let clipboard = TerminalControlPayload::Clipboard {
            selection: b"c".to_vec(),
            data: b"testdata".to_vec(),
        };
        let enc = to_allocvec(&clipboard).expect("encode Clipboard");
        assert_eq!(enc[0], 0, "Clipboard must encode to discriminant 0");

        // Title should be discriminant 1
        let title = TerminalControlPayload::Title {
            title: "test".to_string(),
        };
        let enc = to_allocvec(&title).expect("encode Title");
        assert_eq!(enc[0], 1, "Title must encode to discriminant 1");

        // Hyperlink should be discriminant 2 (new variant, after Title)
        let hyperlink = TerminalControlPayload::Hyperlink {
            uri: "https://example.com".to_string(),
        };
        let enc = to_allocvec(&hyperlink).expect("encode Hyperlink");
        assert_eq!(enc[0], 2, "Hyperlink must encode to discriminant 2");

        // Round-trip test: decode and verify equality
        let decoded_clipboard: TerminalControlPayload =
            postcard::from_bytes(&to_allocvec(&clipboard).unwrap()).unwrap();
        assert_eq!(clipboard, decoded_clipboard, "Clipboard round-trip must preserve data");

        let decoded_title: TerminalControlPayload =
            postcard::from_bytes(&to_allocvec(&title).unwrap()).unwrap();
        assert_eq!(title, decoded_title, "Title round-trip must preserve data");

        let decoded_hyperlink: TerminalControlPayload =
            postcard::from_bytes(&to_allocvec(&hyperlink).unwrap()).unwrap();
        assert_eq!(hyperlink, decoded_hyperlink, "Hyperlink round-trip must preserve data");
    }

    /// Phase 25 / D-04: `InnerAuthFail` must encode to exactly 1 byte (discriminant
    /// only — fieldless). Adding any field would create a key-existence or
    /// signature-validity oracle, violating the no-oracle invariant.
    ///
    /// This test guards the invariant mechanically: if someone adds a field to
    /// `InnerAuthFail`, the encoded length increases and this test fails before merge.
    #[test]
    fn inner_auth_fail_is_fieldless() {
        let fail = Message::InnerAuthFail;
        let encoded = postcard::to_allocvec(&fail).expect("encode InnerAuthFail");
        assert_eq!(
            encoded.len(),
            1,
            "InnerAuthFail must encode as exactly 1 byte (discriminant only); \
             a field would create a key-existence or signature-validity oracle (D-04)"
        );
    }

    /// WR-P-02 / Phase 22: every `ChannelType` variant must encode with its EXACT
    /// expected postcard discriminant byte. Pins the on-wire discriminant ordering
    /// so a future enum reordering is caught at test time before any deployment.
    ///
    /// postcard encodes enum variants as a leading varint equal to the variant's
    /// 0-based source-order index:
    ///   Echo=0, Scrollback=1, PortForward=2, AgentForward=3.
    #[test]
    fn channel_type_discriminant_order_is_stable() {
        use crate::messages::ChannelType;
        use postcard::to_allocvec;

        let cases: &[(u8, ChannelType)] = &[
            (0, ChannelType::Echo),
            (1, ChannelType::Scrollback),
            (2, ChannelType::PortForward),
            (3, ChannelType::AgentForward),
        ];
        for (expected_disc, ct) in cases {
            let encoded = to_allocvec(ct).expect("encode ChannelType");
            assert_eq!(
                encoded[0], *expected_disc,
                "ChannelType::{ct:?} must encode with discriminant {expected_disc}; \
                 encoded[0] = {}",
                encoded[0]
            );
        }
    }

    /// Phase 21 / MUX-06 + Phase 22: all mux `Message` variants must round-trip
    /// exactly through `write_message` → `read_message` (equality preserved).
    #[tokio::test]
    async fn mux_variants_round_trip() {
        use crate::messages::{ChannelType, ScrollbackLine};

        let msgs = [
            Message::ChannelOpen { channel_id: 2, channel_type: ChannelType::Echo },
            Message::ChannelOpen { channel_id: 4, channel_type: ChannelType::Scrollback },
            Message::ChannelOpen { channel_id: 6, channel_type: ChannelType::PortForward },
            Message::ChannelOpen { channel_id: 8, channel_type: ChannelType::AgentForward },
            Message::ChannelAccept { channel_id: 2 },
            Message::ChannelReject { channel_id: 4 },
            Message::ChannelCredit { channel_id: 2, bytes: 256 * 1024 },
            Message::ChannelCredit { channel_id: 2, bytes: 0 },
            Message::ChannelClose { channel_id: 2 },
            // Phase 22 scrollback variants — discriminants 15–17:
            Message::ScrollbackRequest { channel_id: 2, from_line: 0, count: 256 },
            Message::ScrollbackRequest { channel_id: 4, from_line: 100, count: 50 },
            Message::ScrollbackPage {
                channel_id: 2, from_line: 0, total_available: 0,
                epoch_at_snapshot: 0, lines: vec![] },
            Message::ScrollbackPage {
                channel_id: 2, from_line: 5, total_available: 42,
                epoch_at_snapshot: 99,
                lines: vec![ScrollbackLine { width: 80, cells: vec![] }] },
            Message::ScrollbackCredit { channel_id: 2, bytes: 0 },
            Message::ScrollbackCredit { channel_id: 2, bytes: 256 * 1024 },
        ];
        for msg in msgs {
            let mut buf: Vec<u8> = Vec::new();
            write_message(&mut buf, &msg).await.expect("write");
            let mut cursor = std::io::Cursor::new(buf);
            let got = read_message(&mut cursor).await.expect("read");
            assert_eq!(msg, got, "mux variant must round-trip exactly");
        }

        // ChannelReject carries ONLY channel_id — no reason field.
        // Verify there is no extra data encoded beyond discriminant + channel_id varint.
        let reject = Message::ChannelReject { channel_id: 0 };
        let encoded = postcard::to_allocvec(&reject).expect("encode ChannelReject");
        // discriminant byte (1) + channel_id varint for 0 (1 byte) = 2 bytes total.
        assert_eq!(
            encoded.len(), 2,
            "ChannelReject must encode as exactly 2 bytes (discriminant + zero channel_id); \
             a reason field would increase this"
        );
    }
}
