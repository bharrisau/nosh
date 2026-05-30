//! `nosh-client` binary — connects to a `nosh-server` with SSH-key mutual auth
//! (Phase 2), then runs an interactive PTY session (Phase 3): the local
//! terminal is put in raw mode (RAII-restored), keystrokes are forwarded to the
//! remote PTY, shell output is rendered locally, window resizes propagate
//! (coalesced), and the client process exits with the remote shell's exit code.
//!
//! Phase 6: adds a reconnect supervisor that auto-reconnects with exponential
//! backoff on transport drop (D-10). Holds the reattach token in memory; sends
//! `Ack` on a coarse cadence (D-08); handles `ReattachErr` as a terminal
//! condition (D-11). The `RawModeGuard` is entered ONCE and dropped once on
//! final exit regardless of how many reconnects occurred (RAII).
//!
//! Phase 8: adds `--identity-file` for on-disk Ed25519 key auth (WIN-02).
//! Auth-path selection:
//! - If `--identity-file` is given → `ClientIdentity::from_identity_file` (all platforms).
//! - Else on Unix → ssh-agent via `SSH_AUTH_SOCK` (existing default).
//! - Else on Windows → default to `%USERPROFILE%\.ssh\id_ed25519`; error if absent.
//!
//! Resize handling is `#[cfg]`-split via [`platform::ResizeWatcher`]:
//! - Unix: SIGWINCH → debounce → `Message::Resize`
//! - Windows: `crossterm::event::EventStream` `Event::Resize` → debounce → `Message::Resize`
//! Both paths preserve the ~40 ms coalescing and the authoritative `terminal::size()` re-read.
//!
//! The reconnect-window quit uses the cross-platform `platform::quit_signal()`
//! (backed by `tokio::signal::ctrl_c`).

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use clap::Parser;
use nosh_client::client::{self, ClientIdentity, ReattachOutcome};
use nosh_client::platform;
use nosh_proto::Message;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// SIGWINCH / console-resize debounce window (~40 ms) — coalesces a window-drag
/// burst into one `Resize` (SESS-05, avoids resize storms). Preserved on both
/// Unix (SIGWINCH) and Windows (EventStream Event::Resize).
const RESIZE_DEBOUNCE: Duration = Duration::from_millis(40);

/// Ack cadence: send an Ack frame roughly every 750ms when output has been
/// applied (D-08 continuous acking, Claude's discretion).
const ACK_INTERVAL: Duration = Duration::from_millis(750);

/// Reconnect backoff: start 250ms, double on each retry up to 10s cap (D-10).
const BACKOFF_INITIAL: Duration = Duration::from_millis(250);
const BACKOFF_MAX: Duration = Duration::from_secs(10);

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
    /// If omitted, the agent's single key is used (Unix only, D-04).
    #[arg(long)]
    identity: Option<PathBuf>,

    /// On-disk OpenSSH Ed25519 private key for authentication.
    /// Opt-in on Linux (ssh-agent is the default); the ONLY auth path on
    /// Windows (default: %USERPROFILE%\.ssh\id_ed25519 when flag is omitted).
    #[arg(long)]
    identity_file: Option<PathBuf>,

    /// OpenSSH `known_hosts` file for host-key pinning/TOFU (D-05/D-08).
    /// Default `~/.ssh/known_hosts`.
    #[arg(long)]
    known_hosts: Option<PathBuf>,
}

fn default_known_hosts() -> anyhow::Result<PathBuf> {
    let home = dirs::home_dir().context("locate home dir for default known_hosts")?;
    Ok(home.join(".ssh").join("known_hosts"))
}

/// Resolve the client identity based on CLI args and platform.
///
/// Priority:
/// 1. `--identity-file <path>` (all platforms, opt-in)
/// 2. Unix: ssh-agent via `SSH_AUTH_SOCK` + optional `--identity` key selector
/// 3. Windows (no --identity-file): default to `%USERPROFILE%\.ssh\id_ed25519`
fn resolve_identity(args: &Args) -> anyhow::Result<ClientIdentity> {
    // Warn if --identity is supplied on a platform where it has no effect.
    // On Unix, --identity selects which ssh-agent key to use. On Windows,
    // ssh-agent is not available and --identity is silently discarded; the
    // user almost certainly wants --identity-file instead.
    #[cfg(not(unix))]
    if args.identity.is_some() {
        tracing::warn!(
            "--identity is only used on Unix (ssh-agent key selector); \
             on Windows use --identity-file instead"
        );
    }

    // Branch 1: explicit --identity-file (all platforms, no SSH_AUTH_SOCK needed).
    if let Some(ref path) = args.identity_file {
        return ClientIdentity::from_identity_file(path);
    }

    // Branch 2: Unix — use ssh-agent (SSH_AUTH_SOCK required).
    #[cfg(unix)]
    {
        let socket = std::env::var_os("SSH_AUTH_SOCK")
            .map(PathBuf::from)
            .context("SSH_AUTH_SOCK not set — start an ssh-agent and add your key, or use --identity-file")?;
        return ClientIdentity::from_agent(socket, args.identity.as_deref());
    }

    // Branch 3: Windows — no ssh-agent available; default to %USERPROFILE%\.ssh\id_ed25519.
    #[cfg(windows)]
    {
        let default_key = dirs::home_dir()
            .context("locate home directory for default identity file")?
            .join(".ssh")
            .join("id_ed25519");
        if default_key.exists() {
            return ClientIdentity::from_identity_file(&default_key);
        }
        anyhow::bail!(
            "no --identity-file given and no key found at {}; \
             Windows requires --identity-file (ssh-agent is not available in this version)",
            default_key.display()
        );
    }

    // Fallback for platforms that are neither unix nor windows (should not occur).
    #[cfg(not(any(unix, windows)))]
    {
        anyhow::bail!(
            "unsupported platform: use --identity-file to specify an Ed25519 private key"
        );
    }
}

/// How the pump loop ended.
#[derive(Debug)]
enum PumpOutcome {
    /// Server sent SessionClose with exit code.
    CleanExit(i32),
    /// Transport dropped without a SessionClose — reconnect.
    TransportDrop,
    /// User explicitly quit (stdin EOF, or Ctrl-C while active — we treat both
    /// as clean exits in the interactive case).
    UserQuit,
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

    let identity = resolve_identity(&args)?;
    let known_hosts = match args.known_hosts {
        Some(p) => p,
        None => default_known_hosts()?,
    };

    // Raw mode entered ONCE for the lifetime of the reconnect supervisor.
    // Restored via Drop on any exit path — NEVER re-entered per reconnect (D-11).
    let _guard = client::RawModeGuard::enable().context("enter raw mode")?;

    let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
    let term = std::env::var("TERM").unwrap_or_else(|_| "xterm-256color".to_string());

    // Per-session reconnect state held across reconnects (D-01 in-memory only).
    let mut token: Option<[u8; 16]> = None; // None = no session yet
    let mut highest_applied: u64 = 0;       // highest seq applied to terminal

    let mut backoff = BACKOFF_INITIAL;
    let mut exit_code: i32 = 0;

    // Platform-abstracted resize watcher. Unix: SIGWINCH; Windows: EventStream Event::Resize.
    let mut resize = platform::ResizeWatcher::new().context("install resize handler")?;

    // Outer reconnect supervisor loop (D-10).
    loop {
        // Build a fresh endpoint and connection for this attempt.
        let endpoint = match client::make_endpoint(&identity, known_hosts.clone(), args.host.clone()) {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!("make_endpoint failed: {e}");
                // Wait with backoff, honouring quit signal.
                tokio::select! {
                    _ = tokio::time::sleep(backoff) => {}
                    _ = platform::quit_signal() => {
                        eprintln!("\r\nnosh: quit\r");
                        break;
                    }
                }
                backoff = (backoff * 2).min(BACKOFF_MAX);
                continue;
            }
        };

        let conn = match client::connect(&endpoint, server_addr, &args.host).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("connect failed: {e}");
                eprintln!("\r\nnosh: reconnecting…\r");
                tokio::select! {
                    _ = tokio::time::sleep(backoff) => {}
                    _ = platform::quit_signal() => {
                        eprintln!("\r\nnosh: quit\r");
                        break;
                    }
                }
                backoff = (backoff * 2).min(BACKOFF_MAX);
                endpoint.close(0u32.into(), b"connect failed");
                continue;
            }
        };

        // Either fresh open (no token) or reattach (have token).
        let pump_outcome = if let Some(tok) = token {
            // Reattach path (D-03 / ROAM-02).
            let reattach_result = reattach_session(
                &conn,
                tok,
                highest_applied,
                &mut highest_applied,
                &mut resize,
                &mut token,
            )
            .await;
            reattach_result.unwrap_or(PumpOutcome::TransportDrop)
        } else {
            // Fresh session path.
            let fresh_result = fresh_session(
                &conn,
                term.clone(),
                cols,
                rows,
                &mut highest_applied,
                &mut resize,
                &mut token,
            )
            .await;
            fresh_result.unwrap_or(PumpOutcome::TransportDrop)
        };

        conn.close(0u32.into(), b"pump ended");
        endpoint.wait_idle().await;

        match pump_outcome {
            PumpOutcome::CleanExit(code) => {
                exit_code = code;
                break;
            }
            PumpOutcome::UserQuit => {
                break;
            }
            PumpOutcome::TransportDrop => {
                eprintln!("\r\nnosh: reconnecting…\r");
                // Backoff before retry.
                tokio::select! {
                    _ = tokio::time::sleep(backoff) => {}
                    _ = platform::quit_signal() => {
                        // Explicit quit during reconnect window (D-11 escape path).
                        eprintln!("\r\nnosh: quit\r");
                        break;
                    }
                }
                backoff = (backoff * 2).min(BACKOFF_MAX);
            }
        }
    }

    // Terminal is restored by the _guard Drop here.
    std::process::exit(exit_code);
}

/// Run a fresh session (first connect or reconnect without a token).
/// Updates `highest_applied` and `token` in-place.
async fn fresh_session(
    conn: &quinn::Connection,
    term: String,
    cols: u16,
    rows: u16,
    highest_applied: &mut u64,
    resize: &mut platform::ResizeWatcher,
    token_out: &mut Option<[u8; 16]>,
) -> anyhow::Result<PumpOutcome> {
    let (mut send, mut recv, tok) =
        client::open_session_with_token(conn, term, cols, rows, client::collect_client_env())
            .await?;
    *token_out = Some(tok);
    // Fresh session starts at seq 0.
    *highest_applied = 0;

    run_pump(&mut send, &mut recv, highest_applied, resize, 0).await
}

/// Run a reattach session. Updates `highest_applied` and `token_out` in-place.
/// Returns `PumpOutcome::CleanExit(code)` if the session ended cleanly,
/// `PumpOutcome::TransportDrop` if the link dropped again, or
/// `PumpOutcome::UserQuit` if the user quit.
async fn reattach_session(
    conn: &quinn::Connection,
    token: [u8; 16],
    last_acked_seq: u64,
    highest_applied: &mut u64,
    resize: &mut platform::ResizeWatcher,
    token_out: &mut Option<[u8; 16]>,
) -> anyhow::Result<PumpOutcome> {
    let (mut send, mut recv) = conn.open_bi().await.context("open bi for reattach")?;
    client::send_reattach(&mut send, token, last_acked_seq).await?;

    match client::await_reattach_reply(&mut recv).await? {
        ReattachOutcome::Err => {
            // Terminal: the session is gone (D-11). Clear the token so we do
            // not try to reattach again — a new session would be started.
            *token_out = None;
            eprintln!("\r\nnosh: session ended\r");
            // Return CleanExit so the outer loop does not retry with TransportDrop.
            Ok(PumpOutcome::CleanExit(1))
        }
        ReattachOutcome::Ok {
            new_token,
            replaying_from_seq,
            truncated,
        } => {
            *token_out = Some(new_token);
            if truncated {
                eprintln!("\r\nnosh: output truncated\r");
            }
            // Rebase the applied-count to the server's first replayed seq
            // (next-expected-seq convention). `replaying_from_seq` is the seq
            // of the FIRST chunk the server is about to replay; after run_pump
            // applies that chunk it will increment highest_applied to
            // `replaying_from_seq + 1`, which is the correct next-expected.
            //
            // This MUST be `= replaying_from_seq`, NOT `replaying_from_seq - 1`:
            // the prior `- 1` was the compounding off-by-one that dropped one
            // chunk per reconnect cycle (ROAM-02 BLOCKER). On truncation,
            // `replaying_from_seq == lowest_retained_seq` so this resyncs the
            // baseline to exactly what the server is sending.
            *highest_applied = replaying_from_seq;
            run_pump(&mut send, &mut recv, highest_applied, resize, *highest_applied).await
        }
    }
}

/// Core pump loop: render output, forward input, debounce resize, send periodic
/// Ack. Returns the pump outcome.
async fn run_pump(
    send: &mut quinn::SendStream,
    recv: &mut quinn::RecvStream,
    highest_applied: &mut u64,
    resize: &mut platform::ResizeWatcher,
    _seq_baseline: u64,
) -> anyhow::Result<PumpOutcome> {
    let mut stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let mut stdin_buf = [0u8; 8 * 1024];
    let mut resize_deadline: Option<tokio::time::Instant> = None;
    let mut ack_interval = tokio::time::interval(ACK_INTERVAL);
    ack_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Track the last seq for which we sent an Ack to avoid redundant sends.
    let mut last_acked = *highest_applied;
    let exit_code;

    loop {
        let resize_sleep = async {
            match resize_deadline {
                Some(d) => tokio::time::sleep_until(d).await,
                None => std::future::pending::<()>().await,
            }
        };

        tokio::select! {
            // Server → client frames.
            msg = nosh_proto::read_message(recv) => {
                match msg {
                    Ok(Message::PtyData { data }) => {
                        stdout.write_all(&data).await?;
                        stdout.flush().await?;
                        // Count each PtyData chunk as one applied sequence unit.
                        *highest_applied = highest_applied.saturating_add(1);
                    }
                    Ok(Message::SessionClose { exit_code: code, .. }) => {
                        exit_code = code;
                        break;
                    }
                    Ok(Message::SessionOpened { .. }) => {
                        // SessionOpened was already consumed by open_session_with_token;
                        // if it arrives here it's unexpected — ignore.
                    }
                    Ok(_) => {} // ignore other control frames
                    Err(_) => {
                        return Ok(PumpOutcome::TransportDrop);
                    }
                }
            }
            // Keystrokes (incl. Ctrl-C as 0x03 — passed through, SESS-06).
            n = stdin.read(&mut stdin_buf) => {
                match n {
                    Ok(0) => return Ok(PumpOutcome::UserQuit),
                    Ok(n) => {
                        if client::send_input(send, &stdin_buf[..n]).await.is_err() {
                            return Ok(PumpOutcome::TransportDrop);
                        }
                    }
                    Err(_) => return Ok(PumpOutcome::UserQuit),
                }
            }
            // Terminal resize: SIGWINCH (Unix) or EventStream Event::Resize (Windows).
            // Platform abstraction via ResizeWatcher::next_resize().
            // The AUTHORITATIVE size is re-read via crossterm::terminal::size() in
            // the resize_sleep arm (Pitfall 14 — do not trust event fields directly).
            _ = resize.next_resize() => {
                resize_deadline = Some(tokio::time::Instant::now() + RESIZE_DEBOUNCE);
            }
            // Debounce elapsed: send one coalesced Resize.
            // Re-reads terminal::size() for authoritative dims (Pitfall 14).
            _ = resize_sleep => {
                resize_deadline = None;
                if let Ok((c, r)) = crossterm::terminal::size() {
                    let _ = client::send_resize(send, c, r).await;
                }
            }
            // Periodic Ack (D-08): send only when highest_applied advanced.
            _ = ack_interval.tick() => {
                if *highest_applied != last_acked {
                    if client::send_ack(send, *highest_applied).await.is_err() {
                        return Ok(PumpOutcome::TransportDrop);
                    }
                    last_acked = *highest_applied;
                }
            }
        }
    }

    let _ = send.finish();
    Ok(PumpOutcome::CleanExit(exit_code))
}
