//! WhatsApp channel — HTTP API integration (WhatsApp Business Cloud API)

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::mpsc;

use super::formatting::{format_outgoing_text, FormatTarget};
use super::traits::*;

pub struct WhatsAppChannel {
    access_token: String,
    phone_number_id: String,
    verify_token: String,
    api_base: String,
    bind_addr: std::net::SocketAddr,
}

impl WhatsAppChannel {
    pub fn new(
        access_token: impl Into<String>,
        phone_number_id: impl Into<String>,
        verify_token: impl Into<String>,
    ) -> Self {
        Self {
            access_token: access_token.into(),
            phone_number_id: phone_number_id.into(),
            verify_token: verify_token.into(),
            api_base: "https://graph.facebook.com/v18.0".to_string(),
            bind_addr: ([0, 0, 0, 0], 3000).into(),
        }
    }

    /// Point the Cloud API at another origin (conformance tests).
    pub fn with_api_base(mut self, base: impl Into<String>) -> Self {
        self.api_base = base.into().trim_end_matches('/').to_string();
        self
    }

    /// Bind the webhook receiver somewhere other than 0.0.0.0:3000.
    pub fn with_bind_addr(mut self, addr: std::net::SocketAddr) -> Self {
        self.bind_addr = addr;
        self
    }

    /// Load from env vars
    pub fn from_env() -> anyhow::Result<Self> {
        Ok(Self {
            access_token: std::env::var("WHATSAPP_ACCESS_TOKEN")
                .map_err(|_| anyhow::anyhow!("WHATSAPP_ACCESS_TOKEN not set"))?,
            phone_number_id: std::env::var("WHATSAPP_PHONE_NUMBER_ID")
                .map_err(|_| anyhow::anyhow!("WHATSAPP_PHONE_NUMBER_ID not set"))?,
            verify_token: std::env::var("WHATSAPP_VERIFY_TOKEN")
                .unwrap_or_else(|_| "aclaw-verify".to_string()),
            api_base: "https://graph.facebook.com/v18.0".to_string(),
            bind_addr: ([0, 0, 0, 0], 3000).into(),
        })
    }
}

#[async_trait]
impl Channel for WhatsAppChannel {
    fn name(&self) -> &str {
        "whatsapp"
    }

    async fn start(&mut self) -> anyhow::Result<mpsc::Receiver<IncomingMessage>> {
        let (tx, rx) = mpsc::channel(32);

        // WhatsApp Cloud API uses webhooks
        // For now, start a simple HTTP server to receive webhooks
        let verify_token = self.verify_token.clone();
        let _access_token = self.access_token.clone();
        let bind_addr = self.bind_addr;

        tokio::spawn(async move {
            use axum::{
                extract::Query,
                routing::{get, post},
                Json, Router,
            };

            let tx_clone = tx.clone();
            let verify = verify_token.clone();

            let app = Router::new()
                .route(
                    "/webhook",
                    get(
                        move |Query(params): Query<std::collections::HashMap<String, String>>| {
                            let v = verify.clone();
                            async move {
                                // Webhook verification
                                if params.get("hub.verify_token").map(|t| t.as_str()) == Some(&v) {
                                    params.get("hub.challenge").cloned().unwrap_or_default()
                                } else {
                                    "Forbidden".to_string()
                                }
                            }
                        },
                    ),
                )
                .route(
                    "/webhook",
                    post(move |Json(body): Json<Value>| {
                        let tx = tx_clone.clone();
                        async move {
                            // Parse incoming WhatsApp message
                            if let Some(entries) = body["entry"].as_array() {
                                for entry in entries {
                                    if let Some(changes) = entry["changes"].as_array() {
                                        for change in changes {
                                            if let Some(messages) =
                                                change["value"]["messages"].as_array()
                                            {
                                                for msg in messages {
                                                    let text = msg["text"]["body"]
                                                        .as_str()
                                                        .unwrap_or("")
                                                        .to_string();
                                                    if text.is_empty() {
                                                        continue;
                                                    }

                                                    let incoming = IncomingMessage {
                                                        id: msg["id"]
                                                            .as_str()
                                                            .unwrap_or("")
                                                            .to_string(),
                                                        sender_id: msg["from"]
                                                            .as_str()
                                                            .unwrap_or("")
                                                            .to_string(),
                                                        sender_name: None,
                                                        chat_id: msg["from"]
                                                            .as_str()
                                                            .unwrap_or("")
                                                            .to_string(),
                                                        text,
                                                        is_group: false,
                                                        reply_to: None,
                                                        timestamp: chrono::Utc::now(),
                                                    };

                                                    let _ = tx.send(incoming).await;
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            "OK"
                        }
                    }),
                );

            let listener = tokio::net::TcpListener::bind(bind_addr).await.unwrap();
            axum::serve(listener, app).await.unwrap();
        });

        Ok(rx)
    }

    async fn send(&self, message: OutgoingMessage) -> anyhow::Result<Option<String>> {
        let client = crate::http::shared();
        let formatted = format_outgoing_text(FormatTarget::WhatsApp, &message.text);

        let body = serde_json::json!({
            "messaging_product": "whatsapp",
            "to": &message.chat_id,
            "type": "text",
            "text": {
                "body": formatted
            }
        });

        let resp = client
            .post(format!(
                "{}/{}/messages",
                self.api_base, self.phone_number_id
            ))
            .header("Authorization", format!("Bearer {}", self.access_token))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("WhatsApp send failed: {}", text);
        }

        Ok(None)
    }

    async fn stop(&mut self) -> anyhow::Result<()> {
        Ok(())
    }
}
