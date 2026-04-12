use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Minimal config — just identity. Peers are discovered, not configured.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub identity: String,
}

/// Used internally by ssh.rs for SSH execution.
pub struct SshPeer {
    pub ssh: String,
}

impl Config {
    pub fn path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".config/kapwa/config.json")
    }

    pub fn load() -> Result<Self> {
        let path = Self::path();
        if !path.exists() {
            // Auto-create with hostname as identity
            let config = Self { identity: hostname() };
            config.save()?;
            return Ok(config);
        }
        let data = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        serde_json::from_str(&data).context("parsing kapwa config")
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, json)?;
        Ok(())
    }
}

fn hostname() -> String {
    std::process::Command::new("hostname")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_config() {
        let config = Config { identity: "test-machine".to_string() };
        let json = serde_json::to_string_pretty(&config).unwrap();
        let parsed: Config = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.identity, "test-machine");
    }

    #[test]
    fn minimal_json() {
        let json = r#"{"identity": "mini"}"#;
        let config: Config = serde_json::from_str(json).unwrap();
        assert_eq!(config.identity, "mini");
    }
}
