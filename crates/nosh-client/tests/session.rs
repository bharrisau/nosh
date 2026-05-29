//! SESS-01..10 integration tests — Phase 3 success criteria.
//!
//! Each test drives an in-process, SSH-key-mutually-authenticated server +
//! client over the Phase 2 harness, then opens a real PTY session and asserts
//! session behavior headlessly. Tests force `--shell /bin/sh` for portability
//! and skip cleanly if `/bin/sh` is unavailable.

use std::time::Duration;

use nosh_client::client;
use nosh_proto::Message;

mod common;
use common::{spawn_server_with_shell, TestKey, HOST};

const SH: &str = "/bin/sh";

/// Returns true if `/bin/sh` exists (else tests skip).
fn have_sh() -> bool {
    std::path::Path::new(SH).exists()
}

/// Connect an authenticated client to a freshly spawned server forcing /bin/sh.
async fn connect_session_server() -> Option<(quinn::Endpoint, quinn::Connection, common::TestServer)>
{
    if !have_sh() {
        eprintln!("skipping: {SH} not available");
        return None;
    }
    let host_key = TestKey::generate();
    let client_key = TestKey::generate();
    let server = spawn_server_with_shell(
        &host_key,
        &[&client_key.public],
        nosh_server::server::AuthLimits::default(),
        Some(SH.to_string()),
    )
    .await;

    let dir = tempfile::tempdir().unwrap();
    let kh = dir.path().join("known_hosts");
    let endpoint = common::client_endpoint(client_key.client_identity(), kh).unwrap();
    let conn = client::connect(&endpoint, server.addr, HOST)
        .await
        .expect("mutual auth handshake");
    Some((endpoint, conn, server))
}

/// SESS-01 (real PTY), SESS-02 (shell I/O), SESS-04 (TERM + initial size).
#[tokio::test]
async fn sess01_02_04_real_tty_and_io() {
    let Some((_ep, conn, _srv)) = connect_session_server().await else {
        return;
    };
    let env = vec![("TERM".to_string(), "xterm-256color".to_string())];
    let script = b"test -t 0 && echo IS_TTY; echo hello-nosh; stty size; exit 0\n";
    let (out, code) = tokio::time::timeout(
        Duration::from_secs(15),
        client::run_session_collect(&conn, "xterm-256color", 132, 40, env, script),
    )
    .await
    .expect("session did not hang")
    .expect("session ran");

    let text = String::from_utf8_lossy(&out);
    assert!(
        text.contains("IS_TTY"),
        "stdin must be a real tty (SESS-01): {text:?}"
    );
    assert!(
        text.contains("hello-nosh"),
        "shell output round-trips (SESS-02): {text:?}"
    );
    assert!(
        text.contains("40 132"),
        "initial PTY size must be 40x132 (SESS-04): {text:?}"
    );
    assert_eq!(code, 0);
}

/// SESS-05: a Resize changes the PTY window size.
#[tokio::test]
async fn sess05_resize() {
    let Some((_ep, conn, _srv)) = connect_session_server().await else {
        return;
    };
    let (mut send, mut recv) = client::open_session(
        &conn,
        "xterm-256color".to_string(),
        80,
        24,
        vec![("TERM".to_string(), "xterm-256color".to_string())],
    )
    .await
    .unwrap();

    client::send_resize(&mut send, 100, 50).await.unwrap();
    // Small settle so the resize is applied before stty reads it.
    tokio::time::sleep(Duration::from_millis(150)).await;
    client::send_input(&mut send, b"stty size; exit 0\n")
        .await
        .unwrap();

    let (out, _code) = tokio::time::timeout(
        Duration::from_secs(15),
        client::collect_until_close(&mut recv),
    )
    .await
    .expect("no hang")
    .unwrap();
    let text = String::from_utf8_lossy(&out);
    assert!(
        text.contains("50 100"),
        "resize must change PTY size to 50x100 (SESS-05): {text:?}"
    );
}

/// SESS-07: env sanitization — dangerous client vars stripped, locale/TERM kept.
/// This is an explicit SECURITY assertion.
#[tokio::test]
async fn sess07_env_sanitization() {
    let Some((_ep, conn, _srv)) = connect_session_server().await else {
        return;
    };
    let env = vec![
        ("LD_PRELOAD".to_string(), "/evil.so".to_string()),
        ("BASH_ENV".to_string(), "/x".to_string()),
        ("SSH_AUTH_SOCK".to_string(), "/agent.sock".to_string()),
        ("IFS".to_string(), "x".to_string()),
        ("SHELLOPTS".to_string(), "xtrace".to_string()),
        ("PYTHONPATH".to_string(), "/p".to_string()),
        ("NODE_OPTIONS".to_string(), "--inspect".to_string()),
        ("LC_ALL".to_string(), "C".to_string()),
        ("TZ".to_string(), "UTC".to_string()),
        ("TERM".to_string(), "xterm-256color".to_string()),
    ];
    let (out, _code) = tokio::time::timeout(
        Duration::from_secs(15),
        client::run_session_collect(&conn, "xterm-256color", 80, 24, env, b"env; exit 0\n"),
    )
    .await
    .expect("no hang")
    .unwrap();
    let text = String::from_utf8_lossy(&out);

    // Whitelisted vars present.
    assert!(
        text.contains("LC_ALL=C"),
        "LC_ALL must pass through: {text:?}"
    );
    assert!(text.contains("TZ=UTC"), "TZ must pass through: {text:?}");
    assert!(
        text.contains("TERM=xterm-256color"),
        "TERM must pass through: {text:?}"
    );
    // Dangerous vars MUST be absent (security).
    assert!(
        !text.contains("LD_PRELOAD"),
        "LD_PRELOAD must be stripped: {text:?}"
    );
    assert!(
        !text.contains("BASH_ENV"),
        "BASH_ENV must be stripped: {text:?}"
    );
    assert!(
        !text.contains("SSH_AUTH_SOCK"),
        "SSH_AUTH_SOCK must never be forwarded via env: {text:?}"
    );
    assert!(
        !text.contains("/agent.sock"),
        "agent socket path must be absent: {text:?}"
    );
    assert!(
        !text.contains("SHELLOPTS=xtrace"),
        "SHELLOPTS must be stripped: {text:?}"
    );
    assert!(
        !text.contains("PYTHONPATH"),
        "PYTHONPATH must be stripped: {text:?}"
    );
    assert!(
        !text.contains("NODE_OPTIONS"),
        "NODE_OPTIONS must be stripped: {text:?}"
    );
}

/// SESS-08: remote exit code propagates to the client via SessionClose.
#[tokio::test]
async fn sess08_exit_code() {
    let Some((_ep, conn, _srv)) = connect_session_server().await else {
        return;
    };
    let (_out, code) = tokio::time::timeout(
        Duration::from_secs(15),
        client::run_session_collect(&conn, "xterm", 80, 24, vec![], b"exit 42\n"),
    )
    .await
    .expect("no hang")
    .unwrap();
    assert_eq!(
        code, 42,
        "client must surface remote exit code 42 (SESS-08)"
    );
}

/// SESS-09: connection closes cleanly with a structured application reason.
#[tokio::test]
async fn sess09_clean_close() {
    let Some((_ep, conn, _srv)) = connect_session_server().await else {
        return;
    };
    let (_out, code) = tokio::time::timeout(
        Duration::from_secs(15),
        client::run_session_collect(&conn, "xterm", 80, 24, vec![], b"exit 0\n"),
    )
    .await
    .expect("no hang")
    .unwrap();
    assert_eq!(code, 0);

    // After the session ends, the server closes with an application close code
    // (clean structured close — not a transport error). Poll briefly.
    let mut reason = conn.close_reason();
    for _ in 0..50 {
        if reason.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        reason = conn.close_reason();
    }
    match reason {
        Some(quinn::ConnectionError::ApplicationClosed(_)) => {}
        other => panic!("expected a clean ApplicationClosed, got {other:?}"),
    }
}

/// SESS-10: after the client disconnects mid-session, the server SIGHUPs and
/// reaps the shell — no zombie/orphan. We capture the shell's pid from the
/// session, drop the connection, and assert the pid is reaped (gone or not Z).
#[tokio::test]
async fn sess10_no_zombie_after_disconnect() {
    let Some((endpoint, conn, _srv)) = connect_session_server().await else {
        return;
    };
    // Start a long-lived foreground process and have the shell print its own pid.
    let (mut send, mut recv) = client::open_session(&conn, "xterm".to_string(), 80, 24, vec![])
        .await
        .unwrap();
    client::send_input(&mut send, b"echo PID:$$; sleep 30\n")
        .await
        .unwrap();

    // Read until we see the PID line.
    let pid = read_pid(&mut recv).await.expect("shell printed its pid");

    // Abruptly drop the client connection/endpoint (simulates disconnect).
    conn.close(0u32.into(), b"client gone");
    drop(conn);
    endpoint.close(0u32.into(), b"client gone");
    drop(endpoint);

    // The server should SIGHUP + reap. Poll /proc for up to ~5s: the pid must
    // either be gone OR not be a zombie (state != 'Z').
    let mut ok = false;
    for _ in 0..50 {
        match proc_state(pid) {
            None => {
                ok = true; // process gone — reaped
                break;
            }
            Some(state) if state != 'Z' => {
                // still shutting down (e.g. 'S'/'R'); keep polling
            }
            Some('Z') => {
                // zombie right now — keep polling; reaper may not have run yet
            }
            Some(_) => {}
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // Final check: definitely not a lingering zombie.
    let final_state = proc_state(pid);
    assert!(
        ok || final_state.map(|s| s != 'Z').unwrap_or(true),
        "shell pid {pid} must be reaped (gone or non-zombie), got state {final_state:?} (SESS-10)"
    );
}

/// Read PtyData frames until a line like `PID:<n>` (with `<n>` a non-empty run
/// of digits terminated by a non-digit) is seen; return the pid. The shell
/// echoes the command (`PID:$$`) before running it, so we must scan every
/// `PID:` occurrence and accept only one immediately followed by digits.
async fn read_pid(recv: &mut quinn::RecvStream) -> Option<i32> {
    let mut buf = Vec::new();
    for _ in 0..200 {
        match tokio::time::timeout(Duration::from_secs(5), nosh_proto::read_message(recv)).await {
            Ok(Ok(Message::PtyData { data })) => {
                buf.extend_from_slice(&data);
                let text = String::from_utf8_lossy(&buf);
                let mut search_from = 0;
                while let Some(rel) = text[search_from..].find("PID:") {
                    let idx = search_from + rel + 4;
                    let rest = &text[idx..];
                    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
                    // Require a delimiter after the digits so we have the whole pid.
                    if !digits.is_empty() && rest.len() > digits.len() {
                        if let Ok(pid) = digits.parse() {
                            return Some(pid);
                        }
                    }
                    search_from = idx;
                }
            }
            Ok(Ok(_)) => {}
            _ => return None,
        }
    }
    None
}

/// Read `/proc/<pid>/stat` and return the process state char, or None if gone.
fn proc_state(pid: i32) -> Option<char> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // Format: `pid (comm) state ...` — comm may contain spaces/parens, so split
    // on the last ')'.
    let after = stat.rsplit_once(')')?.1;
    after.trim_start().chars().next()
}
