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

/// Put the instance's configured instructions in front of the file-built prompt.
///
/// An empty value, or the stock "You are a helpful AI assistant.", leaves the
/// assembled prompt alone. Anything the user typed is what the model sees first.
pub fn apply_configured(configured: &str, assembled: String) -> String {
    let configured = configured.trim();
    if configured.is_empty() || configured == DEFAULT_PROMPT {
        return assembled;
    }
    if let Some(rest) = assembled.strip_prefix(DEFAULT_PROMPT) {
        format!("{configured}{rest}")
    } else {
        format!("{configured}\n\n---\n\n{assembled}")
    }
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

/// A short note rebuilt every turn from the model, the mode, and the tools
/// actually attached. Original wording: the shape (what you are, what you may
/// do, what you just checked) is the useful part of other agents' prompts,
/// not their sentences.
pub fn situation_note(model: &str, mode: &str, tools: &[&str]) -> String {
    let model = if model.trim().is_empty() {
        "the selected model"
    } else {
        model.trim()
    };
    let mode_line = match mode {
        "auto" => "Mode: auto. Short questions can be answered directly. Anything that depends on the workspace should be checked, then done.".to_string(),
        "unattended" => "Mode: unattended. Finish the task without stopping for a plan. Stay inside the workspace.".to_string(),
        "coding, plan first" => "Mode: coding, plan first. Say what you will change and wait. Do not edit or run mutating commands yet.".to_string(),
        "coding" => "Mode: coding. Change the code, then say what changed and how you checked it.".to_string(),
        "swarm" => "Mode: swarm. Split the work into pieces that can run side by side, then merge what came back.".to_string(),
        other => format!("Mode: {other}. Follow that mode's limits."),
    };
    let tools_line = if tools.is_empty() {
        "No tools are attached this turn. Answer from the conversation, and say so if the task needs a tool you do not have.".to_string()
    } else {
        let shown = tools
            .iter()
            .take(24)
            .copied()
            .collect::<Vec<_>>()
            .join(", ");
        let extra = tools.len().saturating_sub(24);
        let more = if extra > 0 {
            format!(" (+{extra} more)")
        } else {
            String::new()
        };
        format!(
            "Tools on this turn: {shown}{more}. Use one when it can check a fact, read a file, or change the workspace. Do not invent a tool result, and do not tell the user to run a command you can run yourself."
        )
    };
    format!(
        "## This turn\nModel: {model}.\n{mode_line}\n{tools_line}\n\nLead with the result. When a tool was used, say what it showed. If you are unsure, look before you answer. Leave the user's files in their own style unless they asked for a change."
    )
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

    #[test]
    fn situation_note_tracks_model_mode_and_tools() {
        let bare = situation_note("gpt-5.5", "auto", &[]);
        let with_tools = situation_note("claude-sonnet-4-6", "coding", &["shell", "doctor"]);
        assert!(bare.contains("gpt-5.5"));
        assert!(bare.contains("No tools"));
        assert!(with_tools.contains("claude-sonnet-4-6"));
        assert!(with_tools.contains("shell, doctor"));
        assert!(with_tools.contains("Mode: coding"));
        assert_ne!(bare, with_tools);
    }
}
