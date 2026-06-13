//! Tests for the transport abstraction traits (Task 1 TDD RED phase).
//!
//! These tests verify object-safety of the three traits and the sync/async
//! split on NoshTransport.

#[cfg(test)]
mod tests {
    use crate::transport_trait::{
        NoshRecvStream, NoshSendStream, NoshTransport, SendDatagramError,
    };
    use bytes::Bytes;
    use std::net::SocketAddr;

    /// Verify NoshTransport is object-safe: Box<dyn NoshTransport> must compile.
    /// This is a compile-time test — if the trait is NOT object-safe, this module
    /// will fail to compile.
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
}
