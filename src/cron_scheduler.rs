//! SurrealDB-backed Cron Scheduler — persistent scheduled tasks.
//! Stores jobs in SurrealDB, ticks every 60s, spawns agent sessions for due jobs.

use crate::memory::surreal::SurrealMemory;
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use std::sync::Arc;

/// A cron job stored in SurrealDB.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronJob {
    pub id: Option<String>,
    pub name: String,
    pub schedule: String,
    #[serde(default)]
    pub one_shot: bool,
    pub task: String,
    pub channel: String,
    #[serde(default)]
    pub chat_id: String,
    pub model: String,
    pub enabled: bool,
    pub last_run: Option<String>,
    pub next_run: Option<String>,
    #[serde(default = "default_job_status")]
    pub status: String,
    #[serde(default)]
    pub retry_count: u32,
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub lease_until: Option<String>,
    #[serde(default)]
    pub run_token: Option<String>,
}

fn default_job_status() -> String {
    "active".to_string()
}

fn default_max_retries() -> u32 {
    3
}

/// SurrealDB-backed cron scheduler.
pub struct CronScheduler {
    memory: Option<Arc<SurrealMemory>>,
    /// In-memory fallback for noop/testing — keyed by name
    noop_jobs: std::sync::Mutex<Vec<CronJob>>,
}

impl CronScheduler {
    /// Create with a SurrealDB memory backend.
    pub fn new(memory: Arc<SurrealMemory>) -> Self {
        Self::new_maybe(Some(memory))
    }

    /// Create without memory backend (in-memory only).
    pub fn new_noop() -> Self {
        Self::new_maybe(None)
    }

    fn new_maybe(memory: Option<Arc<SurrealMemory>>) -> Self {
        Self {
            memory,
            noop_jobs: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Add a new cron job. Returns the job ID.
    pub async fn add(
        &self,
        name: &str,
        schedule: &str,
        task: &str,
        channel: &str,
        chat_id: &str,
        model: &str,
    ) -> anyhow::Result<String> {
        // Validate cron expression
        let parsed = cron::Schedule::from_str(schedule)
            .map_err(|e| anyhow::anyhow!("Invalid cron expression: {}", e))?;

        // Compute next run
        let next_run = parsed.upcoming(chrono::Utc).next().map(|t| t.to_rfc3339());

        let job = CronJob {
            id: Some(name.to_string()),
            name: name.to_string(),
            schedule: schedule.to_string(),
            one_shot: false,
            task: task.to_string(),
            channel: channel.to_string(),
            chat_id: chat_id.to_string(),
            model: model.to_string(),
            enabled: true,
            last_run: None,
            next_run,
            status: default_job_status(),
            retry_count: 0,
            max_retries: default_max_retries(),
            last_error: None,
            lease_until: None,
            run_token: None,
        };

        if let Some(ref memory) = self.memory {
            let db = memory.db();
            let created: Option<CronJob> = db.create("cron_jobs").content(job).await?;
            let created = created.ok_or_else(|| anyhow::anyhow!("Failed to create cron job"))?;
            Ok(created
                .id
                .ok_or_else(|| anyhow::anyhow!("Created cron job is missing an id"))?
                .to_string())
        } else {
            let mut jobs = self.noop_jobs.lock().unwrap();
            jobs.push(job);
            Ok(name.to_string())
        }
    }

    pub async fn add_once(
        &self,
        name: &str,
        run_at: chrono::DateTime<chrono::Utc>,
        task: &str,
        channel: &str,
        chat_id: &str,
        model: &str,
    ) -> anyhow::Result<String> {
        if run_at <= chrono::Utc::now() {
            anyhow::bail!("run_at must be in the future");
        }
        let job = CronJob {
            id: Some(name.to_string()),
            name: name.to_string(),
            schedule: String::new(),
            one_shot: true,
            task: task.to_string(),
            channel: channel.to_string(),
            chat_id: chat_id.to_string(),
            model: model.to_string(),
            enabled: true,
            last_run: None,
            next_run: Some(run_at.to_rfc3339()),
            status: default_job_status(),
            retry_count: 0,
            max_retries: default_max_retries(),
            last_error: None,
            lease_until: None,
            run_token: None,
        };
        if let Some(ref memory) = self.memory {
            let created: Option<CronJob> = memory.db().create("cron_jobs").content(job).await?;
            Ok(created
                .and_then(|job| job.id)
                .ok_or_else(|| anyhow::anyhow!("Created one-shot job is missing an id"))?)
        } else {
            self.noop_jobs.lock().unwrap().push(job);
            Ok(name.to_string())
        }
    }

    /// List all cron jobs.
    pub async fn list(&self) -> anyhow::Result<Vec<CronJob>> {
        if let Some(ref memory) = self.memory {
            let db = memory.db();
            let mut result: surrealdb::Response =
                db.query("SELECT * FROM cron_jobs ORDER BY name").await?;
            let jobs: Vec<CronJob> = result.take(0)?;
            Ok(jobs)
        } else {
            let jobs = self.noop_jobs.lock().unwrap();
            Ok(jobs.clone())
        }
    }

    /// Remove a cron job by ID or name.
    pub async fn remove(&self, id_or_name: &str) -> anyhow::Result<bool> {
        if let Some(ref memory) = self.memory {
            let db = memory.db();
            let mut result: surrealdb::Response = db
                .query("DELETE FROM cron_jobs WHERE id = $target OR name = $target")
                .bind(("target", id_or_name.to_string()))
                .await?;
            let deleted: Vec<CronJob> = result.take(0)?;
            Ok(!deleted.is_empty())
        } else {
            let mut jobs = self.noop_jobs.lock().unwrap();
            let before = jobs.len();
            jobs.retain(|j| j.name != id_or_name);
            Ok(jobs.len() < before)
        }
    }

    /// Enable a cron job.
    pub async fn enable(&self, id_or_name: &str) -> anyhow::Result<bool> {
        if let Some(ref memory) = self.memory {
            let db = memory.db();
            let mut result: surrealdb::Response = db
                .query("UPDATE cron_jobs SET enabled = true WHERE id = $target OR name = $target")
                .bind(("target", id_or_name.to_string()))
                .await?;
            let updated: Vec<CronJob> = result.take(0)?;
            Ok(!updated.is_empty())
        } else {
            let mut jobs = self.noop_jobs.lock().unwrap();
            if let Some(job) = jobs.iter_mut().find(|j| j.name == id_or_name) {
                job.enabled = true;
                Ok(true)
            } else {
                Ok(false)
            }
        }
    }

    /// Disable a cron job.
    pub async fn disable(&self, id_or_name: &str) -> anyhow::Result<bool> {
        if let Some(ref memory) = self.memory {
            let db = memory.db();
            let mut result: surrealdb::Response = db
                .query("UPDATE cron_jobs SET enabled = false WHERE id = $target OR name = $target")
                .bind(("target", id_or_name.to_string()))
                .await?;
            let updated: Vec<CronJob> = result.take(0)?;
            Ok(!updated.is_empty())
        } else {
            let mut jobs = self.noop_jobs.lock().unwrap();
            if let Some(job) = jobs.iter_mut().find(|j| j.name == id_or_name) {
                job.enabled = false;
                Ok(true)
            } else {
                Ok(false)
            }
        }
    }

    /// Get due jobs (next_run <= now, enabled).
    pub async fn due_jobs(&self) -> anyhow::Result<Vec<CronJob>> {
        let now = chrono::Utc::now().to_rfc3339();
        if let Some(ref memory) = self.memory {
            let db = memory.db();
            let mut result: surrealdb::Response = db
                .query("SELECT * FROM cron_jobs WHERE enabled = true AND ((status = 'active' OR status = NONE) AND next_run != NONE AND next_run <= $now OR status = 'running' AND lease_until != NONE AND lease_until <= $now)")
                .bind(("now", now))
                .await?;
            let jobs: Vec<CronJob> = result.take(0)?;
            Ok(jobs)
        } else {
            let jobs = self.noop_jobs.lock().unwrap();
            Ok(jobs
                .iter()
                .filter(|j| {
                    j.enabled
                        && ((j.status == "active" && j.next_run.as_deref() <= Some(&now))
                            || (j.status == "running" && j.lease_until.as_deref() <= Some(&now)))
                })
                .cloned()
                .collect())
        }
    }

    pub async fn claim_due_jobs(&self, channel: &str) -> anyhow::Result<Vec<CronJob>> {
        let due = self.due_jobs().await?;
        let claim_now = chrono::Utc::now().to_rfc3339();
        let lease_until = (chrono::Utc::now() + chrono::Duration::minutes(10)).to_rfc3339();
        let mut claimed = Vec::new();
        for job in due {
            if job.channel != channel {
                continue;
            }
            let Some(job_id) = job.id.as_deref() else {
                continue;
            };
            let run_token = uuid::Uuid::new_v4().to_string();
            if let Some(ref memory) = self.memory {
                let mut result = memory
                    .db()
                    .query("UPDATE cron_jobs SET status = 'running', lease_until = $lease, run_token = $run_token WHERE id = $id AND enabled = true AND (status = 'active' OR status = NONE OR status = 'running' AND lease_until <= $now) RETURN AFTER")
                    .bind(("id", job_id.to_string()))
                    .bind(("lease", lease_until.clone()))
                    .bind(("run_token", run_token.clone()))
                    .bind(("now", claim_now.clone()))
                    .await?;
                let updated: Vec<CronJob> = result.take(0)?;
                claimed.extend(updated);
            } else {
                let mut jobs = self.noop_jobs.lock().unwrap();
                if let Some(stored) = jobs.iter_mut().find(|stored| {
                    stored.id.as_deref() == Some(job_id)
                        && (stored.status == "active"
                            || stored.lease_until.as_deref() <= Some(claim_now.as_str()))
                }) {
                    stored.status = "running".to_string();
                    stored.lease_until = Some(lease_until.clone());
                    stored.run_token = Some(run_token);
                    claimed.push(stored.clone());
                }
            }
        }
        Ok(claimed)
    }

    /// Mark a job as just run and compute next_run.
    pub async fn mark_run(
        &self,
        job_id: &str,
        run_token: &str,
        schedule: &str,
    ) -> anyhow::Result<()> {
        let now = chrono::Utc::now();
        let next_run = cron::Schedule::from_str(schedule)
            .ok()
            .and_then(|s| s.upcoming(chrono::Utc).next())
            .map(|t| t.to_rfc3339());
        let status = if schedule.is_empty() {
            "completed"
        } else {
            "active"
        };

        if let Some(ref memory) = self.memory {
            let db = memory.db();
            let _ = db
                .query("UPDATE cron_jobs SET last_run = $last, next_run = $next, status = $status, retry_count = 0, last_error = NONE, lease_until = NONE, run_token = NONE WHERE id = $id AND status = 'running' AND run_token = $run_token")
                .bind(("last", now.to_rfc3339()))
                .bind(("next", next_run))
                .bind(("status", status.to_string()))
                .bind(("id", job_id.to_string()))
                .bind(("run_token", run_token.to_string()))
                .await?;
        } else {
            let mut jobs = self.noop_jobs.lock().unwrap();
            if let Some(job) = jobs.iter_mut().find(|job| {
                job.id.as_deref() == Some(job_id) && job.run_token.as_deref() == Some(run_token)
            }) {
                job.last_run = Some(now.to_rfc3339());
                job.next_run = next_run;
                job.status = status.to_string();
                job.retry_count = 0;
                job.last_error = None;
                job.lease_until = None;
                job.run_token = None;
            }
        }
        Ok(())
    }

    pub async fn release_run(&self, job_id: &str, run_token: &str) -> anyhow::Result<()> {
        if let Some(ref memory) = self.memory {
            let _ = memory
                .db()
                .query("UPDATE cron_jobs SET status = 'active', lease_until = NONE, run_token = NONE WHERE id = $id AND status = 'running' AND run_token = $run_token")
                .bind(("id", job_id.to_string()))
                .bind(("run_token", run_token.to_string()))
                .await?;
        } else {
            let mut jobs = self.noop_jobs.lock().unwrap();
            if let Some(job) = jobs.iter_mut().find(|job| {
                job.id.as_deref() == Some(job_id) && job.run_token.as_deref() == Some(run_token)
            }) {
                job.status = "active".to_string();
                job.lease_until = None;
                job.run_token = None;
            }
        }
        Ok(())
    }

    pub async fn fail_run(&self, job_id: &str, run_token: &str, error: &str) -> anyhow::Result<()> {
        let redacted = crate::redaction::redact_text(error);
        if let Some(ref memory) = self.memory {
            let db = memory.db();
            let Some(job) = self.list().await?.into_iter().find(|job| {
                job.id.as_deref() == Some(job_id) && job.run_token.as_deref() == Some(run_token)
            }) else {
                return Ok(());
            };
            let retry_count = job.retry_count.saturating_add(1);
            let status = if retry_count >= job.max_retries {
                "failed"
            } else {
                "active"
            };
            let delay = 2_i64.saturating_pow(retry_count.min(10)) * 60;
            let next_run = (chrono::Utc::now() + chrono::Duration::seconds(delay)).to_rfc3339();
            let _ = db
                .query("UPDATE cron_jobs SET status = $status, retry_count = $retry_count, last_error = $error, next_run = $next_run, lease_until = NONE, run_token = NONE WHERE id = $id AND status = 'running' AND run_token = $run_token")
                .bind(("status", status.to_string()))
                .bind(("retry_count", retry_count))
                .bind(("error", redacted))
                .bind(("next_run", next_run))
                .bind(("id", job_id.to_string()))
                .bind(("run_token", run_token.to_string()))
                .await?;
        } else {
            let mut jobs = self.noop_jobs.lock().unwrap();
            if let Some(job) = jobs.iter_mut().find(|job| {
                job.id.as_deref() == Some(job_id) && job.run_token.as_deref() == Some(run_token)
            }) {
                job.retry_count = job.retry_count.saturating_add(1);
                job.status = if job.retry_count >= job.max_retries {
                    "failed".to_string()
                } else {
                    "active".to_string()
                };
                job.last_error = Some(redacted);
                job.lease_until = None;
                job.run_token = None;
            }
        }
        Ok(())
    }
}

/// A due job ready to execute (returned by the ticker).
#[derive(Debug, Clone)]
pub struct DueJob {
    pub job: CronJob,
}

/// Start the cron ticker as a background task. Returns a receiver for due jobs.
pub fn start_cron_ticker(
    scheduler: Arc<CronScheduler>,
    channel: String,
) -> (
    tokio::sync::mpsc::Receiver<DueJob>,
    Arc<tokio::sync::Notify>,
) {
    let (tx, rx) = tokio::sync::mpsc::channel(32);
    let shutdown = Arc::new(tokio::sync::Notify::new());
    let shutdown_clone = shutdown.clone();

    tokio::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(60));
        // Skip immediate first tick
        interval.tick().await;

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    match scheduler.claim_due_jobs(&channel).await {
                        Ok(jobs) => {
                            for job in jobs {
                                tracing::info!("Cron: job '{}' is due", job.name);

                                if tx.send(DueJob { job }).await.is_err() {
                                    return; // Receiver dropped
                                }
                            }
                        }
                        Err(e) => {
                            tracing::error!("Cron ticker error: {}", e);
                        }
                    }
                }
                _ = shutdown_clone.notified() => {
                    tracing::info!("Cron ticker: shutting down");
                    break;
                }
            }
        }
    });

    (rx, shutdown)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn due_job(channel: &str) -> CronJob {
        CronJob {
            id: Some("job-1".to_string()),
            name: "job".to_string(),
            schedule: "0 0 0 * * * *".to_string(),
            one_shot: false,
            task: "task".to_string(),
            channel: channel.to_string(),
            chat_id: "chat".to_string(),
            model: "model".to_string(),
            enabled: true,
            last_run: None,
            next_run: Some((chrono::Utc::now() - chrono::Duration::minutes(1)).to_rfc3339()),
            status: "active".to_string(),
            retry_count: 0,
            max_retries: 3,
            last_error: None,
            lease_until: None,
            run_token: None,
        }
    }

    #[tokio::test]
    async fn claims_only_active_channel() {
        let scheduler = CronScheduler::new_noop();
        scheduler
            .noop_jobs
            .lock()
            .unwrap()
            .push(due_job("telegram"));
        assert!(scheduler.claim_due_jobs("cli").await.unwrap().is_empty());
        let claimed = scheduler.claim_due_jobs("telegram").await.unwrap();
        assert_eq!(claimed.len(), 1);
        assert!(claimed[0].run_token.is_some());
    }

    #[tokio::test]
    async fn stale_run_token_cannot_complete_job() {
        let scheduler = CronScheduler::new_noop();
        scheduler.noop_jobs.lock().unwrap().push(due_job("cli"));
        let job = scheduler.claim_due_jobs("cli").await.unwrap().remove(0);
        scheduler
            .mark_run("job-1", "stale-token", &job.schedule)
            .await
            .unwrap();
        assert_eq!(scheduler.list().await.unwrap()[0].status, "running");
        scheduler
            .mark_run("job-1", job.run_token.as_deref().unwrap(), &job.schedule)
            .await
            .unwrap();
        assert_eq!(scheduler.list().await.unwrap()[0].status, "active");
    }
}
