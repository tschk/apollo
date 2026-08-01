//! Google Chat channel — Google Workspace Chat API

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{mpsc, Mutex};

use super::traits::*;

/// The fields apollo needs out of a Google service-account JSON key.
#[derive(Debug, Deserialize)]
struct ServiceAccount {
    client_email: String,
    private_key: String,
    #[serde(default = "default_token_uri")]
    token_uri: String,
}

fn default_token_uri() -> String {
    "https://oauth2.googleapis.com/token".to_string()
}

/// The signed JWT assertion Google exchanges for an access token.
#[derive(Debug, Serialize)]
struct Assertion<'a> {
    iss: &'a str,
    scope: &'a str,
    aud: &'a str,
    iat: i64,
    exp: i64,
}

struct CachedToken {
    value: String,
    expires_at: chrono::DateTime<chrono::Utc>,
}

pub struct GoogleChatChannel {
    service_account_key: String,
    space_id: Option<String>,
    api_base: String,
    bind_addr: std::net::SocketAddr,
    token: Mutex<Option<CachedToken>>,
}

impl GoogleChatChannel {
    pub fn new(service_account_key: impl Into<String>) -> Self {
        Self {
            service_account_key: service_account_key.into(),
            space_id: None,
            api_base: "https://chat.googleapis.com/v1".to_string(),
            bind_addr: ([0, 0, 0, 0], 3001).into(),
            token: Mutex::new(None),
        }
    }

    /// A valid OAuth2 access token, minted from the service-account key and
    /// cached until shortly before it expires.
    async fn access_token(&self) -> anyhow::Result<String> {
        let mut cached = self.token.lock().await;
        if let Some(token) = cached.as_ref() {
            if token.expires_at > chrono::Utc::now() {
                return Ok(token.value.clone());
            }
        }

        let account: ServiceAccount =
            serde_json::from_str(&self.service_account_key).map_err(|e| {
                anyhow::anyhow!("Google Chat service-account key is not valid JSON: {e}")
            })?;

        let now = chrono::Utc::now().timestamp();
        let assertion = Assertion {
            iss: &account.client_email,
            scope: "https://www.googleapis.com/auth/chat.bot",
            aud: &account.token_uri,
            iat: now,
            exp: now + 3600,
        };
        let key = jsonwebtoken::EncodingKey::from_rsa_pem(account.private_key.as_bytes()).map_err(
            |e| anyhow::anyhow!("Google Chat service-account private key is unusable: {e}"),
        )?;
        let jwt = jsonwebtoken::encode(
            &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256),
            &assertion,
            &key,
        )?;

        let resp = crate::http::shared()
            .post(&account.token_uri)
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
                ("assertion", &jwt),
            ])
            .send()
            .await?;
        if !resp.status().is_success() {
            anyhow::bail!("Google Chat token exchange failed: {}", resp.status());
        }

        let body = resp.json::<Value>().await?;
        let value = body["access_token"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Google Chat token response carried no access_token"))?
            .to_string();
        let ttl = body["expires_in"].as_i64().unwrap_or(3600);

        *cached = Some(CachedToken {
            value: value.clone(),
            // Refresh a minute early so a token never expires mid-request.
            expires_at: chrono::Utc::now() + chrono::Duration::seconds((ttl - 60).max(0)),
        });
        Ok(value)
    }

    /// Point the Chat API at another origin (conformance tests).
    pub fn with_api_base(mut self, base: impl Into<String>) -> Self {
        self.api_base = base.into().trim_end_matches('/').to_string();
        self
    }

    /// Bind the webhook receiver somewhere other than 0.0.0.0:3001.
    pub fn with_bind_addr(mut self, addr: std::net::SocketAddr) -> Self {
        self.bind_addr = addr;
        self
    }

    pub fn with_space(mut self, space_id: impl Into<String>) -> Self {
        self.space_id = Some(space_id.into());
        self
    }
}

fn parse_webhook_message(body: &Value) -> Vec<IncomingMessage> {
    if body["type"].as_str() == Some("MESSAGE") {
        let msg = &body["message"];
        let text = msg["text"].as_str().unwrap_or("").to_string();
        if !text.is_empty() {
            return vec![IncomingMessage {
                id: msg["name"].as_str().unwrap_or("").to_string(),
                sender_id: body["user"]["name"].as_str().unwrap_or("").to_string(),
                sender_name: body["user"]["displayName"].as_str().map(|s| s.to_string()),
                chat_id: body["space"]["name"].as_str().unwrap_or("").to_string(),
                text,
                is_group: body["space"]["type"].as_str() == Some("ROOM"),
                reply_to: None,
                timestamp: chrono::Utc::now(),
            }];
        }
    }
    Vec::new()
}

#[async_trait]
impl Channel for GoogleChatChannel {
    fn name(&self) -> &str {
        "googlechat"
    }

    async fn start(&mut self) -> anyhow::Result<mpsc::Receiver<IncomingMessage>> {
        let (tx, rx) = mpsc::channel(32);
        // Google Chat pushes over a webhook rather than offering a poll.
        super::webhook::serve_json(
            self.bind_addr,
            "/googlechat/webhook",
            parse_webhook_message,
            tx,
        )
        .await?;
        Ok(rx)
    }

    async fn send(&self, message: OutgoingMessage) -> anyhow::Result<Option<String>> {
        let client = crate::http::shared();

        let body = serde_json::json!({
            "text": &message.text,
        });

        client
            .post(format!("{}/{}/messages", self.api_base, message.chat_id))
            .header(
                "Authorization",
                format!("Bearer {}", self.access_token().await?),
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
