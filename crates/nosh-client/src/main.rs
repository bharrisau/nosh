//! `nosh-client` binary — connects to a `nosh-server` with SSH-key mutual auth
//! (Phase 2), then runs an interactive PTY session (Phase 3): the local
//! terminal is put in raw mode (RAII-restored), keystrokes are forwarded to the
//! remote PTY, shell output is rendered locally, window resizes propagate
//! (SIGWINCH, coalesced), and the client process exits with the remote shell's
//! exit code.

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use clap::Parser;
use nosh_client::client::{self, ClientIdentity};
use nosh_proto::Message;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::signal::unix::{signal, SignalKind};

/// SIGWINCH debounce window (~40 ms) — coalesces a window-drag burst into one
/// `Resize` (SESS-05, avoids resize storms).
const RESIZE_DEBOUNCE: Duration = Duration::from_millis(40);

/// nosh client (Phase 3 — interactive PTY session over authenticated QUIC).
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
        .with_writer(std::io::stderr)
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

    let exit_code = run_interactive(&conn).await?;

    conn.close(0u32.into(), b"client done");
    endpoint.wait_idle().await;
    // The terminal is restored by the RawModeGuard's Drop before we get here.
    std::process::exit(exit_code);
}

/// Drive the interactive session and return the remote shell's exit code.
async fn run_interactive(conn: &quinn::Connection) -> anyhow::Result<i32> {
    let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
    let term = std::env::var("TERM").unwrap_or_else(|_| "xterm-256color".to_string());

    // Raw mode for the lifetime of the session; restored on any exit path
    // (normal, panic, error) via Drop (SESS-03).
    let _guard = client::RawModeGuard::enable().context("enter raw mode")?;

    let (mut send, mut recv) =
        client::open_session(conn, term, cols, rows, client::collect_client_env()).await?;

    let mut stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let mut winch = signal(SignalKind::window_change()).context("install SIGWINCH handler")?;

    let mut stdin_buf = [0u8; 8 * 1024];
    let mut exit_code = 0i32;
    // Coalesce SIGWINCH: arm a deadline on each signal; emit one Resize when it
    // elapses with no further signals (SESS-05).
    let mut resize_deadline: Option<tokio::time::Instant> = None;

    loop {
        let resize_sleep = async {
            match resize_deadline {
                Some(d) => tokio::time::sleep_until(d).await,
                None => std::future::pending::<()>().await,
            }
        };

        tokio::select! {
            // Server → client frames.
            msg = nosh_proto::read_message(&mut recv) => {
                match msg {
                    Ok(Message::PtyData { data }) => {
                        stdout.write_all(&data).await?;
                        stdout.flush().await?;
                    }
                    Ok(Message::SessionClose { exit_code: code, .. }) => {
                        exit_code = code;
                        break;
                    }
                    Ok(_) => {}
                    // Stream/connection closed (abrupt loss included): leave the
                    // loop; the guard restores the terminal on the way out.
                    Err(_) => break,
                }
            }
            // Keystrokes (incl. Ctrl-C as 0x03 — passed through, SESS-06).
            n = stdin.read(&mut stdin_buf) => {
                match n {
                    Ok(0) => break, // local stdin EOF
                    Ok(n) => {
                        if client::send_input(&mut send, &stdin_buf[..n]).await.is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            // Window resize signal: (re)arm the debounce deadline.
            _ = winch.recv() => {
                resize_deadline = Some(tokio::time::Instant::now() + RESIZE_DEBOUNCE);
            }
            // Debounce elapsed: send one coalesced Resize.
            _ = resize_sleep => {
                resize_deadline = None;
                if let Ok((c, r)) = crossterm::terminal::size() {
                    let _ = client::send_resize(&mut send, c, r).await;
                }
            }
        }
    }

    let _ = send.finish();
    Ok(exit_code)
}
