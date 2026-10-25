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

            update_heartbeat_state(&config.workspace).await;
        }
    })
}

async fn update_heartbeat_state(workspace: &Path) {
    let state_path = workspace.join("memory/heartbeat-state.json");
    if let Some(parent) = state_path.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }

    let now = chrono::Utc::now().timestamp();
    let state = serde_json::json!({
        "lastHeartbeat": now,
        "lastChecks": {
            "heartbeat": now
        }
    });

    let _ = tokio::fs::write(
        &state_path,
        serde_json::to_string_pretty(&state).unwrap_or_default(),
    )
    .await;
}

use chrono::Timelike;

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use tokio::time::{advance, Duration};

    #[tokio::test(start_paused = true)]
    async fn test_heartbeat_no_file() {
        let dir = tempdir().unwrap();
        let (tx, mut rx) = mpsc::channel(1);
        let config = HeartbeatConfig {
            workspace: dir.path().to_path_buf(),
            quiet_start_hour: 24, // Never quiet
            quiet_end_hour: 0,
            ..Default::default()
        };

        let handle = start_heartbeat(config, tx);
        advance(Duration::from_secs(1800)).await;

        // Check that no message arrives within a small timeframe
        assert!(tokio::time::timeout(Duration::from_millis(10), rx.recv())
            .await
            .is_err());

        handle.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn test_heartbeat_empty_or_comments() {
        let dir = tempdir().unwrap();
        let (tx, mut rx) = mpsc::channel(1);
        let config = HeartbeatConfig {
            workspace: dir.path().to_path_buf(),
            quiet_start_hour: 24, // Never quiet
            quiet_end_hour: 0,
            ..Default::default()
        };

        let heartbeat_path = dir.path().join("HEARTBEAT.md");
        std::fs::write(&heartbeat_path, "# Comment\n\n// Another comment\n   \n").unwrap();

        let handle = start_heartbeat(config, tx);
        advance(Duration::from_secs(1800)).await;

        // Check that no message arrives within a small timeframe
        assert!(tokio::time::timeout(Duration::from_millis(10), rx.recv())
            .await
            .is_err());

        handle.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn test_heartbeat_valid_file() {
        let dir = tempdir().unwrap();
        let (tx, mut rx) = mpsc::channel(1);
        let config = HeartbeatConfig {
            workspace: dir.path().to_path_buf(),
            quiet_start_hour: 24, // Never quiet
            quiet_end_hour: 0,
            ..Default::default()
        };

        let heartbeat_path = dir.path().join("HEARTBEAT.md");
        std::fs::write(&heartbeat_path, "Task 1\nTask 2").unwrap();

        let handle = start_heartbeat(config, tx);
        advance(Duration::from_secs(1800)).await;

        let msg = rx.recv().await.expect("Should receive a message");
        assert!(msg.text.contains("Task 1\nTask 2"));

        let state_path = dir.path().join("memory/heartbeat-state.json");

        // Since tokio::time::pause is active, real time sleep does not yield properly to the executor.
        // We use tokio::task::yield_now() to allow the spawned task to complete its I/O.
        // We do this in a loop to ensure it has time to create the dir and write the file, and that it's not empty.
        let mut retries = 0;
        let mut state_str = String::new();
        while retries < 50 {
            tokio::task::yield_now().await;
            tokio::time::advance(std::time::Duration::from_millis(10)).await;
            if state_path.exists() {
                if let Ok(s) = std::fs::read_to_string(&state_path) {
                    if !s.is_empty() {
                        state_str = s;
                        break;
                    }
                }
            }
            retries += 1;
        }
        assert!(
            !state_str.is_empty(),
            "state file should be written and not empty"
        );
        let state: serde_json::Value = serde_json::from_str(&state_str).unwrap();
        assert!(state.get("lastHeartbeat").is_some());

        handle.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn test_heartbeat_quiet_hours() {
        let dir = tempdir().unwrap();
        let (tx, mut rx) = mpsc::channel(1);
        let config = HeartbeatConfig {
            workspace: dir.path().to_path_buf(),
            quiet_start_hour: 0, // Always quiet
            quiet_end_hour: 24,
            ..Default::default()
        };

        let heartbeat_path = dir.path().join("HEARTBEAT.md");
        std::fs::write(&heartbeat_path, "Task 1").unwrap();

        let handle = start_heartbeat(config, tx);
        advance(Duration::from_secs(1800)).await;

        // Check that no message arrives within a small timeframe
        assert!(tokio::time::timeout(Duration::from_millis(10), rx.recv())
            .await
            .is_err());

        handle.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn test_heartbeat_channel_closed() {
        let dir = tempdir().unwrap();
        let (tx, rx) = mpsc::channel(1);
        let config = HeartbeatConfig {
            workspace: dir.path().to_path_buf(),
            quiet_start_hour: 24,
            quiet_end_hour: 0,
            ..Default::default()
        };

        let heartbeat_path = dir.path().join("HEARTBEAT.md");
        std::fs::write(&heartbeat_path, "Task 1").unwrap();

        let handle = start_heartbeat(config, tx);

        drop(rx); // Close the channel

        advance(Duration::from_secs(1800)).await;

        // the handle should complete without error
        let result = handle.await;
        assert!(result.is_ok());
    }
}
