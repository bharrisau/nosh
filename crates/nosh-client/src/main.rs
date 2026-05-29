//! `nosh-client` binary — connects to a `nosh-server`, proves the Phase 1
//! transport (handshake + ALPN, stream echo, datagram round-trip, concurrent
//! coexistence), holds the connection idle briefly, then exits.

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use bytes::Bytes;
use clap::Parser;
use nosh_client::client;

/// nosh client (Phase 1 transport skeleton).
#[derive(Parser, Debug)]
#[command(name = "nosh-client", about, version)]
struct Args {
    /// Server address.
    #[arg(long, default_value = "127.0.0.1")]
    addr: IpAddr,

    /// Server port (matches the server's --port; default 4433).
    #[arg(long, default_value_t = 4433)]
    port: u16,

    /// Seconds to hold the connection idle after the round-trips, to
    /// demonstrate keep-alive. Short by default for a quick demo; the honest
    /// 60s idle-survival proof is the `idle_survival_60s` integration test.
    #[arg(long, default_value_t = 2)]
    idle_hold_secs: u64,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let args = Args::parse();
    let server_addr = SocketAddr::new(args.addr, args.port);

    let endpoint = client::make_endpoint()?;
    tracing::info!(%server_addr, "connecting");
    let conn = client::connect(&endpoint, server_addr).await?;
    tracing::info!("connected; ALPN nosh/0 verified (TRANS-01)");

    // TRANS-02: reliable-stream echo round-trip.
    let payload = b"hello-nosh";
    let echoed = client::stream_echo_roundtrip(&conn, payload).await?;
    anyhow::ensure!(echoed == payload, "stream echo mismatch");
    tracing::info!("stream echo matched (TRANS-02)");

    // TRANS-03: datagram round-trip; max_datagram_size is Some.
    let dgram = Bytes::from_static(b"hello-datagram");
    let dgram_echo = client::datagram_roundtrip(&conn, dgram.clone()).await?;
    anyhow::ensure!(dgram_echo == dgram, "datagram echo mismatch");
    tracing::info!(
        max_datagram_size = ?conn.max_datagram_size(),
        "datagram round-trip matched (TRANS-03)"
    );

    // TRANS-04: stream + datagram concurrently, no interference.
    client::concurrent_roundtrip(&conn).await?;
    tracing::info!("concurrent stream + datagram round-trip ok (TRANS-04)");

    // TRANS-05: hold idle, confirm the connection survives.
    tokio::time::sleep(Duration::from_secs(args.idle_hold_secs)).await;
    anyhow::ensure!(
        conn.close_reason().is_none(),
        "connection dropped during idle hold"
    );
    tracing::info!(secs = args.idle_hold_secs, "connection survived idle (TRANS-05)");

    // Clean shutdown.
    conn.close(0u32.into(), b"done");
    endpoint.wait_idle().await;
    tracing::info!("done — all transport checks passed");
    Ok(())
}
