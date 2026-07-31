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

/// Build the system prompt from workspace context files
pub async fn build_system_prompt(workspace: &Path) -> String {
    let body = load_workspace_sections(workspace, &PROMPT_FILES).await;
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
    if trimmed.len() > limit {
        Some(format!(
            "{}...\n(truncated)",
            crate::text::truncate_chars(trimmed, limit)
        ))
    } else {
        Some(trimmed.to_string())
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
}
