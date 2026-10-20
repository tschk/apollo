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
            if hour >= config.quiet_start_hour || hour < config.quiet_end_hour {
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
    use tempfile::tempdir;
    use tokio::time::{advance, pause, Duration};

    #[test]
    fn test_default_config() {
        let config = HeartbeatConfig::default();
        assert_eq!(config.interval_secs, 1800);
        assert_eq!(config.quiet_start_hour, 23);
        assert_eq!(config.quiet_end_hour, 8);
        assert_eq!(config.workspace, PathBuf::from("."));
        assert!(config.deliver_chat_id.is_none());
    }

    #[tokio::test]
    async fn test_heartbeat_sends_task() {
        pause();
        let dir = tempdir().unwrap();
        let config = HeartbeatConfig {
            interval_secs: 10,
            quiet_start_hour: 24, // Never quiet
            quiet_end_hour: 0,
            workspace: dir.path().to_path_buf(),
            deliver_chat_id: Some("test_chat".to_string()),
        };
        let (tx, mut rx) = mpsc::channel(10);

        let handle = start_heartbeat(config, tx);
        tokio::task::yield_now().await;

        // Create HEARTBEAT.md with a valid task
        let heartbeat_path = dir.path().join("HEARTBEAT.md");
        std::fs::write(&heartbeat_path, "Do task A").unwrap();

        advance(Duration::from_secs(11)).await;
        tokio::task::yield_now().await;

        let msg = rx.recv().await.unwrap();
        assert!(msg.text.contains("Do task A"));
        assert_eq!(msg.chat_id, "test_chat");

        // Verify state file is updated
        let state_path = dir.path().join("memory/heartbeat-state.json");
        assert!(state_path.exists());
        let state = std::fs::read_to_string(state_path).unwrap();
        assert!(state.contains("lastHeartbeat"));

        handle.abort();
    }

    #[tokio::test]
    async fn test_heartbeat_ignores_comments_and_empty_lines() {
        pause();
        let dir = tempdir().unwrap();
        let config = HeartbeatConfig {
            interval_secs: 10,
            quiet_start_hour: 24,
            quiet_end_hour: 0,
            workspace: dir.path().to_path_buf(),
            deliver_chat_id: None,
        };
        let (tx, mut rx) = mpsc::channel(10);

        let handle = start_heartbeat(config, tx);
        tokio::task::yield_now().await;

        // Create HEARTBEAT.md with only comments and empty lines
        let heartbeat_path = dir.path().join("HEARTBEAT.md");
        std::fs::write(&heartbeat_path, "\n# Comment\n  // Another comment\n  \n").unwrap();

        advance(Duration::from_secs(11)).await;
        tokio::task::yield_now().await;

        // Should not have received any message
        assert!(rx.try_recv().is_err());

        handle.abort();
    }

    #[tokio::test]
    async fn test_heartbeat_respects_quiet_hours() {
        pause();
        let dir = tempdir().unwrap();

        let current_hour = chrono::Local::now().hour();
        // Set quiet hours so it encompasses current hour
        let quiet_start = current_hour;
        let quiet_end = if current_hour == 23 {
            0
        } else {
            current_hour + 1
        };

        let config = HeartbeatConfig {
            interval_secs: 10,
            quiet_start_hour: quiet_start,
            quiet_end_hour: quiet_end,
            workspace: dir.path().to_path_buf(),
            deliver_chat_id: None,
        };
        let (tx, mut rx) = mpsc::channel(10);

        let handle = start_heartbeat(config, tx);
        tokio::task::yield_now().await;

        // Create HEARTBEAT.md with a valid task
        let heartbeat_path = dir.path().join("HEARTBEAT.md");
        std::fs::write(&heartbeat_path, "Do task A").unwrap();

        advance(Duration::from_secs(11)).await;
        tokio::task::yield_now().await;

        // Should not have received any message due to quiet hours
        assert!(rx.try_recv().is_err());

        handle.abort();
    }
}
