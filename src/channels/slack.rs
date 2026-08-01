//! Slack channel — Bot + App token integration

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::mpsc;

use super::traits::*;

pub struct SlackChannel {
    bot_token: String,
    app_token: Option<String>,
    channel_id: Option<String>,
    api_base: String,
}

impl SlackChannel {
    pub fn new(bot_token: impl Into<String>) -> Self {
        Self {
            bot_token: bot_token.into(),
            app_token: None,
            channel_id: None,
            api_base: "https://slack.com/api".to_string(),
        }
    }

    /// Point the Slack API at another origin (conformance tests).
    pub fn with_api_base(mut self, base: impl Into<String>) -> Self {
        self.api_base = base.into().trim_end_matches('/').to_string();
        self
    }

    pub fn with_app_token(mut self, token: impl Into<String>) -> Self {
        self.app_token = Some(token.into());
        self
    }

    pub fn with_channel(mut self, channel_id: impl Into<String>) -> Self {
        self.channel_id = Some(channel_id.into());
        self
    }
}

#[async_trait]
impl Channel for SlackChannel {
    fn name(&self) -> &str {
        "slack"
    }

    async fn start(&mut self) -> anyhow::Result<mpsc::Receiver<IncomingMessage>> {
        let (tx, rx) = mpsc::channel(32);
        let bot_token = self.bot_token.clone();
        let api_base = self.api_base.clone();
        let Some(channel_id) = self.channel_id.clone() else {
            anyhow::bail!(
                "Slack channel requires a channel id — conversations.history cannot be polled without one"
            );
        };

        // Poll Slack conversations.history for new messages
        tokio::spawn(async move {
            let client = crate::http::shared();
            let mut last_ts = String::new();

            loop {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;

                let resp = client
                    .get(format!("{}/conversations.history", api_base))
                    .header("Authorization", format!("Bearer {}", bot_token))
                    .query(&[("channel", channel_id.as_str()), ("limit", "5")])
                    .send()
                    .await;

                if let Ok(resp) = resp {
                    if let Ok(data) = resp.json::<Value>().await {
                        if let Some(messages) = data["messages"].as_array() {
                            for msg in messages.iter().rev() {
                                let ts = msg["ts"].as_str().unwrap_or("").to_string();
                                if ts <= last_ts {
                                    continue;
                                }
                                if msg["bot_id"].is_string() {
                                    continue;
                                }

                                let text = msg["text"].as_str().unwrap_or("").to_string();
                                if text.is_empty() {
                                    continue;
                                }

                                last_ts = ts.clone();

                                let incoming = IncomingMessage {
                                    id: ts,
                                    sender_id: msg["user"]
                                        .as_str()
                                        .unwrap_or("unknown")
                                        .to_string(),
                                    sender_name: None,
                                    chat_id: msg["channel"]
                                        .as_str()
                                        .unwrap_or(channel_id.as_str())
                                        .to_string(),
                                    text,
                                    is_group: true,
                                    reply_to: msg["thread_ts"].as_str().map(|s| s.to_string()),
                                    timestamp: chrono::Utc::now(),
                                };

                                if tx.send(incoming).await.is_err() {
                                    return;
                                }
                            }
                        }
                    }
                }
            }
        });

        Ok(rx)
    }

    async fn send(&self, message: OutgoingMessage) -> anyhow::Result<Option<String>> {
        let client = crate::http::shared();

        let mut body = serde_json::json!({
            "channel": &message.chat_id,
            "text": &message.text,
        });

        if let Some(reply_to) = &message.reply_to {
            body["thread_ts"] = Value::String(reply_to.clone());
        }

        let resp = client
            .post(format!("{}/chat.postMessage", self.api_base))
            .header("Authorization", format!("Bearer {}", self.bot_token))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            anyhow::bail!("Slack send failed: {}", resp.status());
        }

        Ok(None)
    }

    fn supports_media(&self) -> bool {
        true
    }

    /// Slack's `files.upload` is deprecated and being switched off, so this is
    /// the three-step external flow: reserve an upload URL, PUT the bytes to
    /// the URL Slack hands back, then complete the upload against the channel.
    async fn send_media(&self, media: OutgoingMedia) -> anyhow::Result<Option<String>> {
        let channel = if media.chat_id.is_empty() {
            self.channel_id
                .clone()
                .ok_or_else(|| anyhow::anyhow!("Slack send_media needs a channel"))?
        } else {
            media.chat_id.clone()
        };

        let (bytes, filename) = super::media::load(&media.source).await?;
        let client = crate::http::shared();

        // 1. Reserve.
        let resp = client
            .get(format!("{}/files.getUploadURLExternal", self.api_base))
            .header("Authorization", format!("Bearer {}", self.bot_token))
            .query(&[
                ("filename", filename.as_str()),
                ("length", &bytes.len().to_string()),
            ])
            .send()
            .await?;
        let reserved = resp.json::<Value>().await?;
        if reserved["ok"].as_bool() != Some(true) {
            anyhow::bail!(
                "Slack files.getUploadURLExternal failed: {}",
                reserved["error"].as_str().unwrap_or("unknown")
            );
        }
        let upload_url = reserved["upload_url"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Slack returned no upload_url"))?;
        let file_id = reserved["file_id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Slack returned no file_id"))?
            .to_string();

        // 2. Upload the bytes to the URL Slack chose.
        let form = reqwest::multipart::Form::new().part(
            "file",
            reqwest::multipart::Part::bytes(bytes).file_name(filename.clone()),
        );
        let upload = client.post(upload_url).multipart(form).send().await?;
        if !upload.status().is_success() {
            anyhow::bail!("Slack file upload failed: {}", upload.status());
        }

        // 3. Complete, which is what actually posts it into the channel.
        let mut body = serde_json::json!({
            "files": [{ "id": &file_id, "title": &filename }],
            "channel_id": channel,
        });
        if let Some(caption) = &media.caption {
            body["initial_comment"] = Value::String(caption.clone());
        }
        if let Some(reply_to) = &media.reply_to {
            body["thread_ts"] = Value::String(reply_to.clone());
        }

        let complete = client
            .post(format!("{}/files.completeUploadExternal", self.api_base))
            .header("Authorization", format!("Bearer {}", self.bot_token))
            .json(&body)
            .send()
            .await?;
        let done = complete.json::<Value>().await?;
        if done["ok"].as_bool() != Some(true) {
            anyhow::bail!(
                "Slack files.completeUploadExternal failed: {}",
                done["error"].as_str().unwrap_or("unknown")
            );
        }
        Ok(Some(file_id))
    }

    async fn stop(&mut self) -> anyhow::Result<()> {
        Ok(())
    }
}
