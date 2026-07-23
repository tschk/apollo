//! Session tools — model switching, status, config management.
//! Gives the AI control over its own session (like OpenClaw's session_status).

use async_trait::async_trait;
use serde::Deserialize;
use std::sync::Arc;

use super::traits::*;
use crate::agent::AgentRunner;
use crate::text::truncate_chars;

/// session_status — view/change model, check status
pub struct SessionStatusTool {
    runner: Arc<AgentRunner>,
}

impl SessionStatusTool {
    pub fn new(runner: Arc<AgentRunner>) -> Self {
        Self { runner }
    }
}

#[derive(Deserialize)]
struct SessionStatusArgs {
    /// Set model override (e.g. "claude-opus-4", "claude-haiku-3-5")
    model: Option<String>,
}

#[async_trait]
impl Tool for SessionStatusTool {
    fn name(&self) -> &str {
        "session_status"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "session_status".to_string(),
            description: "Show session status (current model, tools, uptime). Optionally set model override with model parameter.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "model": {
                        "type": "string",
                        "description": "Set model override (e.g. 'claude-opus-4', 'claude-sonnet-4-5', 'claude-haiku-3-5'). Use 'default' to reset."
                    }
                }
            }),
        }
    }

    async fn execute(&self, arguments: &str) -> anyhow::Result<ToolResult> {
        let args: SessionStatusArgs =
            serde_json::from_str(arguments).unwrap_or(SessionStatusArgs { model: None });

        if let Some(model) = &args.model {
            if model == "default" || model == "reset" {
                let default = self.runner.get_default_model().to_string();
                self.runner.reset_model();
                return Ok(ToolResult::success(format!(
                    "Model reset to configured default: {default}"
                )));
            }
            self.runner.set_model(model.as_str());
            return Ok(ToolResult::success(format!("Model switched to: {model}")));
        }

        let tools = self.runner.list_tools().await;
        let cfg = &self.runner.agent_config;
        let status = format!(
            "Session Status:\n\
            Model (current):  {}\n\
            Model (default):  {}\n\
            Model (fast):     {}\n\
            Model (heavy):    {}\n\
            Tools: {} active\n\
            PID: {}\n\
            Runtime: apollo v{}\n\n\
            Tool list: {}\n\n\
            Tip: use session_status{{\"model\":\"...\"}}\n\
            For swarms — use fast model as runner, heavy as orchestrator.\n\
            Available aliases: default/reset (restore configured model)",
            self.runner.get_model(),
            self.runner.get_default_model(),
            cfg.fast_model,
            cfg.heavy_model,
            tools.len(),
            std::process::id(),
            env!("CARGO_PKG_VERSION"),
            tools.join(", "),
        );

        Ok(ToolResult::success(status))
    }
}

/// list_models — fetch available models from Anthropic API
pub struct ListModelsTool;

impl ListModelsTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ListModelsTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for ListModelsTool {
    fn name(&self) -> &str {
        "list_models"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "list_models".to_string(),
            description: "List available Claude models from the Anthropic API. Use this to discover the latest models.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {}
            }),
        }
    }

    async fn execute(&self, _arguments: &str) -> anyhow::Result<ToolResult> {
        let api_key = std::env::var("ANTHROPIC_API_KEY").unwrap_or_default();
        if api_key.is_empty() {
            return Ok(ToolResult::error("No ANTHROPIC_API_KEY set"));
        }

        let is_oauth = api_key.contains("sk-ant-oat");
        let client = reqwest::Client::new();

        let mut req = client
            .get("https://api.anthropic.com/v1/models")
            .header("anthropic-version", "2023-06-01");

        if is_oauth {
            req = req
                .header("Authorization", format!("Bearer {}", api_key))
                .header("anthropic-beta", "claude-code-20250219,oauth-2025-04-20");
        } else {
            req = req.header("x-api-key", &api_key);
        }

        let resp = req.send().await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Ok(ToolResult::error(format!(
                "API error {}: {}",
                status,
                truncate_chars(&text, 300)
            )));
        }

        let data: serde_json::Value = resp.json().await?;

        // Parse model list
        let models = data["data"].as_array();
        match models {
            Some(list) => {
                let mut output = String::from("Available models:\n\n");
                for m in list {
                    let id = m["id"].as_str().unwrap_or("unknown");
                    let display = m["display_name"].as_str().unwrap_or(id);
                    output.push_str(&format!("• {} ({})\n", display, id));
                }
                Ok(ToolResult::success(output))
            }
            None => {
                // Fallback: return known models
                Ok(ToolResult::success(
                    "Available models:\n\n\
                    • claude-sonnet-4-6 (balanced — default)\n\
                    • claude-opus-4-6 (most capable)\n\
                    • claude-haiku-4-5-20251001 (fastest, cheapest — use as swarm runner)\n\n\
                    Use session_status with model parameter to switch.",
                ))
            }
        }
    }
}
