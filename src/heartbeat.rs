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
    use std::time::Duration;
    use tempfile::tempdir;
    use tokio::time;

    #[tokio::test(start_paused = true)]
    async fn test_heartbeat_sends_message() {
        let dir = tempdir().unwrap();
        let workspace = dir.path().to_path_buf();

        let heartbeat_path = workspace.join("HEARTBEAT.md");
        std::fs::write(&heartbeat_path, "Task 1\nTask 2").unwrap();

        let config = HeartbeatConfig {
            interval_secs: 10,
            quiet_start_hour: 25, // ensure it's never quiet
            quiet_end_hour: 0,
            workspace: workspace.clone(),
            deliver_chat_id: Some("test_chat".to_string()),
        };

        let (tx, mut rx) = mpsc::channel(10);
        let handle = start_heartbeat(config, tx);

        // Advance time to trigger first interval tick (the first one is immediate but skipped,
        // the second one triggers the read)
        time::advance(Duration::from_secs(11)).await;

        let msg = rx.recv().await.expect("Should receive a heartbeat message");
        assert_eq!(msg.chat_id, "test_chat");
        assert!(msg.text.contains("Task 1"));
        assert!(msg.text.contains("Task 2"));
        assert!(msg.text.contains("HEARTBEAT_OK"));

        handle.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn test_heartbeat_quiet_hours() {
        let dir = tempdir().unwrap();
        let workspace = dir.path().to_path_buf();

        let heartbeat_path = workspace.join("HEARTBEAT.md");
        std::fs::write(&heartbeat_path, "Task 1").unwrap();

        // Let's create a config where it's always quiet
        let config = HeartbeatConfig {
            interval_secs: 10,
            quiet_start_hour: 0, // start at 0
            quiet_end_hour: 24,  // end at 24 (always quiet)
            workspace: workspace.clone(),
            deliver_chat_id: Some("test_chat".to_string()),
        };

        let (tx, mut rx) = mpsc::channel(10);
        let handle = start_heartbeat(config, tx);

        time::advance(Duration::from_secs(11)).await;

        // Wait briefly to allow the task to process the tick
        tokio::task::yield_now().await;

        // Ensure no message was sent
        assert!(rx.try_recv().is_err());

        handle.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn test_heartbeat_empty_file() {
        let dir = tempdir().unwrap();
        let workspace = dir.path().to_path_buf();

        let heartbeat_path = workspace.join("HEARTBEAT.md");
        // Create an empty file or file with just comments
        std::fs::write(&heartbeat_path, "# Just a comment\n\n// another comment").unwrap();

        let config = HeartbeatConfig {
            interval_secs: 10,
            quiet_start_hour: 25,
            quiet_end_hour: 0,
            workspace: workspace.clone(),
            deliver_chat_id: Some("test_chat".to_string()),
        };

        let (tx, mut rx) = mpsc::channel(10);
        let handle = start_heartbeat(config, tx);

        time::advance(Duration::from_secs(11)).await;
        tokio::task::yield_now().await;

        assert!(rx.try_recv().is_err());

        handle.abort();
    }
}
