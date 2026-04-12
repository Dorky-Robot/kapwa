use anyhow::{Context, Result};
use std::process::Command;

use crate::discovery::Peer;

#[derive(Debug, Clone)]
pub struct SshResult {
    pub stdout: String,
    pub stderr: String,
    pub success: bool,
}

/// Execute a command on a remote peer via SSH.
pub fn exec(peer: &Peer, command: &str, timeout_secs: u64) -> Result<SshResult> {
    exec_raw(&peer.ssh, command, timeout_secs)
}

/// Execute a command via SSH using a raw alias string.
/// Used by both peer-based exec and discovery probing.
pub fn exec_raw(ssh_alias: &str, command: &str, timeout_secs: u64) -> Result<SshResult> {
    let output = Command::new("ssh")
        .args([
            "-o", &format!("ConnectTimeout={}", timeout_secs),
            "-o", "BatchMode=yes",
            "-o", "StrictHostKeyChecking=accept-new",
            ssh_alias,
            command,
        ])
        .output()
        .with_context(|| format!("ssh to {}", ssh_alias))?;

    Ok(SshResult {
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        success: output.status.success(),
    })
}

/// Check if a peer is reachable via SSH.
pub fn ping(peer: &Peer, timeout_secs: u64) -> bool {
    exec(peer, "echo ok", timeout_secs)
        .map(|r| r.success)
        .unwrap_or(false)
}

/// Execute a `kapwa` subcommand on a remote peer.
pub fn kapwa_cmd(peer: &Peer, args: &str, timeout_secs: u64) -> Result<SshResult> {
    let command = format!(
        "if command -v kapwa >/dev/null 2>&1; then kapwa {args}; \
         elif [ -x ~/.local/bin/kapwa ]; then ~/.local/bin/kapwa {args}; \
         else echo 'kapwa not found' >&2; exit 1; fi"
    );
    exec(peer, &command, timeout_secs)
}

/// Execute a `tunnels` subcommand on a remote peer.
pub fn tunnels_cmd(peer: &Peer, args: &str, timeout_secs: u64) -> Result<SshResult> {
    let command = format!(
        "if command -v tunnels >/dev/null 2>&1; then tunnels {args}; \
         elif [ -x ~/.local/bin/tunnels ]; then ~/.local/bin/tunnels {args}; \
         else echo 'tunnels not found' >&2; exit 1; fi"
    );
    exec(peer, &command, timeout_secs)
}
