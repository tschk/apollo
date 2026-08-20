//! Metric-driven autonomous experimentation.
//!
//! Autoresearch is deliberately separate from [`crate::autonomous`]. The
//! autonomous TODO loop completes one task and validates it; this loop keeps a
//! measurable objective, accepts only improvements, and resumes from a small
//! durable ledger.

use std::cmp::Ordering;
use std::collections::HashSet;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::agent::NullChannel;
use crate::channels::IncomingMessage;

const WALL_CLOCK_BUDGET_EXHAUSTED: &str = "autoresearch wall-clock budget exhausted";
const MAX_IGNORED_FILE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_IGNORED_SNAPSHOT_BYTES: u64 = 64 * 1024 * 1024;
const VOLATILE_IGNORED_ROOTS: &[&str] =
    &["target", "node_modules", ".venv", "vendor", "dist", "build"];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AutoresearchConfig {
    /// Human-readable objective supplied to the agent.
    pub objective: String,
    /// Trusted local command that prints one numeric metric value.
    pub metric_command: String,
    /// `minimize` or `maximize`.
    pub direction: String,
    /// Optional command that must pass before the metric is considered.
    pub validation_command: String,
    /// Number of attempts for the validation command.
    pub validation_retries: usize,
    /// Timeout for each validation and metric process.
    pub command_timeout_secs: u64,
    /// Number of metric samples per measurement.
    pub samples: usize,
    /// Minimum accepted improvement as a percentage. Defaults to strict.
    pub min_improvement_percent: f64,
    /// Maximum number of iterations for one invocation.
    pub max_iterations: usize,
    /// Wall-clock budget for one invocation. Zero disables the budget.
    pub max_duration_secs: u64,
    /// Durable ledger path, relative to the workspace unless absolute.
    pub ledger_path: String,
    /// Model override. Empty uses the runner's default.
    pub model: String,
}

impl Default for AutoresearchConfig {
    fn default() -> Self {
        Self {
            objective: String::new(),
            metric_command: String::new(),
            direction: "minimize".to_string(),
            validation_command: String::new(),
            validation_retries: 1,
            command_timeout_secs: 300,
            samples: 3,
            min_improvement_percent: 0.0,
            max_iterations: 10,
            max_duration_secs: 1800,
            ledger_path: ".apollo/autoresearch-ledger.toml".to_string(),
            model: String::new(),
        }
    }
}

impl AutoresearchConfig {
    fn validate(&self) -> anyhow::Result<()> {
        if self.objective.trim().is_empty() {
            bail!("autoresearch objective must not be empty");
        }
        if self.metric_command.trim().is_empty() {
            bail!("autoresearch metric_command must not be empty");
        }
        if !matches!(
            self.direction.trim().to_ascii_lowercase().as_str(),
            "minimize" | "maximize"
        ) {
            bail!("autoresearch direction must be 'minimize' or 'maximize'");
        }
        if self.samples == 0 {
            bail!("autoresearch samples must be at least 1");
        }
        if self.max_iterations == 0 {
            bail!("autoresearch max_iterations must be at least 1");
        }
        if self.validation_retries == 0 {
            bail!("autoresearch validation_retries must be at least 1");
        }
        if self.command_timeout_secs == 0 {
            bail!("autoresearch command_timeout_secs must be at least 1");
        }
        if !self.min_improvement_percent.is_finite() || self.min_improvement_percent < 0.0 {
            bail!("autoresearch min_improvement_percent must be a finite non-negative number");
        }
        Ok(())
    }

    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("reading autoresearch spec {}", path.display()))?;
        let config: Self = toml::from_str(&content)
            .with_context(|| format!("parsing autoresearch spec {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    fn ledger_path(&self, workspace: &Path) -> PathBuf {
        let path = PathBuf::from(&self.ledger_path);
        if path.is_absolute() {
            path
        } else {
            workspace.join(path)
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ExperimentDecision {
    Baseline,
    Accepted,
    Rejected,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExperimentRecord {
    pub iteration: usize,
    pub hypothesis: String,
    pub commit: Option<String>,
    pub metric: Option<f64>,
    pub baseline: f64,
    pub delta_percent: Option<f64>,
    pub decision: ExperimentDecision,
    pub reason: String,
    pub timestamp: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoresearchLedger {
    pub objective: String,
    pub direction: String,
    pub best_metric: f64,
    pub best_commit: String,
    /// Branch on which this experiment started and accepted commits land.
    #[serde(default)]
    pub branch: String,
    /// Stable chat id so a fresh run cannot inherit another run's history.
    #[serde(default)]
    pub chat_id: String,
    /// Hash of the metric/validation definition used to produce this ledger.
    #[serde(default)]
    pub spec_fingerprint: String,
    pub records: Vec<ExperimentRecord>,
}

impl AutoresearchLedger {
    fn new(config: &AutoresearchConfig, baseline: f64, commit: String, branch: String) -> Self {
        Self {
            objective: config.objective.clone(),
            direction: config.direction.trim().to_ascii_lowercase(),
            best_metric: baseline,
            best_commit: commit,
            branch,
            chat_id: format!("autoresearch-{}", uuid::Uuid::new_v4()),
            spec_fingerprint: spec_fingerprint(config),
            records: vec![ExperimentRecord {
                iteration: 0,
                hypothesis: "Initial measurement".to_string(),
                commit: None,
                metric: Some(baseline),
                baseline,
                delta_percent: Some(0.0),
                decision: ExperimentDecision::Baseline,
                reason: "baseline".to_string(),
                timestamp: chrono::Utc::now().to_rfc3339(),
            }],
        }
    }
}

/// Controller for one bounded autoresearch run.
pub struct AutoresearchLoop {
    config: AutoresearchConfig,
    workspace: PathBuf,
}

impl AutoresearchLoop {
    pub fn new(config: AutoresearchConfig, workspace: PathBuf) -> Self {
        Self { config, workspace }
    }

    pub async fn run(
        &self,
        agent: std::sync::Arc<crate::agent::AgentRunner>,
        resume: bool,
    ) -> anyhow::Result<AutoresearchLedger> {
        self.config.validate()?;
        let started = Instant::now();
        let ledger_path = self.config.ledger_path(&self.workspace);
        ensure_ledger_path_safe(&self.workspace, &ledger_path).await?;
        ensure_clean_workspace(&self.workspace).await?;
        let branch = git_branch(&self.workspace).await?;
        if branch.is_empty() {
            bail!("autoresearch requires a named branch; detached HEAD is not supported");
        }

        let mut ledger = if resume {
            let ledger = load_ledger(&ledger_path).await?;
            if ledger.objective != self.config.objective
                || ledger.direction != self.config.direction.trim().to_ascii_lowercase()
            {
                bail!("autoresearch ledger does not match the current spec; use a new ledger path");
            }
            if ledger.spec_fingerprint != spec_fingerprint(&self.config) {
                bail!(
                    "autoresearch ledger uses a different metric definition; use a new ledger path"
                );
            }
            if ledger.branch.is_empty() || ledger.branch != branch {
                bail!(
                    "autoresearch ledger belongs to branch `{}`, current branch is `{}`",
                    if ledger.branch.is_empty() {
                        "<unknown>"
                    } else {
                        &ledger.branch
                    },
                    branch
                );
            }
            let head = git_rev(&self.workspace).await?;
            if ledger.best_commit.is_empty() || ledger.best_commit != head {
                bail!(
                    "autoresearch ledger best commit {} does not match workspace HEAD {}; restore the recorded commit or use a new ledger path",
                    if ledger.best_commit.is_empty() { "<unknown>" } else { &ledger.best_commit },
                    head
                );
            }
            if ledger.chat_id.is_empty() {
                bail!("autoresearch ledger has no run identity; use a new ledger path");
            }
            ledger
        } else {
            let commit = git_rev(&self.workspace).await?;
            let baseline_ignored_state = capture_ignored_state(&self.workspace).await?;
            let baseline_result = async {
                if !run_validation(
                    &self.config,
                    &self.workspace,
                    started,
                    self.config.max_duration_secs,
                )
                .await?
                {
                    bail!("autoresearch baseline validation failed");
                }
                ensure_tracked_state_unchanged(&self.workspace, &branch, &commit).await?;
                let baseline = measure_metric(
                    &self.config,
                    &self.workspace,
                    started,
                    self.config.max_duration_secs,
                )
                .await?;
                ensure_tracked_state_unchanged(&self.workspace, &branch, &commit).await?;
                Ok::<_, anyhow::Error>(baseline)
            }
            .await;
            let baseline = match baseline_result {
                Ok(baseline) => baseline,
                Err(error) => {
                    restore_baseline_state(
                        &self.workspace,
                        &branch,
                        &commit,
                        &baseline_ignored_state,
                    )
                    .await
                    .with_context(|| format!("baseline cleanup failed after: {error}"))?;
                    return Err(error);
                }
            };
            let ledger = AutoresearchLedger::new(&self.config, baseline, commit, branch.clone());
            save_ledger(&ledger_path, &ledger).await?;
            ledger
        };

        let start = ledger
            .records
            .iter()
            .map(|record| record.iteration)
            .max()
            .unwrap_or(0)
            .saturating_add(1);

        for iteration in start..start.saturating_add(self.config.max_iterations) {
            if self.config.max_duration_secs > 0
                && started.elapsed() >= Duration::from_secs(self.config.max_duration_secs)
            {
                tracing::info!("autoresearch wall-clock budget exhausted");
                break;
            }
            let checkpoint = git_rev(&self.workspace).await?;
            if checkpoint != ledger.best_commit {
                bail!(
                    "autoresearch workspace HEAD {} does not match ledger best commit {}",
                    checkpoint,
                    ledger.best_commit
                );
            }
            if git_branch(&self.workspace).await? != ledger.branch {
                bail!("autoresearch branch changed while the run was in progress");
            }
            let ignored_state = capture_ignored_state(&self.workspace).await?;
            let previous_best = ledger.best_metric;
            let prompt = format!(
                "You are running one bounded autoresearch iteration.\n\n\
                 Objective: {objective}\n\
                 Metric command (must print one numeric value): {metric}\n\
                 Direction: {direction}\n\
                 Current best metric: {best}\n\n\
                 Form exactly one concrete hypothesis, implement only that experiment,\
                 and leave the workspace in the candidate state. Do not commit, reset,\
                 edit the autoresearch ledger, or claim success without making a change.\n\n\
                 Hypothesis: ",
                objective = self.config.objective,
                metric = self.config.metric_command,
                direction = self.config.direction,
                best = ledger.best_metric,
            );

            let null_channel = NullChannel::new("autoresearch");
            let message = IncomingMessage {
                id: uuid::Uuid::new_v4().to_string(),
                sender_id: "autoresearch".to_string(),
                sender_name: Some("Autoresearch".to_string()),
                chat_id: ledger.chat_id.clone(),
                text: prompt,
                is_group: false,
                reply_to: None,
                timestamp: chrono::Utc::now(),
            };
            let turn = async {
                if self.config.model.trim().is_empty() {
                    agent.handle_message(&message, &null_channel).await
                } else {
                    agent
                        .handle_message_with_model(
                            &message,
                            &null_channel,
                            Some(self.config.model.trim()),
                        )
                        .await
                }
            };
            let result = run_with_budget(started, self.config.max_duration_secs, turn).await;

            // The agent may edit files, but it must not move the experiment's
            // branch or checkpoint. Verify before running trusted commands so
            // a rejection can never reset an unrelated branch.
            ensure_experiment_state(&self.workspace, &ledger.branch, &checkpoint).await?;
            restore_ignored_state(&self.workspace, &ignored_state).await?;
            let candidate_status = git_status(&self.workspace).await?;

            let hypothesis = result
                .as_ref()
                .map(|response| first_line(response).unwrap_or_else(|| "agent experiment".into()))
                .unwrap_or_else(|error| format!("agent error: {error}"));

            let (decision, metric, reason) = match result {
                Err(error) => (
                    ExperimentDecision::Failed,
                    None,
                    format!("agent error: {error}"),
                ),
                Ok(_) => {
                    let evaluation = async {
                        if !run_validation(
                            &self.config,
                            &self.workspace,
                            started,
                            self.config.max_duration_secs,
                        )
                        .await?
                        {
                            return Ok((
                                ExperimentDecision::Rejected,
                                None,
                                "validation failed".to_string(),
                            ));
                        }
                        match measure_metric(
                            &self.config,
                            &self.workspace,
                            started,
                            self.config.max_duration_secs,
                        )
                        .await
                        {
                            Ok(value) if is_better(&self.config, value, previous_best) => Ok((
                                ExperimentDecision::Accepted,
                                Some(value),
                                "metric improved".to_string(),
                            )),
                            Ok(value) => Ok((
                                ExperimentDecision::Rejected,
                                Some(value),
                                "metric did not improve".to_string(),
                            )),
                            Err(error) => Ok((ExperimentDecision::Failed, None, error.to_string())),
                        }
                    }
                    .await;
                    match evaluation {
                        Err(error) if is_budget_error(&error) => return Err(error),
                        Ok(result) => result,
                        Err(error) => (
                            ExperimentDecision::Failed,
                            None,
                            format!("evaluation error: {error}"),
                        ),
                    }
                }
            };

            // Validation and metric commands are trusted shell, but they are
            // still not allowed to move the branch or checkpoint. Their
            // ignored-file side effects are also discarded before deciding.
            ensure_experiment_state(&self.workspace, &ledger.branch, &checkpoint).await?;
            ensure_status_unchanged(&self.workspace, &candidate_status, "validation or metric")
                .await?;
            restore_ignored_state(&self.workspace, &ignored_state).await?;

            let delta_percent = metric.map(|value| percent_delta(previous_best, value));
            let accepted = decision == ExperimentDecision::Accepted;
            let commit = if accepted {
                match commit_experiment(
                    &self.workspace,
                    iteration,
                    started,
                    self.config.max_duration_secs,
                )
                .await
                {
                    Ok(commit) => Some(commit),
                    Err(error) => {
                        // A timed-out hook may have left git add's index
                        // changes behind. Reset only if the branch and HEAD
                        // are still the checkpoint we verified above.
                        let current_branch = git_branch(&self.workspace).await?;
                        let current_head = git_rev(&self.workspace).await?;
                        if current_branch == ledger.branch && current_head == checkpoint {
                            restore_checkpoint(&self.workspace, &checkpoint).await?;
                            restore_ignored_state(&self.workspace, &ignored_state).await?;
                        }
                        bail!("autoresearch acceptance commit failed: {error}");
                    }
                }
            } else {
                restore_checkpoint(&self.workspace, &checkpoint).await?;
                restore_ignored_state(&self.workspace, &ignored_state).await?;
                None
            };

            if let Some(value) = metric.filter(|_| accepted) {
                ledger.best_metric = value;
                ledger.best_commit = commit.clone().unwrap_or(checkpoint);
            }
            ledger.records.push(ExperimentRecord {
                iteration,
                hypothesis,
                commit,
                metric,
                baseline: previous_best,
                delta_percent,
                decision,
                reason,
                timestamp: chrono::Utc::now().to_rfc3339(),
            });
            save_ledger(&ledger_path, &ledger).await?;
            tracing::info!(
                iteration,
                best_metric = ledger.best_metric,
                "autoresearch iteration complete"
            );
        }

        Ok(ledger)
    }
}

async fn load_ledger(path: &Path) -> anyhow::Result<AutoresearchLedger> {
    let content = tokio::fs::read_to_string(path)
        .await
        .with_context(|| format!("reading autoresearch ledger {}", path.display()))?;
    Ok(toml::from_str(&content)?)
}

#[derive(Serialize)]
struct ExperimentDefinition<'a> {
    objective: &'a str,
    metric_command: &'a str,
    direction: &'a str,
    validation_command: &'a str,
    validation_retries: usize,
    command_timeout_secs: u64,
    samples: usize,
    min_improvement_percent: f64,
    max_iterations: usize,
    max_duration_secs: u64,
    model: &'a str,
}

fn spec_fingerprint(config: &AutoresearchConfig) -> String {
    let direction = config.direction.trim().to_ascii_lowercase();
    let definition = ExperimentDefinition {
        objective: &config.objective,
        metric_command: &config.metric_command,
        direction: &direction,
        validation_command: &config.validation_command,
        validation_retries: config.validation_retries,
        command_timeout_secs: config.command_timeout_secs,
        samples: config.samples,
        min_improvement_percent: config.min_improvement_percent,
        max_iterations: config.max_iterations,
        max_duration_secs: config.max_duration_secs,
        model: &config.model,
    };
    let encoded = serde_json::to_vec(&definition).expect("experiment definition is serializable");
    format!("{:x}", Sha256::digest(encoded))
}

#[derive(Debug)]
struct IgnoredFile {
    relative: PathBuf,
    backup_relative: Option<PathBuf>,
    symlink_target: Option<PathBuf>,
    #[cfg(unix)]
    mode: u32,
}

#[derive(Debug)]
struct IgnoredWorkspaceState {
    backup_dir: PathBuf,
    files: Vec<IgnoredFile>,
}

impl Drop for IgnoredWorkspaceState {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.backup_dir);
    }
}

fn is_volatile_ignored_path(path: &Path) -> bool {
    path.components().next().is_some_and(|component| {
        let std::path::Component::Normal(root) = component else {
            return false;
        };
        VOLATILE_IGNORED_ROOTS
            .iter()
            .any(|candidate| root == *candidate)
    })
}

async fn ignored_paths(workspace: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let output = git_command(
        workspace,
        &[
            "ls-files",
            "--others",
            "--ignored",
            "--exclude-standard",
            "-z",
            "--",
            ".",
            ":(exclude)target",
            ":(exclude)target/**",
            ":(exclude)node_modules",
            ":(exclude)node_modules/**",
            ":(exclude).venv",
            ":(exclude).venv/**",
            ":(exclude)vendor",
            ":(exclude)vendor/**",
            ":(exclude)dist",
            ":(exclude)dist/**",
            ":(exclude)build",
            ":(exclude)build/**",
        ],
    )
    .await?;
    output
        .split('\0')
        .filter(|path| !path.is_empty())
        // Build output can contain millions of files and is deliberately
        // treated as disposable process state rather than experiment input.
        // The source/configuration files that can affect an experiment are
        // still snapshotted and restored below.
        .filter(|path| !is_volatile_ignored_path(Path::new(path)))
        .map(|path| {
            let relative = PathBuf::from(path);
            validate_workspace_relative_path(&relative)?;
            Ok(relative)
        })
        .collect()
}

fn validate_workspace_relative_path(path: &Path) -> anyhow::Result<()> {
    if path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        bail!(
            "git returned an unsafe workspace-relative path: {}",
            path.display()
        );
    }
    Ok(())
}

async fn capture_ignored_state(workspace: &Path) -> anyhow::Result<IgnoredWorkspaceState> {
    let backup_dir = std::env::temp_dir().join(format!(
        "apollo-autoresearch-ignored-{}",
        uuid::Uuid::new_v4()
    ));
    tokio::fs::create_dir_all(&backup_dir).await?;
    let result = async {
        let mut files = Vec::new();
        let mut total_bytes = 0u64;
        for relative in ignored_paths(workspace).await? {
            let path = workspace.join(&relative);
            let metadata = tokio::fs::symlink_metadata(&path)
                .await
                .with_context(|| format!("reading ignored path metadata: {}", path.display()))?;
            let file_type = metadata.file_type();
            if file_type.is_symlink() {
                files.push(IgnoredFile {
                    relative,
                    backup_relative: None,
                    symlink_target: Some(tokio::fs::read_link(&path).await?),
                    #[cfg(unix)]
                    mode: 0,
                });
            } else if file_type.is_file() {
                let size = metadata.len();
                if size > MAX_IGNORED_FILE_BYTES {
                    bail!(
                        "ignored file {} is {} bytes; autoresearch refuses to snapshot files larger than {} bytes",
                        path.display(),
                        size,
                        MAX_IGNORED_FILE_BYTES
                    );
                }
                total_bytes = total_bytes.saturating_add(size);
                if total_bytes > MAX_IGNORED_SNAPSHOT_BYTES {
                    bail!(
                        "ignored workspace state exceeds the {} byte autoresearch snapshot limit; use a dedicated workspace or exclude dependency/data trees",
                        MAX_IGNORED_SNAPSHOT_BYTES
                    );
                }
                let backup_relative = relative.clone();
                let backup_path = backup_dir.join(&backup_relative);
                if let Some(parent) = backup_path.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }
                tokio::fs::copy(&path, &backup_path).await?;
                files.push(IgnoredFile {
                    relative,
                    backup_relative: Some(backup_relative),
                    symlink_target: None,
                    #[cfg(unix)]
                    mode: {
                        use std::os::unix::fs::PermissionsExt;
                        metadata.permissions().mode()
                    },
                });
            }
        }
        Ok::<_, anyhow::Error>(IgnoredWorkspaceState {
            backup_dir: backup_dir.clone(),
            files,
        })
    }
    .await;
    if result.is_err() {
        // The state object owns this directory only after successful capture.
        // Clean partial snapshots on an early size, metadata, or copy error.
        let _ = tokio::fs::remove_dir_all(&backup_dir).await;
    }
    result
}

async fn remove_workspace_path(path: &Path) -> anyhow::Result<()> {
    let Ok(metadata) = tokio::fs::symlink_metadata(path).await else {
        return Ok(());
    };
    if metadata.file_type().is_dir() {
        tokio::fs::remove_dir_all(path).await?;
    } else {
        tokio::fs::remove_file(path).await?;
    }
    Ok(())
}

async fn restore_ignored_state(
    workspace: &Path,
    state: &IgnoredWorkspaceState,
) -> anyhow::Result<()> {
    // Restore tracked/untracked files separately; git clean intentionally does
    // not touch ignored files, which is exactly where autoresearch configs,
    // generated artifacts, and local credentials commonly live.
    git_command(workspace, &["clean", "-fd"]).await?;

    let baseline: HashSet<&Path> = state
        .files
        .iter()
        .map(|file| file.relative.as_path())
        .collect();
    for relative in ignored_paths(workspace).await? {
        if !baseline.contains(relative.as_path()) {
            remove_workspace_path(&workspace.join(relative)).await?;
        }
    }

    for file in &state.files {
        let path = workspace.join(&file.relative);
        if let Some(target) = &file.symlink_target {
            remove_workspace_path(&path).await?;
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            #[cfg(unix)]
            std::os::unix::fs::symlink(target, &path)?;
            #[cfg(windows)]
            std::os::windows::fs::symlink_file(target, &path)?;
        } else if let Some(backup_relative) = &file.backup_relative {
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            if let Ok(metadata) = tokio::fs::symlink_metadata(&path).await {
                if !metadata.file_type().is_file() {
                    remove_workspace_path(&path).await?;
                }
            }
            tokio::fs::copy(state.backup_dir.join(backup_relative), &path).await?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(file.mode))
                    .await?;
            }
        }
    }
    Ok(())
}

async fn run_with_budget<T, F>(
    started: Instant,
    max_duration_secs: u64,
    future: F,
) -> anyhow::Result<T>
where
    F: Future<Output = anyhow::Result<T>>,
{
    if max_duration_secs == 0 {
        return future.await;
    }
    let remaining = Duration::from_secs(max_duration_secs).saturating_sub(started.elapsed());
    tokio::time::timeout(remaining, future)
        .await
        .with_context(|| WALL_CLOCK_BUDGET_EXHAUSTED)?
}

async fn save_ledger(path: &Path, ledger: &AutoresearchLedger) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let content = toml::to_string_pretty(ledger)?;
    let temporary = path.with_extension("tmp");
    tokio::fs::write(&temporary, content).await?;
    replace_ledger_file(&temporary, path).await?;
    Ok(())
}

#[cfg(not(windows))]
async fn replace_ledger_file(temporary: &Path, destination: &Path) -> anyhow::Result<()> {
    tokio::fs::rename(temporary, destination).await?;
    Ok(())
}

#[cfg(windows)]
async fn replace_ledger_file(temporary: &Path, destination: &Path) -> anyhow::Result<()> {
    if !tokio::fs::try_exists(destination).await? {
        tokio::fs::rename(temporary, destination).await?;
        return Ok(());
    }

    let temporary = temporary.to_path_buf();
    let destination = destination.to_path_buf();
    tokio::task::spawn_blocking(move || windows_replace_file(&temporary, &destination)).await??;
    Ok(())
}

#[cfg(windows)]
fn windows_replace_file(temporary: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "kernel32")]
    extern "system" {
        fn ReplaceFileW(
            replaced_file_name: *const u16,
            replacement_file_name: *const u16,
            backup_file_name: *const u16,
            replace_flags: u32,
            exclude: *const std::ffi::c_void,
            reserved: *const std::ffi::c_void,
        ) -> i32;
    }

    let replaced = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let replacement = temporary
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let succeeded = unsafe {
        ReplaceFileW(
            replaced.as_ptr(),
            replacement.as_ptr(),
            std::ptr::null(),
            0,
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    if succeeded == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

async fn ensure_clean_workspace(workspace: &Path) -> anyhow::Result<()> {
    let output = git_status(workspace).await?;
    if !output.trim().is_empty() {
        bail!("autoresearch requires a clean workspace; commit or stash existing changes first");
    }
    Ok(())
}

async fn git_status(workspace: &Path) -> anyhow::Result<String> {
    git_command(workspace, &["status", "--porcelain"]).await
}

async fn ensure_status_unchanged(
    workspace: &Path,
    expected: &str,
    source: &str,
) -> anyhow::Result<()> {
    let actual = git_status(workspace).await?;
    if actual != expected {
        bail!("{source} modified the tracked workspace; refusing to record its result");
    }
    Ok(())
}

async fn ensure_tracked_state_unchanged(
    workspace: &Path,
    expected_branch: &str,
    expected_head: &str,
) -> anyhow::Result<()> {
    ensure_experiment_state(workspace, expected_branch, expected_head).await?;
    let status = git_status(workspace).await?;
    if !status.trim().is_empty() {
        bail!("baseline command modified the workspace");
    }
    Ok(())
}

async fn restore_baseline_state(
    workspace: &Path,
    branch: &str,
    commit: &str,
    ignored_state: &IgnoredWorkspaceState,
) -> anyhow::Result<()> {
    ensure_experiment_state(workspace, branch, commit).await?;
    restore_checkpoint(workspace, commit).await?;
    restore_ignored_state(workspace, ignored_state).await
}

async fn ensure_ledger_path_safe(workspace: &Path, ledger_path: &Path) -> anyhow::Result<()> {
    // `workspace` and the configured ledger can both be relative. Resolve
    // them against the same current directory before canonicalizing existing
    // ancestors; otherwise a relative workspace would be joined twice.
    let current_dir = tokio::fs::canonicalize(".").await?;
    let workspace_absolute = if workspace.is_absolute() {
        workspace.to_path_buf()
    } else {
        current_dir.join(workspace)
    };
    let ledger_absolute = if ledger_path.is_absolute() {
        ledger_path.to_path_buf()
    } else {
        current_dir.join(ledger_path)
    };
    let workspace = tokio::fs::canonicalize(&workspace_absolute)
        .await
        .with_context(|| format!("canonicalizing workspace {}", workspace.display()))?;
    let ledger_path = normalize_path_for_containment(&ledger_absolute, &workspace).await?;
    let Ok(relative) = ledger_path.strip_prefix(&workspace) else {
        return Ok(());
    };
    let relative = relative.to_string_lossy();
    let output = git_command(
        &workspace,
        &["check-ignore", "--quiet", "--", relative.as_ref()],
    )
    .await;
    if output.is_err() {
        bail!(
            "autoresearch ledger path {} is inside the workspace but is not git-ignored; choose an ignored path or store the ledger outside the workspace",
            ledger_path.display()
        );
    }
    Ok(())
}

async fn normalize_path_for_containment(path: &Path, workspace: &Path) -> anyhow::Result<PathBuf> {
    let candidate = if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace.join(path)
    };
    if tokio::fs::try_exists(&candidate).await? {
        return tokio::fs::canonicalize(&candidate)
            .await
            .map_err(Into::into);
    }

    let mut missing = Vec::new();
    let mut existing = candidate.clone();
    while !tokio::fs::try_exists(&existing).await? {
        let Some(name) = existing.file_name() else {
            bail!("cannot normalize path {}", path.display());
        };
        missing.push(name.to_os_string());
        existing.pop();
    }
    let mut normalized = tokio::fs::canonicalize(existing).await?;
    for component in missing.iter().rev() {
        normalized.push(component);
    }
    Ok(normalized)
}

async fn git_rev(workspace: &Path) -> anyhow::Result<String> {
    Ok(git_command(workspace, &["rev-parse", "HEAD"])
        .await?
        .trim()
        .to_string())
}

async fn git_branch(workspace: &Path) -> anyhow::Result<String> {
    Ok(git_command(workspace, &["branch", "--show-current"])
        .await?
        .trim()
        .to_string())
}

async fn ensure_experiment_state(
    workspace: &Path,
    expected_branch: &str,
    expected_head: &str,
) -> anyhow::Result<()> {
    let branch = git_branch(workspace).await?;
    if branch != expected_branch {
        bail!(
            "autoresearch agent moved from branch `{expected_branch}` to `{branch}`; refusing to reset"
        );
    }
    let head = git_rev(workspace).await?;
    if head != expected_head {
        bail!(
            "autoresearch agent moved HEAD from `{expected_head}` to `{head}`; refusing to reset"
        );
    }
    Ok(())
}

async fn commit_experiment(
    workspace: &Path,
    iteration: usize,
    started: Instant,
    max_duration_secs: u64,
) -> anyhow::Result<String> {
    git_command_with_budget(workspace, &["add", "-A"], started, max_duration_secs).await?;
    let status = git_command_with_budget(
        workspace,
        &["status", "--porcelain"],
        started,
        max_duration_secs,
    )
    .await?;
    if status.trim().is_empty() {
        bail!("experiment iteration {iteration} made no changes");
    }
    // Use a fresh empty hooks directory so repository-controlled hooks cannot
    // run during the controller's acceptance commit. `--no-verify` covers the
    // client-side pre-commit and commit-msg hooks as well.
    let hooks_dir = std::env::temp_dir().join(format!(
        "apollo-autoresearch-hooks-{}",
        uuid::Uuid::new_v4()
    ));
    tokio::fs::create_dir(&hooks_dir).await?;
    let hooks_path = hooks_dir.to_string_lossy().into_owned();
    let commit_result = git_command_with_budget(
        workspace,
        &[
            "-c",
            &format!("core.hooksPath={hooks_path}"),
            "commit",
            "--no-verify",
            "-m",
            &format!("autoresearch: iteration {iteration}"),
        ],
        started,
        max_duration_secs,
    )
    .await;
    let _ = tokio::fs::remove_dir(&hooks_dir).await;
    commit_result?;
    git_command_with_budget(
        workspace,
        &["rev-parse", "HEAD"],
        started,
        max_duration_secs,
    )
    .await
    .map(|commit| commit.trim().to_string())
}

async fn restore_checkpoint(workspace: &Path, checkpoint: &str) -> anyhow::Result<()> {
    // The clean-workspace precondition makes these scoped resets recoverable:
    // only changes made by the rejected iteration can exist at this point.
    git_command(
        workspace,
        &["reset", "--hard", "--recurse-submodules", checkpoint],
    )
    .await?;
    git_command(
        workspace,
        &["submodule", "foreach", "--recursive", "git reset --hard"],
    )
    .await
    .or_else(|error| {
        if error.to_string().contains("no submodule") {
            Ok(String::new())
        } else {
            Err(error)
        }
    })?;
    git_command(
        workspace,
        &["submodule", "foreach", "--recursive", "git clean -fd"],
    )
    .await
    .or_else(|error| {
        if error.to_string().contains("no submodule") {
            Ok(String::new())
        } else {
            Err(error)
        }
    })?;
    git_command(workspace, &["clean", "-fd"]).await?;
    Ok(())
}

async fn git_command(workspace: &Path, args: &[&str]) -> anyhow::Result<String> {
    git_command_with_timeout(workspace, args, None).await
}

async fn git_command_with_budget(
    workspace: &Path,
    args: &[&str],
    started: Instant,
    max_duration_secs: u64,
) -> anyhow::Result<String> {
    let timeout = remaining_budget(started, max_duration_secs)?;
    git_command_with_timeout(workspace, args, timeout).await
}

async fn git_command_with_timeout(
    workspace: &Path,
    args: &[&str],
    timeout: Option<Duration>,
) -> anyhow::Result<String> {
    let mut command = tokio::process::Command::new("git");
    command.args(args).current_dir(workspace);
    crate::tools::child_proc::scrub(&mut command);
    let output = run_process(&mut command, timeout, &format!("git {}", args.join(" "))).await?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            crate::text::truncate_chars(&String::from_utf8_lossy(&output.stderr), 1000)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

async fn run_validation(
    config: &AutoresearchConfig,
    workspace: &Path,
    started: Instant,
    max_duration_secs: u64,
) -> anyhow::Result<bool> {
    if config.validation_command.trim().is_empty() {
        return Ok(true);
    }
    for attempt in 1..=config.validation_retries {
        let output = match run_command(
            &config.validation_command,
            workspace,
            config.command_timeout_secs,
            started,
            max_duration_secs,
        )
        .await
        {
            Err(error) if is_budget_error(&error) => return Err(error),
            Ok(output) => output,
            Err(error) => {
                tracing::warn!(attempt, "autoresearch validation could not run: {error}");
                continue;
            }
        };
        if output.status.success() {
            return Ok(true);
        }
        tracing::warn!(
            attempt,
            "autoresearch validation failed: {}",
            crate::text::truncate_chars(&String::from_utf8_lossy(&output.stderr), 1000)
        );
    }
    Ok(false)
}

async fn measure_metric(
    config: &AutoresearchConfig,
    workspace: &Path,
    started: Instant,
    max_duration_secs: u64,
) -> anyhow::Result<f64> {
    let mut values = Vec::with_capacity(config.samples);
    for _ in 0..config.samples {
        let output = run_command(
            &config.metric_command,
            workspace,
            config.command_timeout_secs,
            started,
            max_duration_secs,
        )
        .await?;
        if !output.status.success() {
            bail!(
                "metric command failed: {}",
                crate::text::truncate_chars(&String::from_utf8_lossy(&output.stderr), 1000)
            );
        }
        values.push(parse_metric(&String::from_utf8_lossy(&output.stdout))?);
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
    Ok(values[values.len() / 2])
}

async fn run_command(
    command: &str,
    workspace: &Path,
    timeout_secs: u64,
    started: Instant,
    max_duration_secs: u64,
) -> anyhow::Result<std::process::Output> {
    let command_timeout = Duration::from_secs(timeout_secs);
    let remaining = remaining_budget(started, max_duration_secs)?;
    let budget_is_tighter = remaining.is_some_and(|remaining| remaining <= command_timeout);
    let timeout = remaining
        .map(|remaining| remaining.min(command_timeout))
        .unwrap_or(command_timeout);

    let parts = shlex::split(command).ok_or_else(|| anyhow::anyhow!("invalid command quoting"))?;
    if parts.is_empty() {
        bail!("empty command");
    }

    let mut process = tokio::process::Command::new(&parts[0]);
    process.args(&parts[1..]).current_dir(workspace);
    crate::tools::child_proc::scrub(&mut process);
    let label = if budget_is_tighter {
        WALL_CLOCK_BUDGET_EXHAUSTED
    } else {
        "autoresearch command"
    };
    run_process(&mut process, Some(timeout), label).await
}

fn remaining_budget(started: Instant, max_duration_secs: u64) -> anyhow::Result<Option<Duration>> {
    if max_duration_secs == 0 {
        return Ok(None);
    }
    let remaining = Duration::from_secs(max_duration_secs).saturating_sub(started.elapsed());
    if remaining.is_zero() {
        bail!(WALL_CLOCK_BUDGET_EXHAUSTED);
    }
    Ok(Some(remaining))
}

fn is_budget_error(error: &anyhow::Error) -> bool {
    error.to_string().contains(WALL_CLOCK_BUDGET_EXHAUSTED)
}

fn configure_process_group(command: &mut tokio::process::Command) {
    #[cfg(unix)]
    {
        // A separate process group lets timeout cleanup terminate descendants
        // spawned by `sh -c`, such as cargo/compiler children.
        command.process_group(0);
    }
}

fn terminate_process_group(pid: Option<u32>) {
    #[cfg(unix)]
    if let Some(pid) = pid {
        // SAFETY: pid is the process-group leader created by this command.
        unsafe {
            libc::killpg(pid as libc::pid_t, libc::SIGKILL);
        }
    }
}

async fn run_process(
    command: &mut tokio::process::Command,
    timeout: Option<Duration>,
    label: &str,
) -> anyhow::Result<std::process::Output> {
    configure_process_group(command);
    command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let child = command
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("starting {label}"))?;
    let pid = child.id();
    let output = if let Some(timeout) = timeout {
        match tokio::time::timeout(timeout, child.wait_with_output()).await {
            Ok(result) => result.with_context(|| format!("waiting for {label}"))?,
            Err(_) => {
                terminate_process_group(pid);
                bail!("{label} timed out");
            }
        }
    } else {
        child
            .wait_with_output()
            .await
            .with_context(|| format!("waiting for {label}"))?
    };
    Ok(output)
}

fn parse_metric(output: &str) -> anyhow::Result<f64> {
    output
        .split_whitespace()
        .find_map(|token| {
            token
                .trim_matches(|c: char| !c.is_ascii_digit() && c != '.' && c != '-')
                .parse::<f64>()
                .ok()
        })
        .filter(|value| value.is_finite())
        .ok_or_else(|| anyhow::anyhow!("metric command must print a numeric value"))
}

fn is_better(config: &AutoresearchConfig, candidate: f64, current: f64) -> bool {
    let improvement = current.abs() * config.min_improvement_percent / 100.0;
    match config.direction.trim().to_ascii_lowercase().as_str() {
        "maximize" => candidate > current + improvement,
        _ => candidate < current - improvement,
    }
}

fn percent_delta(previous: f64, candidate: f64) -> f64 {
    if previous == 0.0 {
        0.0
    } else {
        ((candidate - previous) / previous.abs()) * 100.0
    }
}

fn first_line(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_numeric_metric_from_command_output() {
        assert_eq!(parse_metric("median_ms=19.125\n").unwrap(), 19.125);
    }

    #[test]
    fn median_sampling_is_sorted() {
        let mut values = [9.0, 1.0, 4.0];
        values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
        assert_eq!(values[values.len() / 2], 4.0);
    }

    #[test]
    fn direction_and_tolerance_are_respected() {
        let config = AutoresearchConfig {
            direction: "minimize".into(),
            min_improvement_percent: 1.0,
            ..AutoresearchConfig::default()
        };
        assert!(is_better(&config, 98.9, 100.0));
        assert!(!is_better(&config, 100.5, 100.0));
        let strict = AutoresearchConfig::default();
        assert!(!is_better(&strict, 100.0, 100.0));
    }

    #[test]
    fn experiment_definition_fingerprint_changes_when_metric_changes() {
        let config = AutoresearchConfig {
            objective: "startup".into(),
            metric_command: "./measure-a".into(),
            ..AutoresearchConfig::default()
        };
        let mut changed = config.clone();
        changed.metric_command = "./measure-b".into();
        assert_ne!(spec_fingerprint(&config), spec_fingerprint(&changed));
    }

    #[test]
    fn new_ledger_is_bound_to_branch_and_has_a_unique_run_id() {
        let config = AutoresearchConfig {
            objective: "startup".into(),
            metric_command: "./measure".into(),
            ..AutoresearchConfig::default()
        };
        let first = AutoresearchLedger::new(&config, 10.0, "abc".into(), "feature/x".into());
        let second = AutoresearchLedger::new(&config, 10.0, "abc".into(), "feature/x".into());
        assert_eq!(first.branch, "feature/x");
        assert_eq!(first.spec_fingerprint, spec_fingerprint(&config));
        assert_ne!(first.chat_id, second.chat_id);
    }

    #[tokio::test]
    async fn command_timeout_is_enforced() {
        let error = run_command("sleep 5", Path::new("."), 1, Instant::now(), 0)
            .await
            .expect_err("long-running metric should time out");
        assert!(error.to_string().contains("timed out"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn command_timeout_terminates_shell_descendants() {
        let directory = tempfile::tempdir().unwrap();
        let marker = directory.path().join("descendant-finished");
        let inner_command = format!(
            "sleep 2; touch {}",
            shlex::try_quote(&marker.to_string_lossy()).unwrap()
        );
        let command = format!("sh -c {}", shlex::try_quote(&inner_command).unwrap());
        run_command(&command, directory.path(), 1, Instant::now(), 0)
            .await
            .expect_err("command should time out");
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert!(!marker.exists(), "timed-out descendant survived");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn wall_clock_timeout_terminates_shell_descendants() {
        let directory = tempfile::tempdir().unwrap();
        let marker = directory.path().join("wall-clock-descendant-finished");
        let inner_command = format!(
            "sleep 2; touch {}",
            shlex::try_quote(&marker.to_string_lossy()).unwrap()
        );
        let command = format!("sh -c {}", shlex::try_quote(&inner_command).unwrap());
        let error = run_command(&command, directory.path(), 60, Instant::now(), 1)
            .await
            .expect_err("wall-clock budget should time out the command");
        assert!(is_budget_error(&error));
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert!(!marker.exists(), "wall-clock descendant survived");
    }

    #[tokio::test]
    async fn ignored_state_restores_existing_files_and_removes_new_files() {
        let directory = tempfile::tempdir().unwrap();
        git_command(directory.path(), &["init", "-q"])
            .await
            .unwrap();
        tokio::fs::write(directory.path().join(".gitignore"), ".env\nnew-*\n")
            .await
            .unwrap();
        git_command(directory.path(), &["add", ".gitignore"])
            .await
            .unwrap();
        git_command(
            directory.path(),
            &[
                "-c",
                "user.name=Autoresearch Test",
                "-c",
                "user.email=autoresearch@example.invalid",
                "commit",
                "-qm",
                "initial",
            ],
        )
        .await
        .unwrap();
        tokio::fs::write(directory.path().join(".env"), "before\n")
            .await
            .unwrap();

        let state = capture_ignored_state(directory.path()).await.unwrap();
        tokio::fs::write(directory.path().join(".env"), "candidate\n")
            .await
            .unwrap();
        tokio::fs::write(directory.path().join("new-output"), "candidate\n")
            .await
            .unwrap();
        restore_ignored_state(directory.path(), &state)
            .await
            .unwrap();

        assert_eq!(
            tokio::fs::read_to_string(directory.path().join(".env"))
                .await
                .unwrap(),
            "before\n"
        );
        assert!(!directory.path().join("new-output").exists());
    }

    #[tokio::test]
    async fn baseline_rejects_tracked_workspace_changes() {
        let directory = tempfile::tempdir().unwrap();
        git_command(directory.path(), &["init", "-q"])
            .await
            .unwrap();
        tokio::fs::write(directory.path().join("tracked.txt"), "before\n")
            .await
            .unwrap();
        git_command(directory.path(), &["add", "tracked.txt"])
            .await
            .unwrap();
        git_command(
            directory.path(),
            &[
                "-c",
                "user.name=Autoresearch Test",
                "-c",
                "user.email=autoresearch@example.invalid",
                "commit",
                "-qm",
                "initial",
            ],
        )
        .await
        .unwrap();
        let branch = git_branch(directory.path()).await.unwrap();
        let head = git_rev(directory.path()).await.unwrap();
        tokio::fs::write(directory.path().join("tracked.txt"), "changed\n")
            .await
            .unwrap();

        let error = ensure_tracked_state_unchanged(directory.path(), &branch, &head)
            .await
            .expect_err("baseline must reject tracked changes");
        assert!(error.to_string().contains("baseline command modified"));
    }

    #[tokio::test]
    async fn missing_path_is_normalized_before_workspace_containment_check() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = tokio::fs::canonicalize(directory.path()).await.unwrap();
        let ledger = workspace.join(".apollo").join("ledger.toml");
        let normalized = normalize_path_for_containment(&ledger, &workspace)
            .await
            .unwrap();
        assert!(normalized.starts_with(&workspace));
        assert_eq!(normalized, ledger);
    }

    #[tokio::test]
    async fn validation_timeout_is_a_rejected_attempt() {
        let config = AutoresearchConfig {
            validation_command: "sleep 5".into(),
            validation_retries: 1,
            command_timeout_secs: 1,
            ..AutoresearchConfig::default()
        };
        assert!(!run_validation(&config, Path::new("."), Instant::now(), 0)
            .await
            .unwrap());
    }
}
