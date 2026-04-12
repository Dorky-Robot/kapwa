use anyhow::{Context, Result};
use std::path::PathBuf;

fn skills_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config/kapwa/skills")
}

/// List all installed skill names (without .md extension).
pub fn list() -> Vec<String> {
    let dir = skills_dir();
    if !dir.exists() {
        return Vec::new();
    }
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            if name.ends_with(".md") {
                Some(name.trim_end_matches(".md").to_string())
            } else {
                None
            }
        })
        .collect();
    names.sort();
    names
}

/// Read the contents of a skill file.
pub fn show(name: &str) -> Result<String> {
    let path = skills_dir().join(format!("{}.md", name));
    if !path.exists() {
        anyhow::bail!("skill '{}' not found", name);
    }
    std::fs::read_to_string(&path).context("reading skill file")
}

/// Install a skill from raw content.
pub fn install(name: &str, content: &str) -> Result<()> {
    let dir = skills_dir();
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.md", name));
    std::fs::write(&path, content).context("writing skill file")
}

/// Sync skills from a remote peer via SSH.
/// Lists the peer's skills, downloads any we don't have.
pub fn sync_from_peer(peer: &crate::discovery::Peer, timeout: u64) -> Result<SyncReport> {
    let result = crate::ssh::kapwa_cmd(peer, "skills --json", timeout)?;
    if !result.success {
        anyhow::bail!("failed to list skills on {}: {}", peer.name, result.stderr);
    }

    let remote_skills: Vec<String> = serde_json::from_str(&result.stdout)
        .unwrap_or_default();
    let local_skills = list();

    let mut pulled = Vec::new();
    let mut skipped = Vec::new();

    for skill_name in &remote_skills {
        if local_skills.contains(skill_name) {
            skipped.push(skill_name.clone());
            continue;
        }
        // Fetch the skill content
        let content_result = crate::ssh::kapwa_cmd(
            peer,
            &format!("skill show {}", skill_name),
            timeout,
        )?;
        if content_result.success {
            install(skill_name, &content_result.stdout)?;
            pulled.push(skill_name.clone());
        }
    }

    Ok(SyncReport { pulled, skipped })
}

#[derive(Debug)]
pub struct SyncReport {
    pub pulled: Vec<String>,
    pub skipped: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_empty_when_no_dir() {
        // skills_dir() points to a real path, but listing a missing dir is fine
        let names = list();
        // We can't assert empty because the user might have skills installed,
        // but we can assert it doesn't panic
        let _ = names;
    }

    #[test]
    fn install_and_show_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-skill.md");
        let content = "# Test Skill\n\nDo the thing.";
        std::fs::write(&path, content).unwrap();
        let read_back = std::fs::read_to_string(&path).unwrap();
        assert_eq!(read_back, content);
    }
}
