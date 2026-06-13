//! Transport abstraction seam for nosh (Phase 23 / D-01, D-02).
//!
//! This module defines the three object-safe traits that form the I/O boundary
//! between the session pump and the underlying transport (native QUIC or
//! WebTransport). The codec helpers `write_message_ns` / `read_message_ns`
//! parallel the existing `codec::write_message` / `codec::read_message` but
//! accept trait objects rather than generic `AsyncWrite` / `AsyncRead`.
//!
//! # Design decisions
//!
//! * `#[async_trait]` is used on all three traits because native AFIT (stable
//!   since Rust 1.75) desugars to `-> impl Future`, which is NOT object-safe.
//!   `async-trait` rewrites async methods to `Pin<Box<dyn Future + Send>>`
//!   which is object-safe by construction (D-02).
//! * Synchronous methods (`send_datagram`, `datagram_send_buffer_space`,
//!   `max_datagram_size`, `remote_address`, `close`, `reset`, `stop`) are kept
//!   as plain `fn` inside the `#[async_trait]` trait. The macro permits this.
//!   `send_burst` calls `send_datagram` / `datagram_send_buffer_space` in a
//!   tight non-async loop and must not be forced to `.await` (D-03, Pitfall 2).
//! * `max_datagram_size` returns `Option<usize>` — `None` means datagrams are
//!   not negotiated, matching quinn's API exactly (Pitfall 3).

use async_trait::async_trait;
use bytes::Bytes;
use std::net::SocketAddr;

/// Errors that can occur when sending a datagram.
///
/// Maps 1:1 to `quinn::SendDatagramError` for the Quinn wrapper.
/// WebTransport and future transports map their own error types here.
#[derive(Debug, thiserror::Error)]
pub enum SendDatagramError {
    /// The datagram is too large for the current path MTU.
    #[error("datagram too large for current path MTU")]
    TooLarge,
    /// The peer does not support datagrams (not negotiated in QUIC handshake).
    #[error("peer does not support datagrams")]
    UnsupportedByPeer,
    /// Datagrams are disabled on this connection (e.g. `max_udp_payload_size`
    /// set to 0 in transport config).
    #[error("datagrams disabled on this connection")]
    Disabled,
    /// The connection was lost before the datagram could be sent.
    #[error("connection lost: {0}")]
    ConnectionLost(String),
}

/// Connection-level transport abstraction.
///
/// Wraps a QUIC connection (or WebTransport session) with the minimal surface
/// needed by the session pump (`run_session`, `run_reattach_session`,
/// `send_burst`). All implementors must be `Send + Sync + 'static` so they
/// can be placed in a `Box<dyn NoshTransport>` and moved across task boundaries.
///
/// # Object-safety
///
/// This trait is object-safe. The `Box<dyn NoshTransport>` returned by
/// `accept_bi` / `open_bi` already forces the compiler to verify this at
/// definition time.
///
/// # Sync vs async split
///
/// * `send_datagram`, `datagram_send_buffer_space`, `max_datagram_size`,
///   `remote_address`, `close` — synchronous, matching quinn's API.
/// * `read_datagram`, `accept_bi`, `open_bi` — async (network I/O).
#[async_trait]
pub trait NoshTransport: Send + Sync + 'static {
    /// Send an unreliable datagram payload to the peer.
    ///
    /// Synchronous — places the datagram in the QUIC send buffer immediately.
    /// `send_burst` calls this in a tight loop without yielding.
    fn send_datagram(&self, data: Bytes) -> Result<(), SendDatagramError>;

    /// Available capacity in the datagram send buffer (bytes).
    ///
    /// Synchronous — queried in the `send_burst` while-loop guard.
    fn datagram_send_buffer_space(&self) -> usize;

    /// Maximum datagram payload size on the current path.
    ///
    /// Returns `None` when datagrams are not negotiated (e.g. WebTransport
    /// fallback path or QUIC connection without datagram extension).
    fn max_datagram_size(&self) -> Option<usize>;

    /// Receive the next incoming datagram from the peer.
    async fn read_datagram(&self) -> anyhow::Result<Bytes>;

    /// Accept the next inbound bidirectional stream from the peer.
    ///
    /// Returns a send/receive pair as boxed trait objects.
    async fn accept_bi(
        &self,
    ) -> anyhow::Result<(Box<dyn NoshSendStream>, Box<dyn NoshRecvStream>)>;

    /// Open a new outbound bidirectional stream to the peer.
    ///
    /// Returns a send/receive pair as boxed trait objects. The WebTransport
    /// wrapper hides the double-await (`conn.open_bi().await?.await`) behind
    /// this single-await interface.
    async fn open_bi(
        &self,
    ) -> anyhow::Result<(Box<dyn NoshSendStream>, Box<dyn NoshRecvStream>)>;

    /// Peer socket address (used for logging and migration detection).
    fn remote_address(&self) -> SocketAddr;

    /// Close the connection with an application error code and reason bytes.
    ///
    /// The `code` is a `u32`; Quinn wrappers convert to `quinn::VarInt` via
    /// `code.into()`.
    fn close(&self, code: u32, reason: &[u8]);
}

/// Stream-level send abstraction.
///
/// Wraps the write half of a QUIC bidirectional stream (or WebTransport stream).
/// All implementors must be `Send + 'static` for use in `Box<dyn NoshSendStream>`.
///
/// # Object-safety
///
/// This trait is object-safe via `#[async_trait]`.
///
/// # CRITICAL: `finish` must be awaited at every call site
///
/// `NoshSendStream::finish` is `async fn` (so the trait stays object-safe).
/// On a `Box<dyn NoshSendStream>` / `&mut dyn NoshSendStream`, writing
/// `let _ = ch_send.finish();` (no `.await`) builds a `Pin<Box<dyn Future>>`
/// and drops it unpolled — the QUIC half-close never runs. Every `finish()`
/// call site MUST use `.finish().await`.
#[async_trait]
pub trait NoshSendStream: Send + 'static {
    /// Write all bytes to the stream. Equivalent to `AsyncWriteExt::write_all`.
    async fn write_all(&mut self, data: &[u8]) -> anyhow::Result<()>;

    /// Flush pending write data to the peer.
    async fn flush(&mut self) -> anyhow::Result<()>;

    /// Mark the stream as finished (half-close the send side).
    ///
    /// For the Quinn wrapper this wraps `quinn::SendStream::finish()`, which is
    /// synchronous in quinn 0.11 — the wrapper's async fn body calls the sync
    /// method and returns `Ok(())` immediately. The trait's `async fn` wrapper
    /// exists solely to preserve object-safety.
    ///
    /// **Callers on trait objects MUST `.await` this.**
    async fn finish(&mut self) -> anyhow::Result<()>;

    /// Wait until the peer acknowledges the stream has stopped (half-close
    /// confirmation). Typically called after `finish()`.
    async fn stopped(&mut self) -> anyhow::Result<()>;

    /// Reset the stream with an application error code (abort send side).
    ///
    /// Synchronous — matching quinn's `reset()` API.
    fn reset(&mut self, code: u32);
}

/// Stream-level receive abstraction.
///
/// Wraps the read half of a QUIC bidirectional stream (or WebTransport stream).
/// All implementors must be `Send + 'static` for use in `Box<dyn NoshRecvStream>`.
///
/// # Object-safety
///
/// This trait is object-safe via `#[async_trait]`.
#[async_trait]
pub trait NoshRecvStream: Send + 'static {
    /// Read exactly `buf.len()` bytes from the stream, blocking until all bytes
    /// are available. Equivalent to `AsyncReadExt::read_exact`.
    async fn read_exact(&mut self, buf: &mut [u8]) -> anyhow::Result<()>;

    /// Read up to `buf.len()` bytes from the stream. Returns `Ok(Some(n))` for
    /// `n > 0` bytes read, `Ok(None)` on clean end-of-stream, or an error.
    async fn read(&mut self, buf: &mut [u8]) -> anyhow::Result<Option<usize>>;

    /// Stop the stream with an application error code (abort receive side).
    ///
    /// Synchronous — matching quinn's `stop()` API.
    fn stop(&mut self, code: u32);
}

/// Write a [`crate::messages::Message`] via a `NoshSendStream` trait object.
///
/// Parallel to [`crate::codec::write_message`] but works with
/// `&mut dyn NoshSendStream` instead of `AsyncWrite + Unpin`. The wire format
/// is identical: 4-byte big-endian body length prefix followed by the postcard
/// body.
///
/// # Errors
///
/// Returns `crate::codec::ProtoError::Io` if the write or flush fails, mapped
/// from the stream's `anyhow::Error` via `std::io::ErrorKind::BrokenPipe`.
pub async fn write_message_ns(
    stream: &mut dyn NoshSendStream,
    msg: &crate::messages::Message,
) -> Result<(), crate::codec::ProtoError> {
    let frame = crate::codec::encode(msg)?;
    stream.write_all(&frame).await.map_err(|e| {
        crate::codec::ProtoError::Io(std::io::Error::new(std::io::ErrorKind::BrokenPipe, e))
    })?;
    stream.flush().await.map_err(|e| {
        crate::codec::ProtoError::Io(std::io::Error::new(std::io::ErrorKind::BrokenPipe, e))
    })?;
    Ok(())
}

/// Read a [`crate::messages::Message`] via a `NoshRecvStream` trait object.
///
/// Parallel to [`crate::codec::read_message`] but works with
/// `&mut dyn NoshRecvStream` instead of `AsyncRead + Unpin`. The wire format
/// is identical: reads a 4-byte big-endian length prefix, enforces
/// [`crate::codec::MAX_FRAME_LEN`], reads the body, and decodes via
/// [`crate::codec::decode`].
///
/// # Errors
///
/// * `ProtoError::FrameTooLarge` — declared length exceeds `MAX_FRAME_LEN`
///   (16 MiB), preventing unbounded allocation (T-23-01 DoS mitigation).
/// * `ProtoError::Io` — stream read failed, mapped via
///   `std::io::ErrorKind::UnexpectedEof`.
/// * `ProtoError::Postcard` — body deserialization failed.
pub async fn read_message_ns(
    stream: &mut dyn NoshRecvStream,
) -> Result<crate::messages::Message, crate::codec::ProtoError> {
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await.map_err(|e| {
        crate::codec::ProtoError::Io(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            e,
        ))
    })?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > crate::codec::MAX_FRAME_LEN {
        return Err(crate::codec::ProtoError::FrameTooLarge(len));
    }
    let mut body = vec![0u8; len];
    stream.read_exact(&mut body).await.map_err(|e| {
        crate::codec::ProtoError::Io(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            e,
        ))
    })?;
    crate::codec::decode(&body)
}
