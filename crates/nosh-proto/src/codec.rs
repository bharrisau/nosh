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
            Message::Resize { cols: 100, rows: 50 },
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
}
