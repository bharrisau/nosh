//! Tests for the transport abstraction traits.
//!
//! Task 1 (RED phase): object-safety of the three traits and the sync/async
//! split on NoshTransport.
//!
//! Task 2 (RED phase): write_message_ns / read_message_ns codec helpers — wire
//! format is identical to codec::write_message / read_message; MAX_FRAME_LEN
//! guard is preserved.

#[cfg(test)]
mod tests {
    use crate::transport_trait::{
        NoshRecvStream, NoshSendStream, NoshTransport, SendDatagramError,
    };
    use bytes::Bytes;
    use std::net::SocketAddr;

    // -------------------------------------------------------------------------
    // Mock impls shared by all tests
    // -------------------------------------------------------------------------

    struct MockTransport;

    #[async_trait::async_trait]
    impl NoshTransport for MockTransport {
        fn send_datagram(&self, _data: Bytes) -> Result<(), SendDatagramError> {
            Err(SendDatagramError::Disabled)
        }
        fn datagram_send_buffer_space(&self) -> usize {
            0
        }
        fn max_datagram_size(&self) -> Option<usize> {
            None
        }
        async fn read_datagram(&self) -> anyhow::Result<Bytes> {
            Ok(Bytes::new())
        }
        async fn accept_bi(
            &self,
        ) -> anyhow::Result<(Box<dyn NoshSendStream>, Box<dyn NoshRecvStream>)> {
            anyhow::bail!("mock: no streams")
        }
        async fn open_bi(
            &self,
        ) -> anyhow::Result<(Box<dyn NoshSendStream>, Box<dyn NoshRecvStream>)> {
            anyhow::bail!("mock: no streams")
        }
        fn remote_address(&self) -> SocketAddr {
            "127.0.0.1:0".parse().unwrap()
        }
        fn close(&self, _code: u32, _reason: &[u8]) {}
    }

    struct MockSendStream;

    #[async_trait::async_trait]
    impl NoshSendStream for MockSendStream {
        async fn write_all(&mut self, _data: &[u8]) -> anyhow::Result<()> {
            Ok(())
        }
        async fn flush(&mut self) -> anyhow::Result<()> {
            Ok(())
        }
        async fn finish(&mut self) -> anyhow::Result<()> {
            Ok(())
        }
        async fn stopped(&mut self) -> anyhow::Result<()> {
            Ok(())
        }
        fn reset(&mut self, _code: u32) {}
    }

    struct MockRecvStream;

    #[async_trait::async_trait]
    impl NoshRecvStream for MockRecvStream {
        async fn read_exact(&mut self, _buf: &mut [u8]) -> anyhow::Result<()> {
            Ok(())
        }
        async fn read(&mut self, _buf: &mut [u8]) -> anyhow::Result<Option<usize>> {
            Ok(None)
        }
        fn stop(&mut self, _code: u32) {}
    }

    // -------------------------------------------------------------------------
    // Task 1: object-safety and sync/async split tests
    // -------------------------------------------------------------------------

    /// Object-safety: Box<dyn NoshTransport> must compile and dispatch.
    #[test]
    fn nosh_transport_is_object_safe() {
        let t: Box<dyn NoshTransport> = Box::new(MockTransport);
        // max_datagram_size must return Option<usize>, not usize
        assert!(t.max_datagram_size().is_none());
        assert_eq!(t.datagram_send_buffer_space(), 0);
        // send_datagram is synchronous (no .await)
        let result = t.send_datagram(Bytes::new());
        assert!(result.is_err());
    }

    /// Object-safety: Box<dyn NoshSendStream> must compile.
    #[test]
    fn nosh_send_stream_is_object_safe() {
        let _s: Box<dyn NoshSendStream> = Box::new(MockSendStream);
    }

    /// Object-safety: Box<dyn NoshRecvStream> must compile.
    #[test]
    fn nosh_recv_stream_is_object_safe() {
        let _r: Box<dyn NoshRecvStream> = Box::new(MockRecvStream);
    }

    /// Verify send_datagram error variants.
    #[test]
    fn send_datagram_error_variants() {
        let e1 = SendDatagramError::TooLarge;
        let e2 = SendDatagramError::UnsupportedByPeer;
        let e3 = SendDatagramError::Disabled;
        let e4 = SendDatagramError::ConnectionLost("test".to_string());
        // All variants must format without panic
        let _ = format!("{e1}");
        let _ = format!("{e2}");
        let _ = format!("{e3}");
        let _ = format!("{e4}");
    }

    /// Verify max_datagram_size returns Option<usize> (not usize).
    #[test]
    fn max_datagram_size_is_option() {
        let t: Box<dyn NoshTransport> = Box::new(MockTransport);
        // If this compiles and the pattern match works, return type is Option<usize>
        match t.max_datagram_size() {
            None => {}
            Some(n) => assert!(n > 0),
        }
    }

    // -------------------------------------------------------------------------
    // Task 2: write_message_ns / read_message_ns codec helper tests
    //
    // These test the wire format identity between the _ns helpers and the
    // generic codec::write_message / read_message functions.
    // -------------------------------------------------------------------------

    /// In-memory send stream backed by a Vec<u8>.
    struct VecSendStream(Vec<u8>);

    #[async_trait::async_trait]
    impl NoshSendStream for VecSendStream {
        async fn write_all(&mut self, data: &[u8]) -> anyhow::Result<()> {
            self.0.extend_from_slice(data);
            Ok(())
        }
        async fn flush(&mut self) -> anyhow::Result<()> {
            Ok(())
        }
        async fn finish(&mut self) -> anyhow::Result<()> {
            Ok(())
        }
        async fn stopped(&mut self) -> anyhow::Result<()> {
            Ok(())
        }
        fn reset(&mut self, _code: u32) {}
    }

    /// In-memory recv stream backed by an owned Vec<u8> and a position cursor.
    struct OwnedSliceRecvStream {
        data: Vec<u8>,
        pos: usize,
    }

    impl OwnedSliceRecvStream {
        fn new(data: Vec<u8>) -> Self {
            Self { data, pos: 0 }
        }
    }

    #[async_trait::async_trait]
    impl NoshRecvStream for OwnedSliceRecvStream {
        async fn read_exact(&mut self, buf: &mut [u8]) -> anyhow::Result<()> {
            let end = self.pos + buf.len();
            if end > self.data.len() {
                anyhow::bail!("unexpected EOF in test stream");
            }
            buf.copy_from_slice(&self.data[self.pos..end]);
            self.pos = end;
            Ok(())
        }
        async fn read(&mut self, buf: &mut [u8]) -> anyhow::Result<Option<usize>> {
            if self.pos >= self.data.len() {
                return Ok(None);
            }
            let available = self.data.len() - self.pos;
            let n = available.min(buf.len());
            buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
            self.pos += n;
            Ok(Some(n))
        }
        fn stop(&mut self, _code: u32) {}
    }

    /// write_message_ns produces the same bytes as codec::encode (4-byte BE length
    /// prefix + postcard body).
    #[tokio::test]
    async fn write_message_ns_wire_format_matches_encode() {
        use crate::messages::Message;
        use crate::transport_trait::write_message_ns;

        let msg = Message::SessionClose {
            exit_code: 42,
            reason: "wire-format-test".to_string(),
        };

        // Write via _ns helper
        let mut ns_stream = VecSendStream(Vec::new());
        write_message_ns(&mut ns_stream, &msg).await.expect("write_message_ns");

        // Write via generic codec::encode (the reference)
        let reference_frame = crate::codec::encode(&msg).expect("encode");

        assert_eq!(
            ns_stream.0, reference_frame,
            "write_message_ns must produce the same bytes as codec::encode"
        );
    }

    /// A frame written by write_message_ns can be decoded by read_message_ns.
    #[tokio::test]
    async fn write_then_read_ns_round_trips() {
        use crate::messages::Message;
        use crate::transport_trait::{read_message_ns, write_message_ns};

        let msg = Message::Ack { seq: 99_999 };

        let mut send_stream = VecSendStream(Vec::new());
        write_message_ns(&mut send_stream, &msg).await.expect("write_message_ns");

        let mut recv_stream = OwnedSliceRecvStream::new(send_stream.0);
        let decoded = read_message_ns(&mut recv_stream)
            .await
            .expect("read_message_ns");

        assert_eq!(msg, decoded, "round-trip via _ns helpers must yield identical message");
    }

    /// A frame written by codec::write_message (generic) can be decoded by
    /// read_message_ns — same wire format.
    #[tokio::test]
    async fn write_generic_read_ns_cross_compatibility() {
        use crate::messages::Message;
        use crate::transport_trait::read_message_ns;

        let msg = Message::Reattach {
            token: [0xABu8; 16],
            last_acked_seq: 12345,
        };

        // Write via the generic codec helper into a Vec<u8> (which impls AsyncWrite)
        let mut buf: Vec<u8> = Vec::new();
        crate::codec::write_message(&mut buf, &msg)
            .await
            .expect("codec::write_message");

        // Read via the _ns helper
        let mut recv_stream = OwnedSliceRecvStream::new(buf);
        let decoded = read_message_ns(&mut recv_stream)
            .await
            .expect("read_message_ns");

        assert_eq!(
            msg, decoded,
            "read_message_ns must decode a frame written by codec::write_message"
        );
    }

    /// read_message_ns enforces MAX_FRAME_LEN: a declared length exceeding the
    /// limit returns ProtoError::FrameTooLarge (T-23-01 DoS mitigation).
    #[tokio::test]
    async fn read_message_ns_enforces_max_frame_len() {
        use crate::codec::{MAX_FRAME_LEN, ProtoError};
        use crate::transport_trait::read_message_ns;

        // Craft a frame with a length prefix 1 byte over the limit.
        let oversized_len = MAX_FRAME_LEN + 1;
        let len_bytes = (oversized_len as u32).to_be_bytes();
        // Provide only the 4-byte prefix (no body — the guard fires before reading body).
        let mut recv_stream = OwnedSliceRecvStream::new(len_bytes.to_vec());

        let result = read_message_ns(&mut recv_stream).await;
        match result {
            Err(ProtoError::FrameTooLarge(n)) => {
                assert_eq!(n, oversized_len, "FrameTooLarge must report the declared length");
            }
            other => panic!("expected ProtoError::FrameTooLarge, got {other:?}"),
        }
    }

    /// Phase 28 framing-desync diagnostic: reproduce the EXACT field symptom — a
    /// reliable stream that has desynced so the next 4 payload bytes are read as a
    /// length prefix. The operator's Windows log reported
    /// `frame too large: 1718183741 bytes` == 0x6669673D == ASCII "fig=". This test
    /// pins that mapping: feeding the literal bytes `fig=` as a length prefix must
    /// produce `FrameTooLarge(1_718_183_741)` (and, in a real process with a
    /// subscriber, the always-on `reliable-stream framing DESYNC` error log). It
    /// documents the byte→length identity so the diagnostic stays meaningful.
    #[tokio::test]
    async fn read_message_ns_desync_reports_field_fig_value() {
        use crate::codec::ProtoError;
        use crate::transport_trait::read_message_ns;

        // "fig=" read as a big-endian u32 length prefix.
        let fig = *b"fig=";
        assert_eq!(u32::from_be_bytes(fig), 1_718_183_741, "field-report identity");

        let mut recv_stream = OwnedSliceRecvStream::new(fig.to_vec());
        match read_message_ns(&mut recv_stream).await {
            Err(ProtoError::FrameTooLarge(n)) => {
                assert_eq!(
                    n, 1_718_183_741,
                    "the 'fig=' desync must surface as FrameTooLarge with the field-reported length"
                );
            }
            other => panic!("expected ProtoError::FrameTooLarge(1718183741), got {other:?}"),
        }
    }
}
