//! `nosh-client` binary — connects to a `nosh-server` with SSH-key mutual auth
//! (Phase 2): pins the server host key against `known_hosts` (TOFU) and signs
//! the client `CertificateVerify` via ssh-agent. Then proves the transport
//! (handshake + ALPN, stream echo, datagram round-trip), holds idle, exits.

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use bytes::Bytes;
use clap::Parser;
use nosh_client::client::{self, ClientIdentity};

/// nosh client (Phase 2 — SSH-key mutual auth over QUIC).
#[derive(Parser, Debug)]
#[command(name = "nosh-client", about, version)]
struct Args {
    /// Server address.
    #[arg(long, default_value = "127.0.0.1")]
    addr: IpAddr,

    /// Server port (matches the server's --port; default 4433).
    #[arg(long, default_value_t = 4433)]
    port: u16,

    /// Host name used both as the QUIC SNI and the `known_hosts` lookup key.
    #[arg(long, default_value = "localhost")]
    host: String,

    /// Path to the identity public key selecting which ssh-agent key to use.
    /// If omitted, the agent's single key is used (D-04).
    #[arg(long)]
    identity: Option<PathBuf>,

    /// OpenSSH `known_hosts` file for host-key pinning/TOFU (D-05/D-08).
    /// Default `~/.ssh/known_hosts`.
    #[arg(long)]
    known_hosts: Option<PathBuf>,

    /// Seconds to hold the connection idle after the round-trips.
    #[arg(long, default_value_t = 2)]
    idle_hold_secs: u64,
}

fn default_known_hosts() -> anyhow::Result<PathBuf> {
    let home = dirs::home_dir().context("locate home dir for default known_hosts")?;
    Ok(home.join(".ssh").join("known_hosts"))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let args = Args::parse();
    let server_addr = SocketAddr::new(args.addr, args.port);

    let socket = std::env::var_os("SSH_AUTH_SOCK")
        .map(PathBuf::from)
        .context("SSH_AUTH_SOCK not set — start an ssh-agent and add your key")?;
    let identity = ClientIdentity::from_agent(socket, args.identity.as_deref())?;
    let known_hosts = match args.known_hosts {
        Some(p) => p,
        None => default_known_hosts()?,
    };

    let endpoint = client::make_endpoint(&identity, known_hosts, args.host.clone())?;
    tracing::info!(%server_addr, "connecting (SSH-key mutual auth)");
    let conn = client::connect(&endpoint, server_addr, &args.host).await?;
    tracing::info!("connected; mutual auth complete; ALPN nosh/0 verified");

    let payload = b"hello-nosh";
    let echoed = client::stream_echo_roundtrip(&conn, payload).await?;
    anyhow::ensure!(echoed == payload, "stream echo mismatch");
    tracing::info!("stream echo matched");

    let dgram = Bytes::from_static(b"hello-datagram");
    let dgram_echo = client::datagram_roundtrip(&conn, dgram.clone()).await?;
    anyhow::ensure!(dgram_echo == dgram, "datagram echo mismatch");
    tracing::info!(max_datagram_size = ?conn.max_datagram_size(), "datagram round-trip matched");

    client::concurrent_roundtrip(&conn).await?;
    tracing::info!("concurrent stream + datagram round-trip ok");

    tokio::time::sleep(Duration::from_secs(args.idle_hold_secs)).await;
    anyhow::ensure!(
        conn.close_reason().is_none(),
        "connection dropped during idle hold"
    );
    tracing::info!(secs = args.idle_hold_secs, "connection survived idle");

    conn.close(0u32.into(), b"done");
    endpoint.wait_idle().await;
    tracing::info!("done — all checks passed");
    Ok(())
}
