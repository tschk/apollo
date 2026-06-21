//! ModeSwitchTool — let the agent switch its own execution mode at runtime.
//!
//! Useful in multi-phase workflows: start in Auto, switch to Coding for
//! implementation work, then back to Auto for conversation.

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use serde::Deserialize;

use super::traits::*;
use crate::agent::mode::AgentMode;

pub struct ModeSwitchTool {
    mode: Arc<RwLock<AgentMode>>,
}

impl ModeSwitchTool {
    pub fn new(mode: Arc<RwLock<AgentMode>>) -> Self {
        Self { mode }
    }
}

#[derive(Deserialize)]
struct ModeSwitchArgs {
    /// Target mode: "auto", "coding", "bypass", "swarm"
    mode: String,
    /// For coding mode: require user approval before executing the plan
    #[serde(default)]
    plan_approval: bool,
    /// For coding mode: project path to scope file operations
    #[serde(default)]
    project_path: Option<String>,
    /// For swarm mode: max parallel agents (default 3)
    #[serde(default = "default_parallelism")]
    parallelism: usize,
}

fn default_parallelism() -> usize {
    3
}

#[async_trait]
impl Tool for ModeSwitchTool {
    fn name(&self) -> &str {
        "mode_switch"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "mode_switch".to_string(),
            description: "Switch the agent's execution mode at runtime. \
                'auto' = default heuristic mode. \
                'coding' = optimized for software development (always plans, prefers Opus). \
                'bypass' = fully autonomous, skips all approval steps. \
                'swarm' = deploy parallel agents."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "mode": {
                        "type": "string",
                        "enum": ["auto", "coding", "bypass", "swarm"],
                        "description": "Target execution mode"
                    },
                    "plan_approval": {
                        "type": "boolean",
                        "description": "Coding mode only: show plan and wait for approval before executing (default false)"
                    },
                    "project_path": {
                        "type": "string",
                        "description": "Coding mode only: project root path"
                    },
                    "parallelism": {
                        "type": "integer",
                        "description": "Swarm mode only: max concurrent agents (default 3)",
                        "minimum": 1,
                        "maximum": 10
                    }
                },
                "required": ["mode"]
            }),
        }
    }

    async fn execute(&self, arguments: &str) -> anyhow::Result<ToolResult> {
        let args: ModeSwitchArgs = serde_json::from_str(arguments)?;

        let new_mode = match args.mode.as_str() {
            "auto" => AgentMode::Auto,
            "coding" => AgentMode::Coding {
                plan_approval: args.plan_approval,
                project_path: args.project_path.map(PathBuf::from),
            },
            "bypass" => AgentMode::BypassPermissions,
            "swarm" => AgentMode::Swarm {
                parallelism: args.parallelism.clamp(1, 10),
            },
            other => return Ok(ToolResult::error(format!("Unknown mode: {}", other))),
        };

        let description = match &new_mode {
            AgentMode::Auto => "Auto (heuristic mode)".to_string(),
            AgentMode::BypassPermissions => "BypassPermissions (autonomous mode)".to_string(),
            AgentMode::Coding { plan_approval, .. } => {
                format!("Coding (plan_approval={})", plan_approval)
            }
            AgentMode::Swarm { parallelism } => {
                format!("Swarm (parallelism={})", parallelism)
            }
        };

        *self.mode.write().unwrap() = new_mode;
        Ok(ToolResult::success(format!(
            "Switched to {} mode",
            description
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn switch_to_coding() {
        let mode = Arc::new(RwLock::new(AgentMode::Auto));
        let tool = ModeSwitchTool::new(mode.clone());

        let r = tool
            .execute(r#"{"mode":"coding","plan_approval":true}"#)
            .await
            .unwrap();
        assert!(!r.is_error);
        assert!(r.output.contains("Coding"));

        assert!(mode.read().unwrap().is_coding());
    }

    #[tokio::test]
    async fn switch_to_bypass() {
        let mode = Arc::new(RwLock::new(AgentMode::Auto));
        let tool = ModeSwitchTool::new(mode.clone());

        tool.execute(r#"{"mode":"bypass"}"#).await.unwrap();
        assert!(mode.read().unwrap().bypass_permissions());
    }

    #[tokio::test]
    async fn switch_back_to_auto() {
        let mode = Arc::new(RwLock::new(AgentMode::BypassPermissions));
        let tool = ModeSwitchTool::new(mode.clone());

        tool.execute(r#"{"mode":"auto"}"#).await.unwrap();
        assert!(matches!(*mode.read().unwrap(), AgentMode::Auto));
    }
}
