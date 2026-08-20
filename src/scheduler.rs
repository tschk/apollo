//! Cron scheduler — recurring task automation
//! Phase 4 feature: Time-based task scheduling

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;

/// Scheduled task
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Schedule {
    pub id: String,
    pub cron_expression: String,
    pub task_goal: String,
    pub priority: u8,
    pub enabled: bool,
    pub last_run: Option<chrono::DateTime<chrono::Utc>>,
    pub next_run: Option<chrono::DateTime<chrono::Utc>>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Scheduler instance
pub struct Scheduler {
    schedules: Arc<RwLock<Vec<Schedule>>>,
}

impl Scheduler {
    pub fn new() -> Self {
        Self {
            schedules: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Add a new schedule
    pub async fn schedule(&self, cron: &str, goal: &str, priority: u8) -> anyhow::Result<String> {
        // Validate cron expression
        let _parsed = cron::Schedule::from_str(cron)
            .map_err(|e| anyhow::anyhow!("Invalid cron expression: {}", e))?;

        let schedule = Schedule {
            id: uuid::Uuid::new_v4().to_string(),
            cron_expression: cron.to_string(),
            task_goal: goal.to_string(),
            priority,
            enabled: true,
            last_run: None,
            next_run: None,
            created_at: chrono::Utc::now(),
        };

        let schedule_id = schedule.id.clone();
        self.schedules.write().await.push(schedule);
        Ok(schedule_id)
    }

    /// List all schedules
    pub async fn list(&self) -> Vec<Schedule> {
        self.schedules.read().await.clone()
    }

    /// Enable a schedule
    pub async fn enable(&self, schedule_id: &str) -> anyhow::Result<()> {
        let mut schedules = self.schedules.write().await;
        if let Some(sched) = schedules.iter_mut().find(|s| s.id == schedule_id) {
            sched.enabled = true;
            Ok(())
        } else {
            Err(anyhow::anyhow!("Schedule not found"))
        }
    }

    /// Disable a schedule
    pub async fn disable(&self, schedule_id: &str) -> anyhow::Result<()> {
        let mut schedules = self.schedules.write().await;
        if let Some(sched) = schedules.iter_mut().find(|s| s.id == schedule_id) {
            sched.enabled = false;
            Ok(())
        } else {
            Err(anyhow::anyhow!("Schedule not found"))
        }
    }

    /// Delete a schedule
    pub async fn delete(&self, schedule_id: &str) -> anyhow::Result<()> {
        let mut schedules = self.schedules.write().await;
        if let Some(pos) = schedules.iter().position(|s| s.id == schedule_id) {
            schedules.remove(pos);
            Ok(())
        } else {
            Err(anyhow::anyhow!("Schedule not found"))
        }
    }

    /// Get next tasks to run
    pub async fn next_tasks(&self) -> Vec<Schedule> {
        let now = chrono::Utc::now();
        let schedules = self.schedules.read().await;

        schedules
            .iter()
            .filter_map(|sched| {
                if !sched.enabled {
                    return None;
                }

                if let Ok(schedule) = cron::Schedule::from_str(&sched.cron_expression) {
                    // Use the public iterator interface instead of next_after
                    let mut iter = schedule.after(&now);

                    if let Some(next_time) = iter.next() {
                        // Task is due if next time is now or in the past
                        if next_time <= now {
                            return Some(sched.clone());
                        }
                    }
                }

                None
            })
            .collect()
    }
}

impl Default for Scheduler {
    fn default() -> Self {
        Self::new()
    }
}

use std::str::FromStr;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_schedule_creation() {
        let scheduler = Scheduler::new();

        let id = scheduler
            .schedule("0 0 9 * * MON *", "Monday digest", 7)
            .await
            .unwrap();
        assert!(!id.is_empty());

        let schedules = scheduler.list().await;
        assert_eq!(schedules.len(), 1);
    }

    #[tokio::test]
    async fn test_invalid_cron() {
        let scheduler = Scheduler::new();

        // Standard invalid strings
        let result = scheduler.schedule("invalid", "test", 5).await;
        assert!(result.is_err());

        // Empty string
        let result = scheduler.schedule("", "test", 5).await;
        assert!(result.is_err());

        // Too few fields
        let result = scheduler.schedule("* * * *", "test", 5).await;
        assert!(result.is_err());

        // Too many fields
        let result = scheduler.schedule("* * * * * * * * *", "test", 5).await;
        assert!(result.is_err());

        // Out of range value
        let result = scheduler.schedule("60 * * * * * *", "test", 5).await;
        assert!(result.is_err());

        // Exceptionally long string
        let long_string = "a".repeat(10_000);
        let result = scheduler.schedule(&long_string, "test", 5).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_enable_disable() {
        let scheduler = Scheduler::new();

        let id = scheduler
            .schedule("0 0 9 * * * *", "daily", 5)
            .await
            .unwrap();

        scheduler.disable(&id).await.unwrap();
        let schedules = scheduler.list().await;
        assert!(!schedules[0].enabled);

        scheduler.enable(&id).await.unwrap();
        let schedules = scheduler.list().await;
        assert!(schedules[0].enabled);
    }
}
