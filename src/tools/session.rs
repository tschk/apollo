//! Session tools — model switching, status, config management.
//! Gives the AI control over its own session (like OpenClaw's session_status).

use async_trait::async_trait;
use serde::Deserialize;
use std::sync::Arc;

use super::traits::*;
use crate::agent::AgentRunner;

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
                        "description": "Set model override (e.g. 'gpt-5.5', 'gpt-5.4', 'gpt-5.4-mini'). Use 'default' to reset."
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
