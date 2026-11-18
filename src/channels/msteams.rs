//! Microsoft Teams channel — Bot Framework integration

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::mpsc;

use super::traits::*;

pub struct TeamsChannel {
    app_id: String,
    app_password: String,
    token_url: String,
    service_base: String,
    jwks_url: String,
    bind_addr: std::net::SocketAddr,
}

/// Bot Framework signing keys and the issuer of channel-to-bot tokens.
const BOT_FRAMEWORK_JWKS: &str = "https://login.botframework.com/v1/.well-known/keys";
const BOT_FRAMEWORK_ISSUER: &str = "https://api.botframework.com";

impl TeamsChannel {
    pub fn new(app_id: impl Into<String>, app_password: impl Into<String>) -> Self {
        Self {
            app_id: app_id.into(),
            app_password: app_password.into(),
            token_url: "https://login.microsoftonline.com/botframework.com/oauth2/v2.0/token"
                .to_string(),
            service_base: "https://smba.trafficmanager.net/teams/v3".to_string(),
            jwks_url: BOT_FRAMEWORK_JWKS.to_string(),
            bind_addr: ([0, 0, 0, 0], 3978).into(),
        }
    }

    /// Point the Bot Framework token + service endpoints at another origin
    /// (conformance tests).
    pub fn with_api_base(mut self, base: impl Into<String>) -> Self {
        let base = base.into().trim_end_matches('/').to_string();
        self.token_url = format!("{base}/oauth2/v2.0/token");
        self.service_base = format!("{base}/teams/v3");
        self
    }

    /// Fetch Bot Framework signing keys from another URL (conformance tests).
    pub fn with_jwks_url(mut self, url: impl Into<String>) -> Self {
        self.jwks_url = url.into();
        self
    }

    /// Bind the Bot Framework receiver somewhere other than 0.0.0.0:3978.
    pub fn with_bind_addr(mut self, addr: std::net::SocketAddr) -> Self {
        self.bind_addr = addr;
        self
    }
}

#[async_trait]
impl Channel for TeamsChannel {
    fn name(&self) -> &str {
        "msteams"
    }

    async fn start(&mut self) -> anyhow::Result<mpsc::Receiver<IncomingMessage>> {
        let (tx, rx) = mpsc::channel(32);
        // The Bot Framework signs every activity it delivers with a JWT whose
        // audience is this bot's app id.
        if self.app_id.trim().is_empty() {
            anyhow::bail!("msteams: app_id is required to verify webhook requests");
        }
        let verifier = std::sync::Arc::new(super::webhook::JwtVerifier::new(
            self.jwks_url.clone(),
            BOT_FRAMEWORK_ISSUER,
            self.app_id.clone(),
        ));
        super::webhook::serve_json(
            self.bind_addr,
            "/api/messages",
            parse_activity,
            verifier,
            tx,
        )
        .await?;
        Ok(rx)
    }

    async fn send(&self, message: OutgoingMessage) -> anyhow::Result<Option<String>> {
        let client = crate::http::shared();

        // Get Bot Framework token
        let token_resp = client
            .post(&self.token_url)
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
                "{}/conversations/{}/activities",
                self.service_base, message.chat_id
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

/// Bot Framework activity → apollo message. Only `message` activities carry
/// user text; the rest (typing, conversationUpdate) are ignored.
fn parse_activity(body: &Value) -> Vec<IncomingMessage> {
    if body["type"].as_str() != Some("message") {
        return Vec::new();
    }
    let text = body["text"].as_str().unwrap_or("").to_string();
    if text.is_empty() {
        return Vec::new();
    }
    vec![IncomingMessage {
        id: body["id"].as_str().unwrap_or("").to_string(),
        sender_id: body["from"]["id"].as_str().unwrap_or("").to_string(),
        sender_name: body["from"]["name"].as_str().map(|s| s.to_string()),
        chat_id: body["conversation"]["id"]
            .as_str()
            .unwrap_or("")
            .to_string(),
        text,
        is_group: body["conversation"]["conversationType"].as_str() == Some("groupChat"),
        reply_to: None,
        timestamp: chrono::Utc::now(),
    }]
}
