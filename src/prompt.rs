//! System prompt builder — reads SOUL.md, USER.md, AGENTS.md, MEMORY.md, TOOLS.md, IDENTITY.md
//! and assembles them into a system prompt for the LLM.

use std::path::Path;

const DEFAULT_PROMPT: &str = "You are a helpful AI assistant.";

const ROUTING_GUIDANCE: &str = "## Routing guidance
In group chats, respond to questions about the assistant, its plugins, settings, commands, upgrades, or transport even without a direct mention. Ignore unrelated ambient chatter unless the message clearly addresses the assistant or requests help.";

/// Build the system prompt from workspace context files
pub async fn build_system_prompt(workspace: &Path) -> String {
    let files = [
        ("IDENTITY.md", "## Identity", 12_000usize),
        ("SOUL.md", "## Personality & Tone", 12_000),
        ("USER.md", "## About the User", 12_000),
        ("AGENTS.md", "## Workspace Rules", 16_000),
        ("TOOLS.md", "## Tool Notes", 12_000),
        ("MEMORY.md", "## Long-Term Memory", 8_000),
    ];

    let mut readers = Vec::with_capacity(files.len());
    for (filename, header, limit) in files {
        readers.push(async move {
            read_file(workspace, filename, limit)
                .await
                .map(|content| format!("{header}\n{content}"))
        });
    }

    let mut parts = Vec::new();
    for reader in readers {
        if let Some(content) = reader.await {
            parts.push(content);
        }
    }

    let mut prompt = if parts.is_empty() {
        DEFAULT_PROMPT.to_string()
    } else {
        parts.join("\n\n---\n\n")
    };
    prompt.push_str("\n\n");
    prompt.push_str(ROUTING_GUIDANCE);
    prompt
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
        Some(format!("{}...\n(truncated)", &trimmed[..limit]))
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
