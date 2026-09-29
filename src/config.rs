//! Everything a node knows about itself comes from the environment, so the
//! same binary runs on every machine. In production the binary reads
//! `~/.config/kapwa/env` (mode 600) itself — no wrapper script, no launchd
//! EnvironmentVariables full of secrets.
//!
//!   KAPWA_WRITER              name of this node's log; the only log it appends to
//!   KAPWA_DIR                 logs live here (own under log/, pulled under mirror/)
//!   KAPWA_PORT                HTTP port, 127.0.0.1 only
//!   KAPWA_PEERS               comma-separated base URLs of nodes to pull from
//!   KAPWA_MESH_TOKEN          shared secret nodes present to each other to replicate
//!   KAPWA_AGENTS_FILE         name:token:role:products — agent keys for this node
//!   KAPWA_PRIVATE_TOPICS      topics that are on no default board; see `board::Lens`
//!   KAPWA_PUBLIC_URL          what the dashboard is reached as (OIDC redirect, cookie)
//!   KAPWA_SECRET_KEY_BASE     ≥64 random bytes, base64; encrypts the session cookie
//!   KAPWA_OIDC_ISSUER         the network's Pocket ID, e.g. https://id.felixflor.es
//!   KAPWA_OIDC_CLIENT_ID      from Pocket ID → settings → OIDC clients
//!   KAPWA_OIDC_CLIENT_SECRET

use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct Oidc {
    pub issuer: String,
    pub client_id: String,
    pub client_secret: String,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub writer: String,
    pub dir: PathBuf,
    pub port: u16,
    pub peers: Vec<String>,
    pub mesh_token: Option<String>,
    pub agents_file: PathBuf,
    /// Topic patterns. A topic that is one of these, or in its family
    /// (`x-…`), is fenced: it is on no agent's board unless their own key
    /// names it. People are not fenced. Empty by default, so a
    /// node that says nothing behaves exactly as it always did.
    pub private_topics: Vec<String>,
    pub public_url: String,
    pub secret_key_base: Option<String>,
    pub oidc: Option<Oidc>,
}

fn env(k: &str) -> Option<String> {
    std::env::var(k)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// `KEY=VALUE` lines; `#` comments; never overrides what the environment
/// already says, so a one-off `KAPWA_PORT=3411 kapwa` still wins.
pub fn load_env_file(path: &Path) {
    let Ok(body) = std::fs::read_to_string(path) else {
        return;
    };
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let (k, v) = (k.trim(), v.trim());
            if std::env::var_os(k).is_none() {
                std::env::set_var(k, v);
            }
        }
    }
}

impl Config {
    pub fn from_env() -> anyhow::Result<Config> {
        let port: u16 = env("KAPWA_PORT").unwrap_or_else(|| "3410".into()).parse()?;
        let hostname = std::process::Command::new("hostname")
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|h| !h.is_empty())
            .unwrap_or_else(|| "unknown".into());
        let user = env("USER").unwrap_or_else(|| "node".into());

        let oidc = match (
            env("KAPWA_OIDC_ISSUER"),
            env("KAPWA_OIDC_CLIENT_ID"),
            env("KAPWA_OIDC_CLIENT_SECRET"),
        ) {
            (Some(issuer), Some(client_id), Some(client_secret)) => Some(Oidc {
                issuer,
                client_id,
                client_secret,
            }),
            _ => None,
        };
        if let Some(o) = &oidc {
            if o.client_id == o.client_secret {
                anyhow::bail!("KAPWA_OIDC_CLIENT_SECRET is the same as the client id; paste the secret the identity provider showed when the client was created");
            }
        }
        let secret_key_base = env("KAPWA_SECRET_KEY_BASE");
        if oidc.is_some() && secret_key_base.is_none() {
            anyhow::bail!(
                "KAPWA_SECRET_KEY_BASE is required when the dashboard (KAPWA_OIDC_*) is configured"
            );
        }

        Ok(Config {
            writer: env("KAPWA_WRITER").unwrap_or_else(|| format!("{user}@{hostname}")),
            dir: env("KAPWA_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| home().join(".kapwa")),
            port,
            peers: env("KAPWA_PEERS")
                .map(|p| {
                    p.split(',')
                        .map(|s| s.trim().trim_end_matches('/').to_string())
                        .filter(|s| !s.is_empty())
                        .collect()
                })
                .unwrap_or_default(),
            mesh_token: env("KAPWA_MESH_TOKEN"),
            agents_file: env("KAPWA_AGENTS_FILE")
                .map(PathBuf::from)
                .unwrap_or_else(|| home().join(".config/kapwa/agents")),
            private_topics: env("KAPWA_PRIVATE_TOPICS")
                .map(|p| p.split(',').filter_map(crate::board::clean_topic).collect())
                .unwrap_or_default(),
            public_url: env("KAPWA_PUBLIC_URL")
                .unwrap_or_else(|| format!("http://127.0.0.1:{port}")),
            secret_key_base,
            oidc,
        })
    }
}
