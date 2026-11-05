//! CodingSwarmTool — deploys parallel AgentRunner workers for coding tasks.
//!
//! When the LLM identifies a large coding goal that can be broken into independent
//! subtasks, it calls this tool with a list of tasks. Each task is executed by a
//! headless AgentRunner worker. Results are collected and returned as a summary.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use crate::agent::AgentRunner;
use crate::tools::{Tool, ToolResult, ToolSpec};

pub struct CodingSwarmTool {
    runner: Arc<AgentRunner>,
    default_parallelism: usize,
}

impl CodingSwarmTool {
    pub fn new(runner: Arc<AgentRunner>, default_parallelism: usize) -> Self {
        Self {
            runner,
            default_parallelism,
        }
    }
}

#[async_trait]
impl Tool for CodingSwarmTool {
    fn name(&self) -> &str {
        "coding_swarm"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "coding_swarm".to_string(),
            description: "Deploy parallel coding agents to work on independent subtasks. \
                Each agent has its own conversation, but shares the workspace with other agents. \
                Use when a large coding goal can be decomposed into non-overlapping parts \
                (e.g. different modules, files, or features)."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "tasks": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "List of independent coding subtasks to execute in parallel. Each task should be self-contained."
                    },
                    "parallelism": {
                        "type": "integer",
                        "description": "Max concurrent agents (default: 3, max: 10).",
                        "minimum": 1,
                        "maximum": 10
                    }
                },
                "required": ["tasks"]
            }),
        }
    }

    async fn execute(&self, arguments: &str) -> anyhow::Result<ToolResult> {
        let args: Value = serde_json::from_str(arguments)?;

        let tasks: Vec<String> = match args["tasks"].as_array() {
            Some(arr) => arr
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect(),
            None => return Ok(ToolResult::error("tasks must be an array of strings")),
        };

        if tasks.is_empty() {
            return Ok(ToolResult::error("tasks array is empty"));
        }

        if tasks.len() > 20 {
            return Ok(ToolResult::error(
                "too many tasks (max 20) — decompose into fewer, larger subtasks",
            ));
        }

        let parallelism = args["parallelism"]
            .as_u64()
            .map(|n| n.min(10) as usize)
            .unwrap_or(self.default_parallelism);

        tracing::info!(
            "CodingSwarm: {} tasks, parallelism={}",
            tasks.len(),
            parallelism
        );

        let results = self
            .runner
            .clone()
            .deploy_coding_swarm(
                tasks,
                &format!("swarm_{}", uuid::Uuid::new_v4()),
                parallelism,
            )
            .await;

        let output = results
            .iter()
            .enumerate()
            .map(|(i, (task, result))| {
                let task_preview: String = task.chars().take(80).collect();
                format!("### Agent {} — {}\n{}", i + 1, task_preview, result)
            })
            .collect::<Vec<_>>()
            .join("\n\n---\n\n");

        Ok(ToolResult::success(format!(
            "## Swarm Complete ({} agents)\n\n{}",
            results.len(),
            output
        )))
    }
}
