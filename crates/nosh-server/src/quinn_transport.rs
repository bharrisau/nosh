//! Pure pass-through Quinn wrappers over the `nosh-proto` transport traits
//! (Phase 23 / D-04 / SC#3).
//!
//! `QuinnTransport`, `QuinnSendStream`, and `QuinnRecvStream` are thin newtype
//! wrappers. Each method is a direct delegation to the underlying `quinn` type
//! with no added logic, buffering, or retries — their only purpose is to satisfy
//! the `NoshTransport` / `NoshSendStream` / `NoshRecvStream` trait bounds so the
//! session pump can be transport-agnostic (Phase 24 plugs the WebTransport
//! wrapper into the same seam).
//!
//! # CRITICAL: `finish()` vs `.await`
//!
//! `NoshSendStream::finish` is `async fn` (required for object-safety via
//! `#[async_trait]`). The WRAPPER body here calls `quinn::SendStream::finish()`
//! which is **synchronous** in quinn 0.11 — there is nothing to await inside
//! this body. The async wrapper exists solely so trait-object callers can write
//! `.finish().await`. DO NOT add `.await` inside this wrapper.
//!
//! Trait-object CALLERS in `channel.rs` / `server.rs` MUST `.await` every
//! `.finish()` call — an un-awaited `Pin<Box<dyn Future>>` would silently skip
//! the QUIC half-close.
//!
//! # VarInt conversion
//!
//! The `NoshTransport::close`, `NoshSendStream::reset`, and
//! `NoshRecvStream::stop` methods take `u32` application codes. Quinn's
//! corresponding methods take `quinn::VarInt`. `VarInt: From<u32>` so all
//! conversions use `code.into()`.

use async_trait::async_trait;
use bytes::Bytes;
use std::net::SocketAddr;
use tokio::io::AsyncWriteExt as _; // needed: quinn SendStream flush/write_all
#[allow(unused_imports)]
use tokio::io::AsyncReadExt as _; // needed: quinn RecvStream read_exact (async_trait scope)
use nosh_proto::transport_trait::{
    NoshTransport, NoshSendStream, NoshRecvStream, SendDatagramError,
};

// ── QuinnTransport ─────────────────────────────────────────────────────────────

/// Newtype wrapper over `quinn::Connection` implementing `NoshTransport`.
///
/// Pure pass-through: every method delegates directly to `quinn::Connection`.
/// No logic, no state, no buffering beyond what quinn provides.
pub struct QuinnTransport(pub quinn::Connection);

#[async_trait]
impl NoshTransport for QuinnTransport {
    fn send_datagram(&self, data: Bytes) -> Result<(), SendDatagramError> {
        self.0.send_datagram(data).map_err(|e| match e {
            quinn::SendDatagramError::TooLarge => SendDatagramError::TooLarge,
            quinn::SendDatagramError::UnsupportedByPeer => SendDatagramError::UnsupportedByPeer,
            quinn::SendDatagramError::Disabled => SendDatagramError::Disabled,
            quinn::SendDatagramError::ConnectionLost(e) => {
                SendDatagramError::ConnectionLost(e.to_string())
            }
        })
    }

    fn datagram_send_buffer_space(&self) -> usize {
        self.0.datagram_send_buffer_space()
    }

    fn max_datagram_size(&self) -> Option<usize> {
        self.0.max_datagram_size()
    }

    async fn read_datagram(&self) -> anyhow::Result<Bytes> {
        Ok(self.0.read_datagram().await?)
    }

    async fn accept_bi(&self) -> anyhow::Result<(Box<dyn NoshSendStream>, Box<dyn NoshRecvStream>)> {
        let (s, r) = self.0.accept_bi().await?;
        Ok((Box::new(QuinnSendStream(s)), Box::new(QuinnRecvStream(r))))
    }

    async fn open_bi(&self) -> anyhow::Result<(Box<dyn NoshSendStream>, Box<dyn NoshRecvStream>)> {
        let (s, r) = self.0.open_bi().await?;
        Ok((Box::new(QuinnSendStream(s)), Box::new(QuinnRecvStream(r))))
    }

    fn remote_address(&self) -> SocketAddr {
        self.0.remote_address()
    }

    fn close(&self, code: u32, reason: &[u8]) {
        self.0.close(code.into(), reason) // quinn::VarInt: From<u32>
    }
}

// ── QuinnSendStream ────────────────────────────────────────────────────────────

/// Newtype wrapper over `quinn::SendStream` implementing `NoshSendStream`.
///
/// Pure pass-through. See module-level doc for the `finish()` / `.await` note.
pub struct QuinnSendStream(pub quinn::SendStream);

#[async_trait]
impl NoshSendStream for QuinnSendStream {
    async fn write_all(&mut self, data: &[u8]) -> anyhow::Result<()> {
        Ok(self.0.write_all(data).await?)
    }

    async fn flush(&mut self) -> anyhow::Result<()> {
        Ok(self.0.flush().await?)
    }

    async fn finish(&mut self) -> anyhow::Result<()> {
        // quinn 0.11: SendStream::finish() is SYNCHRONOUS (fn finish(&mut self)).
        // The trait's async fn exists only for object-safety. Do NOT .await here.
        // Trait-object callers in channel.rs / server.rs MUST .await this method.
        let _ = self.0.finish();
        Ok(())
    }

    async fn stopped(&mut self) -> anyhow::Result<()> {
        self.0.stopped().await?;
        Ok(())
    }

    fn reset(&mut self, code: u32) {
        let _ = self.0.reset(code.into()); // quinn::VarInt: From<u32>
    }
}

// ── QuinnRecvStream ────────────────────────────────────────────────────────────

/// Newtype wrapper over `quinn::RecvStream` implementing `NoshRecvStream`.
///
/// Pure pass-through.
pub struct QuinnRecvStream(pub quinn::RecvStream);

#[async_trait]
impl NoshRecvStream for QuinnRecvStream {
    async fn read_exact(&mut self, buf: &mut [u8]) -> anyhow::Result<()> {
        // AsyncReadExt::read_exact — imported at module level as `AsyncReadExt as _`
        Ok(self.0.read_exact(buf).await.map(|_| ())?)
    }

    async fn read(&mut self, buf: &mut [u8]) -> anyhow::Result<Option<usize>> {
        Ok(self.0.read(buf).await?)
    }

    fn stop(&mut self, code: u32) {
        let _ = self.0.stop(code.into()); // quinn::VarInt: From<u32>
    }
}
