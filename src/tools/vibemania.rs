//! Vibemania tool — autonomous agent coding & codebase exploration.
//! Integrates Subspace's code-first agent system as a composable tool.

use async_trait::async_trait;
use serde::Deserialize;
use std::path::PathBuf;

use super::traits::*;

pub struct VibemaniaTool {
    workspace: PathBuf,
}

impl VibemaniaTool {
    pub fn new(workspace: PathBuf) -> Self {
        Self { workspace }
    }
}

#[derive(Deserialize)]
struct VibemaiaArgs {
    goal: String,
    #[serde(default)]
    parallel: usize,
    /// Model for the orchestrator (planner). Defaults to configured heavy_model.
    orchestrator_model: Option<String>,
    /// Model for each runner (executor). Defaults to configured fast_model.
    runner_model: Option<String>,
}

#[async_trait]
impl Tool for VibemaniaTool {
    fn name(&self) -> &str {
        "vibemania"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "vibemania".to_string(),
            description: "Run Vibemania for autonomous code exploration, implementation, and codebase analysis. Supports parallel execution with configurable orchestrator/runner models for agent swarms.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "goal": {
                        "type": "string",
                        "description": "What to build/fix/analyze (e.g., 'add WebSocket support', 'fix auth bug', 'explore database schema')"
                    },
                    "parallel": {
                        "type": "integer",
                        "description": "Number of parallel workers (1-8, default 2)",
                        "minimum": 1,
                        "maximum": 8,
                        "default": 2
                    },
                    "orchestrator_model": {
                        "type": "string",
                        "description": "Model for the orchestrator/planner (e.g. 'claude-sonnet-4-6', 'claude-opus-4-6'). Defaults to heavy_model."
                    },
                    "runner_model": {
                        "type": "string",
                        "description": "Model for each parallel runner/executor (e.g. 'claude-haiku-4-5-20251001'). Defaults to fast_model."
                    }
                },
                "required": ["goal"]
            }),
        }
    }

    async fn execute(&self, arguments: &str) -> anyhow::Result<ToolResult> {
        let args: VibemaiaArgs = serde_json::from_str(arguments)?;

        // Check if vibemania is available
        let vibemania_bin = self
            .workspace
            .parent()
            .map(|p| p.join("vibemania/target/release/vibemania"))
            .filter(|p| p.exists());

        if vibemania_bin.is_none() {
            return Ok(ToolResult::error(
                "Vibemania not found. Clone/build from atechnology-company/vibemania first.",
            ));
        }

        let parallel = args.parallel.clamp(1, 8);

        let vibemania_bin = vibemania_bin.expect("checked above");

        // Spawn vibemania directly instead of shelling out through bash.
        // Flags first, then `--` + goal so a leading `-` cannot be a flag
        // (Jules #61). Scrub secrets from the inherited environment.
        let mut cmd = tokio::process::Command::new(vibemania_bin);
        cmd.current_dir(&self.workspace)
            .arg("run")
            .arg("--parallel")
            .arg(parallel.to_string())
            .kill_on_drop(true);
        crate::tools::child_proc::scrub(&mut cmd);

        if let Some(ref omodel) = args.orchestrator_model {
            cmd.arg("--orchestrator-model").arg(omodel);
        }
        if let Some(ref rmodel) = args.runner_model {
            cmd.arg("--runner-model").arg(rmodel);
        }
        cmd.arg("--").arg(&args.goal);

        let output = cmd.output().await?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        let result = if !stderr.is_empty() {
            format!("stdout:\n{}\n\nstderr:\n{}", stdout, stderr)
        } else {
            stdout.to_string()
        };

        Ok(if output.status.success() {
            ToolResult::success(result)
        } else {
            ToolResult::error(result)
        })
    }
}

#[cfg(all(test, unix))]
mod tests {

    use super::*;
    use crate::tools::traits::Tool;
    use std::os::unix::fs::PermissionsExt;

    #[tokio::test]
    async fn missing_binary_returns_error() {
        let temp_dir = tempfile::tempdir().unwrap();
        let workspace = temp_dir.path().join("workspace");

        let tool = VibemaniaTool::new(workspace);

        let args = serde_json::json!({
            "goal": "do something"
        })
        .to_string();

        let result = tool.execute(&args).await.unwrap();

        assert!(result.is_error);
        assert!(result.output.contains("Vibemania not found"));
    }

    #[test]
    fn test_name_and_spec() {
        let tool = VibemaniaTool::new(PathBuf::from("/tmp"));
        assert_eq!(tool.name(), "vibemania");
        let spec = tool.spec();
        assert_eq!(spec.name, "vibemania");
        assert!(spec
            .description
            .contains("Vibemania for autonomous code exploration"));
        assert!(spec
            .parameters
            .get("properties")
            .unwrap()
            .get("goal")
            .is_some());
    }

    #[tokio::test]
    async fn test_execute_invalid_args() {
        let tool = VibemaniaTool::new(PathBuf::from("/tmp"));
        let result = tool.execute("not valid json").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_execute_binary_not_found() {
        let tool = VibemaniaTool::new(PathBuf::from("/does/not/exist"));
        let args = r#"{"goal": "do something"}"#;
        let result = tool.execute(args).await.unwrap();
        assert!(result.is_error);
        assert!(result.output.contains("Vibemania not found"));
    }

    #[tokio::test]
    async fn successful_execution_returns_stdout() {
        let temp_dir = tempfile::tempdir().unwrap();
        let workspace = temp_dir.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();

        let bin_dir = workspace.parent().unwrap().join("vibemania/target/release");
        std::fs::create_dir_all(&bin_dir).unwrap();

        let bin_path = bin_dir.join("vibemania");

        let script = r#"#!/bin/sh
printf "Success: %s" "$*"
"#;
        std::fs::write(&bin_path, script).unwrap();

        let mut perms = std::fs::metadata(&bin_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&bin_path, perms).unwrap();

        let tool = VibemaniaTool::new(workspace);

        let args = serde_json::json!({
            "goal": "do something",
            "parallel": 4,
            "orchestrator_model": "heavy-model",
            "runner_model": "fast-model"
        })
        .to_string();

        let result = tool.execute(&args).await.unwrap();

        assert!(!result.is_error);
        assert!(result.output.starts_with("Success: run --parallel 4"));
        assert!(result.output.contains("--orchestrator-model heavy-model"));
        assert!(result.output.contains("--runner-model fast-model"));
        assert!(result.output.contains("-- do something"));
    }

    #[tokio::test]
    async fn failed_execution_returns_error() {
        let temp_dir = tempfile::tempdir().unwrap();
        let workspace = temp_dir.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();

        let bin_dir = workspace.parent().unwrap().join("vibemania/target/release");
        std::fs::create_dir_all(&bin_dir).unwrap();

        let bin_path = bin_dir.join("vibemania");

        let script = r#"#!/bin/sh
echo "Some output"
echo "Some error" >&2
false
"#;
        std::fs::write(&bin_path, script).unwrap();

        let mut perms = std::fs::metadata(&bin_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&bin_path, perms).unwrap();

        let tool = VibemaniaTool::new(workspace);

        let args = serde_json::json!({
            "goal": "do something"
        })
        .to_string();

        let result = tool.execute(&args).await.unwrap();

        assert!(result.is_error);
        let text = result.output;
        assert!(text.contains("stdout:\nSome output\n\n\nstderr:\nSome error\n"));
    }
}
