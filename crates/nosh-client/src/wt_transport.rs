//! WebTransport client transport wrapper (Phase 24 / Plan 04 / WT-03, WT-05).
//!
//! `WtransportTransport`, `WtransportSendStream`, and `WtransportRecvStream`
//! are thin newtype wrappers over the `wtransport` crate's connection and stream
//! types. They implement the `NoshTransport` / `NoshSendStream` /
//! `NoshRecvStream` traits so the session pump from Plan 02 works unchanged
//! whether the underlying transport is native QUIC or WebTransport.
//!
//! Also provides:
//! - `build_wt_client_config` — Mode A: CA-signed server cert, validated via OS
//!   trust store (D-04, D-05). Requires the server to present a valid TLS cert.
//! - `build_wt_client_config_custom_tls` — test-only path: injects a custom
//!   `rustls::ClientConfig` (e.g. one that accepts self-signed certs from the
//!   integration test fixture). See Plan 05.
//! - `connect_wt` — builds an Endpoint, dials the URL, and returns
//!   `Box<dyn NoshTransport>` ready for the generic pump.
//!
//! # API differences from the QuinnTransport wrapper
//!
//! These are the six non-obvious diffs documented in RESEARCH.md:
//!
//! 1. `send_datagram` — wtransport's `SendDatagramError` has 3 variants
//!    (no `Disabled`); `NotConnected` maps to `SendDatagramError::ConnectionLost`.
//! 2. `datagram_send_buffer_space` — no native method on `wtransport::Connection`;
//!    must go through `quic_connection()` (requires `"quinn"` feature on wtransport).
//! 3. `max_datagram_size` — same method name as quinn; but the wtransport value
//!    already subtracts WebTransport capsule overhead (D-03). Never use
//!    `quic_connection().max_datagram_size()` for payload sizing.
//! 4. `read_datagram` — returns `wtransport::Datagram`, not `Bytes`.
//!    Converted via `.payload()` (a zero-copy `Bytes` slice).
//! 5. `finish()` — `wtransport::SendStream::finish()` IS ASYNC (unlike quinn 0.11
//!    where `finish()` is sync). The wrapper body calls `.finish().await`.
//!    OPPOSITE of the quinn wrapper — see quinn_transport.rs comment.
//! 6. `stop()` on RecvStream — `wtransport::RecvStream::stop(self)` is CONSUMING.
//!    `NoshRecvStream::stop` takes `&mut self`. Adapter: `Option<RecvStream>`
//!    with `.take()`.

#[cfg(feature = "webtransport")]
use async_trait::async_trait;
#[cfg(feature = "webtransport")]
use bytes::Bytes;
#[cfg(feature = "webtransport")]
use std::net::SocketAddr;
#[cfg(feature = "webtransport")]
use nosh_proto::transport_trait::{
    NoshTransport, NoshSendStream, NoshRecvStream, SendDatagramError,
};
#[cfg(feature = "webtransport")]
use wtransport::{Connection, SendStream, RecvStream, VarInt};
#[cfg(feature = "webtransport")]
use wtransport::error::SendDatagramError as WtSendDatagramError;
// rustls is used by build_wt_client_config_custom_tls (test-only path).
#[cfg(feature = "webtransport")]
use rustls;

// ── WtransportTransport ────────────────────────────────────────────────────────

/// Newtype wrapper over `wtransport::Connection` implementing `NoshTransport`.
///
/// Drop-in replacement for `QuinnTransport` in the generic session pump.
/// The underlying `wtransport::Connection` holds a WebTransport-over-HTTP/3
/// session (after the HTTP/3 CONNECT upgrade handshake).
#[cfg(feature = "webtransport")]
pub struct WtransportTransport(pub Connection);

#[cfg(feature = "webtransport")]
#[async_trait]
impl NoshTransport for WtransportTransport {
    fn send_datagram(&self, data: Bytes) -> Result<(), SendDatagramError> {
        // wtransport::SendDatagramError: 3 variants (no Disabled).
        // NotConnected maps to ConnectionLost (closest semantic match).
        self.0.send_datagram(data.as_ref()).map_err(|e| match e {
            WtSendDatagramError::TooLarge => SendDatagramError::TooLarge,
            WtSendDatagramError::UnsupportedByPeer => SendDatagramError::UnsupportedByPeer,
            WtSendDatagramError::NotConnected => {
                SendDatagramError::ConnectionLost("not connected".to_string())
            }
        })
    }

    fn datagram_send_buffer_space(&self) -> usize {
        // wtransport::Connection has NO datagram_send_buffer_space().
        // The `quinn` feature on wtransport exposes quic_connection() -> &quinn::Connection.
        self.0.quic_connection().datagram_send_buffer_space()
    }

    fn max_datagram_size(&self) -> Option<usize> {
        // max_datagram_size() already subtracts WebTransport capsule overhead (D-03).
        // DO NOT use quic_connection().max_datagram_size() — the raw quinn value is too large.
        self.0.max_datagram_size()
    }

    async fn read_datagram(&self) -> anyhow::Result<Bytes> {
        let dg = self.0.receive_datagram().await?;
        // Datagram::payload() returns a zero-copy Bytes slice of the payload.
        Ok(dg.payload())
    }

    async fn accept_bi(
        &self,
    ) -> anyhow::Result<(Box<dyn NoshSendStream>, Box<dyn NoshRecvStream>)> {
        let (s, r) = self.0.accept_bi().await?;
        Ok((
            Box::new(WtransportSendStream(s)),
            Box::new(WtransportRecvStream(Some(r))),
        ))
    }

    async fn open_bi(
        &self,
    ) -> anyhow::Result<(Box<dyn NoshSendStream>, Box<dyn NoshRecvStream>)> {
        // DOUBLE AWAIT: open_bi().await? yields OpeningBiStream, not the stream pair.
        // Second .await? on OpeningBiStream yields (SendStream, RecvStream).
        // Both awaits are hidden inside this single-await interface.
        let (s, r) = self.0.open_bi().await?.await?;
        Ok((
            Box::new(WtransportSendStream(s)),
            Box::new(WtransportRecvStream(Some(r))),
        ))
    }

    fn remote_address(&self) -> SocketAddr {
        self.0.remote_address()
    }

    fn rtt(&self) -> std::time::Duration {
        // wtransport::Connection::rtt() exposed directly (D-17-02a).
        self.0.rtt()
    }

    fn is_closed(&self) -> bool {
        // quic_connection().close_reason().is_some() — same pattern as QuinnTransport.
        self.0.quic_connection().close_reason().is_some()
    }

    fn close(&self, code: u32, reason: &[u8]) {
        // wtransport::Connection::close(error_code: VarInt, reason: &[u8]) — verified.
        self.0.close(VarInt::from_u32(code), reason)
    }

    fn export_keying_material(
        &self,
        output: &mut [u8; 32],
        label: &[u8],
        context: &[u8],
    ) -> anyhow::Result<()> {
        // Delegate to the underlying quinn::Connection via quic_connection().
        // quic_connection() is gated on the "quinn" feature — confirmed active
        // in workspace Cargo.toml (features = [..., "quinn"]).
        // Both TLS endpoints of the same WebTransport session derive identical
        // bytes given the same label, context, and output length (RFC 9266 / D-01).
        self.0
            .quic_connection()
            .export_keying_material(output, label, context)
            .map_err(|e| anyhow::anyhow!("export_keying_material failed: {e:?}"))
    }
}

// ── WtransportSendStream ───────────────────────────────────────────────────────

/// Newtype wrapper over `wtransport::SendStream` implementing `NoshSendStream`.
///
/// # CRITICAL: `finish()` IS ASYNC in wtransport
///
/// Unlike the `QuinnSendStream` wrapper (where `quinn::SendStream::finish()` is
/// synchronous and the wrapper explicitly says "DO NOT .await"), the
/// `wtransport::SendStream::finish()` IS async and the wrapper body MUST call
/// `.finish().await`. This is the OPPOSITE of the quinn wrapper.
#[cfg(feature = "webtransport")]
pub struct WtransportSendStream(pub SendStream);

#[cfg(feature = "webtransport")]
#[async_trait]
impl NoshSendStream for WtransportSendStream {
    async fn write_all(&mut self, data: &[u8]) -> anyhow::Result<()> {
        Ok(self.0.write_all(data).await?)
    }

    async fn flush(&mut self) -> anyhow::Result<()> {
        // wtransport::SendStream implements AsyncWrite via tokio's AsyncWriteExt.
        use tokio::io::AsyncWriteExt as _;
        Ok(self.0.flush().await?)
    }

    async fn finish(&mut self) -> anyhow::Result<()> {
        // UNLIKE quinn: wtransport::SendStream::finish() IS ASYNC. MUST call .await.
        // The quinn_transport.rs wrapper says "DO NOT .await" — the OPPOSITE is true here.
        Ok(self.0.finish().await?)
    }

    async fn stopped(&mut self) -> anyhow::Result<()> {
        // stopped() returns StreamWriteError (not Result) — discard and return Ok.
        let _ = self.0.stopped().await;
        Ok(())
    }

    fn reset(&mut self, code: u32) {
        // wtransport::VarInt: use from_u32() (no From<u32> impl unlike quinn::VarInt).
        let _ = self.0.reset(VarInt::from_u32(code));
    }
}

// ── WtransportRecvStream ───────────────────────────────────────────────────────

/// Newtype wrapper over `wtransport::RecvStream` implementing `NoshRecvStream`.
///
/// # CRITICAL: `stop()` is consuming in wtransport
///
/// `wtransport::RecvStream::stop(self)` takes ownership. `NoshRecvStream::stop`
/// takes `&mut self`. Adapter: `Option<wtransport::RecvStream>` with `.take()`.
/// After `stop()`, subsequent `read` / `read_exact` calls return EOF/error via
/// the `None` branch.
#[cfg(feature = "webtransport")]
pub struct WtransportRecvStream(pub Option<RecvStream>);

#[cfg(feature = "webtransport")]
#[async_trait]
impl NoshRecvStream for WtransportRecvStream {
    async fn read_exact(&mut self, buf: &mut [u8]) -> anyhow::Result<()> {
        match self.0.as_mut() {
            Some(s) => Ok(s.read_exact(buf).await?),
            None => Err(anyhow::anyhow!("stream already stopped")),
        }
    }

    async fn read(&mut self, buf: &mut [u8]) -> anyhow::Result<Option<usize>> {
        match self.0.as_mut() {
            Some(s) => Ok(s.read(buf).await?),
            None => Ok(None), // stopped → EOF
        }
    }

    fn stop(&mut self, code: u32) {
        // take() satisfies wtransport::RecvStream::stop(self) consuming requirement.
        // After take, self.0 is None — subsequent reads return EOF/error.
        if let Some(stream) = self.0.take() {
            stream.stop(VarInt::from_u32(code));
        }
    }
}

// ── Config + connect helpers ───────────────────────────────────────────────────

/// Build a WebTransport `ClientConfig` using the OS native CA trust roots.
///
/// Mode A (D-04, D-05): the server must present a CA-signed TLS certificate
/// (e.g. Let's Encrypt). This validates the outer TLS layer via the OS trust
/// store. Inner SSH-key mutual auth is added in Phase 25.
///
/// For development with a self-signed test cert, use
/// [`build_wt_client_config_custom_tls`] instead.
#[cfg(feature = "webtransport")]
pub fn build_wt_client_config() -> wtransport::ClientConfig {
    // build() returns ClientConfig directly (not a Result) — no ? operator.
    wtransport::ClientConfig::builder()
        .with_bind_default()
        .with_native_certs()
        .build()
}

/// Build a WebTransport `ClientConfig` with a custom `rustls::ClientConfig`.
///
/// Test-only path: injects a custom TLS verifier (e.g. one that accepts the
/// self-signed cert from the integration test fixture in Plan 05). This variant
/// bypasses OS CA validation — MUST NOT be used in production.
///
/// # Example (Plan 05 integration test)
///
/// ```rust,ignore
/// // Construct a rustls ClientConfig that accepts the test server cert by SPKI.
/// let rustls_cfg = build_insecure_test_rustls_client_config();
/// let wt_config = build_wt_client_config_custom_tls(rustls_cfg);
/// let transport = connect_wt(wt_config, &url).await?;
/// ```
#[cfg(feature = "webtransport")]
pub fn build_wt_client_config_custom_tls(
    rustls_client_cfg: rustls::ClientConfig,
) -> wtransport::ClientConfig {
    // build() returns ClientConfig directly (not a Result) — no ? operator.
    wtransport::ClientConfig::builder()
        .with_bind_default()
        .with_custom_tls(rustls_client_cfg)
        .build()
}

/// Dial a WebTransport server and return a boxed `NoshTransport`.
///
/// Builds a `wtransport::Endpoint<Client>`, connects to `url` (which must be an
/// `https://` URL, e.g. `https://server.example.com:443/nosh`), and wraps the
/// resulting `wtransport::Connection` in a `WtransportTransport` boxed as
/// `Box<dyn NoshTransport>`.
///
/// The URL string is passed directly — `&str` implements `IntoConnectOptions`
/// via the `ToString` blanket impl, so no intermediate `ConnectOptions` struct
/// is needed (the struct is in `wtransport::endpoint::ConnectOptions`, not
/// re-exported at the crate root).
///
/// The returned transport is ready to be passed directly into the same session
/// pump (`fresh_session` / `reattach_session`) as a native QUIC connection.
#[cfg(feature = "webtransport")]
pub async fn connect_wt(
    config: wtransport::ClientConfig,
    url: &str,
) -> anyhow::Result<Box<dyn NoshTransport>> {
    let endpoint = wtransport::Endpoint::client(config)?;
    // url: &str implements ToString, which implements IntoConnectOptions via blanket impl.
    let conn = endpoint.connect(url).await?;
    Ok(Box::new(WtransportTransport(conn)))
}
