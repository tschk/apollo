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
    pub task: String,
    pub channel: String,
    pub model: String,
    pub enabled: bool,
    pub last_run: Option<String>,
    pub next_run: Option<String>,
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
            task: task.to_string(),
            channel: channel.to_string(),
            model: model.to_string(),
            enabled: true,
            last_run: None,
            next_run,
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
                .query("SELECT * FROM cron_jobs WHERE enabled = true AND next_run != None AND next_run <= $now")
                .bind(("now", now))
                .await?;
            let jobs: Vec<CronJob> = result.take(0)?;
            Ok(jobs)
        } else {
            let jobs = self.noop_jobs.lock().unwrap();
            Ok(jobs.iter().filter(|j| j.enabled && j.next_run.as_deref() <= Some(&now)).cloned().collect())
        }
    }

    /// Mark a job as just run and compute next_run.
    pub async fn mark_run(&self, job_id: &str, schedule: &str) -> anyhow::Result<()> {
        let now = chrono::Utc::now();
        let next_run = cron::Schedule::from_str(schedule)
            .ok()
            .and_then(|s| s.upcoming(chrono::Utc).next())
            .map(|t| t.to_rfc3339());

        if let Some(ref memory) = self.memory {
            let db = memory.db();
            let _ = db
                .query("UPDATE cron_jobs SET last_run = $last, next_run = $next WHERE id = $id")
                .bind(("last", now.to_rfc3339()))
                .bind(("next", next_run))
                .bind(("id", job_id.to_string()))
                .await?;
        } else {
            let mut jobs = self.noop_jobs.lock().unwrap();
            if let Some(job) = jobs.iter_mut().find(|j| j.id.as_deref() == Some(job_id)) {
                job.last_run = Some(now.to_rfc3339());
                job.next_run = next_run;
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
                    match scheduler.due_jobs().await {
                        Ok(jobs) => {
                            for job in jobs {
                                let schedule = job.schedule.clone();
                                let job_id = job
                                    .id
                                    .as_ref()
                                    .cloned()
                                    .unwrap_or_default();

                                tracing::info!("Cron: job '{}' is due", job.name);

                                if tx.send(DueJob { job }).await.is_err() {
                                    return; // Receiver dropped
                                }

                                // Mark as run and compute next
                                let _ = scheduler.mark_run(&job_id, &schedule).await;
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
