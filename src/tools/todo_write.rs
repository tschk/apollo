//! TodoWriteTool — create, append, and list markdown task files.

use std::path::PathBuf;

use async_trait::async_trait;
use serde::Deserialize;

use super::traits::*;

pub struct TodoWriteTool {
    workspace: PathBuf,
}

impl TodoWriteTool {
    pub fn new(workspace: PathBuf) -> Self {
        Self { workspace }
    }

    fn todo_path(&self, file: &str) -> PathBuf {
        let name = if file.is_empty() { "TODO" } else { file };
        // Sanitize: only allow filename-safe characters
        let clean: String = name
            .chars()
            .filter(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.'))
            .collect();
        let clean = if clean.is_empty() {
            "TODO".to_string()
        } else {
            clean
        };
        let name = if clean.ends_with(".md") {
            clean
        } else {
            format!("{}.md", clean)
        };
        self.workspace.join(".tasks").join(name)
    }
}

#[derive(Deserialize)]
struct TodoArgs {
    /// Action: "write" (overwrite), "append", or "read"
    #[serde(default = "default_action")]
    action: String,
    /// Items or content to write (one per line, or full markdown)
    #[serde(default)]
    content: String,
    /// Optional filename without extension (defaults to "TODO")
    #[serde(default)]
    file: String,
}

fn default_action() -> String {
    "append".to_string()
}

#[async_trait]
impl Tool for TodoWriteTool {
    fn name(&self) -> &str {
        "todo"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "todo".to_string(),
            description: "Create, append to, or read markdown task/todo files in .tasks/. \
                Use to track work items, record decisions, or maintain checklists."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["write", "append", "read"],
                        "description": "write = overwrite file, append = add to end, read = show contents"
                    },
                    "content": {
                        "type": "string",
                        "description": "Content to write or append. Supports markdown."
                    },
                    "file": {
                        "type": "string",
                        "description": "Filename without .md extension (default: TODO)"
                    }
                },
                "required": ["action"]
            }),
        }
    }

    async fn execute(&self, arguments: &str) -> anyhow::Result<ToolResult> {
        let args: TodoArgs = serde_json::from_str(arguments)?;
        let path = self.todo_path(&args.file);

        match args.action.as_str() {
            "read" => match tokio::fs::read_to_string(&path).await {
                Ok(content) => Ok(ToolResult::success(content)),
                Err(_) => Ok(ToolResult::success("(file does not exist yet)")),
            },
            "write" => {
                if let Some(parent) = path.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }
                tokio::fs::write(&path, &args.content).await?;
                Ok(ToolResult::success(format!(
                    "Written to {}",
                    path.display()
                )))
            }
            "append" => {
                if let Some(parent) = path.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }
                let existing = tokio::fs::read_to_string(&path).await.unwrap_or_default();
                let separator = if existing.is_empty() || existing.ends_with('\n') {
                    ""
                } else {
                    "\n"
                };
                let new_content = format!("{}{}{}\n", existing, separator, args.content);
                tokio::fs::write(&path, &new_content).await?;
                Ok(ToolResult::success(format!(
                    "Appended to {}",
                    path.display()
                )))
            }
            other => Ok(ToolResult::error(format!("Unknown action: {}", other))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn write_and_read() {
        let dir = TempDir::new().unwrap();
        let tool = TodoWriteTool::new(dir.path().to_path_buf());

        let w = tool
            .execute(r#"{"action":"write","content":"- [ ] task one"}"#)
            .await
            .unwrap();
        assert!(!w.is_error);

        let r = tool.execute(r#"{"action":"read"}"#).await.unwrap();
        assert!(r.output.contains("task one"));
    }

    #[tokio::test]
    async fn append_adds_line() {
        let dir = TempDir::new().unwrap();
        let tool = TodoWriteTool::new(dir.path().to_path_buf());

        tool.execute(r#"{"action":"write","content":"line1"}"#)
            .await
            .unwrap();
        tool.execute(r#"{"action":"append","content":"line2"}"#)
            .await
            .unwrap();

        let r = tool.execute(r#"{"action":"read"}"#).await.unwrap();
        assert!(r.output.contains("line1"));
        assert!(r.output.contains("line2"));
    }
}
