//! WorktreeTool — create and remove isolated git worktrees for parallel/sandboxed work.
//!
//! Each worktree gets its own branch and working directory under `.worktrees/`.
//! Use before risky refactors or parallel agent tasks — remove when done.

use std::path::PathBuf;

use async_trait::async_trait;
use serde::Deserialize;

use super::traits::*;

pub struct WorktreeTool {
    workspace: PathBuf,
}

impl WorktreeTool {
    pub fn new(workspace: PathBuf) -> Self {
        Self { workspace }
    }

    fn worktrees_dir(&self) -> PathBuf {
        self.workspace.join(".worktrees")
    }
}

#[derive(Deserialize)]
struct WorktreeArgs {
    /// Action: "add", "remove", "list"
    action: String,
    /// Worktree name (used as directory name and branch name)
    #[serde(default)]
    name: String,
    /// Base branch (default: current HEAD). Only for "add".
    #[serde(default)]
    base_branch: String,
}

#[async_trait]
impl Tool for WorktreeTool {
    fn name(&self) -> &str {
        "worktree"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "worktree".to_string(),
            description: "Manage isolated git worktrees for parallel or sandboxed work. \
                'add' creates a new branch + worktree in .worktrees/<name>. \
                'remove' deletes the worktree and its branch. \
                'list' shows all active worktrees."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["add", "remove", "list"],
                        "description": "add = create worktree, remove = clean up, list = show all"
                    },
                    "name": {
                        "type": "string",
                        "description": "Worktree name (becomes the branch name and directory name)"
                    },
                    "base_branch": {
                        "type": "string",
                        "description": "Branch to base the new worktree on (default: current HEAD)"
                    }
                },
                "required": ["action"]
            }),
        }
    }

    async fn execute(&self, arguments: &str) -> anyhow::Result<ToolResult> {
        let args: WorktreeArgs = serde_json::from_str(arguments)?;

        match args.action.as_str() {
            "list" => {
                let output = run_git(&self.workspace, &["worktree", "list", "--porcelain"]).await?;
                Ok(ToolResult::success(if output.is_empty() {
                    "No worktrees.".to_string()
                } else {
                    output
                }))
            }

            "add" => {
                if args.name.is_empty() {
                    return Ok(ToolResult::error("name is required"));
                }
                // Sanitize name
                let name: String = args
                    .name
                    .chars()
                    .filter(|c| c.is_alphanumeric() || matches!(c, '-' | '_'))
                    .collect();
                if name.is_empty() {
                    return Ok(ToolResult::error("invalid name"));
                }

                let worktree_path = self.worktrees_dir().join(&name);
                let branch = format!("worktree/{}", name);

                // Create base dir
                tokio::fs::create_dir_all(self.worktrees_dir()).await?;

                // git worktree add -b <branch> <path> [base]
                let mut cmd_args = vec!["worktree", "add", "-b"];
                let branch_ref = branch.as_str();
                let path_str = worktree_path.to_string_lossy().to_string();
                cmd_args.extend_from_slice(&[branch_ref, &path_str]);

                if !args.base_branch.is_empty() {
                    // We'll pass as separate arg below
                }

                let mut full_args = vec!["worktree", "add", "-b", branch_ref, &path_str];
                let base = args.base_branch.clone();
                if !base.is_empty() {
                    full_args.push(&base);
                }

                let output = run_git(&self.workspace, &full_args).await?;

                Ok(ToolResult::success(format!(
                    "Created worktree '{}' at {} on branch {}\n{}",
                    name,
                    worktree_path.display(),
                    branch,
                    output
                )))
            }

            "remove" => {
                if args.name.is_empty() {
                    return Ok(ToolResult::error("name is required"));
                }
                let name: String = args
                    .name
                    .chars()
                    .filter(|c| c.is_alphanumeric() || matches!(c, '-' | '_'))
                    .collect();

                let worktree_path = self.worktrees_dir().join(&name);
                let branch = format!("worktree/{}", name);

                // Remove worktree
                let path_str = worktree_path.to_string_lossy().to_string();
                let _ = run_git(
                    &self.workspace,
                    &["worktree", "remove", "--force", &path_str],
                )
                .await;

                // Delete the branch
                let _ = run_git(&self.workspace, &["branch", "-D", &branch]).await;

                Ok(ToolResult::success(format!(
                    "Removed worktree '{}' and branch {}",
                    name, branch
                )))
            }

            other => Ok(ToolResult::error(format!("Unknown action: {}", other))),
        }
    }
}

async fn run_git(workspace: &PathBuf, args: &[&str]) -> anyhow::Result<String> {
    let output = tokio::process::Command::new("git")
        .args(args)
        .current_dir(workspace)
        .output()
        .await?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if !output.status.success() && !stderr.is_empty() {
        return Err(anyhow::anyhow!("git error: {}", stderr.trim()));
    }

    Ok(if stdout.is_empty() { stderr } else { stdout })
}
