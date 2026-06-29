//! Self-update support for repository-backed local installs.

use std::path::{Path, PathBuf};

use crate::config::SelfUpdateConfig;

#[derive(Debug, Clone)]
pub struct SelfUpdater {
    workspace: PathBuf,
    config: SelfUpdateConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateOutcome {
    NoRepo,
    Disabled,
    DirtyWorktree,
    AlreadyCurrent,
    Updated { restarted: bool },
}

impl SelfUpdater {
    pub fn new(workspace: PathBuf, config: SelfUpdateConfig) -> Self {
        Self { workspace, config }
    }

    pub fn start(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(self.run_loop())
    }

    async fn run_loop(self) {
        if !self.config.enabled {
            tracing::info!("self-update disabled");
            return;
        }

        let mut interval =
            tokio::time::interval(tokio::time::Duration::from_secs(self.config.interval_secs));
        interval.tick().await;

        loop {
            interval.tick().await;
            match self.run_once().await {
                Ok(UpdateOutcome::Updated { restarted }) => {
                    tracing::info!(restarted, "self-update applied");
                }
                Ok(UpdateOutcome::AlreadyCurrent) => {
                    tracing::debug!("self-update: already current");
                }
                Ok(UpdateOutcome::DirtyWorktree) => {
                    tracing::warn!("self-update: skipping dirty worktree");
                }
                Ok(UpdateOutcome::NoRepo) => {
                    tracing::debug!("self-update: workspace is not a git repo");
                }
                Ok(UpdateOutcome::Disabled) => break,
                Err(error) => {
                    tracing::warn!(error = %error, "self-update failed");
                }
            }
        }
    }

    pub async fn run_once(&self) -> anyhow::Result<UpdateOutcome> {
        if !self.config.enabled {
            return Ok(UpdateOutcome::Disabled);
        }

        if !is_git_repo(&self.workspace).await? {
            return Ok(UpdateOutcome::NoRepo);
        }

        if !worktree_is_clean(&self.workspace).await? {
            return Ok(UpdateOutcome::DirtyWorktree);
        }

        git_fetch(&self.workspace, &self.config.remote, &self.config.branch).await?;

        if current_head(&self.workspace).await?
            == upstream_head(&self.workspace, &self.config).await?
        {
            return Ok(UpdateOutcome::AlreadyCurrent);
        }

        git_fast_forward(&self.workspace, &self.config.remote, &self.config.branch).await?;
        cargo_build_release(&self.workspace).await?;
        let restarted = maybe_restart_service(&self.workspace, &self.config).await?;

        Ok(UpdateOutcome::Updated { restarted })
    }
}

async fn is_git_repo(workspace: &Path) -> anyhow::Result<bool> {
    let output = run_git(workspace, &["rev-parse", "--is-inside-work-tree"]).await?;
    Ok(output.trim() == "true")
}

async fn worktree_is_clean(workspace: &Path) -> anyhow::Result<bool> {
    let output = run_git(workspace, &["status", "--porcelain"]).await?;
    Ok(output.trim().is_empty())
}

async fn git_fetch(workspace: &Path, remote: &str, branch: &str) -> anyhow::Result<()> {
    let _ = run_git(workspace, &["fetch", remote, branch, "--quiet"]).await?;
    Ok(())
}

async fn current_head(workspace: &Path) -> anyhow::Result<String> {
    run_git(workspace, &["rev-parse", "HEAD"]).await
}

async fn upstream_head(workspace: &Path, config: &SelfUpdateConfig) -> anyhow::Result<String> {
    run_git(
        workspace,
        &[
            "rev-parse",
            &format!("refs/remotes/{}/{}", config.remote, config.branch),
        ],
    )
    .await
}

async fn git_fast_forward(workspace: &Path, remote: &str, branch: &str) -> anyhow::Result<()> {
    let _ = run_git(
        workspace,
        &["merge", "--ff-only", &format!("{}/{}", remote, branch)],
    )
    .await?;
    Ok(())
}

async fn cargo_build_release(workspace: &Path) -> anyhow::Result<()> {
    let output = tokio::process::Command::new("cargo")
        .arg("build")
        .arg("--release")
        .current_dir(workspace)
        .output()
        .await?;

    if !output.status.success() {
        anyhow::bail!(
            "cargo build --release failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(())
}

async fn maybe_restart_service(
    workspace: &Path,
    config: &SelfUpdateConfig,
) -> anyhow::Result<bool> {
    let Some(service) = &config.restart_service else {
        return Ok(false);
    };

    let output = tokio::process::Command::new("systemctl")
        .arg("--user")
        .arg("restart")
        .arg(service)
        .current_dir(workspace)
        .output()
        .await?;

    if output.status.success() {
        return Ok(true);
    }

    tracing::warn!(
        service = %service,
        stderr = %String::from_utf8_lossy(&output.stderr),
        "self-update build succeeded but service restart failed"
    );
    Ok(false)
}

async fn run_git(workspace: &Path, args: &[&str]) -> anyhow::Result<String> {
    let output = tokio::process::Command::new("git")
        .args(args)
        .current_dir(workspace)
        .output()
        .await?;

    if !output.status.success() {
        anyhow::bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}
