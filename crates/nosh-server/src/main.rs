//! `nosh-server` binary — a QUIC server enforcing SSH-key mutual auth (Phase 2).

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use clap::Parser;
use nosh_server::server::{self, AuthLimits};

/// nosh server (Phase 2): accepts an SSH-key-mutually-authenticated QUIC
/// connection and echoes reliable-stream bytes and datagrams back. Unknown
/// client keys are rejected inside the TLS handshake.
#[derive(Parser, Debug)]
#[command(name = "nosh-server", about, version)]
struct Args {
    /// Bind address. Default loopback for unprivileged dev/CI.
    #[arg(long, default_value = "127.0.0.1")]
    addr: IpAddr,

    /// Bind port. Default 4433 (unprivileged); UDP/443 is the production target.
    #[arg(long, default_value_t = 4433)]
    port: u16,

    /// Ed25519 host private key file (daemon model — read directly). Default
    /// `~/.config/nosh/host_ed25519` (overridable, D-06/D-08).
    #[arg(long)]
    host_key: Option<PathBuf>,

    /// OpenSSH `authorized_keys` file of permitted client keys (D-07/D-08).
    /// Default `~/.ssh/authorized_keys`.
    #[arg(long)]
    authorized_keys: Option<PathBuf>,

    /// Max concurrent unauthenticated/half-open handshakes (D-13).
    #[arg(long, default_value_t = 64)]
    max_concurrent_handshakes: usize,

    /// Seconds a connection has to complete auth before being dropped (D-13).
    #[arg(long, default_value_t = 5)]
    auth_timeout_secs: u64,
}

fn default_host_key() -> anyhow::Result<PathBuf> {
    let base = dirs::config_dir().context("locate config dir for default host key")?;
    Ok(base.join("nosh").join("host_ed25519"))
}

fn default_authorized_keys() -> anyhow::Result<PathBuf> {
    let home = dirs::home_dir().context("locate home dir for default authorized_keys")?;
    Ok(home.join(".ssh").join("authorized_keys"))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let args = Args::parse();
    let addr = SocketAddr::new(args.addr, args.port);
    let host_key = match args.host_key {
        Some(p) => p,
        None => default_host_key()?,
    };
    let authorized_keys = match args.authorized_keys {
        Some(p) => p,
        None => default_authorized_keys()?,
    };

    tracing::info!(
        %addr,
        host_key = %host_key.display(),
        authorized_keys = %authorized_keys.display(),
        "nosh-server listening (ALPN nosh/0, SSH-key mutual auth)"
    );

    let limits = AuthLimits {
        max_concurrent: args.max_concurrent_handshakes,
        auth_timeout: Duration::from_secs(args.auth_timeout_secs),
    };
    let endpoint = server::make_endpoint(addr, &host_key, &authorized_keys)?;
    server::run_accept_loop(endpoint, limits).await
}
