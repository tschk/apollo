//! Microsoft Teams channel — Bot Framework integration

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::mpsc;

use super::traits::*;

pub struct TeamsChannel {
    app_id: String,
    app_password: String,
}

impl TeamsChannel {
    pub fn new(app_id: impl Into<String>, app_password: impl Into<String>) -> Self {
        Self {
            app_id: app_id.into(),
            app_password: app_password.into(),
        }
    }
}

#[async_trait]
impl Channel for TeamsChannel {
    fn name(&self) -> &str {
        "msteams"
    }

    async fn start(&mut self) -> anyhow::Result<mpsc::Receiver<IncomingMessage>> {
        let (tx, rx) = mpsc::channel(32);
        let _app_id = self.app_id.clone();

        tokio::spawn(async move {
            use axum::{routing::post, Json, Router};

            let app = Router::new().route(
                "/api/messages",
                post(move |Json(body): Json<Value>| {
                    let tx = tx.clone();
                    async move {
                        if body["type"].as_str() == Some("message") {
                            let text = body["text"].as_str().unwrap_or("").to_string();
                            if !text.is_empty() {
                                let incoming = IncomingMessage {
                                    id: body["id"].as_str().unwrap_or("").to_string(),
                                    sender_id: body["from"]["id"]
                                        .as_str()
                                        .unwrap_or("")
                                        .to_string(),
                                    sender_name: body["from"]["name"]
                                        .as_str()
                                        .map(|s| s.to_string()),
                                    chat_id: body["conversation"]["id"]
                                        .as_str()
                                        .unwrap_or("")
                                        .to_string(),
                                    text,
                                    is_group: body["conversation"]["conversationType"].as_str()
                                        == Some("groupChat"),
                                    reply_to: None,
                                    timestamp: chrono::Utc::now(),
                                };
                                let _ = tx.send(incoming).await;
                            }
                        }
                        axum::http::StatusCode::OK
                    }
                }),
            );

            let listener = tokio::net::TcpListener::bind("0.0.0.0:3978").await.unwrap();
            axum::serve(listener, app).await.unwrap();
        });

        Ok(rx)
    }

    async fn send(&self, message: OutgoingMessage) -> anyhow::Result<Option<String>> {
        let client = reqwest::Client::new();

        // Get Bot Framework token
        let token_resp = client
            .post("https://login.microsoftonline.com/botframework.com/oauth2/v2.0/token")
            .form(&[
                ("grant_type", "client_credentials"),
                ("client_id", &self.app_id),
                ("client_secret", &self.app_password),
                ("scope", "https://api.botframework.com/.default"),
            ])
            .send()
            .await?;

        let token_data: Value = token_resp.json().await?;
        let token = token_data["access_token"].as_str().unwrap_or("");

        let body = serde_json::json!({
            "type": "message",
            "text": &message.text,
        });

        client
            .post(format!(
                "https://smba.trafficmanager.net/teams/v3/conversations/{}/activities",
                message.chat_id
            ))
            .header("Authorization", format!("Bearer {}", token))
            .json(&body)
            .send()
            .await?;

        Ok(None)
    }

    async fn stop(&mut self) -> anyhow::Result<()> {
        Ok(())
    }
}
