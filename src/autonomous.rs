//! Autonomous coding mode.
//!
//! 24/7 workspace-driven loop: reads TODO.md (or any task ledger),
//! runs the agent on pending items, validates changes with tests,
//! and optionally commits/pushes only on success.
//!
//! Port from hermes-rs `autonomous.rs`.
//!
//! ponytail: single-workspace, sequential. Parallel workspace support
//! per Hermes-RS pattern can be added when multi-repo management is needed.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::agent::NullChannel;
use crate::autonomous_isolation::AutonomousCheckout;
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
    /// Create a loop for a prepared checkout. Supplied agents and tools are not sandboxed.
    ///
    /// A path to a shared workspace cannot be used in place of a checkout:
    /// ```compile_fail
    /// use apollo::autonomous::{AutonomousConfig, AutonomousLoop};
    /// AutonomousLoop::new(AutonomousConfig::default(), std::path::PathBuf::from("."));
    /// ```
    pub fn new(config: AutonomousConfig, checkout: AutonomousCheckout) -> Self {
        let workspace = checkout.workspace().to_path_buf();
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

    fn save_status_to_file(status: &AutonomousStatus, path: &Path) -> bool {
        match toml::to_string_pretty(status) {
            Ok(content) => {
                if let Err(e) = std::fs::write(path, &content) {
                    tracing::warn!("[autonomous] failed to save status: {}", e);
                    return false;
                }
                true
            }
            Err(e) => {
                tracing::warn!("[autonomous] failed to serialize status: {}", e);
                false
            }
        }
    }

    pub fn start_fresh(&mut self) {
        if self.status.state == AutonomousState::Running {
            tracing::error!("[autonomous] interrupted run requires manual recovery");
            return;
        }
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

    /// Start the autonomous loop, stopping when a run needs recovery.
    pub async fn run(mut self, agent: std::sync::Arc<crate::agent::AgentRunner>) {
        tracing::info!(
            "[autonomous] starting (interval={}s, workspace={:?})",
            self.config.interval_secs,
            self.workspace
        );

        loop {
            tokio::time::sleep(Duration::from_secs(self.config.interval_secs)).await;
            self.step(&agent).await;
            if self.status.paused
                || self.status.state == AutonomousState::Failed
                || self.status.state == AutonomousState::Running
            {
                tracing::warn!("[autonomous] stopped; inspect the workspace before recovery");
                return;
            }
        }
    }

    /// Execute a single iteration of the autonomous loop.
    pub async fn step(&mut self, agent: &crate::agent::AgentRunner) {
        if self.status.paused || self.status.state == AutonomousState::Paused {
            tracing::debug!("[autonomous] paused, skipping");
            return;
        }

        if self.status.state == AutonomousState::Running {
            tracing::error!("[autonomous] interrupted run requires manual recovery");
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

        let baseline = if self.config.git_remote.is_empty() || self.config.test_command.is_empty() {
            None
        } else {
            match check_git_workspace(&self.workspace).await {
                Ok(baseline) => Some(baseline),
                Err(e) => {
                    tracing::error!("[autonomous] refusing to start: {}", e);
                    self.status.paused = true;
                    self.status.state = AutonomousState::Failed;
                    Self::save_status_to_file(&self.status, &self.status_path);
                    return;
                }
            }
        };

        self.status.state = AutonomousState::Running;
        self.status.current_task = Some("processing TODO.md".into());
        if !Self::save_status_to_file(&self.status, &self.status_path) {
            tracing::error!("[autonomous] refusing to start without a saved running status");
            return;
        }

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
                            let publication = if let Some(baseline) = &baseline {
                                git_commit_and_push(
                                    "autonomous: auto-commit",
                                    &self.config.git_remote,
                                    &self.config.git_branch,
                                    &self.workspace,
                                    &self.status_path,
                                    baseline,
                                )
                                .await
                            } else {
                                Ok(())
                            };
                            if let Err(e) = publication {
                                tracing::error!("[autonomous] publication refused: {}", e);
                                self.status.state = AutonomousState::Failed;
                                self.status.paused = true;
                                Self::save_status_to_file(&self.status, &self.status_path);
                                return;
                            }
                            self.status.consecutive_failures = 0;
                            self.status.state = AutonomousState::Succeeded;
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

struct GitBaseline {
    head: String,
    prefix: String,
    root: PathBuf,
    untracked: BTreeSet<String>,
    all_untracked: BTreeSet<String>,
}

async fn git(cwd: &Path, args: &[&str]) -> anyhow::Result<Vec<u8>> {
    let output = tokio::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .await?;
    anyhow::ensure!(output.status.success(), "git {} failed", args[0]);
    Ok(output.stdout)
}

fn git_paths(output: Vec<u8>) -> anyhow::Result<BTreeSet<String>> {
    output
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| Ok(std::str::from_utf8(path)?.to_owned()))
        .collect()
}

/// Reject a shared index or modified tracked files before starting a turn.
async fn check_git_workspace(workspace: &Path) -> anyhow::Result<GitBaseline> {
    for args in [
        &["diff", "--cached", "--quiet"][..],
        &["diff", "--quiet", "--", "."][..],
    ] {
        let output = tokio::process::Command::new("git")
            .args(args)
            .current_dir(workspace)
            .output()
            .await?;
        anyhow::ensure!(
            output.status.success(),
            "Git preflight failed or found existing tracked changes"
        );
    }
    let head = String::from_utf8(git(workspace, &["rev-parse", "--verify", "HEAD"]).await?)?;
    let prefix = String::from_utf8(git(workspace, &["rev-parse", "--show-prefix"]).await?)?;
    let root = String::from_utf8(git(workspace, &["rev-parse", "--show-toplevel"]).await?)?;
    let root = PathBuf::from(root.strip_suffix('\n').unwrap_or(&root));
    let untracked = git_paths(
        git(
            workspace,
            &[
                "ls-files",
                "--others",
                "--exclude-standard",
                "-z",
                "--",
                ".",
            ],
        )
        .await?,
    )?;
    let all_untracked =
        git_paths(git(&root, &["ls-files", "--others", "--exclude-standard", "-z"]).await?)?;
    Ok(GitBaseline {
        head: head.trim_end_matches('\n').to_owned(),
        prefix: prefix.strip_suffix('\n').unwrap_or(&prefix).to_owned(),
        root,
        untracked,
        all_untracked,
    })
}

async fn git_commit_and_push(
    message: &str,
    remote: &str,
    branch: &str,
    cwd: &Path,
    status_path: &Path,
    baseline: &GitBaseline,
) -> anyhow::Result<()> {
    let head = String::from_utf8(git(cwd, &["rev-parse", "--verify", "HEAD"]).await?)?;
    anyhow::ensure!(
        head.trim_end_matches('\n') == baseline.head,
        "HEAD changed during the turn"
    );
    anyhow::ensure!(
        git(cwd, &["diff", "--cached", "--name-only", "-z"])
            .await?
            .is_empty(),
        "index changed during the turn"
    );
    let all_tracked = git_paths(
        git(
            &baseline.root,
            &["diff", "--name-only", "--no-renames", "-z"],
        )
        .await?,
    )?;
    let all_untracked = git_paths(
        git(
            &baseline.root,
            &["ls-files", "--others", "--exclude-standard", "-z"],
        )
        .await?,
    )?;
    anyhow::ensure!(
        all_tracked
            .iter()
            .all(|path| path.starts_with(&baseline.prefix))
            && all_untracked
                .difference(&baseline.all_untracked)
                .all(|path| path.starts_with(&baseline.prefix)),
        "changes appeared outside the workspace"
    );
    let mut paths = git_paths(
        git(
            cwd,
            &[
                "diff",
                "--name-only",
                "--no-renames",
                "--relative",
                "-z",
                "--",
                ".",
            ],
        )
        .await?,
    )?;
    let untracked = git_paths(
        git(
            cwd,
            &[
                "ls-files",
                "--others",
                "--exclude-standard",
                "-z",
                "--",
                ".",
            ],
        )
        .await?,
    )?;
    paths.extend(untracked.difference(&baseline.untracked).cloned());
    let resolved_workspace = tokio::fs::canonicalize(cwd).await?;
    let resolved_status = tokio::fs::canonicalize(status_path).await?;
    if let Some(relative) = resolved_status
        .strip_prefix(resolved_workspace)
        .ok()
        .and_then(Path::to_str)
    {
        paths.remove(relative);
    }
    anyhow::ensure!(!paths.is_empty(), "no eligible changes to commit");
    let expected: BTreeSet<String> = paths
        .iter()
        .map(|path| format!("{}{path}", baseline.prefix))
        .collect();
    let mut add = vec!["--literal-pathspecs", "add", "--"];
    add.extend(paths.iter().map(String::as_str));
    git(cwd, &add).await?;
    let staged = git_paths(
        git(
            cwd,
            &["diff", "--cached", "--name-only", "--no-renames", "-z"],
        )
        .await?,
    )?;
    anyhow::ensure!(staged == expected, "index contains unexpected paths");
    let mut commit = vec![
        "--literal-pathspecs",
        "commit",
        "--only",
        "-m",
        message,
        "--",
    ];
    commit.extend(paths.iter().map(String::as_str));
    git(cwd, &commit).await?;
    let commit_id = String::from_utf8(git(cwd, &["rev-parse", "HEAD"]).await?)?;
    let commit_id = commit_id.trim_end_matches('\n');
    let parent_ref = format!("{commit_id}^");
    let parent = String::from_utf8(git(cwd, &["rev-parse", &parent_ref]).await?)?;
    anyhow::ensure!(
        parent.trim_end_matches('\n') == baseline.head,
        "commit has unexpected parent"
    );
    let committed = git_paths(
        git(
            cwd,
            &[
                "diff-tree",
                "--no-commit-id",
                "--name-only",
                "--no-renames",
                "-r",
                "-z",
                commit_id,
            ],
        )
        .await?,
    )?;
    anyhow::ensure!(committed == expected, "commit contains unexpected paths");
    git(cwd, &["check-ref-format", "--branch", branch]).await?;
    let target = format!("refs/heads/{branch}");
    let remote_head = String::from_utf8(
        git(
            cwd,
            &["ls-remote", "--exit-code", "--refs", "--", remote, &target],
        )
        .await?,
    )?;
    anyhow::ensure!(
        remote_head.split_whitespace().next() == Some(baseline.head.as_str()),
        "remote branch differs from the starting commit"
    );
    let refspec = format!("{commit_id}:{target}");
    git(cwd, &["push", "--", remote, &refspec]).await?;
    tracing::info!("[autonomous] published commit {} to {}", commit_id, target);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    async fn checkout(dir: &Path) -> AutonomousCheckout {
        let source = dir.join("source");
        std::fs::create_dir(&source).unwrap();
        for args in [
            vec!["init"],
            vec!["config", "user.name", "Test"],
            vec!["config", "user.email", "test@example.invalid"],
        ] {
            assert!(tokio::process::Command::new("git")
                .current_dir(&source)
                .args(args)
                .status()
                .await
                .unwrap()
                .success());
        }
        std::fs::write(source.join("TODO.md"), "- [ ] task").unwrap();
        for args in [vec!["add", "TODO.md"], vec!["commit", "-m", "initial"]] {
            assert!(tokio::process::Command::new("git")
                .current_dir(&source)
                .args(args)
                .status()
                .await
                .unwrap()
                .success());
        }
        crate::autonomous_isolation::prepare_autonomous_checkout(&source, dir, Path::new("TODO.md"))
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn test_status_persistence() {
        let dir = tempdir().unwrap();
        let checkout = checkout(dir.path()).await;
        let config = AutonomousConfig {
            status_path: "test-status.toml".into(),
            ..AutonomousConfig::default()
        };
        let aloop = AutonomousLoop::new(config, checkout.clone());
        assert_eq!(aloop.status.state, AutonomousState::Idle);

        // Save should create file
        AutonomousLoop::save_status_to_file(
            &aloop.status,
            &checkout.workspace().join("test-status.toml"),
        );
        let path = checkout.workspace().join("test-status.toml");
        assert!(path.exists());

        // Test again
        AutonomousLoop::save_status_to_file(&aloop.status, &path);
        assert!(path.exists());
    }

    #[tokio::test]
    async fn test_status_roundtrip() {
        let dir = tempdir().unwrap();
        let checkout = checkout(dir.path()).await;
        let config = AutonomousConfig {
            status_path: "test-status.toml".into(),
            ..AutonomousConfig::default()
        };

        // First creation
        let aloop = AutonomousLoop::new(config.clone(), checkout.clone());
        AutonomousLoop::save_status_to_file(
            &aloop.status,
            &checkout.workspace().join("test-status.toml"),
        );

        // Second creation should load saved state
        let aloop2 = AutonomousLoop::new(config, checkout);
        assert_eq!(aloop2.status.state, aloop.status.state);
    }
}
