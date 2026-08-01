//! Discord channel for aclaw
//! Simple text-based Discord bot integration

use async_trait::async_trait;
use tokio::sync::mpsc;

use super::formatting::{format_outgoing_text, FormatTarget};
use super::traits::{Channel, IncomingMessage, OutgoingMedia, OutgoingMessage};

/// How inbound messages arrive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Transport {
    /// The gateway websocket — sees DMs and every channel, with no poll delay.
    /// Requires the privileged MESSAGE_CONTENT intent on the bot.
    #[default]
    Gateway,
    /// REST polling of the one configured channel. No DMs. The fallback for a
    /// bot whose privileged intents have not been granted.
    Polling,
}

#[derive(Clone)]
pub struct DiscordChannel {
    bot_token: String,
    channel_id: String,
    api_base: String,
    transport: Transport,
    gateway_url: String,
}

impl DiscordChannel {
    pub fn new(bot_token: String, channel_id: String) -> Self {
        Self {
            bot_token,
            channel_id,
            api_base: "https://discordapp.com/api".to_string(),
            transport: Transport::default(),
            gateway_url: super::discord_gateway::DEFAULT_GATEWAY_URL.to_string(),
        }
    }

    /// Choose the inbound transport.
    pub fn with_transport(mut self, transport: Transport) -> Self {
        self.transport = transport;
        self
    }

    /// Point the gateway at another websocket (conformance tests).
    pub fn with_gateway_url(mut self, url: impl Into<String>) -> Self {
        self.gateway_url = url.into();
        self
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

        if self.transport == Transport::Gateway {
            super::discord_gateway::spawn(bot_token, self.gateway_url.clone(), tx);
            return Ok(rx);
        }

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

    fn supports_media(&self) -> bool {
        true
    }

    async fn send_media(&self, media: OutgoingMedia) -> anyhow::Result<Option<String>> {
        let channel_id = if media.chat_id.is_empty() {
            self.channel_id.clone()
        } else {
            media.chat_id.clone()
        };

        // Discord does not fetch remote URLs on your behalf the way Telegram
        // does, so a URL source is downloaded here and re-uploaded.
        let (bytes, filename) = super::media::load(&media.source).await?;

        let mut payload = serde_json::json!({});
        if let Some(caption) = &media.caption {
            payload["content"] =
                serde_json::Value::String(format_outgoing_text(FormatTarget::Discord, caption));
        }
        if let Some(reply_to) = &media.reply_to {
            payload["message_reference"] = serde_json::json!({ "message_id": reply_to });
        }

        let form = reqwest::multipart::Form::new()
            .text("payload_json", payload.to_string())
            .part(
                "files[0]",
                reqwest::multipart::Part::bytes(bytes).file_name(filename),
            );

        let url = format!("{}/channels/{}/messages", self.api_base, channel_id);
        let resp = crate::http::shared()
            .post(&url)
            .header("Authorization", format!("Bot {}", self.bot_token))
            .multipart(form)
            .send()
            .await?;
        if !resp.status().is_success() {
            anyhow::bail!("Discord attachment upload failed: {}", resp.status());
        }
        let body = resp.json::<serde_json::Value>().await?;
        Ok(body["id"].as_str().map(|s| s.to_string()))
    }

    async fn stop(&mut self) -> anyhow::Result<()> {
        Ok(())
    }
}
