//! Server-side PTY session: shell lookup, deny-by-default env sanitization, PTY
//! allocation + login-shell spawn, resize, exit-code capture, and SIGHUP+reap
//! teardown.
//!
//! The session is a discrete struct (decision D-08 / SESS-10) so that M3 cold
//! reattach is additive rather than a refactor — it carries the `session_id`,
//! the authenticated SSH identity, the PTY master handle, the child pid, and an
//! `idle_since` seam. Reattach itself is NOT implemented here.
//!
//! Security: env is built deny-by-default (FOOTGUN-1/2). Only an explicit
//! whitelist of client vars is ever applied; `LD_*`, `BASH_ENV`, `SSH_AUTH_SOCK`
//! and friends can never reach the shell because they are simply not copied.

use std::io::{Read, Write};
use std::time::Instant;

use anyhow::Context as _;
use nosh_auth::NoshPublicKey;
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use uuid::Uuid;

/// Exact client env keys that may pass through to the shell (SESS-07 / D-06).
/// Any key starting with `LC_` is also allowed (locale categories). EVERYTHING
/// else the client sends is dropped — this is a whitelist, not a blacklist, so
/// the deny list below documents attacker intent rather than gating behavior.
pub const ENV_WHITELIST_EXACT: &[&str] = &["TERM", "LANG", "TZ"];

/// Keys that must NEVER be set from client input. Documented for clarity and to
/// back the security tests; enforcement is structural (deny-by-default copy).
pub const ENV_DENY_DOC: &[&str] = &[
    "LD_*",
    "DYLD_*",
    "BASH_ENV",
    "ENV",
    "IFS",
    "SHELLOPTS",
    "PYTHONPATH",
    "NODE_OPTIONS",
    "SSH_AUTH_SOCK",
];

/// The login account the server runs as (single-account spike — no privilege
/// drop, D-03). Sourced from `/etc/passwd` for the effective uid.
#[derive(Debug, Clone)]
pub struct PasswdEntry {
    pub name: String,
    pub shell: String,
    pub home: String,
}

/// Look up the effective-uid account from the password database, optionally
/// overriding the shell (server `--shell` flag / tests). Falls back to the
/// process environment when the passwd lookup is unavailable.
pub fn lookup_self(shell_override: Option<&str>) -> PasswdEntry {
    let euid = nix::unistd::geteuid();
    let from_passwd = nix::unistd::User::from_uid(euid).ok().flatten();

    let (name, shell, home) = match from_passwd {
        Some(u) => (
            u.name,
            u.shell.to_string_lossy().into_owned(),
            u.dir.to_string_lossy().into_owned(),
        ),
        None => (
            std::env::var("USER").unwrap_or_else(|_| "nosh".to_string()),
            std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string()),
            std::env::var("HOME").unwrap_or_else(|_| "/".to_string()),
        ),
    };

    let shell = match shell_override {
        Some(s) if !s.is_empty() => s.to_string(),
        _ if shell.is_empty() => "/bin/sh".to_string(),
        _ => shell,
    };

    PasswdEntry { name, shell, home }
}

/// True if a client-supplied env key is allowed through the whitelist.
fn env_key_allowed(key: &str) -> bool {
    ENV_WHITELIST_EXACT.contains(&key) || key.starts_with("LC_")
}

/// Build the child shell environment deny-by-default (FOOTGUN-1/2, SESS-07):
/// start from a minimal server-owned baseline (`HOME`/`USER`/`LOGNAME`/`SHELL`/
/// `PATH`) and then copy ONLY whitelisted client vars. Returns ordered pairs.
pub fn build_child_env(
    passwd: &PasswdEntry,
    client_env: &[(String, String)],
) -> Vec<(String, String)> {
    let path = std::env::var("PATH")
        .ok()
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| "/usr/local/bin:/usr/bin:/bin".to_string());

    let mut env: Vec<(String, String)> = vec![
        ("HOME".to_string(), passwd.home.clone()),
        ("USER".to_string(), passwd.name.clone()),
        ("LOGNAME".to_string(), passwd.name.clone()),
        ("SHELL".to_string(), passwd.shell.clone()),
        ("PATH".to_string(), path),
    ];

    for (k, v) in client_env {
        if env_key_allowed(k) {
            env.push((k.clone(), v.clone()));
        }
    }
    env
}

/// A live PTY session. Owns the master PTY and the shell child. The child is
/// kept in an `Option` so it can be `take()`n for the blocking wait/reap path.
pub struct Session {
    pub session_id: Uuid,
    /// The authenticated SSH identity (Phase 2). `None` for this spike: the
    /// connection handler does not yet surface the peer cert key (noted M3 seam).
    pub identity: Option<NoshPublicKey>,
    /// The login account the shell runs as.
    pub username: String,
    master: Box<dyn MasterPty + Send>,
    child: Option<Box<dyn Child + Send + Sync>>,
    child_pid: Option<u32>,
    /// M3 reattach seam: set when the client disconnects but the session lingers
    /// (reattach NOT implemented; always `None` for now).
    pub idle_since: Option<Instant>,
}

impl Session {
    /// The shell's process id, if known.
    pub fn child_pid(&self) -> Option<u32> {
        self.child_pid
    }

    /// Resize the PTY (SESS-05).
    pub fn resize(&self, cols: u16, rows: u16) -> anyhow::Result<()> {
        self.master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("resize pty")
    }

    /// Take the child out of the session so its exit can be awaited on a
    /// blocking thread (the borrow checker forbids holding a `&mut self` across
    /// a `tokio::select!` loop that also calls `resize`). Returns `None` if the
    /// child was already taken.
    pub fn take_child(&mut self) -> Option<Box<dyn Child + Send + Sync>> {
        self.child.take()
    }

    /// SIGHUP the shell (best effort). Pair with [`reap_child`] to guarantee no
    /// zombie/orphan remains (SESS-10).
    pub fn sighup(&self) {
        if let Some(pid) = self.child_pid {
            let _ = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(pid as i32),
                nix::sys::signal::Signal::SIGHUP,
            );
        }
    }
}

/// Wait for a taken child to exit and return its exit code (SESS-08). Runs the
/// blocking `Child::wait` on a blocking thread so the async runtime is never
/// stalled, and reaps the child (no zombie — SESS-10).
pub async fn wait_child(mut child: Box<dyn Child + Send + Sync>) -> i32 {
    match tokio::task::spawn_blocking(move || child.wait()).await {
        Ok(Ok(status)) => status.exit_code() as i32,
        Ok(Err(e)) => {
            tracing::warn!("child wait failed: {e}");
            1
        }
        Err(e) => {
            tracing::warn!("child wait task join failed: {e}");
            1
        }
    }
}

/// Reap a taken child on a blocking thread (no zombie). Used on disconnect after
/// [`Session::sighup`].
pub async fn reap_child(mut child: Box<dyn Child + Send + Sync>) {
    let _ = tokio::task::spawn_blocking(move || child.wait()).await;
}

/// The master PTY's blocking reader handle.
pub type PtyReader = Box<dyn Read + Send>;
/// The master PTY's blocking writer handle.
pub type PtyWriter = Box<dyn Write + Send>;

/// Open a PTY and spawn the user's login shell with a sanitized environment.
///
/// Returns the [`Session`] plus the master PTY's blocking reader and writer
/// (bridged to async by the caller via `spawn_blocking`).
pub fn open(
    passwd: &PasswdEntry,
    term: &str,
    cols: u16,
    rows: u16,
    client_env: &[(String, String)],
    identity: Option<NoshPublicKey>,
) -> anyhow::Result<(Session, PtyReader, PtyWriter)> {
    let pty = native_pty_system();
    let pair = pty
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .context("openpty")?;

    // `new_default_prog()` makes portable-pty spawn the shell as a LOGIN shell
    // (argv[0] = "-<basename>", D-03), reading the shell path from the builder's
    // `SHELL` env var — which we set explicitly below. We never inherit the
    // server process env (env_clear), satisfying deny-by-default (D-06).
    let mut cmd = CommandBuilder::new_default_prog();
    cmd.env_clear();

    let mut env = build_child_env(passwd, client_env);
    // Force the chosen TERM even if the client did not send one in env.
    if !env.iter().any(|(k, _)| k == "TERM") {
        env.push(("TERM".to_string(), term.to_string()));
    } else {
        for (k, v) in env.iter_mut() {
            if k == "TERM" {
                *v = term.to_string();
            }
        }
    }
    // `SHELL` (set in build_child_env from passwd) drives get_shell(); ensure it
    // points at the resolved shell so the login-shell path uses it.
    for (k, v) in env.iter_mut() {
        if k == "SHELL" {
            *v = passwd.shell.clone();
        }
    }
    for (k, v) in &env {
        cmd.env(k, v);
    }
    cmd.cwd(&passwd.home);

    let child = pair.slave.spawn_command(cmd).context("spawn login shell")?;
    let child_pid = child.process_id();

    let reader = pair.master.try_clone_reader().context("clone pty reader")?;
    let writer = pair.master.take_writer().context("take pty writer")?;

    let session = Session {
        session_id: Uuid::new_v4(),
        identity,
        username: passwd.name.clone(),
        master: pair.master,
        child: Some(child),
        child_pid,
        idle_since: None,
    };
    Ok((session, reader, writer))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_child_env_is_deny_by_default() {
        let passwd = PasswdEntry {
            name: "alice".to_string(),
            shell: "/bin/sh".to_string(),
            home: "/home/alice".to_string(),
        };
        let client = vec![
            ("LD_PRELOAD".to_string(), "/evil.so".to_string()),
            ("BASH_ENV".to_string(), "/x".to_string()),
            ("SSH_AUTH_SOCK".to_string(), "/a".to_string()),
            ("IFS".to_string(), "x".to_string()),
            ("SHELLOPTS".to_string(), "y".to_string()),
            ("PYTHONPATH".to_string(), "/p".to_string()),
            ("NODE_OPTIONS".to_string(), "--x".to_string()),
            ("LC_ALL".to_string(), "C".to_string()),
            ("TZ".to_string(), "UTC".to_string()),
            ("TERM".to_string(), "xterm".to_string()),
        ];
        let env = build_child_env(&passwd, &client);
        let keys: Vec<&str> = env.iter().map(|(k, _)| k.as_str()).collect();

        // Baseline + whitelisted present.
        for present in [
            "HOME", "USER", "LOGNAME", "SHELL", "PATH", "TERM", "LC_ALL", "TZ",
        ] {
            assert!(keys.contains(&present), "{present} must be present");
        }
        // Dangerous client vars absent (security assertion, SESS-07).
        for absent in [
            "LD_PRELOAD",
            "BASH_ENV",
            "SSH_AUTH_SOCK",
            "IFS",
            "SHELLOPTS",
            "PYTHONPATH",
            "NODE_OPTIONS",
        ] {
            assert!(!keys.contains(&absent), "{absent} must NOT be present");
        }
    }
}
