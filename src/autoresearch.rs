//! Metric-driven autonomous experimentation.
//!
//! Autoresearch is deliberately separate from [`crate::autonomous`]. The
//! autonomous TODO loop completes one task and validates it; this loop keeps a
//! measurable objective, accepts only improvements, and resumes from a small
//! durable ledger.

use std::cmp::Ordering;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};

use crate::agent::NullChannel;
use crate::channels::IncomingMessage;

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
    pub records: Vec<ExperimentRecord>,
}

impl AutoresearchLedger {
    fn new(config: &AutoresearchConfig, baseline: f64, commit: String) -> Self {
        Self {
            objective: config.objective.clone(),
            direction: config.direction.trim().to_ascii_lowercase(),
            best_metric: baseline,
            best_commit: commit,
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

        let mut ledger = if resume {
            load_ledger(&ledger_path).await?
        } else {
            let commit = git_rev(&self.workspace).await?;
            let baseline = run_with_budget(
                started,
                self.config.max_duration_secs,
                measure_metric(&self.config, &self.workspace),
            )
            .await?;
            let ledger = AutoresearchLedger::new(&self.config, baseline, commit);
            save_ledger(&ledger_path, &ledger).await?;
            ledger
        };

        if ledger.objective != self.config.objective
            || ledger.direction != self.config.direction.trim().to_ascii_lowercase()
        {
            bail!("autoresearch ledger does not match the current spec; use a new ledger path");
        }

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
                chat_id: "autoresearch".to_string(),
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
                    let evaluation =
                        run_with_budget(started, self.config.max_duration_secs, async {
                            if !run_validation(&self.config, &self.workspace).await? {
                                return Ok((
                                    ExperimentDecision::Rejected,
                                    None,
                                    "validation failed".to_string(),
                                ));
                            }
                            match measure_metric(&self.config, &self.workspace).await {
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
                                Err(error) => {
                                    Ok((ExperimentDecision::Failed, None, error.to_string()))
                                }
                            }
                        })
                        .await;
                    evaluation.unwrap_or_else(|error| {
                        (
                            ExperimentDecision::Failed,
                            None,
                            format!("evaluation error: {error}"),
                        )
                    })
                }
            };

            let delta_percent = metric.map(|value| percent_delta(previous_best, value));
            let accepted = decision == ExperimentDecision::Accepted;
            let commit = if accepted {
                Some(commit_experiment(&self.workspace, iteration).await?)
            } else {
                restore_checkpoint(&self.workspace, &checkpoint).await?;
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
        .with_context(|| "autoresearch wall-clock budget exhausted")?
}

async fn save_ledger(path: &Path, ledger: &AutoresearchLedger) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let content = toml::to_string_pretty(ledger)?;
    let temporary = path.with_extension("tmp");
    tokio::fs::write(&temporary, content).await?;
    tokio::fs::rename(&temporary, path).await?;
    Ok(())
}

async fn ensure_clean_workspace(workspace: &Path) -> anyhow::Result<()> {
    let output = git_command(workspace, &["status", "--porcelain"]).await?;
    if !output.trim().is_empty() {
        bail!("autoresearch requires a clean workspace; commit or stash existing changes first");
    }
    Ok(())
}

async fn ensure_ledger_path_safe(workspace: &Path, ledger_path: &Path) -> anyhow::Result<()> {
    let Ok(relative) = ledger_path.strip_prefix(workspace) else {
        return Ok(());
    };
    let relative = relative.to_string_lossy();
    let status = tokio::process::Command::new("git")
        .args(["check-ignore", "--quiet", "--", relative.as_ref()])
        .current_dir(workspace)
        .status()
        .await
        .with_context(|| format!("checking whether ledger path is ignored: {relative}"))?;
    if !status.success() {
        bail!(
            "autoresearch ledger path {} is inside the workspace but is not git-ignored; choose an ignored path or store the ledger outside the workspace",
            ledger_path.display()
        );
    }
    Ok(())
}

async fn git_rev(workspace: &Path) -> anyhow::Result<String> {
    Ok(git_command(workspace, &["rev-parse", "HEAD"])
        .await?
        .trim()
        .to_string())
}

async fn commit_experiment(workspace: &Path, iteration: usize) -> anyhow::Result<String> {
    git_command(workspace, &["add", "-A"]).await?;
    let status = git_command(workspace, &["status", "--porcelain"]).await?;
    if status.trim().is_empty() {
        bail!("experiment iteration {iteration} made no changes");
    }
    git_command(
        workspace,
        &[
            "commit",
            "-m",
            &format!("autoresearch: iteration {iteration}"),
        ],
    )
    .await?;
    git_rev(workspace).await
}

async fn restore_checkpoint(workspace: &Path, checkpoint: &str) -> anyhow::Result<()> {
    // The clean-workspace precondition makes these scoped resets recoverable:
    // only changes made by the rejected iteration can exist at this point.
    git_command(workspace, &["reset", "--hard", checkpoint]).await?;
    git_command(workspace, &["clean", "-fd"]).await?;
    Ok(())
}

async fn git_command(workspace: &Path, args: &[&str]) -> anyhow::Result<String> {
    let output = tokio::process::Command::new("git")
        .args(args)
        .current_dir(workspace)
        .output()
        .await
        .with_context(|| format!("running git {}", args.join(" ")))?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            crate::text::truncate_chars(&String::from_utf8_lossy(&output.stderr), 1000)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

async fn run_validation(config: &AutoresearchConfig, workspace: &Path) -> anyhow::Result<bool> {
    if config.validation_command.trim().is_empty() {
        return Ok(true);
    }
    for attempt in 1..=config.validation_retries {
        let output = match run_shell(
            &config.validation_command,
            workspace,
            config.command_timeout_secs,
        )
        .await
        {
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

async fn measure_metric(config: &AutoresearchConfig, workspace: &Path) -> anyhow::Result<f64> {
    let mut values = Vec::with_capacity(config.samples);
    for _ in 0..config.samples {
        let output = run_shell(
            &config.metric_command,
            workspace,
            config.command_timeout_secs,
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

async fn run_shell(
    command: &str,
    workspace: &Path,
    timeout_secs: u64,
) -> anyhow::Result<std::process::Output> {
    let child = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(workspace)
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("starting autoresearch command: {command}"))?;
    tokio::time::timeout(Duration::from_secs(timeout_secs), child.wait_with_output())
        .await
        .with_context(|| format!("command timed out after {timeout_secs}s"))?
        .with_context(|| format!("waiting for autoresearch command: {command}"))
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

    #[tokio::test]
    async fn command_timeout_is_enforced() {
        let error = run_shell("sleep 5", Path::new("."), 1)
            .await
            .expect_err("long-running metric should time out");
        assert!(error.to_string().contains("timed out"));
    }

    #[tokio::test]
    async fn validation_timeout_is_a_rejected_attempt() {
        let config = AutoresearchConfig {
            validation_command: "sleep 5".into(),
            validation_retries: 1,
            command_timeout_secs: 1,
            ..AutoresearchConfig::default()
        };
        assert!(!run_validation(&config, Path::new(".")).await.unwrap());
    }
}
