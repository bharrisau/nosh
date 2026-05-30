//! Shared quinn [`TransportConfig`] builder used by both endpoints so the
//! client and server stay consistent on the settings that matter for Phase 1.

use std::time::Duration;

use quinn::TransportConfig;

/// Datagram receive/send buffer size (1 MiB).
const DATAGRAM_BUFFER: usize = 1 << 20;
/// Keep-alive interval (client side). Comfortably below both the 30s QUIC
/// default idle timeout and our 300s `MAX_IDLE_TIMEOUT`.
///
/// Pitfall #4 / ROAM-01: these two constants are intentionally LEFT UNCHANGED
/// for connection migration. The 300 s idle timeout is far longer than any
/// loopback or real-network path-validation window (ms-to-seconds scale), so a
/// migrating connection will NOT idle-out mid-path-validation. The client
/// keep-alive at 15 s keeps the new path warm and prevents the server from
/// treating a quiet post-migration shell as idle. Do NOT lower MAX_IDLE_TIMEOUT
/// or disable KEEP_ALIVE without re-validating the migration test.
const KEEP_ALIVE: Duration = Duration::from_secs(15);
/// Finite idle timeout for a quiet interactive shell. Never `None` — an
/// infinite timeout risks permanently hung futures on a broken path.
const MAX_IDLE_TIMEOUT: Duration = Duration::from_secs(300);

/// Build the shared quinn transport configuration.
///
/// - `datagram_receive_buffer_size(Some(..))` and `datagram_send_buffer_size`
///   ENABLE RFC 9221 datagrams. They default to `None`, which silently
///   *disables* incoming datagrams (research PITFALL 1) — so this is mandatory
///   on both endpoints (satisfies TRANS-03).
/// - `max_idle_timeout` is finite (300s), never `None` (PITFALL 3).
/// - When `enable_keep_alive` is `true` (client side), `keep_alive_interval` is
///   set to 15s so a connection left idle for 60s does not drop (PITFALL 3,
///   satisfies TRANS-05). One side setting keep-alive is sufficient per the
///   quinn docs, so the server passes `false`.
pub fn transport_config(enable_keep_alive: bool) -> TransportConfig {
    let mut transport = TransportConfig::default();

    // TRANS-03: enable datagrams on this endpoint.
    transport.datagram_receive_buffer_size(Some(DATAGRAM_BUFFER));
    transport.datagram_send_buffer_size(DATAGRAM_BUFFER);

    // TRANS-05: finite idle timeout, never infinite.
    transport.max_idle_timeout(Some(
        MAX_IDLE_TIMEOUT
            .try_into()
            .expect("300s is a valid QUIC idle timeout"),
    ));

    // TRANS-05: the client drives keep-alive so a quiet shell survives idle.
    if enable_keep_alive {
        transport.keep_alive_interval(Some(KEEP_ALIVE));
    }

    transport
}
