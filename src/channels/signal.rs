//! Signal channel — via signal-cli REST API

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::mpsc;

use super::traits::*;

pub struct SignalChannel {
    api_url: String,
    phone_number: String,
}

impl SignalChannel {
    pub fn new(api_url: impl Into<String>, phone_number: impl Into<String>) -> Self {
        Self {
            api_url: api_url.into(),
            phone_number: phone_number.into(),
        }
    }

    /// Default: signal-cli REST API on localhost
    pub fn local(phone_number: impl Into<String>) -> Self {
        Self::new("http://localhost:8080", phone_number)
    }
}

#[async_trait]
impl Channel for SignalChannel {
    fn name(&self) -> &str {
        "signal"
    }

    async fn start(&mut self) -> anyhow::Result<mpsc::Receiver<IncomingMessage>> {
        let (tx, rx) = mpsc::channel(32);
        let api_url = self.api_url.clone();
        let phone = self.phone_number.clone();

        tokio::spawn(async move {
            let client = crate::http::shared();

            loop {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;

                let resp = client
                    .get(format!("{}/v1/receive/{}", api_url, phone))
                    .send()
                    .await;

                if let Ok(resp) = resp {
                    if let Ok(messages) = resp.json::<Vec<Value>>().await {
                        for msg in messages {
                            let envelope = &msg["envelope"];
                            let data = &envelope["dataMessage"];
                            let text = data["message"].as_str().unwrap_or("").to_string();
                            if text.is_empty() {
                                continue;
                            }

                            let incoming = IncomingMessage {
                                id: envelope["timestamp"].to_string(),
                                sender_id: envelope["source"].as_str().unwrap_or("").to_string(),
                                sender_name: envelope["sourceName"].as_str().map(|s| s.to_string()),
                                chat_id: data["groupInfo"]["groupId"]
                                    .as_str()
                                    .unwrap_or(envelope["source"].as_str().unwrap_or(""))
                                    .to_string(),
                                text,
                                is_group: data["groupInfo"].is_object(),
                                reply_to: None,
                                timestamp: chrono::Utc::now(),
                            };

                            if tx.send(incoming).await.is_err() {
                                return;
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

        let body = serde_json::json!({
            "message": &message.text,
            "number": &self.phone_number,
            "recipients": [&message.chat_id],
        });

        client
            .post(format!("{}/v2/send", self.api_url))
            .json(&body)
            .send()
            .await?;

        Ok(None)
    }

    async fn stop(&mut self) -> anyhow::Result<()> {
        Ok(())
    }
}
