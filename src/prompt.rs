//! System prompt builder — reads SOUL.md, USER.md, AGENTS.md, MEMORY.md, TOOLS.md, IDENTITY.md
//! and assembles them into a system prompt for the LLM.

use std::path::Path;

const DEFAULT_PROMPT: &str = "You are a helpful AI assistant.";

const ROUTING_GUIDANCE: &str = "## Routing guidance
In group chats, respond to questions about the assistant, its plugins, settings, commands, upgrades, or transport even without a direct mention. Ignore unrelated ambient chatter unless the message clearly addresses the assistant or requests help.";

const PROMPT_FILES: [(&str, &str, usize); 6] = [
    ("IDENTITY.md", "## Identity", 12_000),
    ("SOUL.md", "## Personality & Tone", 12_000),
    ("USER.md", "## About the User", 12_000),
    ("AGENTS.md", "## Workspace Rules", 16_000),
    ("TOOLS.md", "## Tool Notes", 12_000),
    ("MEMORY.md", "## Long-Term Memory", 8_000),
];

// Automation may need repository instructions, but personal profile and
// long-term-memory files must never become ambient provider context.
const RESTRICTED_PROMPT_FILES: [(&str, &str, usize); 4] = [
    ("IDENTITY.md", "## Identity", 12_000),
    ("SOUL.md", "## Personality & Tone", 12_000),
    ("AGENTS.md", "## Workspace Rules", 16_000),
    ("TOOLS.md", "## Tool Notes", 12_000),
];

/// Build the system prompt from workspace context files
pub async fn build_system_prompt(workspace: &Path) -> String {
    build_system_prompt_from_files(workspace, &PROMPT_FILES).await
}

/// Build an automation prompt without personal or long-term-memory files.
pub async fn build_restricted_system_prompt(workspace: &Path) -> String {
    build_system_prompt_from_files(workspace, &RESTRICTED_PROMPT_FILES).await
}

async fn build_system_prompt_from_files(workspace: &Path, files: &[(&str, &str, usize)]) -> String {
    let body = load_workspace_sections(workspace, files).await;
    let mut prompt = if body.is_empty() {
        DEFAULT_PROMPT.to_string()
    } else {
        body
    };
    prompt.push_str("\n\n");
    prompt.push_str(ROUTING_GUIDANCE);
    prompt
}

async fn load_workspace_sections(workspace: &Path, files: &[(&str, &str, usize)]) -> String {
    let mut parts = Vec::new();
    for &(filename, header, limit) in files {
        if let Some(content) = read_file(workspace, filename, limit).await {
            parts.push(format!("{header}\n{content}"));
        }
    }
    parts.join("\n\n---\n\n")
}

/// Read a file from workspace, return None if missing
async fn read_file(workspace: &Path, filename: &str, limit: usize) -> Option<String> {
    let path = workspace.join(filename);
    let content = tokio::fs::read_to_string(&path).await.ok()?;
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return None;
    }
    match crate::text::truncate_chars_counted(trimmed, limit) {
        Some((head, dropped)) => Some(format!("{head}...\n(truncated {dropped} chars)")),
        None => Some(trimmed.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[tokio::test]
    async fn test_build_system_prompt_empty_workspace() {
        let prompt = build_system_prompt(&PathBuf::from("/nonexistent")).await;
        assert!(prompt.contains(DEFAULT_PROMPT));
        assert!(prompt.contains("Routing guidance"));
    }

    #[tokio::test]
    async fn restricted_prompt_excludes_personal_files() {
        let directory = tempfile::tempdir().unwrap();
        tokio::fs::write(directory.path().join("USER.md"), "private user data")
            .await
            .unwrap();
        tokio::fs::write(directory.path().join("MEMORY.md"), "private memory")
            .await
            .unwrap();
        tokio::fs::write(directory.path().join("AGENTS.md"), "repository rules")
            .await
            .unwrap();

        let prompt = build_restricted_system_prompt(directory.path()).await;
        assert!(prompt.contains("repository rules"));
        assert!(!prompt.contains("private user data"));
        assert!(!prompt.contains("private memory"));
    }
}
