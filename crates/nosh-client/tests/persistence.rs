//! Phase 5 persistence integration tests — PERSIST-01 success criteria.
//!
//! These tests drive an in-process server + client over the Phase 2/3 harness
//! and verify the three distinct session-end outcomes introduced in Phase 5:
//!
//! 1. `clean_session_close_does_not_orphan` — explicit SessionClose (shell
//!    exits normally): session is torn down immediately, no orphan lingers.
//! 2. `shell_exit_does_not_orphan` — shell exits on its own: torn down, no orphan.
//! 3. `transport_loss_orphans_without_sighup` — abrupt transport-level disconnect
//!    (not a SessionClose): session is ORPHANED, MasterPty stays open, shell is
//!    NOT SIGHUP'd (D-02 / Pitfall #7).

use std::sync::Arc;
use std::time::Duration;

use nosh_client::client;
use nosh_proto::Message;
use nosh_server::registry::SessionRegistry;

mod common;
use common::{spawn_server_with_registry, TestKey, HOST};

const SH: &str = "/bin/sh";

/// Returns true if `/bin/sh` exists (else tests skip).
fn have_sh() -> bool {
    std::path::Path::new(SH).exists()
}

/// Spawn a server with the given registry (so the test can assert orphan counts).
async fn server_with_registry(
    registry: Arc<SessionRegistry>,
) -> (quinn::Endpoint, common::TestServer) {
    let host_key = TestKey::generate();
    let client_key = TestKey::generate();
    let server = spawn_server_with_registry(
        &host_key,
        &[&client_key.public],
        nosh_server::server::AuthLimits::default(),
        Some(SH.to_string()),
        registry,
    )
    .await;

    let dir = tempfile::tempdir().unwrap();
    let kh = dir.path().join("known_hosts");
    // Keep `dir` alive by leaking it (the endpoint does not need the tempdir to
    // persist — the known_hosts file is only needed for TOFU on first connect).
    // Use Box::leak to extend its lifetime for the test duration.
    let _ = Box::leak(Box::new(dir));
    let endpoint = common::client_endpoint(client_key.client_identity(), kh).unwrap();
    (endpoint, server)
}

/// PERSIST-01 criterion 2: a normally-exiting shell does NOT leave an orphan.
#[tokio::test]
async fn shell_exit_does_not_orphan() {
    if !have_sh() {
        eprintln!("skipping shell_exit_does_not_orphan: {SH} not available");
        return;
    }

    let registry = SessionRegistry::new(5, Duration::ZERO);
    let (endpoint, server) = server_with_registry(registry.clone()).await;

    let conn = client::connect(&endpoint, server.addr, HOST)
        .await
        .expect("connect");

    // Run a script that exits immediately.
    let _ = tokio::time::timeout(
        Duration::from_secs(15),
        client::run_session_collect(&conn, "xterm", 80, 24, vec![], b"exit 0\n"),
    )
    .await
    .expect("session did not hang")
    .expect("session ran");

    // Allow the server some time to process the teardown.
    tokio::time::sleep(Duration::from_millis(300)).await;

    assert_eq!(
        server.registry.total_orphans(),
        0,
        "shell exit must NOT leave an orphan (PERSIST-01 criterion 2)"
    );
}

/// PERSIST-01 criterion 1: an explicit client SessionClose does NOT orphan.
///
/// We let the shell run `exit 0` which causes the server to send a SessionClose
/// first (ShellExited path), completing the clean close.
#[tokio::test]
async fn clean_session_close_does_not_orphan() {
    if !have_sh() {
        eprintln!("skipping clean_session_close_does_not_orphan: {SH} not available");
        return;
    }

    let registry = SessionRegistry::new(5, Duration::ZERO);
    let (endpoint, server) = server_with_registry(registry.clone()).await;

    let conn = client::connect(&endpoint, server.addr, HOST)
        .await
        .expect("connect");

    // Run a quick script to get a clean close.
    let _ = tokio::time::timeout(
        Duration::from_secs(15),
        client::run_session_collect(&conn, "xterm", 80, 24, vec![], b"exit 0\n"),
    )
    .await
    .expect("session did not hang")
    .expect("session ran");

    tokio::time::sleep(Duration::from_millis(300)).await;

    assert_eq!(
        server.registry.total_orphans(),
        0,
        "clean client close must NOT leave an orphan (PERSIST-01 criterion 1)"
    );
}

/// PERSIST-01 criterion 3 (CORE): abrupt transport-level disconnect ORPHANS the
/// session without SIGHUP'ing the shell (D-02 / Pitfall #7).
///
/// The test:
/// 1. Starts a shell that installs a HUP trap and runs a long sleep.
/// 2. Abruptly closes the QUIC connection WITHOUT sending SessionClose.
/// 3. Polls until `total_orphans() == 1`.
/// 4. Asserts the shell is still running (try_wait returns None on the orphaned
///    slot would be nice but we verify via /proc here).
/// 5. Asserts no HUP-trap file was written (the shell was NOT SIGHUP'd).
#[tokio::test]
async fn transport_loss_orphans_without_sighup() {
    if !have_sh() {
        eprintln!("skipping transport_loss_orphans_without_sighup: {SH} not available");
        return;
    }

    let registry = SessionRegistry::new(5, Duration::ZERO);
    let (endpoint, server) = server_with_registry(registry.clone()).await;

    let conn = client::connect(&endpoint, server.addr, HOST)
        .await
        .expect("connect");

    // Use a unique temp file path for the HUP indicator.
    let hup_file = format!("/tmp/nosh_hup_test_{}", std::process::id());
    let _ = std::fs::remove_file(&hup_file); // clean up any stale file

    // Script: install a HUP trap that writes to a file, print our own PID so the
    // test can probe shell liveness via /proc after disconnect, then sleep.
    let script = format!(
        "trap 'echo GOTHUP > {hup_file}' HUP; echo PID=$$; echo READY; sleep 60\n"
    );

    let (mut send, mut recv) = client::open_session(
        &conn,
        "xterm".to_string(),
        80,
        24,
        vec![],
    )
    .await
    .unwrap();

    client::send_input(&mut send, script.as_bytes())
        .await
        .unwrap();

    // Parse the shell's executed PID (a bare `PID=<digits>` line, distinct from
    // the echoed `PID=$$` command) so we can probe shell liveness via /proc, and
    // wait until we see READY (also from execution) so the HUP trap is installed.
    // The PTY echoes typed input back, so we must skip the echoed script line and
    // read until the shell's actual output appears.
    // Match a bare numeric `PID=<digits>` (the executed output). The echoed
    // command line contains `PID=$$` whose first char after `=` is `$`, which
    // fails the digit parse, so only the real PID is captured. A shell prompt
    // (`$ `) may prefix the line, so we scan all `PID=` occurrences.
    let parse_pid = |s: &str| -> Option<u32> {
        s.match_indices("PID=").find_map(|(i, _)| {
            let rest = &s[i + 4..];
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            digits.parse::<u32>().ok()
        })
    };
    let mut buf = Vec::new();
    let ready_deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        match tokio::time::timeout(Duration::from_secs(5), nosh_proto::read_message(&mut recv))
            .await
        {
            Ok(Ok(Message::PtyData { data })) => {
                buf.extend_from_slice(&data);
                let out = String::from_utf8_lossy(&buf);
                // Require BOTH the executed PID line AND READY (post-execution).
                if out.contains("READY") && parse_pid(&out).is_some() {
                    break;
                }
            }
            Ok(Ok(_)) | Err(_) => {}
            Ok(Err(_)) => break,
        }
        if std::time::Instant::now() > ready_deadline {
            panic!(
                "shell did not print PID + READY within 10s; buffer so far: {:?}",
                String::from_utf8_lossy(&buf)
            );
        }
    }

    let out = String::from_utf8_lossy(&buf).into_owned();
    let shell_pid = parse_pid(&out);
    let proc_alive = |pid: u32| std::path::Path::new(&format!("/proc/{pid}")).exists();

    // Abruptly drop the QUIC connection WITHOUT sending SessionClose.
    // From the server's perspective this is a transport-level loss → orphan.
    conn.close(1u32.into(), b"simulated transport loss");
    drop(conn);
    endpoint.close(0u32.into(), b"done");
    drop(endpoint);

    // Poll until the registry shows exactly 1 orphan.
    let orphan_deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if server.registry.total_orphans() == 1 {
            break;
        }
        if std::time::Instant::now() > orphan_deadline {
            panic!(
                "server did not register 1 orphan within 5s; total_orphans={}",
                server.registry.total_orphans()
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // PRIMARY assertion: the session was orphaned (not torn down).
    assert_eq!(
        server.registry.total_orphans(),
        1,
        "transport loss must orphan exactly 1 session"
    );

    // SECONDARY assertion: the shell was NOT SIGHUP'd (best-effort; may be
    // flaky in highly loaded CI). Allow a short settle window.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let hup_received = std::path::Path::new(&hup_file).exists();
    assert!(
        !hup_received,
        "shell must NOT have received SIGHUP on transport loss (Pitfall #7); \
         HUP indicator file exists: {hup_file}"
    );

    // TERTIARY assertion: the orphaned shell process is STILL ALIVE after the
    // transport loss (SC#1: MasterPty kept open, shell not killed). Probe /proc
    // directly using the PID the shell printed.
    if let Some(pid) = shell_pid {
        assert!(
            proc_alive(pid),
            "orphaned shell (pid {pid}) must still be running after transport loss (SC#1)"
        );
    } else {
        panic!("could not parse shell PID from session output: {out:?}");
    }

    // Cleanup: clean up the HUP file if somehow present.
    let _ = std::fs::remove_file(&hup_file);
}
