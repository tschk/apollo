//! Memory search — full-text search over MEMORY.md + memory/*.md files
//! Provides memory_search and memory_get as agent tools.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::memory::MemoryBackend;

#[derive(Debug, Clone)]
pub struct SearchResult {
    pub path: String,
    pub line_number: usize,
    pub snippet: String,
    pub score: f32,
}

fn truncate_text(input: &str, max_chars: usize) -> String {
    input.chars().take(max_chars).collect()
}

/// Search MEMORY.md + memory/*.md for a query string
pub async fn memory_search(workspace: &Path, query: &str, max_results: usize) -> Vec<SearchResult> {
    let query_lower = query.to_lowercase();
    let query_words: Vec<&str> = query_lower.split_whitespace().collect();
    let mut results = Vec::new();

    // Files to search
    let mut files: Vec<PathBuf> = Vec::new();

    // MEMORY.md
    let memory_md = workspace.join("MEMORY.md");
    if tokio::fs::metadata(&memory_md).await.is_ok() {
        files.push(memory_md);
    }

    // memory/*.md
    let memory_dir = workspace.join("memory");
    if tokio::fs::metadata(&memory_dir)
        .await
        .map(|m| m.is_dir())
        .unwrap_or(false)
    {
        if let Ok(mut entries) = tokio::fs::read_dir(&memory_dir).await {
            while let Ok(Some(entry)) = entries.next_entry().await {
                let path = entry.path();
                if path.extension().is_some_and(|e| e == "md") {
                    files.push(path);
                }
            }
        }
    }

    for file in &files {
        if let Ok(content) = tokio::fs::read_to_string(file).await {
            let rel_path = file
                .strip_prefix(workspace)
                .unwrap_or(file)
                .to_string_lossy()
                .to_string();

            for (i, line) in content.lines().enumerate() {
                let line_lower = line.to_lowercase();
                let mut score = 0.0f32;

                // Exact substring match
                if line_lower.contains(&query_lower) {
                    score += 10.0;
                }

                // Word-level matches
                for word in &query_words {
                    if line_lower.contains(word) {
                        score += 2.0;
                    }
                }

                if score > 0.0 {
                    // Get context (surrounding lines)
                    let lines: Vec<&str> = content.lines().collect();
                    let start = i.saturating_sub(1);
                    let end = (i + 2).min(lines.len());
                    let snippet = lines[start..end].join("\n");

                    results.push(SearchResult {
                        path: rel_path.clone(),
                        line_number: i + 1,
                        snippet,
                        score,
                    });
                }
            }
        }
    }

    // Sort by score descending
    results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    results.truncate(max_results);
    results
}

/// Get specific lines from a memory file
pub async fn memory_get(
    workspace: &Path,
    file_path: &str,
    from_line: usize,
    num_lines: usize,
) -> Option<String> {
    let full_path = workspace.join(file_path);

    // Security: ensure path stays within workspace
    let canonical = tokio::fs::canonicalize(&full_path).await.ok()?;
    let workspace_canonical = tokio::fs::canonicalize(workspace).await.ok()?;
    if !canonical.starts_with(&workspace_canonical) {
        return None;
    }

    let content = tokio::fs::read_to_string(&full_path).await.ok()?;
    let lines: Vec<&str> = content.lines().collect();

    let start = from_line.saturating_sub(1); // 1-indexed to 0-indexed
    let end = (start + num_lines).min(lines.len());

    if start >= lines.len() {
        return None;
    }

    Some(lines[start..end].join("\n"))
}

// -- Tool wrappers for the agent loop --

use crate::tools::{Tool, ToolResult, ToolSpec};
use async_trait::async_trait;
use serde_json::json;

/// Tool: memory_search — search memory files for a query.
pub struct MemorySearchTool {
    workspace: PathBuf,
}

impl MemorySearchTool {
    pub fn new(workspace: PathBuf) -> Self {
        Self { workspace }
    }
}

#[async_trait]
impl Tool for MemorySearchTool {
    fn name(&self) -> &str {
        "memory_search"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "memory_search".to_string(),
            description:
                "Search workspace memory files (MEMORY.md, memory/*.md) for relevant information."
                    .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Search query" },
                    "limit": { "type": "integer", "description": "Maximum results (default 10)" }
                },
                "required": ["query"]
            }),
        }
    }

    async fn execute(&self, arguments: &str) -> anyhow::Result<ToolResult> {
        let args: serde_json::Value = serde_json::from_str(arguments)?;
        let query = args["query"].as_str().unwrap_or("");
        let limit = args["limit"].as_u64().unwrap_or(10) as usize;

        if query.is_empty() {
            return Ok(ToolResult::error("Query is required"));
        }

        let results = memory_search(&self.workspace, query, limit).await;
        if results.is_empty() {
            return Ok(ToolResult::success("No results found."));
        }

        let output: Vec<String> = results
            .iter()
            .map(|r| {
                format!(
                    "{}:{} (score {:.1}) — {}",
                    r.path,
                    r.line_number,
                    r.score,
                    r.snippet.replace('\n', " | ")
                )
            })
            .collect();

        Ok(ToolResult::success(output.join("\n")))
    }
}

/// Tool: memory_get — read lines from a memory file.
pub struct MemoryGetTool {
    workspace: PathBuf,
}

pub struct SessionSearchTool {
    memory: Arc<dyn MemoryBackend>,
}

impl SessionSearchTool {
    pub fn new(memory: Arc<dyn MemoryBackend>) -> Self {
        Self { memory }
    }
}

#[async_trait]
impl Tool for SessionSearchTool {
    fn name(&self) -> &str {
        "session_search"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "session_search".to_string(),
            description: "Search persisted conversation history across sessions or within a specific chat_id.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Search query" },
                    "limit": { "type": "integer", "description": "Maximum results (default 10)" },
                    "chat_id": { "type": "string", "description": "Optional chat/session identifier to restrict the search" }
                },
                "required": ["query"]
            }),
        }
    }

    async fn execute(&self, arguments: &str) -> anyhow::Result<ToolResult> {
        let args: serde_json::Value = serde_json::from_str(arguments)?;
        let query = args["query"].as_str().unwrap_or("");
        let limit = args["limit"].as_u64().unwrap_or(10) as usize;
        let chat_id = args["chat_id"].as_str();

        if query.is_empty() {
            return Ok(ToolResult::error("Query is required"));
        }

        let results = self
            .memory
            .search_conversations(query, limit, chat_id)
            .await?;
        if results.is_empty() {
            return Ok(ToolResult::success("No matching conversation history."));
        }

        let output = results
            .iter()
            .map(|hit| {
                format!(
                    "{} [{}] {} — {}",
                    hit.chat_id,
                    hit.role,
                    hit.created_at.to_rfc3339(),
                    truncate_text(&hit.content.replace('\n', " | "), 160)
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        Ok(ToolResult::success(output))
    }
}

impl MemoryGetTool {
    pub fn new(workspace: PathBuf) -> Self {
        Self { workspace }
    }
}

#[async_trait]
impl Tool for MemoryGetTool {
    fn name(&self) -> &str {
        "memory_get"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "memory_get".to_string(),
            description: "Read lines from a memory file in the workspace.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Relative path (e.g. MEMORY.md, memory/notes.md)" },
                    "from_line": { "type": "integer", "description": "Starting line (1-based, default 1)" },
                    "num_lines": { "type": "integer", "description": "Lines to read (default 50)" }
                },
                "required": ["path"]
            }),
        }
    }

    async fn execute(&self, arguments: &str) -> anyhow::Result<ToolResult> {
        let args: serde_json::Value = serde_json::from_str(arguments)?;
        let path = args["path"].as_str().unwrap_or("");
        let from_line = args["from_line"].as_u64().unwrap_or(1) as usize;
        let num_lines = args["num_lines"].as_u64().unwrap_or(50) as usize;

        if path.is_empty() {
            return Ok(ToolResult::error("Path is required"));
        }

        match memory_get(&self.workspace, path, from_line, num_lines).await {
            Some(content) => Ok(ToolResult::success(content)),
            None => Ok(ToolResult::error("File not found or path not allowed")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[tokio::test]
    async fn test_memory_search_nonexistent() {
        let results = memory_search(&PathBuf::from("/nonexistent"), "test", 5).await;
        assert!(results.is_empty());
    }
}
