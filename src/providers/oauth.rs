//! OAuth token support for Anthropic
//! Converts Claude.dev OAuth tokens (oat01) to API calls via token exchange

use serde::Deserialize;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;

/// Public OAuth client id for the Claude CLI token endpoint.
const CLAUDE_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";

/// Anthropic OAuth token endpoint (console host, not the API host).
const CLAUDE_TOKEN_URL: &str = "https://console.anthropic.com/v1/oauth/token";

/// Refresh this long before actual expiry so an in-flight request does not
/// race the deadline.
const EXPIRY_SKEW_MS: i64 = 60_000;

#[derive(Deserialize)]
struct RefreshResponse {
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
    /// Credentials file to write refreshed tokens back to, when the cache was
    /// loaded from one. Without this a long-running process refreshes in
    /// memory only and every restart starts from an expired token.
    credentials_path: Arc<RwLock<Option<PathBuf>>>,
}

impl OAuthTokenCache {
    pub fn new(initial_token: String, refresh_token: Option<String>, expires_at: i64) -> Self {
        Self {
            token: Arc::new(RwLock::new(Some(initial_token))),
            refresh_token: Arc::new(RwLock::new(refresh_token)),
            expires_at: Arc::new(RwLock::new(expires_at)),
            credentials_path: Arc::new(RwLock::new(None)),
        }
    }

    /// Build a cache from the Claude credentials file. Refreshed tokens are
    /// written back to that same file.
    pub fn from_credentials_file() -> anyhow::Result<Self> {
        let path = claude_credentials_path()?;
        let (token, refresh_token, expires_at) = load_oauth_token_from_path(&path)?;
        Ok(Self {
            token: Arc::new(RwLock::new(Some(token))),
            refresh_token: Arc::new(RwLock::new(refresh_token)),
            expires_at: Arc::new(RwLock::new(expires_at)),
            credentials_path: Arc::new(RwLock::new(Some(path))),
        })
    }

    /// Get current token, refresh if expired
    pub async fn get_token(&self) -> anyhow::Result<String> {
        let token = self.token.read().await;
        if let Some(t) = token.as_ref() {
            let expires = *self.expires_at.read().await;
            if expires > chrono::Utc::now().timestamp_millis() + EXPIRY_SKEW_MS {
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

    /// Refresh token from Anthropic OAuth endpoint
    async fn refresh(&self) -> anyhow::Result<()> {
        let refresh_token = {
            let rt = self.refresh_token.read().await;
            rt.clone()
                .ok_or_else(|| anyhow::anyhow!("No refresh token available"))?
        };

        let body = Self::fetch_new_token(&refresh_token).await?;

        let expires_in = body.expires_in.unwrap_or(3600) * 1000; // Convert to ms
        let new_expires = chrono::Utc::now().timestamp_millis() + expires_in;
        let new_refresh = body.refresh_token.unwrap_or(refresh_token);

        *self.token.write().await = Some(body.access_token.clone());
        *self.refresh_token.write().await = Some(new_refresh.clone());
        *self.expires_at.write().await = new_expires;

        if let Some(path) = self.credentials_path.read().await.clone() {
            if let Err(e) =
                persist_oauth_token(&path, &body.access_token, &new_refresh, new_expires)
            {
                tracing::warn!("failed to persist refreshed OAuth token: {}", e);
            }
        }

        Ok(())
    }

    async fn fetch_new_token(refresh_token: &str) -> anyhow::Result<RefreshResponse> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()?;

        let response = client
            .post(CLAUDE_TOKEN_URL)
            .json(&serde_json::json!({
                "grant_type": "refresh_token",
                "refresh_token": refresh_token,
                "client_id": CLAUDE_CLIENT_ID,
            }))
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(anyhow::anyhow!(
                "OAuth token refresh failed: {}",
                response.status()
            ));
        }

        let body = response.json::<RefreshResponse>().await?;
        Ok(body)
    }
}

/// Write refreshed tokens back into the credentials file, preserving every
/// other key. Written to a sibling temp file and renamed so a crash mid-write
/// cannot truncate the caller's credentials.
fn persist_oauth_token(
    path: &Path,
    access_token: &str,
    refresh_token: &str,
    expires_at: i64,
) -> anyhow::Result<()> {
    let content = std::fs::read_to_string(path)?;
    let mut creds: Value = serde_json::from_str(&content)?;

    let oauth = creds
        .get_mut("claudeAiOauth")
        .ok_or_else(|| anyhow::anyhow!("no claudeAiOauth section in credentials"))?;
    oauth["accessToken"] = Value::String(access_token.to_string());
    oauth["refreshToken"] = Value::String(refresh_token.to_string());
    oauth["expiresAt"] = Value::Number(expires_at.into());

    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(&creds)?)?;
    restrict_to_owner(&tmp)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(unix)]
fn restrict_to_owner(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn restrict_to_owner(_path: &Path) -> anyhow::Result<()> {
    Ok(())
}

/// Path to the Claude credentials file.
pub fn claude_credentials_path() -> anyhow::Result<PathBuf> {
    Ok(dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?
        .join(".claude")
        .join(".credentials.json"))
}

/// Load OAuth token from Claude.dev credentials file
pub fn load_oauth_token_from_file() -> anyhow::Result<(String, Option<String>, i64)> {
    load_oauth_token_from_path(&claude_credentials_path()?)
}

fn load_oauth_token_from_path(
    credentials_path: &Path,
) -> anyhow::Result<(String, Option<String>, i64)> {
    let content = std::fs::read_to_string(credentials_path)
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
    fn persist_updates_tokens_and_keeps_other_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".credentials.json");
        std::fs::write(
            &path,
            r#"{"claudeAiOauth":{"accessToken":"old","refreshToken":"oldr","expiresAt":1,"scopes":["a"]},"otherTool":{"keep":true}}"#,
        )
        .unwrap();

        persist_oauth_token(&path, "new", "newr", 42).unwrap();

        let v: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(v["claudeAiOauth"]["accessToken"], "new");
        assert_eq!(v["claudeAiOauth"]["refreshToken"], "newr");
        assert_eq!(v["claudeAiOauth"]["expiresAt"], 42);
        assert_eq!(v["claudeAiOauth"]["scopes"][0], "a");
        assert_eq!(v["otherTool"]["keep"], true);
    }

    #[test]
    fn persist_leaves_file_intact_without_oauth_section() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".credentials.json");
        std::fs::write(&path, r#"{"otherTool":{"keep":true}}"#).unwrap();

        assert!(persist_oauth_token(&path, "new", "newr", 42).is_err());
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(v["otherTool"]["keep"], true);
    }

    #[test]
    fn load_reads_tokens_from_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".credentials.json");
        std::fs::write(
            &path,
            r#"{"claudeAiOauth":{"accessToken":"tok","refreshToken":"ref","expiresAt":99}}"#,
        )
        .unwrap();

        let (token, refresh, expires) = load_oauth_token_from_path(&path).unwrap();
        assert_eq!(token, "tok");
        assert_eq!(refresh.as_deref(), Some("ref"));
        assert_eq!(expires, 99);
    }

    #[test]
    fn test_load_oauth_fails_gracefully() {
        // This will fail if credentials don't exist, which is expected
        let result = load_oauth_token_from_file();
        // Either it succeeds or it fails gracefully
        let _ = result;
    }
}
