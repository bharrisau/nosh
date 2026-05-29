//! `nosh-server` binary — a QUIC echo server for the Phase 1 transport
//! skeleton.

use std::net::{IpAddr, SocketAddr};

use clap::Parser;
use nosh_server::server;

/// nosh server (Phase 1 transport skeleton): accepts a QUIC connection and
/// echoes reliable-stream bytes and datagrams back to the client.
#[derive(Parser, Debug)]
#[command(name = "nosh-server", about, version)]
struct Args {
    /// Bind address. Default loopback for unprivileged dev/CI.
    #[arg(long, default_value = "127.0.0.1")]
    addr: IpAddr,

    /// Bind port. Default 4433 so dev/CI run unprivileged; UDP/443 is the
    /// documented production target.
    #[arg(long, default_value_t = 4433)]
    port: u16,
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
    let addr = SocketAddr::new(args.addr, args.port);

    tracing::info!(
        %addr,
        "nosh-server listening (ALPN nosh/0); 4433 is the unprivileged dev default, UDP/443 is the production target"
    );

    let endpoint = server::make_endpoint(addr)?;
    server::run_accept_loop(endpoint).await
}
