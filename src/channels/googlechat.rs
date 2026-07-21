//! Google Chat channel — Google Workspace Chat API

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::mpsc;

use super::traits::*;

pub struct GoogleChatChannel {
    service_account_key: String,
    space_id: Option<String>,
}

impl GoogleChatChannel {
    pub fn new(service_account_key: impl Into<String>) -> Self {
        Self {
            service_account_key: service_account_key.into(),
            space_id: None,
        }
    }

    pub fn with_space(mut self, space_id: impl Into<String>) -> Self {
        self.space_id = Some(space_id.into());
        self
    }
}

fn parse_webhook_message(body: &Value) -> Option<IncomingMessage> {
    if body["type"].as_str() == Some("MESSAGE") {
        let msg = &body["message"];
        let text = msg["text"].as_str().unwrap_or("").to_string();
        if !text.is_empty() {
            return Some(IncomingMessage {
                id: msg["name"].as_str().unwrap_or("").to_string(),
                sender_id: body["user"]["name"].as_str().unwrap_or("").to_string(),
                sender_name: body["user"]["displayName"].as_str().map(|s| s.to_string()),
                chat_id: body["space"]["name"].as_str().unwrap_or("").to_string(),
                text,
                is_group: body["space"]["type"].as_str() == Some("ROOM"),
                reply_to: None,
                timestamp: chrono::Utc::now(),
            });
        }
    }
    None
}

#[async_trait]
impl Channel for GoogleChatChannel {
    fn name(&self) -> &str {
        "googlechat"
    }

    async fn start(&mut self) -> anyhow::Result<mpsc::Receiver<IncomingMessage>> {
        let (tx, rx) = mpsc::channel(32);
        // Google Chat uses webhooks/pub-sub — webhook receiver needed
        let _key = self.service_account_key.clone();

        tokio::spawn(async move {
            use axum::{routing::post, Json, Router};

            let app = Router::new().route(
                "/googlechat/webhook",
                post(move |Json(body): Json<Value>| {
                    let tx = tx.clone();
                    async move {
                        if let Some(incoming) = parse_webhook_message(&body) {
                            let _ = tx.send(incoming).await;
                        }
                        "{}"
                    }
                }),
            );

            let listener = tokio::net::TcpListener::bind("0.0.0.0:3001").await.unwrap();
            axum::serve(listener, app).await.unwrap();
        });

        Ok(rx)
    }

    async fn send(&self, message: OutgoingMessage) -> anyhow::Result<Option<String>> {
        let client = reqwest::Client::new();

        let body = serde_json::json!({
            "text": &message.text,
        });

        client
            .post(format!(
                "https://chat.googleapis.com/v1/{}/messages",
                message.chat_id
            ))
            .header(
                "Authorization",
                format!("Bearer {}", self.service_account_key),
            )
            .json(&body)
            .send()
            .await?;

        Ok(None)
    }

    async fn stop(&mut self) -> anyhow::Result<()> {
        Ok(())
    }
}
