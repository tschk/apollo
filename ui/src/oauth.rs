//! Browser sign-in for the pinned providers, through `rs_ai_oauth` — the
//! same PKCE flows and the same shared credential store
//! (`~/.config/rs_ai/credentials/<provider>.json`, 0600) that apollo reads.
//!
//! Whether a sign-in is *useful* depends on the apollo side, so each kind
//! carries a [`Support`] that the UI shows before anyone clicks:
//!
//! - ChatGPT: apollo loads the shared ChatGPT login when `provider.api_key`
//!   is unset (`bootstrap::load_config_workspace`) and runs it through its
//!   Codex provider. End to end.
//! - GitHub Copilot: the login lands in the shared store and apollo's
//!   Copilot provider reads it. That provider is part of the default build.
//!
//! The browser is the system default: `open` on macOS, `xdg-open` on Linux,
//! `start` on Windows. Nothing embedded.
//! - Claude: apollo's `anthropic` provider (rs_ai's `ClaudeProvider`) uses
//!   the shared Claude login when no `ANTHROPIC_API_KEY` is set, sending it
//!   the way Claude Code does and refreshing it when it expires. Anthropic
//!   may bill this as "extra usage" rather than against the plan.

use std::sync::atomic::{AtomicBool, Ordering};

use rs_ai_oauth::{credentials, OAuthProvider};

/// One flag per kind: a browser flow is listening on its callback port.
/// Process-wide, so a rebuilt onboarding view re-attaches instead of
/// starting a second listener on a busy port.
static RUNNING: [AtomicBool; 3] = [
    AtomicBool::new(false),
    AtomicBool::new(false),
    AtomicBool::new(false),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OAuthKind {
    ChatGpt,
    Copilot,
    Claude,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Support {
    /// Sign-in works and apollo uses it.
    Live,
    /// Sign-in works; apollo needs a non-default build to use it.
    #[allow(dead_code)]
    NeedsFeature(&'static str),
    /// apollo cannot use this login; sign-in is not offered. (No provider
    /// is in this state today; kept so the UI handles one honestly.)
    #[allow(dead_code)]
    Unsupported(&'static str),
}

impl OAuthKind {
    fn slot(self) -> &'static AtomicBool {
        &RUNNING[self as usize]
    }

    /// A sign-in for this kind is in flight somewhere in the app.
    pub fn in_flight(self) -> bool {
        self.slot().load(Ordering::SeqCst)
    }

    /// Claim the flow; `false` if one is already running.
    pub fn claim(self) -> bool {
        !self.slot().swap(true, Ordering::SeqCst)
    }

    fn release(self) {
        self.slot().store(false, Ordering::SeqCst);
    }

    pub fn provider(self) -> OAuthProvider {
        match self {
            OAuthKind::ChatGpt => OAuthProvider::ChatGpt,
            OAuthKind::Copilot => OAuthProvider::Copilot,
            OAuthKind::Claude => OAuthProvider::Claude,
        }
    }

    pub fn support(self) -> Support {
        match self {
            OAuthKind::ChatGpt => Support::Live,
            OAuthKind::Copilot => Support::Live,
            OAuthKind::Claude => Support::Live,
        }
    }

    /// Local port the provider's redirect comes back to.
    /// (`rs_ai_oauth` keeps its own accessor private; these match its
    /// documented redirect URIs.)
    #[allow(dead_code)]
    pub fn callback_port(self) -> u16 {
        match self {
            OAuthKind::ChatGpt => 1455,
            OAuthKind::Copilot => 9876,
            OAuthKind::Claude => 53692,
        }
    }

    /// A usable login is already in the shared store (live, or refreshable).
    pub fn signed_in(self) -> bool {
        credentials::load(&self.provider())
            .map(|t| !credentials::is_expired(&t) || t.refresh_token.is_some())
            .unwrap_or(false)
    }

    /// The stored access token, for listing the plan's models. Stays on the
    /// worker thread that asked; never shown.
    pub fn access_token(self) -> Option<String> {
        credentials::load(&self.provider())
            .filter(|t| !credentials::is_expired(t))
            .map(|t| t.access_token)
    }

    /// Run the browser flow and store the tokens (call [`claim`] first; this
    /// releases the claim when it returns). Blocks until the callback
    /// arrives or the flow's own 180s timeout; call it off the UI thread.
    /// Only the outcome is returned — never a token.
    pub fn sign_in(self) -> Result<(), String> {
        if let Support::Unsupported(why) = self.support() {
            return Err(why.to_string());
        }
        let provider = self.provider();
        let result = rs_ai_oauth::start_oauth_flow(provider)
            .map_err(|e| format!("sign-in failed: {e}"))
            .and_then(|tokens| {
                credentials::save(&provider, &tokens)
                    .map(|_| ())
                    .map_err(|e| format!("signed in, but could not store the login: {e}"))
            });
        self.release();
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn support_matches_what_apollo_can_run() {
        assert_eq!(OAuthKind::ChatGpt.support(), Support::Live);
        assert_eq!(OAuthKind::Copilot.support(), Support::Live);
        assert_eq!(OAuthKind::Claude.support(), Support::Live);
    }

    #[test]
    fn signed_in_reads_the_shared_store() {
        let dir = tempfile::tempdir().unwrap();
        temp_env::with_var("RS_AI_CREDENTIALS_DIR", Some(dir.path()), || {
            assert!(!OAuthKind::ChatGpt.signed_in());
            let tokens = rs_ai_oauth::OAuthTokens {
                access_token: "a".into(),
                refresh_token: Some("r".into()),
                expires_at: 1,
            };
            credentials::save(&OAuthProvider::ChatGpt, &tokens).unwrap();
            assert!(
                OAuthKind::ChatGpt.signed_in(),
                "expired but refreshable counts"
            );
        });
    }
}
