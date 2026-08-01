//! Telegram channel — polling mode with progress feedback via message editing.
//! - Sends "thinking..." message immediately
//! - Edits it with tool call progress
//! - Deletes progress and sends final clean message

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::mpsc;

use super::formatting::{chunk_outgoing_text, format_outgoing_text, FormatTarget};
use super::traits::{Channel, IncomingMessage, OutgoingMessage};
use crate::memory::MemoryBackend;

/// Telegram message length limit
const TELEGRAM_MAX_LEN: usize = 4096;

#[derive(Clone, Default)]
pub struct TelegramIngressFilter {
    pub allowed_chat_ids: Vec<String>,
    pub allowed_sender_ids: Vec<String>,
}

impl TelegramIngressFilter {
    pub fn allows(&self, chat_id: &str, sender_id: &str) -> bool {
        if !self.allowed_chat_ids.is_empty() && !self.allowed_chat_ids.iter().any(|c| c == chat_id)
        {
            return false;
        }
        if !self.allowed_sender_ids.is_empty()
            && !sender_id.is_empty()
            && !self.allowed_sender_ids.iter().any(|s| s == sender_id)
        {
            return false;
        }
        true
    }
}

#[derive(Clone)]
pub struct TelegramChannel {
    bot_token: String,
    api_base: String,
    chat_id: i64,
    client: reqwest::Client,
    memory: Option<Arc<dyn MemoryBackend>>,
    ingress: TelegramIngressFilter,
}

#[derive(Debug, Serialize, Deserialize)]
struct TelegramResponse {
    ok: bool,
    result: Option<Vec<Update>>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct Update {
    update_id: i64,
    message: Option<Message>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct Voice {
    file_id: String,
    #[serde(default)]
    file_unique_id: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct Audio {
    file_id: String,
    #[serde(default)]
    file_unique_id: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct Location {
    latitude: f64,
    longitude: f64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct Sticker {
    file_id: String,
    #[serde(default)]
    file_unique_id: String,
    #[serde(default)]
    emoji: Option<String>,
    #[serde(default)]
    set_name: Option<String>,
    #[serde(default)]
    is_animated: bool,
    #[serde(default)]
    is_video: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct Message {
    message_id: i64,
    chat: Chat,
    text: Option<String>,
    from: Option<User>,
    voice: Option<Voice>,
    audio: Option<Audio>,
    location: Option<Location>,
    sticker: Option<Sticker>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct Chat {
    id: i64,
    #[serde(rename = "type", default)]
    chat_type: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct User {
    id: i64,
    first_name: Option<String>,
    username: Option<String>,
}

impl TelegramChannel {
    pub fn new(bot_token: String, chat_id: i64) -> Self {
        Self {
            bot_token,
            api_base: "https://api.telegram.org".to_string(),
            chat_id,
            client: crate::http::shared(),
            memory: None,
            ingress: TelegramIngressFilter::default(),
        }
    }

    /// Point the Telegram API at another origin (conformance tests).
    pub fn with_api_base(mut self, base: impl Into<String>) -> Self {
        self.api_base = base.into().trim_end_matches('/').to_string();
        self
    }

    pub fn with_memory(mut self, memory: Arc<dyn MemoryBackend>) -> Self {
        self.memory = Some(memory);
        self
    }

    pub fn with_ingress_filter(mut self, ingress: TelegramIngressFilter) -> Self {
        self.ingress = ingress;
        self
    }

    fn api_url(&self, method: &str) -> String {
        format!("{}/bot{}/{}", self.api_base, self.bot_token, method)
    }

    /// Transcribe voice/audio file using faster-whisper
    async fn transcribe_voice(&self, file_id: &str) -> anyhow::Result<String> {
        // 1. Get file info from Telegram API
        let file_info_url = format!(
            "{}/bot{}/getFile?file_id={}",
            self.api_base, self.bot_token, file_id
        );

        let resp = self.client.get(&file_info_url).send().await?;
        if !resp.status().is_success() {
            tracing::error!("Telegram getFile failed: HTTP {}", resp.status());
            return Ok(String::new());
        }

        let body: Value = resp.json().await?;
        if body["ok"].as_bool() != Some(true) {
            tracing::error!("Telegram getFile error: {:?}", body["error_description"]);
            return Ok(String::new());
        }

        let file_path = body["result"]["file_path"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("No file_path in response"))?;

        // 2. Download the file
        let download_url = format!("{}/file/bot{}/{}", self.api_base, self.bot_token, file_path);

        let file_resp = self.client.get(&download_url).send().await?;
        if !file_resp.status().is_success() {
            tracing::error!("Telegram download failed: HTTP {}", file_resp.status());
            return Ok(String::new());
        }
        let file_bytes = file_resp.bytes().await?;

        // 3. Create temp file
        let temp_dir = std::env::temp_dir();
        let temp_path = temp_dir.join(format!("apollo_voice_{}.ogg", uuid::Uuid::new_v4()));
        tokio::fs::write(&temp_path, file_bytes).await?;

        // 4. Call faster-whisper via Python (async)
        // Check for python3 first
        let py_check = tokio::process::Command::new("python3")
            .arg("--version")
            .output()
            .await;

        if py_check.is_err() {
            tracing::warn!("python3 not found, skipping transcription");
            let _ = tokio::fs::remove_file(&temp_path).await;
            return Ok(
                "[Voice message received but python3 is missing for transcription]".to_string(),
            );
        }

        let output = tokio::process::Command::new("python3")
            .arg("-c")
            .arg(format!(
                r#"
import sys
try:
    from faster_whisper import WhisperModel
    model = WhisperModel("tiny", device="cpu", compute_type="int8")
    segments, _ = model.transcribe(r"{}", language="en")
    text = " ".join([segment.text for segment in segments])
    print(text.strip())
except ImportError:
    print("ERROR: faster-whisper not installed")
    sys.exit(1)
except Exception as e:
    print(f"ERROR: {{e}}")
    sys.exit(1)
"#,
                temp_path.display()
            ))
            .output()
            .await?;

        // Clean up temp file
        let _ = tokio::fs::remove_file(&temp_path).await;

        if output.status.success() {
            let transcription = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if transcription.is_empty() {
                Ok("[Voice message: no speech detected]".to_string())
            } else {
                Ok(transcription)
            }
        } else {
            let err_msg = String::from_utf8_lossy(&output.stdout);
            if err_msg.contains("faster-whisper not installed") {
                Ok("[Voice message: faster-whisper not installed]".to_string())
            } else {
                tracing::error!("Transcription failed: {}", err_msg);
                Ok("[Voice message: transcription failed]".to_string())
            }
        }
    }

    /// Send a message and return its message_id (of the last chunk if split)
    pub async fn send_message(&self, text: &str) -> anyhow::Result<i64> {
        self.send_message_to(self.chat_id, text).await
    }

    async fn send_message_to(&self, chat_id: i64, text: &str) -> anyhow::Result<i64> {
        let formatted = format_outgoing_text(FormatTarget::Telegram, text);
        let chunks = chunk_outgoing_text(FormatTarget::Telegram, &formatted, TELEGRAM_MAX_LEN);

        let mut last_msg_id = 0;

        for (i, chunk) in chunks.iter().enumerate() {
            // Try with Markdown first
            let resp = self
                .client
                .post(self.api_url("sendMessage"))
                .json(&serde_json::json!({
                    "chat_id": chat_id,
                    "text": chunk,
                    "parse_mode": "Markdown",
                }))
                .send()
                .await?;

            let body: Value = resp.json().await?;

            if body["ok"].as_bool() == Some(true) {
                last_msg_id = body["result"]["message_id"].as_i64().unwrap_or(0);
            } else {
                // Markdown failed, retry without parse_mode
                let resp = self
                    .client
                    .post(self.api_url("sendMessage"))
                    .json(&serde_json::json!({
                        "chat_id": chat_id,
                        "text": chunk,
                    }))
                    .send()
                    .await?;
                let body: Value = resp.json().await?;
                last_msg_id = body["result"]["message_id"].as_i64().unwrap_or(0);
            }

            // Add delay between chunks to avoid rate limiting
            if i < chunks.len() - 1 {
                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
            }
        }

        Ok(last_msg_id)
    }

    /// Edit an existing message
    pub async fn edit_message(&self, message_id: i64, text: &str) -> anyhow::Result<()> {
        let formatted = format_outgoing_text(FormatTarget::Telegram, text);
        let edit_text = formatted.chars().take(TELEGRAM_MAX_LEN).collect::<String>();
        let _ = self
            .client
            .post(self.api_url("editMessageText"))
            .json(&serde_json::json!({
                "chat_id": self.chat_id,
                "message_id": message_id,
                "text": edit_text,
            }))
            .send()
            .await?;
        Ok(())
    }

    async fn sticker_text(&self, sticker: &Sticker) -> anyhow::Result<String> {
        if let Some(memory) = &self.memory {
            if !sticker.file_unique_id.is_empty() {
                if let Some(cached) = memory.get_sticker_cache(&sticker.file_unique_id).await? {
                    return Ok(cached);
                }
            }
        }

        let mut parts = vec!["Sticker received".to_string()];
        if let Some(emoji) = &sticker.emoji {
            if !emoji.is_empty() {
                parts.push(format!("emoji {}", emoji));
            }
        }
        if let Some(set_name) = &sticker.set_name {
            if !set_name.is_empty() {
                parts.push(format!("set {}", set_name));
            }
        }
        if sticker.is_animated {
            parts.push("animated".to_string());
        }
        if sticker.is_video {
            parts.push("video".to_string());
        }

        let description = format!("🎨 {}", parts.join(" • "));

        if let Some(memory) = &self.memory {
            if !sticker.file_unique_id.is_empty() {
                let _ = memory
                    .store_sticker_cache(&sticker.file_unique_id, &sticker.file_id, &description)
                    .await;
            }
        }

        Ok(description)
    }

    /// Delete a message
    pub async fn delete_message(&self, message_id: i64) -> anyhow::Result<()> {
        let _ = self
            .client
            .post(self.api_url("deleteMessage"))
            .json(&serde_json::json!({
                "chat_id": self.chat_id,
                "message_id": message_id,
            }))
            .send()
            .await?;
        Ok(())
    }

    /// Send typing indicator
    pub async fn send_typing(&self, _chat_id: &str) -> anyhow::Result<()> {
        let _ = self
            .client
            .post(self.api_url("sendChatAction"))
            .json(&serde_json::json!({
                "chat_id": self.chat_id,
                "action": "typing",
            }))
            .send()
            .await?;
        Ok(())
    }

    /// Get updates (long polling)
    async fn get_updates(&self, offset: i64) -> anyhow::Result<Vec<Update>> {
        let url = format!(
            "{}?offset={}&limit=100&timeout=30",
            self.api_url("getUpdates"),
            offset
        );

        match self.client.get(&url).send().await {
            Ok(resp) => {
                if let Ok(data) = resp.json::<TelegramResponse>().await {
                    Ok(data.result.unwrap_or_default())
                } else {
                    Ok(Vec::new())
                }
            }
            Err(_) => Ok(Vec::new()),
        }
    }
}

#[async_trait]
impl Channel for TelegramChannel {
    fn name(&self) -> &str {
        "telegram"
    }

    async fn start(&mut self) -> anyhow::Result<mpsc::Receiver<IncomingMessage>> {
        let (tx, rx) = mpsc::channel(100);
        let bot_token = self.bot_token.clone();
        let api_base = self.api_base.clone();
        let chat_id = self.chat_id;
        let client = self.client.clone();
        let memory = self.memory.clone();
        let ingress = self.ingress.clone();

        tokio::spawn(async move {
            let ch = TelegramChannel {
                bot_token,
                api_base,
                chat_id,
                client,
                memory,
                ingress,
            };
            let mut offset = 0;
            loop {
                if let Ok(updates) = ch.get_updates(offset).await {
                    for update in updates {
                        if let Some(msg) = &update.message {
                            let from = msg.from.as_ref();
                            let is_group = msg
                                .chat
                                .chat_type
                                .as_deref()
                                .map(|t| t == "group" || t == "supergroup")
                                .unwrap_or(false);

                            // Determine message content based on message type
                            let text = if let Some(loc) = &msg.location {
                                // Location: format as coordinates with Google Maps link
                                format!(
                                    "📍 Location: {}, {} (https://maps.google.com/?q={},{})",
                                    loc.latitude, loc.longitude, loc.latitude, loc.longitude
                                )
                            } else if let Some(sticker) = &msg.sticker {
                                ch.sticker_text(sticker)
                                    .await
                                    .unwrap_or_else(|_| "🎨 Sticker received".to_string())
                            } else if let Some(voice) = &msg.voice {
                                // Voice: transcribe with faster-whisper
                                ch.transcribe_voice(&voice.file_id)
                                    .await
                                    .unwrap_or_default()
                            } else if let Some(audio) = &msg.audio {
                                // Audio: transcribe with faster-whisper
                                ch.transcribe_voice(&audio.file_id)
                                    .await
                                    .unwrap_or_default()
                            } else if let Some(text_content) = &msg.text {
                                // Regular text
                                text_content.clone()
                            } else {
                                // Unknown message type, skip
                                continue;
                            };

                            if text.is_empty() {
                                continue;
                            }

                            let chat_id_str = msg.chat.id.to_string();
                            let sender_id_str = from.map(|u| u.id.to_string()).unwrap_or_default();
                            if !ch.ingress.allows(&chat_id_str, &sender_id_str) {
                                tracing::warn!(
                                    "Telegram ingress denied chat={} sender={}",
                                    chat_id_str,
                                    sender_id_str
                                );
                                continue;
                            }

                            let incoming = IncomingMessage {
                                id: msg.message_id.to_string(),
                                sender_id: from.map(|u| u.id.to_string()).unwrap_or_default(),
                                sender_name: from.and_then(|u| {
                                    u.username.clone().or_else(|| u.first_name.clone())
                                }),
                                chat_id: msg.chat.id.to_string(),
                                text,
                                is_group,
                                reply_to: None,
                                timestamp: chrono::Utc::now(),
                            };
                            let _ = tx.send(incoming).await;
                        }
                        offset = update.update_id + 1;
                    }
                }
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            }
        });

        Ok(rx)
    }

    async fn send(&self, message: OutgoingMessage) -> anyhow::Result<Option<String>> {
        let chat_id = message.chat_id.parse::<i64>()?;
        let msg_id = self.send_message_to(chat_id, &message.text).await?;
        if msg_id > 0 {
            Ok(Some(msg_id.to_string()))
        } else {
            Ok(None)
        }
    }

    async fn send_typing(&self, chat_id: &str) -> anyhow::Result<()> {
        let chat_id = chat_id.parse::<i64>()?;
        let _ = self
            .client
            .post(self.api_url("sendChatAction"))
            .json(&serde_json::json!({
                "chat_id": chat_id,
                "action": "typing",
            }))
            .send()
            .await?;
        Ok(())
    }

    async fn edit(&self, _chat_id: &str, message_id: &str, new_text: &str) -> anyhow::Result<()> {
        let message_id = message_id.parse::<i64>().unwrap_or(0);
        if message_id > 0 {
            self.edit_message(message_id, new_text).await?;
        }
        Ok(())
    }

    async fn stop(&mut self) -> anyhow::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::surreal::SurrealMemory;

    #[tokio::test]
    async fn sticker_cache_is_used_when_available() {
        let dir = tempfile::tempdir().unwrap();
        let memory: Arc<dyn MemoryBackend> =
            Arc::new(SurrealMemory::new(dir.path()).await.unwrap());
        memory
            .store_sticker_cache("uniq-1", "file-1", "🎨 cached sticker")
            .await
            .unwrap();

        let channel = TelegramChannel::new("token".to_string(), 1).with_memory(memory);
        let sticker = Sticker {
            file_id: "file-1".to_string(),
            file_unique_id: "uniq-1".to_string(),
            emoji: Some("🙂".to_string()),
            set_name: Some("test_set".to_string()),
            is_animated: false,
            is_video: false,
        };

        let text = channel.sticker_text(&sticker).await.unwrap();
        assert_eq!(text, "🎨 cached sticker");
    }
}
