//! Autonomous coding mode.
//!
//! 24/7 workspace-driven loop: reads TODO.md (or any task ledger),
//! runs the agent on pending items, validates changes with tests,
//! and optionally commits/pushes only on success.
//!
//! Port from hermes-rs `autonomous.rs`.
//!
//! pontytail: single-workspace, sequential. Parallel workspace support
//! per Hermes-RS pattern can be added when multi-repo management is needed.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::agent::NullChannel;
use crate::channels::IncomingMessage;

/// Configuration for autonomous mode.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutonomousConfig {
    /// How often to run a check cycle (seconds)
    pub interval_secs: u64,
    /// Path to task ledger (default: TODO.md)
    pub todo_path: String,
    /// Path to write status file
    pub status_path: String,
    /// Test command (empty = skip tests)
    pub test_command: String,
    /// Remote to push to (empty = no push)
    pub git_remote: String,
    /// Branch to push to
    pub git_branch: String,
}

impl Default for AutonomousConfig {
    fn default() -> Self {
        Self {
            interval_secs: 300,
            todo_path: "TODO.md".to_string(),
            status_path: "autonomous-status.toml".to_string(),
            test_command: "cargo test --workspace".to_string(),
            git_remote: "origin".to_string(),
            git_branch: "main".to_string(),
        }
    }
}

/// State of the autonomous loop.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AutonomousState {
    Idle,
    Running,
    Failed,
    Paused,
    Succeeded,
}

/// Status persisted to disk across restarts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutonomousStatus {
    pub state: AutonomousState,
    pub current_task: Option<String>,
    pub last_run: Option<String>,
    pub last_result: Option<String>,
    pub consecutive_failures: usize,
    pub paused: bool,
}

impl AutonomousStatus {
    fn new() -> Self {
        Self {
            state: AutonomousState::Idle,
            current_task: None,
            last_run: None,
            last_result: None,
            consecutive_failures: 0,
            paused: false,
        }
    }
}

/// The autonomous loop controller.
pub struct AutonomousLoop {
    config: AutonomousConfig,
    workspace: PathBuf,
    status: AutonomousStatus,
    status_path: PathBuf,
}

impl AutonomousLoop {
    /// Create a new autonomous loop for a given workspace.
    pub fn new(config: AutonomousConfig, workspace: PathBuf) -> Self {
        let status_path = workspace.join(&config.status_path);
        let status = Self::load_status(&status_path);
        Self {
            config,
            workspace,
            status,
            status_path,
        }
    }

    fn load_status(path: &Path) -> AutonomousStatus {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_else(AutonomousStatus::new)
    }

    fn save_status_to_file(status: &AutonomousStatus, path: &Path) {
        match toml::to_string_pretty(status) {
            Ok(content) => {
                if let Err(e) = std::fs::write(path, &content) {
                    tracing::warn!("[autonomous] failed to save status: {}", e);
                }
            }
            Err(e) => tracing::warn!("[autonomous] failed to serialize status: {}", e),
        }
    }

    /// Start the autonomous loop. Runs indefinitely.
    pub async fn run(self, agent: std::sync::Arc<crate::agent::AgentRunner>) {
        let config = self.config.clone();
        let workspace = self.workspace.clone();
        let mut status = self.status;

        tracing::info!(
            "[autonomous] starting (interval={}s, workspace={:?})",
            config.interval_secs,
            workspace
        );

        loop {
            tokio::time::sleep(Duration::from_secs(config.interval_secs)).await;

            if status.paused || status.state == AutonomousState::Paused {
                tracing::debug!("[autonomous] paused, skipping");
                continue;
            }

            // Read the task ledger
            let todo_path = workspace.join(&config.todo_path);
            let todo_content = match std::fs::read_to_string(&todo_path) {
                Ok(c) => c.trim().to_string(),
                Err(e) => {
                    tracing::debug!("[autonomous] no TODO.md ({}), skipping", e);
                    status.state = AutonomousState::Idle;
                    Self::save_status_to_file(&status, &self.status_path);
                    continue;
                }
            };

            if todo_content.is_empty() || todo_content == "## Implemented\n\n## Pending\n" {
                status.state = AutonomousState::Idle;
                Self::save_status_to_file(&status, &self.status_path);
                continue;
            }

            // Check workspace is clean before starting a new task
            if status.state == AutonomousState::Idle
                || status.state == AutonomousState::Succeeded
                || status.state == AutonomousState::Failed
            {
                let workspace_changed = workspace_has_changes(&workspace);
                if workspace_changed {
                    tracing::info!("[autonomous] workspace has changes, stashing before new task");
                    git_stash(&workspace);
                }
            }

            status.state = AutonomousState::Running;
            status.current_task = Some("processing TODO.md".into());
            Self::save_status_to_file(&status, &self.status_path);

            tracing::info!("[autonomous] running agent on TODO.md");
            let instruction = format!(
                "Working autonomously. Task ledger:\n\n{}\n\n\
                 Read the TODO.md, pick the next pending item, implement it, \
                 and validate with: {}",
                todo_content, config.test_command
            );

            let null_ch = NullChannel::new("autonomous");
            match agent
                .handle_message(
                    &IncomingMessage {
                        id: uuid::Uuid::new_v4().to_string(),
                        sender_id: "autonomous".into(),
                        sender_name: Some("Autonomous".into()),
                        chat_id: "autonomous".into(),
                        text: instruction,
                        is_group: false,
                        reply_to: None,
                        timestamp: chrono::Utc::now(),
                    },
                    &null_ch,
                )
                .await
            {
                Ok(response) => {
                    status.last_result =
                        Some(response.chars().take(500).collect::<String>() + "...");
                    status.last_run = Some(chrono::Utc::now().to_rfc3339());

                    // Run validation tests
                    if !config.test_command.is_empty() {
                        match run_test_command(&config.test_command, &workspace) {
                            Ok(true) => {
                                tracing::info!("[autonomous] tests passed");
                                status.consecutive_failures = 0;
                                status.state = AutonomousState::Succeeded;

                                // Git commit + push
                                if !config.git_remote.is_empty() {
                                    git_commit_and_push(
                                        "autonomous: auto-commit",
                                        &config.git_remote,
                                        &config.git_branch,
                                        &workspace,
                                    );
                                }
                            }
                            Ok(false) => {
                                tracing::warn!("[autonomous] tests failed");
                                status.consecutive_failures += 1;
                                status.state = AutonomousState::Failed;

                                if status.consecutive_failures >= 3 {
                                    status.paused = true;
                                    status.state = AutonomousState::Paused;
                                    tracing::warn!(
                                        "[autonomous] paused after {} consecutive failures",
                                        status.consecutive_failures
                                    );
                                }
                            }
                            Err(e) => {
                                tracing::error!("[autonomous] test error: {}", e);
                                status.state = AutonomousState::Failed;
                            }
                        }
                    } else {
                        status.state = AutonomousState::Succeeded;
                    }
                }
                Err(e) => {
                    tracing::error!("[autonomous] agent error: {}", e);
                    status.state = AutonomousState::Failed;
                    status.consecutive_failures += 1;

                    if status.consecutive_failures >= 3 {
                        status.paused = true;
                        tracing::warn!(
                            "[autonomous] paused after {} consecutive failures",
                            status.consecutive_failures
                        );
                    }
                }
            }

            Self::save_status_to_file(&status, &self.status_path);
        }
    }
}

/// Run a test command, return true if it passes.
fn run_test_command(cmd: &str, cwd: &Path) -> anyhow::Result<bool> {
    tracing::info!("[autonomous] running tests: {}", cmd);
    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .current_dir(cwd)
        .output()?;

    if output.status.success() {
        Ok(true)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::warn!(
            "[autonomous] tests failed:\n{}",
            &stderr[..stderr.len().min(1000)]
        );
        Ok(false)
    }
}

/// Check if workspace has uncommitted changes.
fn workspace_has_changes(workspace: &Path) -> bool {
    let output = std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(workspace)
        .output();

    match output {
        Ok(out) => !out.stdout.is_empty(),
        Err(_) => false,
    }
}

/// Stash workspace changes.
fn git_stash(workspace: &Path) {
    let _ = std::process::Command::new("git")
        .args(["stash", "push", "-m", "autonomous: pre-task stash"])
        .current_dir(workspace)
        .output();
}

/// Commit all changes and push.
fn git_commit_and_push(message: &str, remote: &str, branch: &str, cwd: &Path) {
    let _ = std::process::Command::new("git")
        .args(["add", "-A"])
        .current_dir(cwd)
        .output();

    let _ = std::process::Command::new("git")
        .args(["commit", "-m", message])
        .current_dir(cwd)
        .output();

    tracing::info!("[autonomous] pushing to {}/{}", remote, branch);
    if let Ok(output) = std::process::Command::new("git")
        .args(["push", remote, branch])
        .current_dir(cwd)
        .output()
    {
        if output.status.success() {
            tracing::info!("[autonomous] push successful");
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::warn!(
                "[autonomous] push failed: {}",
                &stderr[..stderr.len().min(500)]
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_status_persistence() {
        let dir = tempdir().unwrap();
        let config = AutonomousConfig {
            status_path: "test-status.toml".into(),
            ..AutonomousConfig::default()
        };
        let aloop = AutonomousLoop::new(config, dir.path().to_path_buf());
        assert_eq!(aloop.status.state, AutonomousState::Idle);

        // Save should create file
        AutonomousLoop::save_status_to_file(&aloop.status, &dir.path().join("test-status.toml"));
        let path = dir.path().join("test-status.toml");
        assert!(path.exists());

        // Test again
        AutonomousLoop::save_status_to_file(&aloop.status, &dir.path().join("test-status.toml"));
        assert!(path.exists());
    }

    #[test]
    fn test_status_roundtrip() {
        let dir = tempdir().unwrap();
        let config = AutonomousConfig {
            status_path: "test-status.toml".into(),
            ..AutonomousConfig::default()
        };

        // First creation
        let aloop = AutonomousLoop::new(config.clone(), dir.path().to_path_buf());
        AutonomousLoop::save_status_to_file(&aloop.status, &dir.path().join("test-status.toml"));

        // Second creation should load saved state
        let aloop2 = AutonomousLoop::new(config, dir.path().to_path_buf());
        assert_eq!(aloop2.status.state, aloop.status.state);
    }
}
