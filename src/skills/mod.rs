//! Skills system — scan SKILL.md files, match against user requests,
//! inject matched skill instructions into the system prompt.
//! Supports template variables and inline shell execution.

pub mod curator;

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub location: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagedSkillInput {
    pub name: String,
    pub description: String,
    pub body: String,
}

/// Scan known skill directories for SKILL.md files
pub fn discover_skills() -> Vec<Skill> {
    discover_skills_for_workspace(None)
}

pub fn discover_skills_for_workspace(workspace: Option<&Path>) -> Vec<Skill> {
    let mut skills = Vec::new();
    let home = dirs::home_dir().unwrap_or_default();

    let openclaw_skills = home.join(".npm-global/lib/node_modules/openclaw/skills");
    scan_skill_dir(&openclaw_skills, &mut skills);

    let workspace_skills = home.join(".openclaw/workspace/skills");
    scan_skill_dir(&workspace_skills, &mut skills);

    if let Some(workspace) = workspace {
        let managed = managed_skills_dir(workspace);
        scan_skill_dir(&managed, &mut skills);
    }

    skills
}

pub fn managed_skills_dir(workspace: &Path) -> PathBuf {
    workspace.join(".apollo/skills")
}

fn scan_skill_dir(dir: &Path, skills: &mut Vec<Skill>) {
    if !dir.is_dir() {
        return;
    }

    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let skill_dir = entry.path();
            if !skill_dir.is_dir() {
                continue;
            }

            let skill_md = skill_dir.join("SKILL.md");
            if !skill_md.exists() {
                continue;
            }

            if let Ok(content) = std::fs::read_to_string(&skill_md) {
                if let Some(skill) = parse_skill_frontmatter(&content, &skill_md) {
                    skills.push(skill);
                }
            }
        }
    }
}

fn extract_frontmatter(content: &str) -> Option<&str> {
    let rest = content.strip_prefix("---")?;
    let end = rest.find("---")?;
    Some(&rest[..end])
}

fn default_skill_name(path: &Path) -> String {
    path.parent()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Parse YAML-like frontmatter from SKILL.md
fn parse_skill_frontmatter(content: &str, path: &Path) -> Option<Skill> {
    let frontmatter = extract_frontmatter(content)?;

    let mut name = None;
    let mut description = None;

    for line in frontmatter.lines() {
        let trimmed = line.trim();
        if let Some(val) = trimmed.strip_prefix("name:") {
            name = Some(val.trim().trim_matches('\'').trim_matches('"').to_string());
        }
        if let Some(val) = trimmed.strip_prefix("description:") {
            description = Some(val.trim().trim_matches('\'').trim_matches('"').to_string());
        }
    }

    Some(Skill {
        name: name.unwrap_or_else(|| default_skill_name(path)),
        description: description.unwrap_or_default(),
        location: path.to_path_buf(),
    })
}

// ── Matching ──────────────────────────────────────────────────────────────

const STOPWORDS: &[&str] = &[
    "the", "and", "for", "are", "but", "not", "you", "all", "can", "had", "her", "was", "one",
    "our", "out", "has", "have", "been", "some", "them", "than", "its", "over", "such", "that",
    "this", "with", "will", "each", "from", "they", "were", "which", "their", "said", "what",
    "when", "who", "how", "use", "new", "now", "way", "may", "get", "got", "set", "let", "any",
    "also", "into", "just", "only", "very", "even", "most", "other", "need", "make", "like",
    "does", "your", "more", "want", "should",
];

fn is_stopword(word: &str) -> bool {
    STOPWORDS.contains(&word)
}

pub fn match_skill<'a>(skills: &'a [Skill], user_message: &str) -> Option<&'a Skill> {
    let msg_lower = user_message.to_lowercase();
    let msg_words: Vec<&str> = msg_lower.split_whitespace().collect();

    let mut best_score = 0.0f32;
    let mut best_skill = None;

    for skill in skills {
        let desc_lower = skill.description.to_lowercase();
        let name_lower = skill.name.to_lowercase();
        let mut score = 0.0f32;

        if msg_lower.contains(&name_lower) && name_lower.len() >= 4 {
            score += 10.0;
        }

        for word in &msg_words {
            if word.len() < 4 || is_stopword(word) {
                continue;
            }
            if desc_lower.contains(word) {
                score += 1.0;
            }
        }

        for word in desc_lower.split(|c: char| !c.is_alphanumeric()) {
            if word.len() < 4 || is_stopword(word) {
                continue;
            }
            if msg_lower.contains(word) {
                score += 1.0;
            }
        }

        if score > best_score {
            best_score = score;
            best_skill = Some(skill);
        }
    }

    if best_score >= 5.0 {
        best_skill
    } else {
        None
    }
}

// ── Template variable substitution / inline shell ─────────────────
// Ported from hermes-agent skill_preprocessing.py

use regex::Regex;
use std::sync::OnceLock;

fn skill_template_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\$\{(HERMES_SKILL_DIR|HERMES_SESSION_ID)\}").unwrap())
}

fn inline_shell_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"!`([^`\n]+)`").unwrap())
}

/// Load skill content raw (no preprocessing).
pub fn load_skill_content(skill: &Skill) -> Option<String> {
    std::fs::read_to_string(&skill.location).ok()
}

/// Preprocess skill content: substitute template vars and expand inline shell.
pub fn preprocess_skill_content(
    content: &str,
    skill_dir: Option<&Path>,
    session_id: Option<&str>,
    cwd: Option<&Path>,
) -> String {
    let content = substitute_template_vars(content, skill_dir, session_id);
    expand_inline_shell(&content, cwd, 30)
}

/// Substitute `${HERMES_SKILL_DIR}` and `${HERMES_SESSION_ID}` in skill content.
/// Unresolved tokens are left as-is so the author can debug them.
pub fn substitute_template_vars(
    content: &str,
    skill_dir: Option<&Path>,
    session_id: Option<&str>,
) -> String {
    skill_template_re()
        .replace_all(content, |caps: &regex::Captures| {
            match caps.get(1).map(|m| m.as_str()) {
                Some("HERMES_SKILL_DIR") => skill_dir
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|| caps[0].to_string()),
                Some("HERMES_SESSION_ID") => session_id
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| caps[0].to_string()),
                _ => caps[0].to_string(),
            }
        })
        .to_string()
}

/// Execute inline shell snippets (`!\`command\``) in skill content.
/// Replaces each snippet with its stdout (trimmed).
/// Failures produce a short `[inline-shell error: ...]` marker.
pub fn expand_inline_shell(content: &str, cwd: Option<&Path>, _timeout_secs: u64) -> String {
    inline_shell_re()
        .replace_all(content, |caps: &regex::Captures| {
            let cmd = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            if cmd.is_empty() {
                return String::new();
            }
            match std::process::Command::new("sh")
                .arg("-c")
                .arg(cmd)
                .current_dir(cwd.unwrap_or(Path::new(".")))
                .output()
            {
                Ok(output) if output.status.success() => {
                    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
                    if stdout.len() > 4000 {
                        format!(
                            "{}… [truncated]",
                            crate::text::truncate_chars(&stdout, 4000)
                        )
                    } else {
                        stdout
                    }
                }
                Ok(output) => {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    format!(
                        "[inline-shell error: {}]",
                        stderr.trim().chars().take(120).collect::<String>()
                    )
                }
                Err(e) => format!("[inline-shell error: {}]", e),
            }
        })
        .to_string()
}

// ── Save managed skill ────────────────────────────────────────────────────

pub fn save_managed_skill(workspace: &Path, input: &ManagedSkillInput) -> anyhow::Result<PathBuf> {
    let dir = managed_skills_dir(workspace).join(slugify(&input.name));
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("SKILL.md");
    let content = format!(
        "---\nname: {}\ndescription: {}\n---\n\n{}\n",
        input.name, input.description, input.body
    );
    std::fs::write(&path, content)?;
    Ok(path)
}

fn slugify(name: &str) -> String {
    name.to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_frontmatter() {
        let content = "---\nname: test-skill\ndescription: A test skill\n---\n# Content";
        let skill = parse_skill_frontmatter(content, Path::new("/tmp/test/SKILL.md"));
        assert!(skill.is_some());
        let s = skill.unwrap();
        assert_eq!(s.name, "test-skill");
        assert_eq!(s.description, "A test skill");
    }

    #[test]
    fn test_match_skill() {
        let skills = vec![
            Skill {
                name: "weather".to_string(),
                description: "Get weather forecasts for any location".to_string(),
                location: PathBuf::from("/tmp/weather/SKILL.md"),
            },
            Skill {
                name: "github".to_string(),
                description: "GitHub operations, PRs, issues, code review".to_string(),
                location: PathBuf::from("/tmp/github/SKILL.md"),
            },
        ];

        let matched = match_skill(&skills, "what's the weather in Melbourne?");
        assert!(matched.is_some());
        assert_eq!(matched.unwrap().name, "weather");

        let matched = match_skill(&skills, "review the github issues");
        assert!(matched.is_some());
        assert_eq!(matched.unwrap().name, "github");
    }

    #[test]
    fn test_template_substitution() {
        let result = substitute_template_vars(
            "Run from ${HERMES_SKILL_DIR} for session ${HERMES_SESSION_ID}",
            Some(Path::new("/tmp/myskill")),
            Some("sess_123"),
        );
        assert_eq!(result, "Run from /tmp/myskill for session sess_123");
    }

    #[test]
    fn test_unknown_template_preserved() {
        let content = "Token ${UNKNOWN} stays";
        let result = substitute_template_vars(content, None, None);
        assert_eq!(result, "Token ${UNKNOWN} stays");
    }

    #[test]
    fn test_inline_shell_expansion() {
        let result = expand_inline_shell("Date is !`echo hello`", None, 5);
        assert!(result.contains("hello"));
    }
}
