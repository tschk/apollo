//! Core Tool trait — defines agent capabilities.

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::pty_worker::PtyWorker;

/// Tool specification for LLM function calling.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: Value, // JSON Schema
}

/// Result of executing a tool.
#[derive(Debug, Clone)]
pub struct ToolResult {
    pub output: String,
    pub is_error: bool,
}

impl ToolResult {
    pub fn success(output: impl Into<String>) -> Self {
        Self {
            output: crate::redaction::redact_text(&output.into()),
            is_error: false,
        }
    }
    pub fn error(output: impl Into<String>) -> Self {
        Self {
            output: crate::redaction::redact_text(&output.into()),
            is_error: true,
        }
    }
}

/// The core Tool trait. Each tool implements this.
#[async_trait]
pub trait Tool: Send + Sync {
    /// Tool name (must match the ToolSpec name)
    fn name(&self) -> &str;

    /// Get the tool specification for LLM function calling
    fn spec(&self) -> ToolSpec;

    /// Execute the tool with the given arguments (JSON string)
    async fn execute(&self, arguments: &str) -> anyhow::Result<ToolResult>;

    fn pty_worker(&self) -> Option<Arc<PtyWorker>> {
        None
    }
}
