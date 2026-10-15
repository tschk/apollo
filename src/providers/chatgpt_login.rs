//! ChatGPT and Claude logins that already exist on this machine.
//!
//! The desktop must not start a fresh browser sign-in when one of these
//! stores already has a login. Codex CLI's `~/.codex/auth.json` is preferred
//! over `~/.config/rs_ai/credentials/chatgpt.json`: the CLI token is the one
//! the user signed in with, and it carries the connector scopes the Codex
//! endpoint requires. A token without those scopes is answered with a
//! Cloudflare HTML 403, which is not a new login.
//!
//! Claude falls back to Claude Code's `~/.claude/.credentials.json` when the
//! shared store has nothing. Tokens are never logged.

use serde_json::Value;

use super::shared_credentials::{self, ApolloTokens};

pub struct ChatGptLogin {
    pub access_token: String,
    pub account_id: Option<String>,
}

/// The ChatGPT login apollo should send, if one is already on disk.
pub fn load() -> Option<ChatGptLogin> {
    load_codex_cli().or_else(load_shared_store)
}

fn load_codex_cli() -> Option<ChatGptLogin> {
    let path = dirs::home_dir()?.join(".codex").join("auth.json");
    let value: Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    let access = value
        .pointer("/tokens/access_token")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())?;
    let account_id = value
        .pointer("/tokens/account_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    Some(ChatGptLogin {
        access_token: access.to_string(),
        account_id,
    })
}

fn load_shared_store() -> Option<ChatGptLogin> {
    let (access_token, _, _) = shared_credentials::load(rs_ai_oauth::OAuthProvider::ChatGpt)?;
    if access_token.trim().is_empty() {
        return None;
    }
    Some(ChatGptLogin {
        access_token,
        account_id: None,
    })
}

/// Claude access token from the shared store, then Claude Code's file.
pub fn load_claude_access() -> Option<String> {
    shared_credentials::load(rs_ai_oauth::OAuthProvider::Claude)
        .map(|(token, _, _)| token)
        .filter(|token| !token.trim().is_empty())
        .or_else(|| claude_code_tokens().map(|(token, _, _)| token))
}

fn claude_code_tokens() -> Option<ApolloTokens> {
    let path = dirs::home_dir()?.join(".claude").join(".credentials.json");
    let value: Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    let access = value
        .pointer("/claudeAiOauth/accessToken")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())?;
    let refresh = value
        .pointer("/claudeAiOauth/refreshToken")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let expires_at = value
        .pointer("/claudeAiOauth/expiresAt")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    Some((access.to_string(), refresh, expires_at))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &std::path::Path, body: &str) {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::write(path, body).unwrap();
    }

    #[test]
    fn codex_cli_login_wins_over_the_shared_store() {
        let home = tempfile::tempdir().unwrap();
        let creds = tempfile::tempdir().unwrap();
        write(
            &home.path().join(".codex/auth.json"),
            r#"{"auth_mode":"chatgpt","tokens":{"access_token":"from-codex","account_id":"acct-1"}}"#,
        );
        temp_env::with_vars(
            [
                ("HOME", Some(home.path().as_os_str())),
                ("RS_AI_CREDENTIALS_DIR", Some(creds.path().as_os_str())),
            ],
            || {
                rs_ai_oauth::credentials::save(
                    &rs_ai_oauth::OAuthProvider::ChatGpt,
                    &rs_ai_oauth::OAuthTokens {
                        access_token: "from-shared".into(),
                        refresh_token: Some("r".into()),
                        expires_at: u64::MAX / 2,
                    },
                )
                .unwrap();
                let login = load().expect("login");
                assert_eq!(login.access_token, "from-codex");
                assert_eq!(login.account_id.as_deref(), Some("acct-1"));
            },
        );
    }

    #[test]
    fn shared_store_is_used_when_codex_cli_is_absent() {
        let home = tempfile::tempdir().unwrap();
        let creds = tempfile::tempdir().unwrap();
        temp_env::with_vars(
            [
                ("HOME", Some(home.path().as_os_str())),
                ("RS_AI_CREDENTIALS_DIR", Some(creds.path().as_os_str())),
            ],
            || {
                rs_ai_oauth::credentials::save(
                    &rs_ai_oauth::OAuthProvider::ChatGpt,
                    &rs_ai_oauth::OAuthTokens {
                        access_token: "from-shared".into(),
                        refresh_token: Some("r".into()),
                        expires_at: u64::MAX / 2,
                    },
                )
                .unwrap();
                let login = load().expect("login");
                assert_eq!(login.access_token, "from-shared");
                assert!(login.account_id.is_none());
            },
        );
    }

    #[test]
    fn claude_code_file_is_used_when_the_shared_store_is_empty() {
        let home = tempfile::tempdir().unwrap();
        let creds = tempfile::tempdir().unwrap();
        write(
            &home.path().join(".claude/.credentials.json"),
            r#"{"claudeAiOauth":{"accessToken":"from-claude-code","refreshToken":"r","expiresAt":1}}"#,
        );
        temp_env::with_vars(
            [
                ("HOME", Some(home.path().as_os_str())),
                ("RS_AI_CREDENTIALS_DIR", Some(creds.path().as_os_str())),
            ],
            || {
                assert_eq!(load_claude_access().as_deref(), Some("from-claude-code"));
            },
        );
    }
}
