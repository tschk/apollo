//! OpenClaw + Hermes plugin discovery (host-agnostic).

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HostPluginKind {
    OpenClawSkill,
    Hermes,
    ManifestJson,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostPluginEntry {
    pub id: String,
    pub kind: HostPluginKind,
    pub path: PathBuf,
    pub name: Option<String>,
    pub description: Option<String>,
    /// Executables the manifest declares. Discovering these does **not** make
    /// them runnable — see `plugin_layer.trusted_host_plugins`.
    #[serde(default)]
    pub tools: Vec<HostPluginTool>,
}

/// A command a host plugin manifest offers as an agent tool.
///
/// `command` is executed directly, never through a shell, and its JSON
/// arguments arrive on stdin rather than in argv, so a crafted argument cannot
/// become another flag or another command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostPluginTool {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
}

pub fn discover_host_plugins(workspace: &Path, extra_roots: &[PathBuf]) -> Vec<HostPluginEntry> {
    let mut roots = vec![workspace.to_path_buf()];
    roots.extend(extra_roots.iter().cloned());
    let mut out = Vec::new();
    for root in &roots {
        scan_plugins_dir(&root.join("plugins"), &mut out);
        scan_plugins_dir(&root.join(".openclaw/plugins"), &mut out);
        scan_plugins_dir(&root.join(".hermes/plugins"), &mut out);
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out.dedup_by(|a, b| a.path == b.path);
    out
}

fn scan_plugins_dir(dir: &Path, out: &mut Vec<HostPluginEntry>) {
    if !dir.is_dir() {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            let skill = p.join("SKILL.md");
            if skill.is_file() {
                out.push(entry_from_skill(&skill));
            }
            let hermes = p.join("plugin.json");
            if hermes.is_file() {
                if let Ok(e) = entry_from_hermes(&hermes) {
                    out.push(e);
                }
            }
        }
    }
    let skill_root = dir.join("SKILL.md");
    if skill_root.is_file() {
        out.push(entry_from_skill(&skill_root));
    }
}

fn entry_from_skill(path: &Path) -> HostPluginEntry {
    let (name, description) = parse_yaml_frontmatter(path);
    HostPluginEntry {
        id: format!(
            "openclaw:{}",
            name.clone().unwrap_or_else(|| path.display().to_string())
        ),
        kind: HostPluginKind::OpenClawSkill,
        path: path.to_path_buf(),
        name,
        description,
        // A SKILL.md is instructions, not an executable.
        tools: Vec::new(),
    }
}

fn parse_yaml_frontmatter(path: &Path) -> (Option<String>, Option<String>) {
    let Ok(body) = std::fs::read_to_string(path) else {
        return (None, None);
    };
    if !body.starts_with("---") {
        return (None, None);
    }
    let mut name = None;
    let mut desc = None;
    for line in body.lines().skip(1) {
        if line.trim() == "---" {
            break;
        }
        if let Some(v) = line.strip_prefix("name:") {
            name = Some(v.trim().to_string());
        }
        if let Some(v) = line.strip_prefix("description:") {
            desc = Some(v.trim().to_string());
        }
    }
    (name, desc)
}

#[derive(Debug, Deserialize)]
struct HermesFile {
    id: String,
    name: Option<String>,
    description: Option<String>,
    #[serde(default)]
    tools: Vec<HostPluginTool>,
}

fn entry_from_hermes(path: &Path) -> anyhow::Result<HostPluginEntry> {
    let raw = std::fs::read_to_string(path)?;
    let m: HermesFile = serde_json::from_str(&raw)?;
    Ok(HostPluginEntry {
        id: format!("hermes:{}", m.id),
        kind: HostPluginKind::Hermes,
        path: path.to_path_buf(),
        name: m.name,
        description: m.description,
        tools: m.tools,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn finds_openclaw_skill_in_workspace() {
        let dir = tempdir().unwrap();
        let plug = dir.path().join("plugins/demo");
        fs::create_dir_all(&plug).unwrap();
        fs::write(
            plug.join("SKILL.md"),
            "---\nname: demo\ndescription: test skill\n---\n",
        )
        .unwrap();
        let found = discover_host_plugins(dir.path(), &[]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].kind, HostPluginKind::OpenClawSkill);
    }

    #[test]
    fn finds_hermes_json() {
        let dir = tempdir().unwrap();
        let plug = dir.path().join("plugins/brain");
        fs::create_dir_all(&plug).unwrap();
        fs::write(
            plug.join("plugin.json"),
            r#"{"id":"brain","name":"brain","description":"x"}"#,
        )
        .unwrap();
        let found = discover_host_plugins(dir.path(), &[]);
        assert!(found.iter().any(|p| p.kind == HostPluginKind::Hermes));
    }
}
