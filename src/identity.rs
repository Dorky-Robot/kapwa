use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::process::Command;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Identity {
    pub name: String,
    pub hostname: String,
    pub arch: String,
    pub os_version: String,
    pub uptime: String,
    pub load: String,
    pub disk_free: String,
    pub tools: Vec<ToolVersion>,
    pub tunnels: Vec<TunnelState>,
    pub pending_updates: PendingUpdates,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolVersion {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TunnelState {
    pub name: String,
    pub status: String,
    pub pid: Option<String>,
    pub tunnel_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingUpdates {
    pub brew: Vec<String>,
    pub system: Vec<String>,
}

/// Gather the full identity of this machine.
pub fn gather(identity_name: &str) -> Identity {
    Identity {
        name: identity_name.to_string(),
        hostname: cmd_line("hostname"),
        arch: std::env::consts::ARCH.to_string(),
        os_version: os_version(),
        uptime: uptime(),
        load: load_avg(),
        disk_free: disk_free(),
        tools: installed_tools(),
        tunnels: tunnel_states(),
        pending_updates: PendingUpdates {
            brew: brew_outdated(),
            system: system_updates(),
        },
    }
}

/// Gather identity as pretty-printed JSON.
pub fn gather_json(identity_name: &str) -> Result<String> {
    let id = gather(identity_name);
    Ok(serde_json::to_string_pretty(&id)?)
}

fn cmd_line(cmd: &str) -> String {
    Command::new(cmd)
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

fn cmd(program: &str, args: &[&str]) -> String {
    Command::new(program)
        .args(args)
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

fn os_version() -> String {
    let version = cmd("sw_vers", &["-productVersion"]);
    let build = cmd("sw_vers", &["-buildVersion"]);
    if build.is_empty() {
        version
    } else {
        format!("macOS {} ({})", version, build)
    }
}

fn uptime() -> String {
    // Parse boot time from sysctl, compute duration
    let boottime = cmd("sysctl", &["-n", "kern.boottime"]);
    // Format: { sec = 1744123456, usec = 0 } ...
    if let Some(sec_str) = boottime.split("sec = ").nth(1) {
        if let Some(sec_str) = sec_str.split(',').next() {
            if let Ok(boot_secs) = sec_str.trim().parse::<u64>() {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                let up = now.saturating_sub(boot_secs);
                let days = up / 86400;
                let hours = (up % 86400) / 3600;
                let mins = (up % 3600) / 60;
                if days > 0 {
                    return format!("{}d {}h {}m", days, hours, mins);
                } else if hours > 0 {
                    return format!("{}h {}m", hours, mins);
                } else {
                    return format!("{}m", mins);
                }
            }
        }
    }
    cmd_line("uptime")
}

fn load_avg() -> String {
    let raw = cmd("sysctl", &["-n", "vm.loadavg"]);
    // Format: { 1.23 0.45 0.67 }
    raw.trim_matches(|c: char| c == '{' || c == '}' || c.is_whitespace())
        .split_whitespace()
        .next()
        .unwrap_or("?")
        .to_string()
}

fn disk_free() -> String {
    let raw = cmd("df", &["-h", "/"]);
    // Second line, 4th column is "Avail", 5th is "Capacity" (used%)
    raw.lines()
        .nth(1)
        .and_then(|line| {
            let cols: Vec<&str> = line.split_whitespace().collect();
            // cols: Filesystem Size Used Avail Capacity ...
            if cols.len() >= 5 {
                Some(format!("{} used, {} free", cols[4], cols[3]))
            } else {
                None
            }
        })
        .unwrap_or_default()
}

fn installed_tools() -> Vec<ToolVersion> {
    let mut tools = Vec::new();

    for (name, try_cmd) in [
        ("tunnels", "tunnels --version"),
        ("kapwa", "kapwa --version"),
        ("diwa", "diwa --version"),
    ] {
        let parts: Vec<&str> = try_cmd.split_whitespace().collect();
        if let Ok(out) = Command::new(parts[0]).args(&parts[1..]).output() {
            if out.status.success() {
                let version = String::from_utf8_lossy(&out.stdout)
                    .trim()
                    .strip_prefix(&format!("{} ", name))
                    .unwrap_or(String::from_utf8_lossy(&out.stdout).trim())
                    .to_string();
                tools.push(ToolVersion {
                    name: name.to_string(),
                    version,
                });
            }
        }
    }

    tools
}

fn tunnel_states() -> Vec<TunnelState> {
    // Try to get JSON output from tunnels
    let out = Command::new("sh")
        .args(["-c", "tunnels list --json 2>/dev/null || ~/.local/bin/tunnels list --json 2>/dev/null"])
        .output();

    match out {
        Ok(o) if o.status.success() => {
            let json = String::from_utf8_lossy(&o.stdout);
            serde_json::from_str(&json).unwrap_or_default()
        }
        _ => Vec::new(),
    }
}

fn brew_outdated() -> Vec<String> {
    let out = Command::new("sh")
        .args(["-c", "brew outdated 2>/dev/null"])
        .output();
    match out {
        Ok(o) if o.status.success() => {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter(|l| !l.is_empty())
                .map(|l| l.to_string())
                .collect()
        }
        _ => Vec::new(),
    }
}

fn system_updates() -> Vec<String> {
    // softwareupdate --list is slow (~10s), so we cache or skip in fast path
    // For now, return empty — this gets populated on explicit `kapwa identity --full`
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gather_returns_something() {
        let id = gather("test");
        assert_eq!(id.name, "test");
        assert!(!id.hostname.is_empty());
        assert!(!id.arch.is_empty());
    }

    #[test]
    fn gather_json_is_valid() {
        let json = gather_json("test").unwrap();
        let _: Identity = serde_json::from_str(&json).unwrap();
    }

    #[test]
    fn tunnel_state_deserializes() {
        let json = r#"[{"name":"default","status":"running","pid":"1234","tunnel_id":"abc-123"}]"#;
        let states: Vec<TunnelState> = serde_json::from_str(json).unwrap();
        assert_eq!(states.len(), 1);
        assert_eq!(states[0].name, "default");
    }
}
