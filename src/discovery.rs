use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::ssh;

/// A discovered peer in the mesh.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Peer {
    /// The kapwa identity name (e.g., "mini")
    pub name: String,
    /// The SSH config alias used to reach this peer
    pub ssh: String,
    /// When this peer was last seen
    pub last_seen: String,
}

/// Cached discovery results.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct Cache {
    peers: Vec<Peer>,
    last_scan: String,
}

fn cache_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config/kapwa/peers.json")
}

fn ssh_config_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".ssh/config")
}

/// Parse SSH config for Host entries, filtering out wildcards and
/// obvious non-machine aliases.
fn parse_ssh_hosts() -> Vec<String> {
    let path = ssh_config_path();
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    let mut hosts = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        // Match "Host <alias>" lines (case-insensitive)
        if let Some(rest) = trimmed.strip_prefix("Host ").or_else(|| trimmed.strip_prefix("host ")) {
            for alias in rest.split_whitespace() {
                // Skip wildcards and patterns
                if alias.contains('*') || alias.contains('?') || alias.contains('!') {
                    continue;
                }
                // Skip common non-machine hosts
                if is_non_machine(alias) {
                    continue;
                }
                hosts.push(alias.to_string());
            }
        }
    }

    hosts.sort();
    hosts.dedup();
    hosts
}

/// Heuristic: skip hosts that are obviously not personal machines.
fn is_non_machine(alias: &str) -> bool {
    let lower = alias.to_lowercase();
    // Common service hostnames
    lower.contains("github")
        || lower.contains("gitlab")
        || lower.contains("bitbucket")
        || lower.contains("heroku")
        || lower.contains("aws")
        || lower.contains("azure")
        || lower.contains("gcp")
        || lower == "localhost"
}

/// Probe a single SSH host to see if it has kapwa installed.
/// Returns the peer's identity name if kapwa responds.
fn probe(ssh_alias: &str, timeout_secs: u64) -> Option<String> {
    let peer = crate::config::SshPeer { ssh: ssh_alias.to_string() };
    let result = ssh::exec_raw(&peer.ssh, "kapwa identity --name-only 2>/dev/null || ~/.local/bin/kapwa identity --name-only 2>/dev/null", timeout_secs);
    match result {
        Ok(r) if r.success => {
            let name = r.stdout.trim().to_string();
            if !name.is_empty() && !name.contains("not found") {
                Some(name)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Discover all kapwa peers by scanning SSH config and probing.
/// Deduplicates by identity name (same machine reachable via multiple aliases).
pub fn scan(my_identity: &str, timeout_secs: u64) -> Vec<Peer> {
    let hosts = parse_ssh_hosts();
    let now = now_iso();
    let mut peers: Vec<Peer> = Vec::new();
    let mut seen_names: std::collections::HashSet<String> = std::collections::HashSet::new();

    for alias in &hosts {
        if let Some(name) = probe(alias, timeout_secs) {
            // Skip self
            if name == my_identity {
                continue;
            }
            // Dedup: keep first alias found for each identity
            if seen_names.contains(&name) {
                continue;
            }
            seen_names.insert(name.clone());
            peers.push(Peer {
                name,
                ssh: alias.clone(),
                last_seen: now.clone(),
            });
        }
    }

    // Save to cache
    let _ = save_cache(&peers, &now);

    peers
}

/// Load cached peers (for fast display without re-probing).
pub fn cached() -> Vec<Peer> {
    let path = cache_path();
    if !path.exists() {
        return Vec::new();
    }
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|data| serde_json::from_str::<Cache>(&data).ok())
        .map(|c| c.peers)
        .unwrap_or_default()
}

fn save_cache(peers: &[Peer], timestamp: &str) -> Result<()> {
    let path = cache_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let cache = Cache {
        peers: peers.to_vec(),
        last_scan: timestamp.to_string(),
    };
    let json = serde_json::to_string_pretty(&cache)?;
    std::fs::write(&path, json).context("writing peer cache")
}

/// Find a peer by name (checks cache first, then scans if not found).
pub fn find_peer(name: &str, my_identity: &str) -> Option<Peer> {
    // Check cache first
    if let Some(peer) = cached().into_iter().find(|p| p.name == name) {
        return Some(peer);
    }
    // Scan and try again
    scan(my_identity, 5)
        .into_iter()
        .find(|p| p.name == name)
}

fn now_iso() -> String {
    std::process::Command::new("date")
        .args(["-u", "+%Y-%m-%dT%H:%M:%SZ"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_machine_filtering() {
        assert!(is_non_machine("github.com"));
        assert!(is_non_machine("GitLab"));
        assert!(is_non_machine("localhost"));
        assert!(!is_non_machine("mini"));
        assert!(!is_non_machine("mac2019"));
        assert!(!is_non_machine("my-server"));
    }

    #[test]
    fn parse_ssh_hosts_handles_missing_file() {
        // If SSH config doesn't exist, should return empty
        // (can't easily test without mocking the path)
        let _ = parse_ssh_hosts();
    }

    #[test]
    fn cache_roundtrip() {
        let peers = vec![Peer {
            name: "mini".to_string(),
            ssh: "mini".to_string(),
            last_seen: "2026-04-12T00:00:00Z".to_string(),
        }];
        let json = serde_json::to_string_pretty(&Cache {
            peers: peers.clone(),
            last_scan: "2026-04-12T00:00:00Z".to_string(),
        }).unwrap();
        let parsed: Cache = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.peers.len(), 1);
        assert_eq!(parsed.peers[0].name, "mini");
    }
}
