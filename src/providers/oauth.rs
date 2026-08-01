//! OAuth token support for Anthropic
//! Converts Claude.dev OAuth tokens (oat01) to API calls via token exchange

use serde::Deserialize;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;

/// Public OAuth client id for the Claude CLI token endpoint.
const CLAUDE_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";

/// Anthropic OAuth token endpoints, tried in order.
///
/// `platform.claude.com` is the current host and the one that answers a
/// `grant_type` probe directly; the `console.anthropic.com` host it replaced
/// still resolves but now sits behind a bot challenge that can answer a POST
/// with an HTML interstitial instead of a token. Keeping it as a fallback
/// costs one extra request only when the new host has already failed, and
/// keeps a refresh working if the migration is not finished everywhere.
const CLAUDE_TOKEN_URLS: &[&str] = &[
    "https://platform.claude.com/v1/oauth/token",
    "https://console.anthropic.com/v1/oauth/token",
];

/// Refresh this long before actual expiry so an in-flight request does not
/// race the deadline.
const EXPIRY_SKEW_MS: i64 = 60_000;

#[derive(Deserialize)]
struct RefreshResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<i64>,
}

/// Where a cache's tokens came from, and so where a refreshed token is
/// written back. Without this a long-running process refreshes in memory only
/// and every restart starts from an expired token.
#[derive(Clone, Debug, PartialEq, Eq)]
enum TokenStore {
    /// The store shared with telekinesis, via `rs_ai_oauth::credentials`.
    #[cfg(feature = "rs-ai")]
    Shared,
    /// Claude Code's own `~/.claude/.credentials.json`.
    ClaudeCode(PathBuf),
}

/// OAuth token cache (refreshed as needed)
#[derive(Clone)]
pub struct OAuthTokenCache {
    token: Arc<RwLock<Option<String>>>,
    refresh_token: Arc<RwLock<Option<String>>>,
    expires_at: Arc<RwLock<i64>>,
    store: Arc<RwLock<Option<TokenStore>>>,
    token_urls: Arc<Vec<String>>,
}

fn default_token_urls() -> Arc<Vec<String>> {
    Arc::new(CLAUDE_TOKEN_URLS.iter().map(|u| u.to_string()).collect())
}

impl OAuthTokenCache {
    pub fn new(initial_token: String, refresh_token: Option<String>, expires_at: i64) -> Self {
        Self {
            token: Arc::new(RwLock::new(Some(initial_token))),
            refresh_token: Arc::new(RwLock::new(refresh_token)),
            expires_at: Arc::new(RwLock::new(expires_at)),
            store: Arc::new(RwLock::new(None)),
            token_urls: default_token_urls(),
        }
    }

    /// Point the refresh at another token endpoint. Used by the tests to hit a
    /// local server instead of Anthropic.
    #[cfg(test)]
    fn with_token_url(self, url: impl Into<String>) -> Self {
        self.with_token_urls(vec![url.into()])
    }

    #[cfg(test)]
    fn with_token_urls(mut self, urls: Vec<String>) -> Self {
        self.token_urls = Arc::new(urls);
        self
    }

    fn from_parts(tokens: (String, Option<String>, i64), store: TokenStore) -> Self {
        let (token, refresh_token, expires_at) = tokens;
        Self {
            token: Arc::new(RwLock::new(Some(token))),
            refresh_token: Arc::new(RwLock::new(refresh_token)),
            expires_at: Arc::new(RwLock::new(expires_at)),
            store: Arc::new(RwLock::new(Some(store))),
            token_urls: default_token_urls(),
        }
    }

    /// Build a cache from an on-disk login: the store shared with telekinesis
    /// first, then Claude Code's own credentials file. Refreshed tokens are
    /// written back to whichever one supplied them, so neither tool's login is
    /// rewritten in the other's format.
    pub fn from_credentials_file() -> anyhow::Result<Self> {
        #[cfg(feature = "rs-ai")]
        if let Some(tokens) = shared_claude_tokens() {
            return Ok(Self::from_parts(tokens, TokenStore::Shared));
        }

        let path = claude_credentials_path()?;
        let tokens = load_oauth_token_from_path(&path)?;
        Ok(Self::from_parts(tokens, TokenStore::ClaudeCode(path)))
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

        let body = Self::fetch_new_token(&self.token_urls, &refresh_token).await?;

        let expires_in = body.expires_in.unwrap_or(3600) * 1000; // Convert to ms
        let new_expires = chrono::Utc::now().timestamp_millis() + expires_in;
        let new_refresh = body.refresh_token.unwrap_or(refresh_token);

        *self.token.write().await = Some(body.access_token.clone());
        *self.refresh_token.write().await = Some(new_refresh.clone());
        *self.expires_at.write().await = new_expires;

        if let Some(store) = self.store.read().await.clone() {
            let persisted = match &store {
                #[cfg(feature = "rs-ai")]
                TokenStore::Shared => crate::providers::shared_credentials::save(
                    rs_ai_oauth::OAuthProvider::Claude,
                    &body.access_token,
                    &new_refresh,
                    new_expires,
                ),
                TokenStore::ClaudeCode(path) => {
                    persist_oauth_token(path, &body.access_token, &new_refresh, new_expires)
                }
            };
            if let Err(e) = persisted {
                tracing::warn!("failed to persist refreshed OAuth token: {}", e);
            }
        }

        Ok(())
    }

    /// Refresh against each configured endpoint in turn, returning the first
    /// that answers with a token. A failure on the last one is the error the
    /// caller sees, so a genuinely rejected refresh token still reports as one.
    async fn fetch_new_token(
        urls: &[String],
        refresh_token: &str,
    ) -> anyhow::Result<RefreshResponse> {
        let mut last: Option<anyhow::Error> = None;
        for url in urls {
            match Self::fetch_new_token_from(url, refresh_token).await {
                Ok(body) => return Ok(body),
                Err(e) => {
                    tracing::debug!("OAuth token refresh failed against {url}: {e}");
                    last = Some(e);
                }
            }
        }
        Err(last.unwrap_or_else(|| anyhow::anyhow!("no OAuth token endpoint configured")))
    }

    async fn fetch_new_token_from(
        url: &str,
        refresh_token: &str,
    ) -> anyhow::Result<RefreshResponse> {
        let client = crate::http::standard();

        let response = client
            .post(url)
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

/// Claude tokens from the store shared with telekinesis, if any.
#[cfg(feature = "rs-ai")]
fn shared_claude_tokens() -> Option<(String, Option<String>, i64)> {
    crate::providers::shared_credentials::load(rs_ai_oauth::OAuthProvider::Claude)
}

/// Load OAuth token: the shared `rs_ai` store first, then Claude.dev's own
/// credentials file.
///
/// Preferring the shared store means logging in once with either apollo or
/// telekinesis serves both; falling back keeps every setup that predates the
/// shared store working untouched.
pub fn load_oauth_token_from_file() -> anyhow::Result<(String, Option<String>, i64)> {
    #[cfg(feature = "rs-ai")]
    if let Some(tokens) = shared_claude_tokens() {
        return Ok(tokens);
    }

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

    // A token that has expired and carries nothing to refresh with is dead.
    // Returning it produces a 401 several layers away from the cause, so say
    // what is actually wrong here. On macOS this is the common case: Claude
    // Code keeps its live credentials in the login keychain and leaves this
    // file behind at whatever it last held. apollo does not read the keychain.
    if refresh_token.is_none() && expires_at <= chrono::Utc::now().timestamp_millis() {
        anyhow::bail!(
            "The Claude OAuth credential in {} has expired and has no refresh token, \
             so apollo cannot renew it. On macOS, Claude Code keeps its live \
             credentials in the login keychain rather than this file, and apollo \
             does not read the keychain. Log in again with `apollo login`, or set an \
             Anthropic API key with `apollo config set provider.api_key sk-ant-...` \
             or the ANTHROPIC_API_KEY environment variable.",
            credentials_path.display()
        );
    }

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

    /// Serve one canned token response and record how many requests arrived.
    async fn token_server(payload: Value) -> (String, Arc<std::sync::atomic::AtomicUsize>) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&seen);

        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                counter.fetch_add(1, Ordering::SeqCst);
                let mut buf = vec![0u8; 8192];
                let _ = socket.read(&mut buf).await;
                let body = payload.to_string();
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
                     content-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.flush().await;
            }
        });

        (format!("http://{addr}/v1/oauth/token"), seen)
    }

    #[tokio::test]
    async fn an_expired_token_is_refreshed_rather_than_used() {
        use std::sync::atomic::Ordering;

        let (url, seen) = token_server(serde_json::json!({
            "access_token": "fresh",
            "refresh_token": "next-refresh",
            "expires_in": 3600,
        }))
        .await;

        let cache = OAuthTokenCache::new(
            "stale".to_string(),
            Some("refresh".to_string()),
            chrono::Utc::now().timestamp_millis() - 1,
        )
        .with_token_url(url);

        assert_eq!(cache.get_token().await.unwrap(), "fresh");
        assert_eq!(seen.load(Ordering::SeqCst), 1);
        // The rotated refresh token replaces the old one, so the next refresh
        // does not replay a token the server has already retired.
        assert_eq!(
            cache.refresh_token.read().await.as_deref(),
            Some("next-refresh")
        );
        // Still valid, so a second call must not hit the endpoint again.
        assert_eq!(cache.get_token().await.unwrap(), "fresh");
        assert_eq!(seen.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_dead_first_endpoint_falls_through_to_the_next() {
        use std::sync::atomic::Ordering;

        // Bound and dropped, so the port is closed and the request fails.
        let dead = {
            let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            format!("http://{}/v1/oauth/token", l.local_addr().unwrap())
        };
        let (live, seen) = token_server(serde_json::json!({
            "access_token": "fresh",
            "expires_in": 3600,
        }))
        .await;

        let cache = OAuthTokenCache::new("stale".to_string(), Some("refresh".to_string()), 1)
            .with_token_urls(vec![dead, live]);

        assert_eq!(cache.get_token().await.unwrap(), "fresh");
        assert_eq!(seen.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn an_expired_token_without_a_refresh_token_reports_why() {
        let cache = OAuthTokenCache::new("stale".to_string(), None, 1);
        let err = cache.get_token().await.unwrap_err().to_string();
        assert!(err.contains("No refresh token available"), "got {err}");
    }

    #[test]
    fn a_dead_file_credential_says_why_it_is_dead() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".credentials.json");
        std::fs::write(
            &path,
            r#"{"claudeAiOauth":{"accessToken":"tok","expiresAt":1}}"#,
        )
        .unwrap();

        let err = load_oauth_token_from_path(&path).unwrap_err().to_string();
        assert!(err.contains("expired"), "got {err}");
        assert!(err.contains("keychain"), "got {err}");
        assert!(err.contains("ANTHROPIC_API_KEY"), "got {err}");
    }

    #[test]
    fn an_expired_file_credential_with_a_refresh_token_still_loads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".credentials.json");
        std::fs::write(
            &path,
            r#"{"claudeAiOauth":{"accessToken":"tok","refreshToken":"r","expiresAt":1}}"#,
        )
        .unwrap();

        // The cache refreshes it; refusing here would break a working setup.
        assert!(load_oauth_token_from_path(&path).is_ok());
    }

    #[test]
    fn test_load_oauth_fails_gracefully() {
        // This will fail if credentials don't exist, which is expected
        let result = load_oauth_token_from_file();
        // Either it succeeds or it fails gracefully
        let _ = result;
    }
}
