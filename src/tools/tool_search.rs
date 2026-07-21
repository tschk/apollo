//! ToolSearchTool — keyword search over all available tools.
//!
//! When many tools are registered, this lets the agent find the right tool
//! by searching names and descriptions without having to remember them all.

use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use tokio::sync::RwLock;

use super::traits::*;

pub struct ToolSearchTool {
    tools: Arc<RwLock<Vec<Arc<dyn Tool>>>>,
}

impl ToolSearchTool {
    pub fn new(tools: Arc<RwLock<Vec<Arc<dyn Tool>>>>) -> Self {
        Self { tools }
    }
}

#[derive(Deserialize)]
struct SearchArgs {
    /// Keywords to search for in tool names and descriptions
    query: String,
    /// Max results to return (default 10)
    #[serde(default = "default_limit")]
    limit: usize,
}

fn default_limit() -> usize {
    10
}

#[async_trait]
impl Tool for ToolSearchTool {
    fn name(&self) -> &str {
        "tool_search"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "tool_search".to_string(),
            description: "Search for available tools by keyword. \
                Returns matching tool names and descriptions. \
                Use when you need to find a tool but aren't sure of its exact name."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Keywords to search (searches tool name and description)"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Max results to return (default 10)",
                        "minimum": 1,
                        "maximum": 50
                    }
                },
                "required": ["query"]
            }),
        }
    }

    async fn execute(&self, arguments: &str) -> anyhow::Result<ToolResult> {
        let args: SearchArgs = serde_json::from_str(arguments)?;

        if args.query.trim().is_empty() {
            return Ok(ToolResult::error("query must not be empty"));
        }

        let terms: Vec<String> = args
            .query
            .to_lowercase()
            .split_whitespace()
            .map(String::from)
            .collect();

        let tools = self.tools.read().await;
        let limit = args.limit.min(50);

        // Score each tool: +2 for name match, +1 for description match per term
        let mut scored: Vec<(usize, &Arc<dyn Tool>)> = tools
            .iter()
            .filter_map(|t| {
                let spec = t.spec();
                let name_lower = spec.name.to_lowercase();
                let desc_lower = spec.description.to_lowercase();
                let score: usize = terms
                    .iter()
                    .map(|term| {
                        let in_name = if name_lower.contains(term.as_str()) {
                            2
                        } else {
                            0
                        };
                        let in_desc = if desc_lower.contains(term.as_str()) {
                            1
                        } else {
                            0
                        };
                        in_name + in_desc
                    })
                    .sum();
                if score > 0 {
                    Some((score, t))
                } else {
                    None
                }
            })
            .collect();

        scored.sort_by_key(|item| std::cmp::Reverse(item.0));
        scored.truncate(limit);

        if scored.is_empty() {
            return Ok(ToolResult::success(format!(
                "No tools found matching '{}'.",
                args.query
            )));
        }

        let lines: Vec<String> = scored
            .iter()
            .map(|(score, t)| {
                let spec = t.spec();
                let desc_preview: String = spec.description.chars().take(100).collect();
                format!("- **{}** (score:{}) — {}", spec.name, score, desc_preview)
            })
            .collect();

        Ok(ToolResult::success(format!(
            "Found {} tool(s) matching '{}':\n\n{}",
            lines.len(),
            args.query,
            lines.join("\n")
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::ExecutionPolicy;
    use crate::tools::shell::ShellTool;
    use std::path::PathBuf;

    #[tokio::test]
    async fn finds_shell_tool() {
        let shell = Arc::new(ShellTool::new(
            PathBuf::from("."),
            Arc::new(ExecutionPolicy::default()),
        ));
        let tools: Arc<RwLock<Vec<Arc<dyn Tool>>>> = Arc::new(RwLock::new(vec![shell]));
        let tool = ToolSearchTool::new(tools);

        let r = tool
            .execute(r#"{"query":"shell execute command"}"#)
            .await
            .unwrap();
        assert!(!r.is_error);
        assert!(r.output.contains("exec") || r.output.contains("shell"));
    }

    #[tokio::test]
    async fn empty_query_errors() {
        let tools: Arc<RwLock<Vec<Arc<dyn Tool>>>> = Arc::new(RwLock::new(vec![]));
        let tool = ToolSearchTool::new(tools);
        let r = tool.execute(r#"{"query":""}"#).await.unwrap();
        assert!(r.is_error);
    }

    #[tokio::test]
    async fn no_match_returns_message() {
        let tools: Arc<RwLock<Vec<Arc<dyn Tool>>>> = Arc::new(RwLock::new(vec![]));
        let tool = ToolSearchTool::new(tools);
        let r = tool.execute(r#"{"query":"xyzzy"}"#).await.unwrap();
        assert!(!r.is_error);
        assert!(r.output.contains("No tools found"));
    }
}
