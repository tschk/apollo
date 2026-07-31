//! File operations tools — Read, Write (matching OpenClaw's API).

use async_trait::async_trait;
use serde::Deserialize;
use std::path::PathBuf;

use super::sandbox::{resolve_workspace_existing_path, resolve_workspace_write_path};
use super::traits::*;
use crate::text::truncate_chars_counted;

// ============================================================
// Read tool — read file contents with optional offset/limit
// ============================================================

pub struct FileReadTool {
    workspace: PathBuf,
}

impl FileReadTool {
    pub fn new(workspace: PathBuf) -> Self {
        Self { workspace }
    }
}

#[derive(Deserialize)]
struct ReadArgs {
    #[serde(alias = "file_path")]
    path: String,
    /// Line number to start reading from (1-indexed)
    offset: Option<usize>,
    /// Maximum number of lines to read
    limit: Option<usize>,
}

#[async_trait]
impl Tool for FileReadTool {
    fn name(&self) -> &str {
        "Read"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "Read".to_string(),
            description: "Read the contents of a file. Use offset/limit for large files."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to read (relative or absolute)"
                    },
                    "offset": {
                        "type": "integer",
                        "description": "Line number to start reading from (1-indexed)"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of lines to read"
                    }
                },
                "required": ["path"]
            }),
        }
    }

    async fn execute(&self, arguments: &str) -> anyhow::Result<ToolResult> {
        let args: ReadArgs = serde_json::from_str(arguments)?;
        let full_path = match resolve_workspace_existing_path(&self.workspace, &args.path) {
            Ok(p) => p,
            Err(e) => return Ok(ToolResult::error(e.to_string())),
        };

        match tokio::fs::read_to_string(&full_path).await {
            Ok(content) => {
                let lines: Vec<&str> = content.lines().collect();
                let total_lines = lines.len();

                let offset = args.offset.unwrap_or(1).max(1) - 1; // Convert 1-indexed to 0-indexed
                let limit = args.limit.unwrap_or(2000);

                let selected: Vec<&str> = lines.iter().skip(offset).take(limit).copied().collect();

                let result = selected.join("\n");

                // Truncate if too large
                let truncated = match truncate_chars_counted(&result, 50_000) {
                    Some((head, dropped)) => {
                        format!("{}...\n[truncated {} chars]", head, dropped)
                    }
                    None => result,
                };

                let remaining = total_lines.saturating_sub(offset + limit);
                if remaining > 0 {
                    Ok(ToolResult::success(format!(
                        "{}\n\n[{} more lines in file. Use offset={} to continue.]",
                        truncated,
                        remaining,
                        offset + limit + 1
                    )))
                } else {
                    Ok(ToolResult::success(truncated))
                }
            }
            Err(e) => Ok(ToolResult::error(format!(
                "Cannot read '{}': {}",
                args.path, e
            ))),
        }
    }
}

// ============================================================
// Write tool — create or overwrite files, auto-create parent dirs
// ============================================================

pub struct FileWriteTool {
    workspace: PathBuf,
}

impl FileWriteTool {
    pub fn new(workspace: PathBuf) -> Self {
        Self { workspace }
    }
}

#[derive(Deserialize)]
struct WriteArgs {
    #[serde(alias = "file_path")]
    path: String,
    content: String,
}

#[async_trait]
impl Tool for FileWriteTool {
    fn name(&self) -> &str {
        "Write"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "Write".to_string(),
            description: "Write content to a file. Creates the file if it doesn't exist, overwrites if it does. Automatically creates parent directories.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to write (relative or absolute)"
                    },
                    "content": {
                        "type": "string",
                        "description": "Content to write to the file"
                    }
                },
                "required": ["path", "content"]
            }),
        }
    }

    async fn execute(&self, arguments: &str) -> anyhow::Result<ToolResult> {
        let args: WriteArgs = serde_json::from_str(arguments)?;
        let full_path = match resolve_workspace_write_path(&self.workspace, &args.path) {
            Ok(p) => p,
            Err(e) => return Ok(ToolResult::error(e.to_string())),
        };

        // Create parent directories
        if let Some(parent) = full_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        match tokio::fs::write(&full_path, &args.content).await {
            Ok(_) => Ok(ToolResult::success(format!(
                "Successfully wrote {} bytes to {}",
                args.content.len(),
                args.path
            ))),
            Err(e) => Ok(ToolResult::error(format!("Failed to write: {}", e))),
        }
    }
}
