//! Message tool — send messages via Telegram (like OpenClaw's message tool).
//! Allows the AI to proactively send messages, edit, delete.

use async_trait::async_trait;
use serde::Deserialize;
use std::sync::Arc;

use super::traits::*;
use crate::channels::telegram::TelegramChannel;

pub struct MessageTool {
    tg: Arc<TelegramChannel>,
}

impl MessageTool {
    pub fn new(tg: Arc<TelegramChannel>) -> Self {
        Self { tg }
    }
}

#[derive(Deserialize)]
struct MessageArgs {
    /// Action: send, edit, delete
    action: String,
    /// Message text (for send/edit)
    message: Option<String>,
    /// Message ID (for edit/delete)
    message_id: Option<i64>,
}

#[async_trait]
impl Tool for MessageTool {
    fn name(&self) -> &str {
        "message"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "message".to_string(),
            description: "Send, edit, or delete Telegram messages. Actions: send, edit, delete."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["send", "edit", "delete"],
                        "description": "Message action"
                    },
                    "message": {
                        "type": "string",
                        "description": "Message text (for send/edit)"
                    },
                    "message_id": {
                        "type": "integer",
                        "description": "Message ID (for edit/delete)"
                    }
                },
                "required": ["action"]
            }),
        }
    }

    async fn execute(&self, arguments: &str) -> anyhow::Result<ToolResult> {
        let args: MessageArgs = serde_json::from_str(arguments)?;

        match args.action.as_str() {
            "send" => {
                let text = args.message.unwrap_or_default();
                if text.is_empty() {
                    return Ok(ToolResult::error("Message text required for send action"));
                }
                let msg_id = self.tg.send_message(&text).await?;
                Ok(ToolResult::success(format!(
                    "Sent message (id: {})",
                    msg_id
                )))
            }
            "edit" => {
                let msg_id = args.message_id.unwrap_or(0);
                let text = args.message.unwrap_or_default();
                if msg_id == 0 || text.is_empty() {
                    return Ok(ToolResult::error(
                        "message_id and message required for edit action",
                    ));
                }
                self.tg.edit_message(msg_id, &text).await?;
                Ok(ToolResult::success(format!("Edited message {}", msg_id)))
            }
            "delete" => {
                let msg_id = args.message_id.unwrap_or(0);
                if msg_id == 0 {
                    return Ok(ToolResult::error("message_id required for delete action"));
                }
                self.tg.delete_message(msg_id).await?;
                Ok(ToolResult::success(format!("Deleted message {}", msg_id)))
            }
            other => Ok(ToolResult::error(format!("Unknown action: {}", other))),
        }
    }
}
