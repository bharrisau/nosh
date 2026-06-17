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

    /// Measured round-trip time for the connection path (smoothed RTT).
    ///
    /// Used by the predictive-echo overlay to time prediction culling (D-17-02a).
    /// Returns `Duration::ZERO` if the transport does not expose RTT
    /// (e.g. WebTransport in Mode A before per-path metrics are available).
    ///
    /// Quinn wrapper: delegates to `quinn::Connection::rtt()`.
    fn rtt(&self) -> std::time::Duration {
        std::time::Duration::ZERO
    }

    /// Returns `true` when the connection is known to be closed or lost.
    ///
    /// Used to gate the connection-loss overlay (QOL-01 / BUG-C fix): datagram
    /// silence is NOT connection loss on an idle healthy shell. Only activate the
    /// overlay when the connection is confirmed closed.
    ///
    /// Returns `false` by default — WebTransport transports can override if they
    /// expose a synchronous close-reason query. Quinn wrapper: delegates to
    /// `quinn::Connection::close_reason().is_some()`.
    fn is_closed(&self) -> bool {
        false
    }

    /// Export keying material from the underlying TLS 1.3 session (RFC 9266 /
    /// RFC 5705 channel binding, D-01).
    ///
    /// Used by the inner SSH-key handshake (Phase 25) to bind the inner-auth
    /// transcript to the outer TLS session so that a trusted proxy cannot
    /// replay inner-auth messages from a different connection. Both the server
    /// and the client call this with the same `label` and `context` and derive
    /// identical 32-byte output because RFC 9266 EKM is session-symmetric.
    ///
    /// Only the WebTransport transport implements this by delegating to
    /// `quic_connection().export_keying_material(output, label, context)`.
    /// The native Quinn wrapper can also implement it. Test doubles and any
    /// transport that does not expose RFC 9266 inherit this default `Err` impl
    /// and must fall back to CSPRNG-nonce channel binding instead.
    ///
    /// `output` is populated in-place on success (exactly `output.len()` bytes
    /// of keying material). `label` and `context` follow RFC 5705: use
    /// `crate::INNER_AUTH_EKM_LABEL` and `crate::INNER_AUTH_EKM_CONTEXT`
    /// as the single source of truth so both endpoints derive identical material
    /// (guards against Pitfall 1 — label divergence between client and server).
    fn export_keying_material(
        &self,
        output: &mut [u8; 32],
        label: &[u8],
        context: &[u8],
    ) -> anyhow::Result<()> {
        let _ = (output, label, context);
        anyhow::bail!("export_keying_material not supported by this transport")
    }

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

    /// Resolves when either:
    /// - the peer acknowledges receipt of all stream data after a `finish()` (clean drain), OR
    /// - the peer sends a STOP_SENDING frame (aborting the stream, `Some(error_code)`)
    ///
    /// The returned `anyhow::Result<()>` discards the stop code; callers that need to
    /// distinguish abort from clean drain must use the underlying transport's native API.
    /// Typically called with a timeout after `finish()` as a best-effort drain wait.
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
        // DIAGNOSTIC (Phase 28 framing-desync blocker): a length prefix larger
        // than MAX_FRAME_LEN almost always means the reliable stream has DESYNCED
        // — we are reading payload bytes as a 4-byte big-endian length. Surface
        // the raw prefix bytes (hex + ASCII) so the offending content is visible.
        // 1718183741 == 0x6669673D == "fig=" was the field report. This log is
        // always-on (it only fires on the bug) and never leaks normal traffic.
        tracing::error!(
            len,
            len_bytes_hex = %format!("{:02x} {:02x} {:02x} {:02x}", len_buf[0], len_buf[1], len_buf[2], len_buf[3]),
            len_bytes_ascii = %bytes_as_ascii(&len_buf),
            "reliable-stream framing DESYNC: length prefix exceeds MAX_FRAME_LEN \
             (read payload as a frame length — stream is misaligned)"
        );
        return Err(crate::codec::ProtoError::FrameTooLarge(len));
    }
    let mut body = vec![0u8; len];
    stream.read_exact(&mut body).await.map_err(|e| {
        crate::codec::ProtoError::Io(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            e,
        ))
    })?;
    let msg = crate::codec::decode(&body)?;
    // DIAGNOSTIC (env-gated full frame trace): when NOSH_FRAME_TRACE is set, log
    // every reliable frame read on this process (client AND server) — variant,
    // body length, and the full body bytes as hex. The operator opted into full
    // dumps; size is not a concern for diagnosis. Off by default (zero overhead
    // beyond a OnceLock load once the bool is cached).
    if frame_trace_enabled() {
        tracing::info!(
            target: "nosh_frame_trace",
            variant = msg.variant_name(),
            body_len = len,
            body_hex = %hex_dump(&body),
            "rx frame"
        );
    }
    Ok(msg)
}

/// Render bytes as a printable-ASCII string, replacing non-printables with `.`.
/// Used by the framing-desync diagnostic — safe for arbitrary bytes.
fn bytes_as_ascii(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|&b| if (0x20..0x7f).contains(&b) { b as char } else { '.' })
        .collect()
}

/// Lowercase hex dump (no separators) of an arbitrary byte slice.
fn hex_dump(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Whether the env-gated full frame trace (`NOSH_FRAME_TRACE`) is active.
/// Read once and cached — the env var is sampled a single time per process.
fn frame_trace_enabled() -> bool {
    use std::sync::OnceLock;
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("NOSH_FRAME_TRACE").is_some())
}

// ── Blanket impls for Box<dyn NoshSendStream> / Box<dyn NoshRecvStream> ────────
//
// These allow callers to hold a `Box<dyn NoshSendStream>` value and pass
// `&mut boxed_stream` directly to helpers that accept `&mut (impl NoshSendStream)`.
// Without these impls, callers would need to write `&mut *boxed_stream` at every
// call site (deref-coercion for &mut is not automatic). Blanket impls also mean
// `tokio::spawn(task(boxed_send, boxed_recv))` works without explicit dereffing.

#[async_trait]
impl NoshSendStream for Box<dyn NoshSendStream> {
    async fn write_all(&mut self, data: &[u8]) -> anyhow::Result<()> {
        (**self).write_all(data).await
    }
    async fn flush(&mut self) -> anyhow::Result<()> {
        (**self).flush().await
    }
    async fn finish(&mut self) -> anyhow::Result<()> {
        (**self).finish().await
    }
    async fn stopped(&mut self) -> anyhow::Result<()> {
        (**self).stopped().await
    }
    fn reset(&mut self, code: u32) {
        (**self).reset(code)
    }
}

#[async_trait]
impl NoshRecvStream for Box<dyn NoshRecvStream> {
    async fn read_exact(&mut self, buf: &mut [u8]) -> anyhow::Result<()> {
        (**self).read_exact(buf).await
    }
    async fn read(&mut self, buf: &mut [u8]) -> anyhow::Result<Option<usize>> {
        (**self).read(buf).await
    }
    fn stop(&mut self, code: u32) {
        (**self).stop(code)
    }
}
