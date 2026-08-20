//! CronTool — schedule, list, and manage recurring agent tasks.

use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;

use super::traits::*;
use crate::cron_scheduler::CronScheduler;

pub struct CronTool {
    scheduler: Arc<CronScheduler>,
    channel: String,
    chat_id: String,
    model: String,
}

impl CronTool {
    pub fn new(
        scheduler: Arc<CronScheduler>,
        channel: impl Into<String>,
        chat_id: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            scheduler,
            channel: channel.into(),
            chat_id: chat_id.into(),
            model: model.into(),
        }
    }
}

#[derive(Deserialize)]
struct CronArgs {
    /// Action: "schedule", "list", "enable", "disable", "delete"
    action: String,
    /// Cron expression (required for schedule)
    #[serde(default)]
    cron: String,
    #[serde(default)]
    run_at: String,
    /// Goal/task description (required for schedule)
    #[serde(default)]
    goal: String,
    /// Schedule ID (required for enable/disable/delete)
    #[serde(default)]
    id: String,
}

#[async_trait]
impl Tool for CronTool {
    fn name(&self) -> &str {
        "cron"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "cron".to_string(),
            description: "Schedule recurring agent tasks using cron expressions. \
                Actions: schedule (create), list, enable, disable, delete."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["schedule", "list", "enable", "disable", "delete"],
                        "description": "Operation to perform"
                    },
                    "cron": {
                        "type": "string",
                        "description": "Cron expression (e.g. '0 9 * * MON' = every Monday at 9am)"
                    },
                    "run_at": {
                        "type": "string",
                        "description": "RFC3339 timestamp for a one-shot task"
                    },
                    "goal": {
                        "type": "string",
                        "description": "What the agent should do when this schedule fires"
                    },
                    "id": {
                        "type": "string",
                        "description": "Schedule ID (required for enable/disable/delete)"
                    }
                },
                "required": ["action"]
            }),
        }
    }

    async fn execute(&self, arguments: &str) -> anyhow::Result<ToolResult> {
        let args: CronArgs = serde_json::from_str(arguments)?;

        match args.action.as_str() {
            "schedule" => {
                if args.cron.is_empty() && args.run_at.is_empty() {
                    return Ok(ToolResult::error("cron or run_at is required"));
                }
                if !args.cron.is_empty() && !args.run_at.is_empty() {
                    return Ok(ToolResult::error("provide cron or run_at, not both"));
                }
                if args.goal.is_empty() {
                    return Ok(ToolResult::error("goal is required"));
                }
                let result = if args.run_at.is_empty() {
                    self.scheduler
                        .add(
                            "agent_task",
                            &args.cron,
                            &args.goal,
                            &self.channel,
                            &self.chat_id,
                            &self.model,
                        )
                        .await
                } else {
                    match chrono::DateTime::parse_from_rfc3339(&args.run_at) {
                        Ok(run_at) => {
                            self.scheduler
                                .add_once(
                                    "agent_task",
                                    run_at.with_timezone(&chrono::Utc),
                                    &args.goal,
                                    &self.channel,
                                    &self.chat_id,
                                    &self.model,
                                )
                                .await
                        }
                        Err(error) => Err(anyhow::anyhow!("invalid run_at: {error}")),
                    }
                };
                match result {
                    Ok(id) => Ok(ToolResult::success(format!(
                        "Scheduled '{}' with id={} (cron: {})",
                        args.goal, id, args.cron
                    ))),
                    Err(e) => Ok(ToolResult::error(format!("Failed to schedule: {}", e))),
                }
            }

            "list" => {
                let jobs = self.scheduler.list().await?;
                if jobs.is_empty() {
                    return Ok(ToolResult::success("No schedules configured."));
                }
                let lines: Vec<String> = jobs
                    .iter()
                    .map(|j| {
                        let id = j.id.as_ref().map(|id| id.to_string()).unwrap_or_default();
                        format!(
                            "- [{}] id={} cron='{}' goal='{}' enabled={}",
                            if j.enabled { "✓" } else { "✗" },
                            id,
                            j.schedule,
                            j.task,
                            j.enabled
                        )
                    })
                    .collect();
                Ok(ToolResult::success(lines.join("\n")))
            }

            "enable" => {
                if args.id.is_empty() {
                    return Ok(ToolResult::error("id is required"));
                }
                match self.scheduler.enable(&args.id).await {
                    Ok(true) => Ok(ToolResult::success(format!("Enabled {}", args.id))),
                    Ok(false) => Ok(ToolResult::error("Job not found".to_string())),
                    Err(e) => Ok(ToolResult::error(e.to_string())),
                }
            }

            "disable" => {
                if args.id.is_empty() {
                    return Ok(ToolResult::error("id is required"));
                }
                match self.scheduler.disable(&args.id).await {
                    Ok(true) => Ok(ToolResult::success(format!("Disabled {}", args.id))),
                    Ok(false) => Ok(ToolResult::error("Job not found".to_string())),
                    Err(e) => Ok(ToolResult::error(e.to_string())),
                }
            }

            "delete" => {
                if args.id.is_empty() {
                    return Ok(ToolResult::error("id is required"));
                }
                match self.scheduler.remove(&args.id).await {
                    Ok(true) => Ok(ToolResult::success(format!("Deleted {}", args.id))),
                    Ok(false) => Ok(ToolResult::error("Job not found".to_string())),
                    Err(e) => Ok(ToolResult::error(e.to_string())),
                }
            }

            other => Ok(ToolResult::error(format!("Unknown action: {}", other))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cron_scheduler::CronScheduler;

    #[tokio::test]
    async fn schedule_and_list() {
        let scheduler = Arc::new(CronScheduler::new_noop());
        let tool = CronTool::new(scheduler, "cli", "cli", "test-model");

        let r = tool
            .execute(r#"{"action":"schedule","cron":"0 0 9 * * * *","goal":"daily standup"}"#)
            .await
            .unwrap();
        assert!(!r.is_error, "{}", r.output);

        let l = tool.execute(r#"{"action":"list"}"#).await.unwrap();
        assert!(l.output.contains("daily standup"));
    }

    #[tokio::test]
    async fn one_shot_schedule_is_stored() {
        let scheduler = Arc::new(CronScheduler::new_noop());
        let tool = CronTool::new(scheduler.clone(), "cli", "cli", "test-model");
        let run_at = (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
        let result = tool
            .execute(&format!(
                r#"{{"action":"schedule","run_at":"{run_at}","goal":"remind me"}}"#
            ))
            .await
            .unwrap();
        assert!(!result.is_error, "{}", result.output);
        assert!(scheduler.list().await.unwrap()[0].one_shot);
    }

    #[tokio::test]
    async fn invalid_cron_fails() {
        let scheduler = Arc::new(CronScheduler::new_noop());
        let tool = CronTool::new(scheduler, "cli", "cli", "test-model");
        let r = tool
            .execute(r#"{"action":"schedule","cron":"not-valid","goal":"test"}"#)
            .await
            .unwrap();
        assert!(r.is_error);
    }
}
