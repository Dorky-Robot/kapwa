use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub from: String,
    pub timestamp: String,
    pub body: String,
}

fn inbox_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config/kapwa/inbox.json")
}

/// Append a message to the local inbox.
pub fn append(from: &str, body: &str) -> Result<()> {
    let path = inbox_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut messages = load_messages(&path);
    messages.push(Message {
        from: from.to_string(),
        timestamp: now_iso(),
        body: body.to_string(),
    });

    let json = serde_json::to_string_pretty(&messages)?;
    std::fs::write(&path, json).context("writing inbox")
}

/// Read all messages from the inbox.
pub fn read() -> Result<Vec<Message>> {
    let path = inbox_path();
    Ok(load_messages(&path))
}

/// Clear the inbox.
pub fn clear() -> Result<()> {
    let path = inbox_path();
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
    Ok(())
}

/// Format messages for display.
pub fn format_messages(messages: &[Message]) -> String {
    if messages.is_empty() {
        return "No messages.".to_string();
    }
    let mut out = String::new();
    for (i, msg) in messages.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(&format!(
            "[{}] from {}\n  {}\n",
            msg.timestamp, msg.from, msg.body
        ));
    }
    out
}

fn load_messages(path: &std::path::Path) -> Vec<Message> {
    if !path.exists() {
        return Vec::new();
    }
    std::fs::read_to_string(path)
        .ok()
        .and_then(|data| serde_json::from_str(&data).ok())
        .unwrap_or_default()
}

fn now_iso() -> String {
    // Simple UTC timestamp without chrono dependency
    let out = std::process::Command::new("date")
        .args(["-u", "+%Y-%m-%dT%H:%M:%SZ"])
        .output();
    match out {
        Ok(o) if o.status.success() => {
            String::from_utf8_lossy(&o.stdout).trim().to_string()
        }
        _ => "unknown".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_roundtrip() {
        let msg = Message {
            from: "mini".to_string(),
            timestamp: "2026-04-12T00:00:00Z".to_string(),
            body: "tunnels need restart".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.from, "mini");
        assert_eq!(parsed.body, "tunnels need restart");
    }

    #[test]
    fn format_empty() {
        assert_eq!(format_messages(&[]), "No messages.");
    }

    #[test]
    fn format_messages_shows_all() {
        let msgs = vec![
            Message {
                from: "mini".to_string(),
                timestamp: "2026-04-12T00:00:00Z".to_string(),
                body: "hello".to_string(),
            },
            Message {
                from: "mac2019".to_string(),
                timestamp: "2026-04-12T00:01:00Z".to_string(),
                body: "world".to_string(),
            },
        ];
        let out = format_messages(&msgs);
        assert!(out.contains("mini"));
        assert!(out.contains("mac2019"));
        assert!(out.contains("hello"));
        assert!(out.contains("world"));
    }
}
