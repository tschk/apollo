//! Heartbeat system — periodic check-ins that read HEARTBEAT.md
//! and trigger agent actions when tasks are present.

use std::path::{Path, PathBuf};
use tokio::sync::mpsc;

use crate::channels::IncomingMessage;

pub struct HeartbeatConfig {
    pub interval_secs: u64,
    pub quiet_start_hour: u32, // 23
    pub quiet_end_hour: u32,   // 8
    pub workspace: PathBuf,
    /// Deliver synthetic heartbeat to this chat_id (else "heartbeat").
    pub deliver_chat_id: Option<String>,
}

impl Default for HeartbeatConfig {
    fn default() -> Self {
        Self {
            interval_secs: 1800, // 30 minutes
            quiet_start_hour: 23,
            quiet_end_hour: 8,
            workspace: PathBuf::from("."),
            deliver_chat_id: None,
        }
    }
}

impl HeartbeatConfig {
    /// Determines whether a given hour falls within the configured quiet hours.
    /// Handles both overnight (start > end) and daytime (start < end) ranges.
    pub fn is_quiet_hour(&self, hour: u32) -> bool {
        if self.quiet_start_hour == self.quiet_end_hour {
            // No quiet hours if start and end are the same
            false
        } else if self.quiet_start_hour > self.quiet_end_hour {
            // Overnight quiet hours (e.g., 23 to 8)
            hour >= self.quiet_start_hour || hour < self.quiet_end_hour
        } else {
            // Daytime quiet hours (e.g., 10 to 14)
            hour >= self.quiet_start_hour && hour < self.quiet_end_hour
        }
    }
}

/// Start the heartbeat background task.
/// Sends synthetic IncomingMessages to the agent loop when HEARTBEAT.md has tasks.
pub fn start_heartbeat(
    config: HeartbeatConfig,
    tx: mpsc::Sender<IncomingMessage>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval =
            tokio::time::interval(tokio::time::Duration::from_secs(config.interval_secs));

        // Skip first tick (immediate)
        interval.tick().await;

        loop {
            interval.tick().await;

            // Check quiet hours
            let now = chrono::Local::now();
            let hour = now.hour();
            if config.is_quiet_hour(hour) {
                tracing::debug!("Heartbeat: quiet hours ({:02}:00), skipping", hour);
                continue;
            }

            // Read HEARTBEAT.md
            let heartbeat_path = config.workspace.join("HEARTBEAT.md");
            let content = match std::fs::read_to_string(&heartbeat_path) {
                Ok(c) => c,
                Err(_) => continue, // No file = skip
            };

            // Check if there are actual tasks (not just comments/empty)
            let has_tasks = content.lines().any(|line| {
                let trimmed = line.trim();
                !trimmed.is_empty() && !trimmed.starts_with('#') && !trimmed.starts_with("//")
            });

            if !has_tasks {
                tracing::debug!("Heartbeat: HEARTBEAT.md empty/comments only, skipping");
                continue;
            }

            // Build heartbeat prompt
            let prompt = format!(
                "Read HEARTBEAT.md if it exists (workspace context). Follow it strictly. \
                 Do not infer or repeat old tasks from prior chats. \
                 If nothing needs attention, reply HEARTBEAT_OK.\n\n\
                 Current HEARTBEAT.md:\n```\n{}\n```",
                content
            );

            let chat_id = config
                .deliver_chat_id
                .clone()
                .unwrap_or_else(|| "heartbeat".to_string());

            let msg = IncomingMessage {
                id: format!("heartbeat-{}", chrono::Utc::now().timestamp()),
                sender_id: "system".to_string(),
                sender_name: Some("heartbeat".to_string()),
                chat_id,
                text: prompt,
                is_group: false,
                reply_to: None,
                timestamp: chrono::Utc::now(),
            };

            if tx.send(msg).await.is_err() {
                tracing::warn!("Heartbeat: channel closed, stopping");
                break;
            }

            update_heartbeat_state(&config.workspace);
        }
    })
}

fn update_heartbeat_state(workspace: &Path) {
    let state_path = workspace.join("memory/heartbeat-state.json");
    if let Some(parent) = state_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let now = chrono::Utc::now().timestamp();
    let state = serde_json::json!({
        "lastHeartbeat": now,
        "lastChecks": {
            "heartbeat": now
        }
    });

    let _ = std::fs::write(
        &state_path,
        serde_json::to_string_pretty(&state).unwrap_or_default(),
    );
}

use chrono::Timelike;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_quiet_hours_overnight() {
        let mut config = HeartbeatConfig::default();
        config.quiet_start_hour = 23;
        config.quiet_end_hour = 8;

        assert!(config.is_quiet_hour(23));
        assert!(config.is_quiet_hour(0));
        assert!(config.is_quiet_hour(7));

        assert!(!config.is_quiet_hour(8));
        assert!(!config.is_quiet_hour(12));
        assert!(!config.is_quiet_hour(22));
    }

    #[test]
    fn test_quiet_hours_daytime() {
        let mut config = HeartbeatConfig::default();
        config.quiet_start_hour = 10;
        config.quiet_end_hour = 14;

        assert!(config.is_quiet_hour(10));
        assert!(config.is_quiet_hour(12));
        assert!(config.is_quiet_hour(13));

        assert!(!config.is_quiet_hour(9));
        assert!(!config.is_quiet_hour(14));
        assert!(!config.is_quiet_hour(23));
    }

    #[test]
    fn test_quiet_hours_none() {
        let mut config = HeartbeatConfig::default();
        config.quiet_start_hour = 0;
        config.quiet_end_hour = 0;

        for hour in 0..24 {
            assert!(!config.is_quiet_hour(hour));
        }
    }
}
