//! BuildRunnerTool — agent-callable wrapper around `BuildRunner`.
//!
//! Exposes cargo build/test/run cycles to the agent loop as a single tool so
//! the model can compile, parse diagnostics, and retry without shelling out
//! repeatedly through `exec`. Returns a structured summary of compiler
//! errors the model can act on directly.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;

use crate::agent::build_runner::{BuildRunner, BuildRunnerConfig};
use crate::policy::ExecutionPolicy;

use super::traits::*;

pub struct BuildRunnerTool {
    workspace: PathBuf,
    config: BuildRunnerConfig,
    policy: Arc<ExecutionPolicy>,
}

impl BuildRunnerTool {
    pub fn new(
        workspace: PathBuf,
        config: BuildRunnerConfig,
        policy: Arc<ExecutionPolicy>,
    ) -> Self {
        Self {
            workspace,
            config,
            policy,
        }
    }

    pub fn with_default_config(workspace: PathBuf, policy: Arc<ExecutionPolicy>) -> Self {
        Self::new(workspace, BuildRunnerConfig::default(), policy)
    }
}

#[derive(Deserialize)]
struct BuildRunnerArgs {
    /// What to run: "build", "test", or "cycle" (build then test).
    #[serde(default)]
    action: String,
    /// Override the package filter (`cargo <cmd> -p <name>`).
    #[serde(default)]
    package: String,
    /// Override the per-invocation timeout in seconds.
    #[serde(default)]
    timeout: Option<u64>,
    /// Override the max retry count.
    #[serde(default)]
    max_retries: Option<usize>,
}

#[async_trait]
impl Tool for BuildRunnerTool {
    fn name(&self) -> &str {
        "build_runner"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "build_runner".to_string(),
            description: "Run cargo build/test/cycle with structured error parsing and retry. \
                Returns a summary of compiler errors (file:line:col + message) the model \
                can act on directly, plus the raw output. \
                'build' = cargo build, 'test' = cargo test, 'cycle' = build then test."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["build", "test", "cycle"],
                        "description": "build = cargo build, test = cargo test, cycle = build then test (default build)"
                    },
                    "package": {
                        "type": "string",
                        "description": "Package filter (cargo -p <name>). Empty = whole workspace."
                    },
                    "timeout": {
                        "type": "integer",
                        "description": "Per-invocation timeout in seconds"
                    },
                    "max_retries": {
                        "type": "integer",
                        "description": "Maximum retries on failure"
                    }
                }
            }),
        }
    }

    async fn execute(&self, arguments: &str) -> anyhow::Result<ToolResult> {
        if !self.policy.allow_shell {
            return ExecutionPolicy::deny("Build execution is disabled by policy");
        }

        let args: BuildRunnerArgs = serde_json::from_str(arguments).unwrap_or(BuildRunnerArgs {
            action: String::new(),
            package: String::new(),
            timeout: None,
            max_retries: None,
        });

        let mut config = self.config.clone();
        if !args.package.is_empty() {
            config.package = args.package;
        }
        if let Some(t) = args.timeout {
            config.timeout_secs = t;
        }
        if let Some(r) = args.max_retries {
            config.max_retries = r;
        }

        let runner = BuildRunner::new(self.workspace.clone(), config);
        let action = if args.action.is_empty() {
            "build"
        } else {
            args.action.as_str()
        };

        let output = match action {
            "build" => {
                let r = runner.run_build().await;
                if r.success {
                    ToolResult::success(r.summary())
                } else {
                    ToolResult::error(format!(
                        "{}\n\n--- raw output ---\n{}",
                        r.summary(),
                        r.raw_output
                    ))
                }
            }
            "test" => {
                let r = runner.run_test().await;
                if r.success {
                    ToolResult::success(r.summary())
                } else {
                    ToolResult::error(format!(
                        "{}\n\n--- raw output ---\n{}",
                        r.summary(),
                        r.raw_output
                    ))
                }
            }
            "cycle" => {
                let (build, test) = runner.run_cycle().await;
                if !build.success {
                    ToolResult::error(format!(
                        "{}\n\n--- raw output ---\n{}",
                        build.summary(),
                        build.raw_output
                    ))
                } else if let Some(t) = test {
                    if t.success {
                        ToolResult::success(format!("{}\n{}", build.summary(), t.summary()))
                    } else {
                        ToolResult::error(format!(
                            "{}\n\n--- raw output ---\n{}",
                            t.summary(),
                            t.raw_output
                        ))
                    }
                } else {
                    ToolResult::success(build.summary())
                }
            }
            other => ToolResult::error(format!(
                "Unknown action: {} (use build, test, or cycle)",
                other
            )),
        };

        Ok(output)
    }
}
