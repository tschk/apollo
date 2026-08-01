//! Discord channel for aclaw
//! Simple text-based Discord bot integration

use async_trait::async_trait;
use tokio::sync::mpsc;

use super::formatting::{format_outgoing_text, FormatTarget};
use super::traits::{Channel, IncomingMessage, OutgoingMessage};

#[derive(Clone)]
pub struct DiscordChannel {
    bot_token: String,
    channel_id: String,
    api_base: String,
}

impl DiscordChannel {
    pub fn new(bot_token: String, channel_id: String) -> Self {
        Self {
            bot_token,
            channel_id,
            api_base: "https://discordapp.com/api".to_string(),
        }
    }

    /// Point the Discord API at another origin (conformance tests).
    pub fn with_api_base(mut self, base: impl Into<String>) -> Self {
        self.api_base = base.into().trim_end_matches('/').to_string();
        self
    }

    /// Send message to Discord
    async fn send_message(&self, channel_id: &str, text: &str) -> anyhow::Result<()> {
        let formatted = format_outgoing_text(FormatTarget::Discord, text);
        let url = format!("{}/channels/{}/messages", self.api_base, channel_id);
        let _resp = crate::http::shared()
            .post(&url)
            .header("Authorization", format!("Bot {}", self.bot_token))
            .json(&serde_json::json!({
                "content": formatted,
            }))
            .send()
            .await?;
        Ok(())
    }
}

#[async_trait]
impl Channel for DiscordChannel {
    fn name(&self) -> &str {
        "discord"
    }

    async fn start(&mut self) -> anyhow::Result<mpsc::Receiver<IncomingMessage>> {
        let (tx, rx) = mpsc::channel(100);
        let bot_token = self.bot_token.clone();
        let channel_id = self.channel_id.clone();
        let api_base = self.api_base.clone();

        tokio::spawn(async move {
            let client = crate::http::shared();
            let url = format!("{}/channels/{}/messages", api_base, channel_id);
            // Seeded by the first poll so startup does not replay history.
            let mut after: Option<String> = None;

            loop {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;

                let mut req = client
                    .get(&url)
                    .header("Authorization", format!("Bot {}", bot_token))
                    .query(&[("limit", "50")]);
                if let Some(after) = &after {
                    req = req.query(&[("after", after.as_str())]);
                }

                let Ok(resp) = req.send().await else { continue };
                let Ok(data) = resp.json::<serde_json::Value>().await else {
                    continue;
                };
                let Some(messages) = data.as_array() else {
                    continue;
                };

                // Discord returns newest first; oldest first is the delivery order.
                for msg in messages.iter().rev() {
                    let Some(id) = msg["id"].as_str() else {
                        continue;
                    };
                    after = Some(id.to_string());

                    if msg["author"]["bot"].as_bool().unwrap_or(false) {
                        continue;
                    }
                    let text = msg["content"].as_str().unwrap_or("").to_string();
                    if text.is_empty() {
                        continue;
                    }

                    let incoming = IncomingMessage {
                        id: id.to_string(),
                        sender_id: msg["author"]["id"].as_str().unwrap_or("").to_string(),
                        sender_name: msg["author"]["username"].as_str().map(|s| s.to_string()),
                        chat_id: msg["channel_id"]
                            .as_str()
                            .unwrap_or(&channel_id)
                            .to_string(),
                        text,
                        is_group: true,
                        reply_to: msg["referenced_message"]["id"]
                            .as_str()
                            .map(|s| s.to_string()),
                        timestamp: chrono::Utc::now(),
                    };

                    if tx.send(incoming).await.is_err() {
                        return;
                    }
                }
            }
        });

        Ok(rx)
    }

    async fn send(&self, message: OutgoingMessage) -> anyhow::Result<Option<String>> {
        let channel_id = if message.chat_id.is_empty() {
            &self.channel_id
        } else {
            &message.chat_id
        };
        self.send_message(channel_id, &message.text).await?;
        Ok(None)
    }

    async fn stop(&mut self) -> anyhow::Result<()> {
        Ok(())
    }
}
