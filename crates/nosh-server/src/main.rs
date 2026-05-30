//! `nosh-server` binary — a QUIC server enforcing SSH-key mutual auth (Phase 2/5).

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use clap::Parser;
use nosh_server::registry::SessionRegistry;
use nosh_server::server::{self, AuthLimits};

/// nosh server: accepts an SSH-key-mutually-authenticated QUIC connection and
/// runs a real PTY login-shell session. Unknown client keys are rejected inside
/// the TLS handshake. Sessions survive a transport-level disconnect (orphaned,
/// PTY kept alive) and are bounded per identity by `--max-sessions-per-identity`.
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

    /// Override the login shell spawned for sessions (default: the account's
    /// shell from /etc/passwd, run as a login shell).
    #[arg(long)]
    shell: Option<String>,

    /// Idle timeout (seconds) for ORPHANED sessions; 0 = disabled (Mosh
    /// behavior, D-08). CLI flag overrides NOSH_IDLE_TIMEOUT_SECS env, which
    /// overrides the default 0 (D-09).
    #[arg(long, env = "NOSH_IDLE_TIMEOUT_SECS", default_value_t = 0)]
    idle_timeout_secs: u64,

    /// Maximum number of orphaned sessions retained per SSH identity (D-05).
    /// When this cap is exceeded the least-recently-active orphan is evicted.
    #[arg(long, default_value_t = 5)]
    max_sessions_per_identity: usize,
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

    // Build the session registry from CLI/env config (D-08/D-09).
    // idle_timeout = 0 → disabled (Mosh behavior, default).
    let registry = SessionRegistry::new(
        args.max_sessions_per_identity,
        Duration::from_secs(args.idle_timeout_secs),
    );
    tracing::info!(
        idle_timeout_secs = args.idle_timeout_secs,
        max_sessions_per_identity = args.max_sessions_per_identity,
        "session persistence config"
    );

    let endpoint = server::make_endpoint(addr, &host_key, &authorized_keys)?;
    server::run_accept_loop(endpoint, registry, limits, args.shell).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify clap parsing + env precedence for idle-timeout config (D-08/D-09).
    ///
    /// All env-manipulation assertions run in a single test to avoid cross-test
    /// env races (process-global state).
    #[test]
    fn cli_env_precedence_idle_timeout() {
        // 1. Default: no flag, no env → idle_timeout_secs == 0.
        // We must ensure the env var is unset before testing the default.
        std::env::remove_var("NOSH_IDLE_TIMEOUT_SECS");
        let args = Args::try_parse_from(["nosh-server"]).unwrap();
        assert_eq!(
            args.idle_timeout_secs, 0,
            "idle_timeout_secs default must be 0 (Mosh behavior, D-08)"
        );
        assert_eq!(
            args.max_sessions_per_identity, 5,
            "max_sessions_per_identity default must be 5 (D-05)"
        );

        // 2. CLI flag supplied → honored.
        let args = Args::try_parse_from(["nosh-server", "--idle-timeout-secs", "30"]).unwrap();
        assert_eq!(
            args.idle_timeout_secs, 30,
            "parsed --idle-timeout-secs 30 must yield 30"
        );

        // 3. Env var set, no CLI flag → env value wins over default.
        std::env::set_var("NOSH_IDLE_TIMEOUT_SECS", "45");
        let args = Args::try_parse_from(["nosh-server"]).unwrap();
        assert_eq!(
            args.idle_timeout_secs, 45,
            "NOSH_IDLE_TIMEOUT_SECS=45 with no CLI flag must yield 45 (D-09)"
        );

        // 4. Env var set AND CLI flag supplied → CLI wins (D-09 precedence).
        let args =
            Args::try_parse_from(["nosh-server", "--idle-timeout-secs", "7"]).unwrap();
        assert_eq!(
            args.idle_timeout_secs, 7,
            "CLI flag must override NOSH_IDLE_TIMEOUT_SECS env (D-09)"
        );

        // Restore clean state.
        std::env::remove_var("NOSH_IDLE_TIMEOUT_SECS");
    }
}
