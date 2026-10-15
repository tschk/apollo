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

    pub fn start_fresh(&mut self) {
        self.status = AutonomousStatus::new();
        Self::save_status_to_file(&self.status, &self.status_path);
    }

    pub fn resume(&mut self) {
        self.status.paused = false;
        if self.status.state == AutonomousState::Paused {
            self.status.state = AutonomousState::Idle;
        }
        Self::save_status_to_file(&self.status, &self.status_path);
    }

    /// Start the autonomous loop. Runs indefinitely.
    pub async fn run(mut self, agent: std::sync::Arc<crate::agent::AgentRunner>) {
        tracing::info!(
            "[autonomous] starting (interval={}s, workspace={:?})",
            self.config.interval_secs,
            self.workspace
        );

        loop {
            tokio::time::sleep(Duration::from_secs(self.config.interval_secs)).await;
            self.step(&agent).await;
        }
    }

    /// Execute a single iteration of the autonomous loop.
    pub async fn step(&mut self, agent: &crate::agent::AgentRunner) {
        if self.status.paused || self.status.state == AutonomousState::Paused {
            tracing::debug!("[autonomous] paused, skipping");
            return;
        }

        // Read the task ledger
        let todo_path = self.workspace.join(&self.config.todo_path);
        let todo_content = match std::fs::read_to_string(&todo_path) {
            Ok(c) => c.trim().to_string(),
            Err(e) => {
                tracing::debug!("[autonomous] no TODO.md ({}), skipping", e);
                self.status.state = AutonomousState::Idle;
                Self::save_status_to_file(&self.status, &self.status_path);
                return;
            }
        };

        if todo_content.is_empty() || todo_content == "## Implemented\n\n## Pending\n" {
            self.status.state = AutonomousState::Idle;
            Self::save_status_to_file(&self.status, &self.status_path);
            return;
        }

        // Check workspace is clean before starting a new task
        if self.status.state == AutonomousState::Idle
            || self.status.state == AutonomousState::Succeeded
            || self.status.state == AutonomousState::Failed
        {
            let workspace_changed = workspace_has_changes(&self.workspace).await;
            if workspace_changed {
                tracing::info!("[autonomous] workspace has changes, stashing before new task");
                git_stash(&self.workspace).await;
            }
        }

        self.status.state = AutonomousState::Running;
        self.status.current_task = Some("processing TODO.md".into());
        Self::save_status_to_file(&self.status, &self.status_path);

        tracing::info!("[autonomous] running agent on TODO.md");
        let instruction = format!(
            "Working autonomously. Task ledger:\n\n{}\n\n\
             Read the TODO.md, pick the next pending item, implement it, \
             and validate with: {}",
            todo_content, self.config.test_command
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
                self.status.last_result =
                    Some(response.chars().take(500).collect::<String>() + "...");
                self.status.last_run = Some(chrono::Utc::now().to_rfc3339());

                // Run validation tests
                if !self.config.test_command.is_empty() {
                    match run_test_command(&self.config.test_command, &self.workspace).await {
                        Ok(true) => {
                            tracing::info!("[autonomous] tests passed");
                            self.status.consecutive_failures = 0;
                            self.status.state = AutonomousState::Succeeded;

                            // Git commit + push
                            if !self.config.git_remote.is_empty() {
                                git_commit_and_push(
                                    "autonomous: auto-commit",
                                    &self.config.git_remote,
                                    &self.config.git_branch,
                                    &self.workspace,
                                )
                                .await;
                            }
                        }
                        Ok(false) => {
                            tracing::warn!("[autonomous] tests failed");
                            self.status.consecutive_failures += 1;
                            self.status.state = AutonomousState::Failed;

                            if self.status.consecutive_failures >= 3 {
                                self.status.paused = true;
                                self.status.state = AutonomousState::Paused;
                                tracing::warn!(
                                    "[autonomous] paused after {} consecutive failures",
                                    self.status.consecutive_failures
                                );
                            }
                        }
                        Err(e) => {
                            tracing::error!("[autonomous] test error: {}", e);
                            self.status.state = AutonomousState::Failed;
                        }
                    }
                } else {
                    self.status.state = AutonomousState::Succeeded;
                }
            }
            Err(e) => {
                tracing::error!("[autonomous] agent error: {}", e);
                self.status.state = AutonomousState::Failed;
                self.status.consecutive_failures += 1;

                if self.status.consecutive_failures >= 3 {
                    self.status.paused = true;
                    tracing::warn!(
                        "[autonomous] paused after {} consecutive failures",
                        self.status.consecutive_failures
                    );
                }
            }
        }

        Self::save_status_to_file(&self.status, &self.status_path);
    }
}

/// Run a test command, return true if it passes.
async fn run_test_command(cmd: &str, cwd: &Path) -> anyhow::Result<bool> {
    tracing::info!("[autonomous] running tests: {}", cmd);
    let output = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .current_dir(cwd)
        .output()
        .await?;

    if output.status.success() {
        Ok(true)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::warn!(
            "[autonomous] tests failed:\n{}",
            crate::text::truncate_chars(&stderr, 1000)
        );
        Ok(false)
    }
}

/// Check if workspace has uncommitted changes.
async fn workspace_has_changes(workspace: &Path) -> bool {
    let output = tokio::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(workspace)
        .output()
        .await;

    match output {
        Ok(out) => !out.stdout.is_empty(),
        Err(_) => false,
    }
}

/// Stash workspace changes.
async fn git_stash(workspace: &Path) {
    let _ = tokio::process::Command::new("git")
        .args(["stash", "push", "-m", "autonomous: pre-task stash"])
        .current_dir(workspace)
        .output()
        .await;
}

/// Commit all changes and push.
async fn git_commit_and_push(message: &str, remote: &str, branch: &str, cwd: &Path) {
    let _ = tokio::process::Command::new("git")
        .args(["add", "-A"])
        .current_dir(cwd)
        .output()
        .await;

    let _ = tokio::process::Command::new("git")
        .args(["commit", "-m", message])
        .current_dir(cwd)
        .output()
        .await;

    tracing::info!("[autonomous] pushing to {}/{}", remote, branch);
    if let Ok(output) = tokio::process::Command::new("git")
        .args(["push", remote, branch])
        .current_dir(cwd)
        .output()
        .await
    {
        if output.status.success() {
            tracing::info!("[autonomous] push successful");
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::warn!(
                "[autonomous] push failed: {}",
                crate::text::truncate_chars(&stderr, 500)
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
