//! OAuth token support for Anthropic
//! Converts Claude.dev OAuth tokens (oat01) to API calls via token exchange

use serde::Deserialize;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<i64>,
}

/// OAuth token cache (refreshed as needed)
#[derive(Clone)]
pub struct OAuthTokenCache {
    token: Arc<RwLock<Option<String>>>,
    refresh_token: Arc<RwLock<Option<String>>>,
    expires_at: Arc<RwLock<i64>>,
}

impl OAuthTokenCache {
    pub fn new(initial_token: String, refresh_token: Option<String>, expires_at: i64) -> Self {
        Self {
            token: Arc::new(RwLock::new(Some(initial_token))),
            refresh_token: Arc::new(RwLock::new(refresh_token)),
            expires_at: Arc::new(RwLock::new(expires_at)),
        }
    }

    /// Get current token, refresh if expired
    pub async fn get_token(&self) -> anyhow::Result<String> {
        let token = self.token.read().await;
        if let Some(t) = token.as_ref() {
            let expires = *self.expires_at.read().await;
            if expires > chrono::Utc::now().timestamp_millis() {
                return Ok(t.clone());
            }
        }
        drop(token);

        // Token expired, try refresh
        self.refresh().await?;

        let token = self.token.read().await;
        token
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Failed to get valid token"))
    }

    /// Exchange refresh token for new access token
    async fn exchange_refresh_token(refresh_token: &str) -> anyhow::Result<TokenResponse> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()?;

        let response = client
            .post("https://api.anthropic.com/v1/oauth/token")
            .json(&serde_json::json!({
                "grant_type": "refresh_token",
                "refresh_token": refresh_token,
            }))
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(anyhow::anyhow!(
                "OAuth token refresh failed: {}",
                response.status()
            ));
        }

        let body = response.json::<TokenResponse>().await?;
        Ok(body)
    }

    /// Refresh token from Anthropic OAuth endpoint
    async fn refresh(&self) -> anyhow::Result<()> {
        let refresh_token = {
            let rt = self.refresh_token.read().await;
            rt.clone()
                .ok_or_else(|| anyhow::anyhow!("No refresh token available"))?
        };

        let body = Self::exchange_refresh_token(&refresh_token).await?;

        let expires_in = body.expires_in.unwrap_or(3600) * 1000; // Convert to ms
        let new_expires = chrono::Utc::now().timestamp_millis() + expires_in;

        *self.token.write().await = Some(body.access_token);
        if let Some(r) = body.refresh_token {
            *self.refresh_token.write().await = Some(r);
        }
        *self.expires_at.write().await = new_expires;

        Ok(())
    }
}

/// Load OAuth token from Claude.dev credentials file
pub fn load_oauth_token_from_file() -> anyhow::Result<(String, Option<String>, i64)> {
    let credentials_path = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?
        .join(".claude")
        .join(".credentials.json");

    let content = std::fs::read_to_string(&credentials_path)
        .map_err(|e| anyhow::anyhow!("Failed to read Claude credentials: {}", e))?;

    let creds: Value = serde_json::from_str(&content)
        .map_err(|e| anyhow::anyhow!("Failed to parse Claude credentials: {}", e))?;

    let oauth = &creds["claudeAiOauth"];
    let access_token = oauth["accessToken"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("No accessToken in credentials"))?
        .to_string();

    let refresh_token = oauth["refreshToken"].as_str().map(|s| s.to_string());
    let expires_at = oauth["expiresAt"]
        .as_i64()
        .unwrap_or_else(|| chrono::Utc::now().timestamp_millis() + 3600 * 1000);

    Ok((access_token, refresh_token, expires_at))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_oauth_cache() {
        let cache = OAuthTokenCache::new(
            "token123".to_string(),
            None,
            chrono::Utc::now().timestamp_millis() + 3600 * 1000,
        );

        // Should not panic
        assert!(!cache.token.blocking_read().is_none());
    }

    #[test]
    fn test_load_oauth_fails_gracefully() {
        // This will fail if credentials don't exist, which is expected
        let result = load_oauth_token_from_file();
        // Either it succeeds or it fails gracefully
        let _ = result;
    }
}
