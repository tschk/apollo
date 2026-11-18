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

        // Host plugin directories carry SKILL.md too. `discover_host_plugins`
        // scanned these and only logged what it found, so an OpenClaw plugin
        // dropped into `plugins/` was discovered and then ignored by the one
        // system that could have used it.
        for dir in host_plugin_skill_dirs(workspace) {
            scan_skill_dir(&dir, &mut skills);
        }
    }

    skills
}

/// Plugin roots that may contain SKILL.md-bearing directories. Same roots
/// `plugin_hosts::discover_host_plugins` scans, deliberately.
fn host_plugin_skill_dirs(workspace: &Path) -> Vec<PathBuf> {
    vec![
        workspace.join("plugins"),
        workspace.join(".openclaw/plugins"),
        workspace.join(".hermes/plugins"),
    ]
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

/// Shell syntax `expand_inline_shell` refuses to run. Snippets execute as
/// literal argv with no shell, so these characters would not do what the
/// author wrote — `a | b` would run `a` with the arguments `|` and `b`.
/// Rejecting them loudly also keeps a future reintroduction of `sh -c` from
/// quietly bringing command injection back. Backtick and newline cannot reach
/// the check through `inline_shell_re` today; they are listed anyway in case
/// that regex ever loosens.
const INLINE_SHELL_FORBIDDEN: &[char] =
    &['|', '&', ';', '<', '>', '(', ')', '$', '`', '~', '\n', '\r'];

/// Load skill content raw (no preprocessing).
pub fn load_skill_content(skill: &Skill) -> Option<String> {
    std::fs::read_to_string(&skill.location).ok()
}

/// Preprocess skill content: substitute template vars and, only when
/// `allow_inline_shell` is set, expand inline shell. Otherwise every snippet is
/// replaced with a marker and nothing runs — see `inline_shell_trusted`.
pub fn preprocess_skill_content(
    content: &str,
    skill_dir: Option<&Path>,
    session_id: Option<&str>,
    cwd: Option<&Path>,
    allow_inline_shell: bool,
) -> String {
    let content = substitute_template_vars(content, skill_dir, session_id);
    if allow_inline_shell {
        expand_inline_shell(&content, cwd, 30)
    } else {
        inline_shell_re()
            .replace_all(&content, "[inline-shell disabled: skill is not trusted]")
            .to_string()
    }
}

/// Whether the skill at `skill_md` may run its `!`cmd`` snippets.
///
/// Skills are loaded from roots that untrusted parties can write to — an npm
/// package, workspace `plugins/` directories, and `.apollo/skills`, which the
/// agent itself writes via `save_managed_skill` (so a prompt injection could
/// persist a skill). Snippets run at skill-match time, before any tool hook or
/// policy check, so they need their own gate: `policy.allow_shell` must be on
/// *and* the skill must sit under one of `policy.trusted_skill_dirs`, which is
/// empty by default. Paths are canonicalised, so a symlink inside a trusted
/// directory that points elsewhere is not trusted.
pub fn inline_shell_trusted(
    skill_md: &Path,
    allow_shell: bool,
    trusted_dirs: &[PathBuf],
    workspace: &Path,
) -> bool {
    if !allow_shell || trusted_dirs.is_empty() {
        return false;
    }
    let Ok(skill) = skill_md.canonicalize() else {
        return false;
    };
    trusted_dirs.iter().any(|dir| {
        let dir = if dir.is_absolute() {
            dir.clone()
        } else {
            workspace.join(dir)
        };
        match dir.canonicalize() {
            Ok(dir) => skill.starts_with(dir),
            Err(e) => {
                // Otherwise skills silently stop running their snippets.
                tracing::warn!(
                    "policy.trusted_skill_dirs entry {} cannot be resolved ({e}); \
                     skills there will not run inline shell",
                    dir.display()
                );
                false
            }
        }
    })
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
///
/// Snippets run as literal argv with no shell, so shell syntax — pipes,
/// redirections, substitutions — would be passed to the program as plain
/// arguments. It is rejected loudly instead (see `INLINE_SHELL_FORBIDDEN`).
pub fn expand_inline_shell(content: &str, cwd: Option<&Path>, _timeout_secs: u64) -> String {
    inline_shell_re()
        .replace_all(content, |caps: &regex::Captures| {
            let cmd = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            if cmd.is_empty() {
                return String::new();
            }

            if let Some(c) = cmd.chars().find(|c| INLINE_SHELL_FORBIDDEN.contains(c)) {
                return format!("[inline-shell error: shell syntax {c:?} is not allowed]");
            }

            let parts = match shlex::split(cmd) {
                Some(p) => p,
                None => return "[inline-shell error: invalid command quoting]".to_string(),
            };

            if parts.is_empty() {
                return String::new();
            }

            let mut cmd_obj = std::process::Command::new(&parts[0]);
            if parts.len() > 1 {
                cmd_obj.args(&parts[1..]);
            }

            cmd_obj
                .current_dir(cwd.unwrap_or(Path::new(".")))
                .env_clear()
                .envs(crate::tools::child_proc::child_env());

            match cmd_obj.output() {
                Ok(output) if output.status.success() => {
                    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
                    match crate::text::truncate_chars_counted(&stdout, 4000) {
                        Some((head, dropped)) => {
                            format!("{}… [truncated {} chars]", head, dropped)
                        }
                        None => stdout,
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
    fn host_plugin_skills_are_discovered() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path();

        // An OpenClaw-style plugin dropped into the workspace. Discovery used
        // to find this and log it; nothing loaded it.
        let plugin = workspace.join("plugins/weather");
        std::fs::create_dir_all(&plugin).unwrap();
        std::fs::write(
            plugin.join("SKILL.md"),
            "---\nname: weather\ndescription: forecast lookup\n---\n# Weather\n",
        )
        .unwrap();

        let skills = discover_skills_for_workspace(Some(workspace));
        assert!(
            skills.iter().any(|s| s.name == "weather"),
            "host plugin SKILL.md was discovered but never loaded; got {:?}",
            skills.iter().map(|s| &s.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn hermes_and_openclaw_plugin_roots_are_both_scanned() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path();

        for (root, name) in [
            (".hermes/plugins", "hermes-skill"),
            (".openclaw/plugins", "openclaw-skill"),
        ] {
            let plugin = workspace.join(root).join(name);
            std::fs::create_dir_all(&plugin).unwrap();
            std::fs::write(
                plugin.join("SKILL.md"),
                format!("---\nname: {name}\ndescription: d\n---\n"),
            )
            .unwrap();
        }

        let found: Vec<String> = discover_skills_for_workspace(Some(workspace))
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert!(found.contains(&"hermes-skill".to_string()), "{found:?}");
        assert!(found.contains(&"openclaw-skill".to_string()), "{found:?}");
    }

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
    fn untrusted_skill_snippets_do_not_run() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("pwned");
        let content = format!("Hi !`touch {}` there", marker.display());

        let out = preprocess_skill_content(&content, None, None, None, false);
        assert!(!marker.exists(), "snippet ran for an untrusted skill");
        assert_eq!(
            out,
            "Hi [inline-shell disabled: skill is not trusted] there"
        );

        // Control: the same content does run once trusted.
        preprocess_skill_content(&content, None, None, None, true);
        assert!(marker.exists());
    }

    #[test]
    fn inline_shell_requires_allow_shell_and_a_trusted_dir() {
        let ws = tempfile::tempdir().unwrap();
        let trusted = ws.path().join("trusted/deploy");
        let untrusted = ws.path().join(".apollo/skills/evil");
        std::fs::create_dir_all(&trusted).unwrap();
        std::fs::create_dir_all(&untrusted).unwrap();
        let good = trusted.join("SKILL.md");
        let bad = untrusted.join("SKILL.md");
        std::fs::write(&good, "x").unwrap();
        std::fs::write(&bad, "x").unwrap();
        let dirs = vec![PathBuf::from("trusted")]; // relative to the workspace

        assert!(inline_shell_trusted(&good, true, &dirs, ws.path()));
        assert!(!inline_shell_trusted(&good, false, &dirs, ws.path()));
        assert!(!inline_shell_trusted(&good, true, &[], ws.path()));
        assert!(!inline_shell_trusted(&bad, true, &dirs, ws.path()));

        // A symlink planted inside the trusted dir does not extend trust to
        // the file it points at.
        #[cfg(unix)]
        {
            let link = trusted.join("link");
            std::os::unix::fs::symlink(&untrusted, &link).unwrap();
            assert!(!inline_shell_trusted(
                &link.join("SKILL.md"),
                true,
                &dirs,
                ws.path()
            ));
        }
    }

    #[test]
    fn test_inline_shell_expansion() {
        let result = expand_inline_shell("Date is !`echo hello`", None, 5);
        assert!(result.contains("hello"));
    }

    #[test]
    fn test_inline_shell_rejects_shell_syntax() {
        // Snippets run as literal argv; shell syntax must fail loudly instead
        // of passing `|`, `;`, `$`, … to the program as plain arguments.
        for cmd in [
            "echo hi | wc -c",
            "echo a; echo b",
            "echo a && echo b",
            "echo a || echo b",
            "echo hi > marker.txt",
            "cat < /etc/hostname",
            "echo $(whoami)",
            "echo ~",
        ] {
            let result = expand_inline_shell(&format!("X is !`{cmd}`"), None, 5);
            assert!(
                result.contains("[inline-shell error: shell syntax"),
                "{cmd:?} should be rejected, got {result:?}"
            );
        }
    }

    #[test]
    fn test_inline_shell_injection_is_not_executed() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("pwned");
        let result = expand_inline_shell(
            &format!("!`echo safe; touch {}`", marker.display()),
            None,
            5,
        );
        assert!(result.starts_with("[inline-shell error:"), "{result:?}");
        assert!(!result.contains("safe"), "nothing may run: {result:?}");
        assert!(!marker.exists(), "injected command must not run");
    }

    #[test]
    fn test_inline_shell_quoted_args_still_work() {
        let result = expand_inline_shell("!`echo \"hello world\"`", None, 5);
        assert_eq!(result, "hello world");
    }
}
