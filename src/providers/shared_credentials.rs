//! Shared OAuth credential store, common to every tool built on `rs_ai`.
//!
//! apollo and telekinesis each used to keep their own login: apollo read
//! Claude Code's `~/.claude/.credentials.json`, telekinesis wrote
//! `~/.telekinesis/<provider>_token.json`. `rs_ai_oauth::credentials` is the
//! single canonical store (`~/.config/rs_ai/credentials/<provider>.json`,
//! mode 0600) both now consult, so one login covers both binaries.
//!
//! This module is a thin adapter, not a replacement. apollo still falls back
//! to the paths it has always read, so an existing setup keeps working even
//! when the shared store is empty. That fallback is load-bearing rather than
//! belt-and-braces: `rs_ai_oauth` deserializes every candidate file as a flat
//! `OAuthTokens`, which the nested `claudeAiOauth` layout of a real Claude
//! Code credentials file does not match, so its own legacy probe cannot read
//! that file.

use rs_ai_oauth::credentials;
use rs_ai_oauth::{OAuthProvider, OAuthTokens};

/// Tokens as the rest of apollo expects them: `(access, refresh, expires_at)`
/// with `expires_at` in **milliseconds**.
///
/// `rs_ai_oauth` stores `expires_at` in seconds; apollo's `OAuthTokenCache`
/// compares against `chrono::Utc::now().timestamp_millis()`. Converting here
/// keeps the unit mismatch in one place instead of every call site.
pub type ApolloTokens = (String, Option<String>, i64);

fn to_apollo(tokens: OAuthTokens) -> ApolloTokens {
    let expires_at_ms = (tokens.expires_at as i64).saturating_mul(1000);
    (tokens.access_token, tokens.refresh_token, expires_at_ms)
}

/// Load a provider's tokens from the shared store.
///
/// An expired token is still returned when it carries a refresh token — the
/// caller's cache refreshes it — but a token that is both expired and
/// unrefreshable is treated as absent so the caller falls through to its own
/// credential path rather than failing on a dead token.
pub fn load(provider: OAuthProvider) -> Option<ApolloTokens> {
    let tokens = credentials::load(&provider)?;
    if credentials::is_expired(&tokens) && tokens.refresh_token.is_none() {
        return None;
    }
    Some(to_apollo(tokens))
}

/// Write refreshed tokens back to the shared store.
pub fn save(
    provider: OAuthProvider,
    access: &str,
    refresh: &str,
    expires_at_ms: i64,
) -> anyhow::Result<()> {
    let tokens = OAuthTokens {
        access_token: access.to_string(),
        refresh_token: Some(refresh.to_string()),
        expires_at: (expires_at_ms / 1000).max(0) as u64,
    };
    credentials::save(&provider, &tokens)?;
    Ok(())
}

/// One provider's login status, for `apollo doctor`.
pub struct SharedLogin {
    pub provider: &'static str,
    pub expired: bool,
    pub refreshable: bool,
}

/// Every provider with a credential in the shared store.
///
/// Reports names and expiry only — never a token — so the result is safe to
/// render in a diagnostics report.
pub fn logins() -> Vec<SharedLogin> {
    credentials::logged_in_providers()
        .into_iter()
        .filter(|provider| !matches!(provider, OAuthProvider::Claude))
        .filter_map(|provider| {
            let tokens = credentials::load(&provider)?;
            Some(SharedLogin {
                provider: provider_name(provider),
                expired: credentials::is_expired(&tokens),
                refreshable: tokens.refresh_token.is_some(),
            })
        })
        .collect()
}

/// `OAuthProvider::name` borrows from the enum, but a `SharedLogin` outlives
/// the value it came from, so map to a static name here.
fn provider_name(provider: OAuthProvider) -> &'static str {
    match provider {
        OAuthProvider::ChatGpt => "chatgpt",
        OAuthProvider::Xai => "grok",
        OAuthProvider::Claude => "claude",
        OAuthProvider::Gemini => "gemini",
        OAuthProvider::Antigravity => "antigravity",
        OAuthProvider::Copilot => "copilot",
        OAuthProvider::Kimi => "kimi",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now_secs() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    #[test]
    fn converts_expiry_seconds_to_milliseconds() {
        let (access, refresh, expires) = to_apollo(OAuthTokens {
            access_token: "a".into(),
            refresh_token: Some("r".into()),
            expires_at: 1_700_000_000,
        });
        assert_eq!(access, "a");
        assert_eq!(refresh.as_deref(), Some("r"));
        assert_eq!(expires, 1_700_000_000_000);
    }

    #[test]
    fn zero_expiry_stays_zero() {
        let (_, _, expires) = to_apollo(OAuthTokens {
            access_token: "a".into(),
            refresh_token: None,
            expires_at: 0,
        });
        assert_eq!(expires, 0);
    }

    /// The shared store is a real directory, so the round trip is exercised
    /// against a redirected one rather than the developer's own tokens.
    #[test]
    fn save_then_load_round_trips_through_a_redirected_store() {
        let dir = tempfile::tempdir().unwrap();
        temp_env::with_var(
            "RS_AI_CREDENTIALS_DIR",
            Some(dir.path().as_os_str()),
            || {
                save(
                    OAuthProvider::ChatGpt,
                    "access",
                    "refresh",
                    (now_secs() as i64 + 3600) * 1000,
                )
                .unwrap();

                let (access, refresh, expires) = load(OAuthProvider::ChatGpt).unwrap();
                assert_eq!(access, "access");
                assert_eq!(refresh.as_deref(), Some("refresh"));
                assert!(expires > now_secs() as i64 * 1000);

                let names: Vec<&str> = logins().iter().map(|l| l.provider).collect();
                assert!(names.contains(&"chatgpt"), "got {names:?}");
            },
        );
    }

    #[test]
    fn expired_token_without_refresh_reads_as_absent() {
        let dir = tempfile::tempdir().unwrap();
        temp_env::with_var(
            "RS_AI_CREDENTIALS_DIR",
            Some(dir.path().as_os_str()),
            || {
                let tokens = OAuthTokens {
                    access_token: "dead".into(),
                    refresh_token: None,
                    expires_at: 1,
                };
                credentials::save(&OAuthProvider::Claude, &tokens).unwrap();
                assert!(load(OAuthProvider::Claude).is_none());
            },
        );
    }

    #[test]
    fn expired_token_with_refresh_is_still_returned() {
        let dir = tempfile::tempdir().unwrap();
        temp_env::with_var(
            "RS_AI_CREDENTIALS_DIR",
            Some(dir.path().as_os_str()),
            || {
                let tokens = OAuthTokens {
                    access_token: "stale".into(),
                    refresh_token: Some("r".into()),
                    expires_at: 1,
                };
                credentials::save(&OAuthProvider::Claude, &tokens).unwrap();
                let (access, _, _) = load(OAuthProvider::Claude).unwrap();
                assert_eq!(access, "stale");
            },
        );
    }

    /// `logins()` is deliberately not asserted to be empty here: the shared
    /// store is redirected but the crate's legacy probe still reads `$HOME`,
    /// and redirecting `$HOME` process-wide would disturb unrelated tests.
    #[test]
    fn missing_provider_reads_as_absent() {
        let dir = tempfile::tempdir().unwrap();
        temp_env::with_var(
            "RS_AI_CREDENTIALS_DIR",
            Some(dir.path().as_os_str()),
            || {
                assert!(load(OAuthProvider::Kimi).is_none());
                assert!(!logins().iter().any(|l| l.provider == "kimi"));
            },
        );
    }
}
