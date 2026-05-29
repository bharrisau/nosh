//! Shared test helpers (used by `nosh-auth` unit tests and the `nosh-client`
//! integration tests). Not part of the public API surface — kept `pub` only so
//! the cross-crate integration tests can reuse the ephemeral-agent harness.
//!
//! The marquee helper is [`EphemeralAgent`]: it spawns a throwaway `ssh-agent`,
//! generates an Ed25519 key with `ssh-keygen`, and `ssh-add`s it — the live
//! signing path for AUTH-04. When `ssh-agent`/`ssh-keygen` are unavailable it
//! returns `None` so callers can skip cleanly and defer to human verification.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A running ephemeral `ssh-agent` with a single Ed25519 identity added.
/// Killed on drop.
pub struct EphemeralAgent {
    _dir: tempfile::TempDir,
    socket: PathBuf,
    agent_pid: u32,
    public_key: ssh_key::PublicKey,
}

impl EphemeralAgent {
    /// Start an ephemeral agent and add a fresh Ed25519 key. Returns `None` if
    /// `ssh-agent` or `ssh-keygen` are not on `PATH` (caller should skip).
    pub fn start() -> Option<Self> {
        if !tool_exists("ssh-agent") || !tool_exists("ssh-keygen") || !tool_exists("ssh-add") {
            return None;
        }
        let dir = tempfile::tempdir().ok()?;
        let socket = dir.path().join("agent.sock");
        let key_path = dir.path().join("id_ed25519");

        // Generate a throwaway Ed25519 key.
        let kg = Command::new("ssh-keygen")
            .args(["-t", "ed25519", "-N", "", "-q", "-f"])
            .arg(&key_path)
            .status()
            .ok()?;
        if !kg.success() {
            return None;
        }

        // Start the agent bound to our socket.
        let out = Command::new("ssh-agent")
            .arg("-a")
            .arg(&socket)
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let agent_pid = parse_agent_pid(&String::from_utf8_lossy(&out.stdout))?;

        // Add the key to the agent.
        let add = Command::new("ssh-add")
            .arg(&key_path)
            .env("SSH_AUTH_SOCK", &socket)
            .status()
            .ok()?;
        if !add.success() {
            kill_pid(agent_pid);
            return None;
        }

        let public_key =
            ssh_key::PublicKey::read_openssh_file(&key_path.with_extension("pub")).ok()?;

        Some(Self {
            _dir: dir,
            socket,
            agent_pid,
            public_key,
        })
    }

    /// The agent's Unix socket path (`SSH_AUTH_SOCK`).
    pub fn socket_path(&self) -> PathBuf {
        self.socket.clone()
    }

    /// The added identity's public key.
    pub fn public_key(&self) -> ssh_key::PublicKey {
        self.public_key.clone()
    }
}

impl Drop for EphemeralAgent {
    fn drop(&mut self) {
        kill_pid(self.agent_pid);
    }
}

fn tool_exists(name: &str) -> bool {
    Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {name}"))
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn parse_agent_pid(stdout: &str) -> Option<u32> {
    // ssh-agent prints: `SSH_AGENT_PID=12345; export SSH_AGENT_PID;`
    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix("SSH_AGENT_PID=") {
            let pid: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            return pid.parse().ok();
        }
    }
    None
}

fn kill_pid(pid: u32) {
    let _ = Command::new("kill").arg(pid.to_string()).status();
}

/// Write an OpenSSH-format public key line to `path` (an `authorized_keys` file).
pub fn write_authorized_keys(path: &Path, key: &ssh_key::PublicKey) {
    std::fs::write(path, key.to_openssh().expect("encode pubkey")).expect("write authorized_keys");
}
