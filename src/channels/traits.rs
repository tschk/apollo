//! Core Channel trait — messaging interface.
//! Inspired by ZeroClaw's channel abstraction + NanoClaw's group isolation.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

/// Incoming message from a channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncomingMessage {
    pub id: String,
    pub sender_id: String,
    pub sender_name: Option<String>,
    pub chat_id: String,
    pub text: String,
    pub is_group: bool,
    pub reply_to: Option<String>,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

/// Outgoing message to a channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutgoingMessage {
    pub chat_id: String,
    pub text: String,
    pub reply_to: Option<String>,
}

/// The core Channel trait.
/// Implement for each messaging platform (Telegram, Discord, CLI, WebSocket, etc.)
#[async_trait]
pub trait Channel: Send + Sync {
    /// Channel name (e.g., "telegram", "discord", "cli")
    fn name(&self) -> &str;

    /// Start receiving messages. Returns a receiver for incoming messages.
    async fn start(&mut self) -> anyhow::Result<mpsc::Receiver<IncomingMessage>>;

    /// Send a message through this channel. Returns an optional message ID for tracking/editing.
    async fn send(&self, message: OutgoingMessage) -> anyhow::Result<Option<String>>;

    /// Send a typing indicator (or equivalent) to the user.
    async fn send_typing(&self, _chat_id: &str) -> anyhow::Result<()> {
        Ok(())
    }

    /// Edit a previously sent message if the channel supports it.
    async fn edit(&self, _chat_id: &str, _message_id: &str, _new_text: &str) -> anyhow::Result<()> {
        Ok(())
    }

    /// Live draft support, if this channel implements it.
    ///
    /// Returning `Some` is the *only* way to opt into draft delivery, and
    /// `DraftChannel` has no default methods, so opting in forces a real
    /// `finalize_draft`.
    fn as_draft(&self) -> Option<&dyn DraftChannel> {
        None
    }

    /// Stop the channel gracefully.
    async fn stop(&mut self) -> anyhow::Result<()>;
}

/// Live draft updates: send a placeholder → edit with progress → finalize with
/// the answer.
///
/// Deliberately has no default implementations: a channel that claims draft
/// support must actually be able to put the final text on screen, because the
/// agent loop will not also return it to the caller.
#[async_trait]
pub trait DraftChannel: Send + Sync {
    /// Send an initial draft placeholder (e.g. "⏳"). Returns message ID for edits.
    async fn send_draft(&self, chat_id: &str, text: &str) -> anyhow::Result<Option<String>>;

    /// Edit the draft with a live progress update (tool start/end). Rate-limited by impl.
    async fn update_draft_progress(
        &self,
        chat_id: &str,
        message_id: &str,
        text: &str,
    ) -> anyhow::Result<()>;

    /// Finalize the draft with the full formatted response, replacing any progress text.
    async fn finalize_draft(
        &self,
        chat_id: &str,
        message_id: &str,
        text: &str,
    ) -> anyhow::Result<()>;
}

/// Who is responsible for putting the final reply in front of the user.
///
/// Constructed once per turn and matched exhaustively at the end of it, so
/// "the caller gets the text" and "the channel already showed the text" are
/// two arms of one decision instead of two independent `if` statements that
/// can drift apart.
pub enum Delivery<'a> {
    /// A live draft message owns the reply; `deliver` edits it in place.
    Draft {
        channel: &'a dyn DraftChannel,
        message_id: String,
    },
    /// Nobody has shown anything; `deliver` hands the text back to the caller.
    Return,
}

impl<'a> Delivery<'a> {
    /// Open a draft on `channel` if it supports drafts and the placeholder
    /// send succeeds; otherwise the reply is returned to the caller.
    pub async fn open(channel: &'a dyn Channel, chat_id: &str, placeholder: &str) -> Self {
        let Some(draft) = channel.as_draft() else {
            return Delivery::Return;
        };
        match draft.send_draft(chat_id, placeholder).await {
            Ok(Some(message_id)) => Delivery::Draft {
                channel: draft,
                message_id,
            },
            Ok(None) => Delivery::Return,
            Err(e) => {
                tracing::warn!("send_draft failed, falling back to returned reply: {e}");
                Delivery::Return
            }
        }
    }

    /// The live draft, if one is open.
    pub fn draft(&self) -> Option<(&dyn DraftChannel, &str)> {
        match self {
            Delivery::Draft {
                channel,
                message_id,
            } => Some((*channel, message_id.as_str())),
            Delivery::Return => None,
        }
    }

    /// Deliver `text`. Returns the string `handle_message` should yield:
    /// empty when the draft already carries it, the text itself otherwise.
    pub async fn deliver(&self, chat_id: &str, text: &str) -> anyhow::Result<String> {
        match self {
            Delivery::Draft {
                channel,
                message_id,
            } => {
                channel.finalize_draft(chat_id, message_id, text).await?;
                Ok(String::new())
            }
            Delivery::Return => Ok(text.to_string()),
        }
    }
}
