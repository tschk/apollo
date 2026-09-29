//! Self-update support for repository-backed local installs.

use std::path::{Path, PathBuf};

use crate::config::SelfUpdateConfig;

#[async_trait::async_trait]
pub trait UpdateEnv: Send + Sync {
    async fn is_git_repo(&self, workspace: &Path) -> anyhow::Result<bool>;
    async fn worktree_is_clean(&self, workspace: &Path) -> anyhow::Result<bool>;
    async fn git_fetch(&self, workspace: &Path, remote: &str, branch: &str) -> anyhow::Result<()>;
    async fn current_head(&self, workspace: &Path) -> anyhow::Result<String>;
    async fn upstream_head(
        &self,
        workspace: &Path,
        config: &SelfUpdateConfig,
    ) -> anyhow::Result<String>;
    async fn git_fast_forward(
        &self,
        workspace: &Path,
        remote: &str,
        branch: &str,
    ) -> anyhow::Result<()>;
    async fn cargo_build_release(&self, workspace: &Path) -> anyhow::Result<()>;
    async fn maybe_restart_service(
        &self,
        workspace: &Path,
        config: &SelfUpdateConfig,
    ) -> anyhow::Result<bool>;
}

#[derive(Debug, Clone, Default)]
pub struct RealUpdateEnv;

#[async_trait::async_trait]
impl UpdateEnv for RealUpdateEnv {
    async fn is_git_repo(&self, workspace: &Path) -> anyhow::Result<bool> {
        is_git_repo(workspace).await
    }
    async fn worktree_is_clean(&self, workspace: &Path) -> anyhow::Result<bool> {
        worktree_is_clean(workspace).await
    }
    async fn git_fetch(&self, workspace: &Path, remote: &str, branch: &str) -> anyhow::Result<()> {
        git_fetch(workspace, remote, branch).await
    }
    async fn current_head(&self, workspace: &Path) -> anyhow::Result<String> {
        current_head(workspace).await
    }
    async fn upstream_head(
        &self,
        workspace: &Path,
        config: &SelfUpdateConfig,
    ) -> anyhow::Result<String> {
        upstream_head(workspace, config).await
    }
    async fn git_fast_forward(
        &self,
        workspace: &Path,
        remote: &str,
        branch: &str,
    ) -> anyhow::Result<()> {
        git_fast_forward(workspace, remote, branch).await
    }
    async fn cargo_build_release(&self, workspace: &Path) -> anyhow::Result<()> {
        cargo_build_release(workspace).await
    }
    async fn maybe_restart_service(
        &self,
        workspace: &Path,
        config: &SelfUpdateConfig,
    ) -> anyhow::Result<bool> {
        maybe_restart_service(workspace, config).await
    }
}

#[derive(Debug, Clone)]
pub struct SelfUpdater<E = RealUpdateEnv> {
    workspace: PathBuf,
    config: SelfUpdateConfig,
    env: E,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateOutcome {
    NoRepo,
    Disabled,
    DirtyWorktree,
    AlreadyCurrent,
    Updated { restarted: bool },
}

impl SelfUpdater<RealUpdateEnv> {
    pub fn new(workspace: PathBuf, config: SelfUpdateConfig) -> Self {
        Self {
            workspace,
            config,
            env: RealUpdateEnv,
        }
    }
}

impl<E: UpdateEnv + 'static> SelfUpdater<E> {
    pub fn with_env(workspace: PathBuf, config: SelfUpdateConfig, env: E) -> Self {
        Self {
            workspace,
            config,
            env,
        }
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

        if !self.env.is_git_repo(&self.workspace).await? {
            return Ok(UpdateOutcome::NoRepo);
        }

        if !self.env.worktree_is_clean(&self.workspace).await? {
            return Ok(UpdateOutcome::DirtyWorktree);
        }

        self.env
            .git_fetch(&self.workspace, &self.config.remote, &self.config.branch)
            .await?;

        if self.env.current_head(&self.workspace).await?
            == self
                .env
                .upstream_head(&self.workspace, &self.config)
                .await?
        {
            return Ok(UpdateOutcome::AlreadyCurrent);
        }

        self.env
            .git_fast_forward(&self.workspace, &self.config.remote, &self.config.branch)
            .await?;
        self.env.cargo_build_release(&self.workspace).await?;
        let restarted = self
            .env
            .maybe_restart_service(&self.workspace, &self.config)
            .await?;

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::Mutex;

    #[derive(Clone)]
    struct MockUpdateEnv {
        pub is_git_repo: bool,
        pub worktree_is_clean: bool,
        pub current_head: String,
        pub upstream_head: String,
        pub fetch_success: bool,
        pub fast_forward_success: bool,
        pub build_success: bool,
        pub restart_success: bool,

        pub methods_called: Arc<Mutex<Vec<String>>>,
    }

    impl Default for MockUpdateEnv {
        fn default() -> Self {
            Self {
                is_git_repo: true,
                worktree_is_clean: true,
                current_head: "aaaa".to_string(),
                upstream_head: "bbbb".to_string(),
                fetch_success: true,
                fast_forward_success: true,
                build_success: true,
                restart_success: true,
                methods_called: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    #[async_trait::async_trait]
    impl UpdateEnv for MockUpdateEnv {
        async fn is_git_repo(&self, _workspace: &Path) -> anyhow::Result<bool> {
            self.methods_called
                .lock()
                .unwrap()
                .push("is_git_repo".to_string());
            Ok(self.is_git_repo)
        }
        async fn worktree_is_clean(&self, _workspace: &Path) -> anyhow::Result<bool> {
            self.methods_called
                .lock()
                .unwrap()
                .push("worktree_is_clean".to_string());
            Ok(self.worktree_is_clean)
        }
        async fn git_fetch(
            &self,
            _workspace: &Path,
            _remote: &str,
            _branch: &str,
        ) -> anyhow::Result<()> {
            self.methods_called
                .lock()
                .unwrap()
                .push("git_fetch".to_string());
            if self.fetch_success {
                Ok(())
            } else {
                Err(anyhow::anyhow!("error"))
            }
        }
        async fn current_head(&self, _workspace: &Path) -> anyhow::Result<String> {
            self.methods_called
                .lock()
                .unwrap()
                .push("current_head".to_string());
            Ok(self.current_head.clone())
        }
        async fn upstream_head(
            &self,
            _workspace: &Path,
            _config: &SelfUpdateConfig,
        ) -> anyhow::Result<String> {
            self.methods_called
                .lock()
                .unwrap()
                .push("upstream_head".to_string());
            Ok(self.upstream_head.clone())
        }
        async fn git_fast_forward(
            &self,
            _workspace: &Path,
            _remote: &str,
            _branch: &str,
        ) -> anyhow::Result<()> {
            self.methods_called
                .lock()
                .unwrap()
                .push("git_fast_forward".to_string());
            if self.fast_forward_success {
                Ok(())
            } else {
                Err(anyhow::anyhow!("error"))
            }
        }
        async fn cargo_build_release(&self, _workspace: &Path) -> anyhow::Result<()> {
            self.methods_called
                .lock()
                .unwrap()
                .push("cargo_build_release".to_string());
            if self.build_success {
                Ok(())
            } else {
                Err(anyhow::anyhow!("error"))
            }
        }
        async fn maybe_restart_service(
            &self,
            _workspace: &Path,
            _config: &SelfUpdateConfig,
        ) -> anyhow::Result<bool> {
            self.methods_called
                .lock()
                .unwrap()
                .push("maybe_restart_service".to_string());
            Ok(self.restart_success)
        }
    }

    fn mock_config(enabled: bool) -> SelfUpdateConfig {
        SelfUpdateConfig {
            enabled,
            interval_secs: 10,
            remote: "origin".to_string(),
            branch: "main".to_string(),
            restart_service: Some("apollo.service".to_string()),
        }
    }

    #[tokio::test]
    async fn test_disabled() {
        let env = MockUpdateEnv::default();
        let updater = SelfUpdater::with_env(PathBuf::from("."), mock_config(false), env);
        let outcome = updater.run_once().await.unwrap();
        assert_eq!(outcome, UpdateOutcome::Disabled);
    }

    #[tokio::test]
    async fn test_no_repo() {
        let mut env = MockUpdateEnv::default();
        env.is_git_repo = false;
        let updater = SelfUpdater::with_env(PathBuf::from("."), mock_config(true), env);
        let outcome = updater.run_once().await.unwrap();
        assert_eq!(outcome, UpdateOutcome::NoRepo);
    }

    #[tokio::test]
    async fn test_dirty_worktree() {
        let mut env = MockUpdateEnv::default();
        env.worktree_is_clean = false;
        let updater = SelfUpdater::with_env(PathBuf::from("."), mock_config(true), env);
        let outcome = updater.run_once().await.unwrap();
        assert_eq!(outcome, UpdateOutcome::DirtyWorktree);
    }

    #[tokio::test]
    async fn test_already_current() {
        let mut env = MockUpdateEnv::default();
        env.current_head = "aaaa".to_string();
        env.upstream_head = "aaaa".to_string();
        let updater = SelfUpdater::with_env(PathBuf::from("."), mock_config(true), env);
        let outcome = updater.run_once().await.unwrap();
        assert_eq!(outcome, UpdateOutcome::AlreadyCurrent);
    }

    #[tokio::test]
    async fn test_updated_restarted() {
        let env = MockUpdateEnv::default();
        let updater = SelfUpdater::with_env(PathBuf::from("."), mock_config(true), env.clone());
        let outcome = updater.run_once().await.unwrap();
        assert_eq!(outcome, UpdateOutcome::Updated { restarted: true });

        let calls = env.methods_called.lock().unwrap().clone();
        assert_eq!(
            calls,
            vec![
                "is_git_repo",
                "worktree_is_clean",
                "git_fetch",
                "current_head",
                "upstream_head",
                "git_fast_forward",
                "cargo_build_release",
                "maybe_restart_service"
            ]
        );
    }

    #[tokio::test]
    async fn test_updated_no_restart() {
        let mut env = MockUpdateEnv::default();
        env.restart_success = false;
        let updater = SelfUpdater::with_env(PathBuf::from("."), mock_config(true), env);
        let outcome = updater.run_once().await.unwrap();
        assert_eq!(outcome, UpdateOutcome::Updated { restarted: false });
    }
}
