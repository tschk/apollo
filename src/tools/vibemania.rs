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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_vibemania_not_found() {
        let dir = tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        fs::create_dir_all(&workspace).unwrap();

        let tool = VibemaniaTool::new(workspace);
        let args = r#"{"goal": "test"}"#;

        let result = tool.execute(args).await.unwrap();
        assert!(result.is_error);
        assert!(result.output.contains("Vibemania not found"));
    }

    #[tokio::test]
    async fn test_vibemania_execute_success() {
        let dir = tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        fs::create_dir_all(&workspace).unwrap();

        let bin_dir = dir.path().join("vibemania/target/release");
        fs::create_dir_all(&bin_dir).unwrap();
        let bin_path = bin_dir.join("vibemania");

        let script_content = "#!/bin/sh\necho \"success output\"\n";
        fs::write(&bin_path, script_content).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&bin_path).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&bin_path, perms).unwrap();
        }

        let tool = VibemaniaTool::new(workspace);
        let args = r#"{"goal": "test goal", "parallel": 4}"#;

        let result = tool.execute(args).await.unwrap();
        assert!(!result.is_error);
        assert!(result.output.contains("success output"));
    }
}
