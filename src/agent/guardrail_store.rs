//! Cross-restart guardrail state.
//!
//! Two guardrails run, and they answer different questions.
//!
//! rx4 owns the **within-turn** one: it builds a fresh `ToolGuardrails` for
//! every `prompt()`, warns the model when it repeats itself, and stops a
//! runaway turn. That state is deliberately per-turn — it must not leak into
//! the next message.
//!
//! This owns the **across-turn** one. A bot that gets restarted mid-flail
//! otherwise wakes up with no idea that `shell` has failed nine times in a
//! row in this chat, and tries it a tenth. The store keeps one
//! [`ToolGuardrails`] per chat, persists the streaks as JSON, and reloads
//! them at startup. Its effects are deliberately narrow:
//!
//! - a note at the top of a turn naming the tools that keep failing, so the
//!   model can pick a different approach rather than rediscover the wall;
//! - a hard block, only when `hard_stop_enabled` is set, once a tool passes
//!   the block threshold.
//!
//! The file holds tool names, hashes of arguments, and counts. No arguments,
//! no outputs, no message text.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::RwLock;

use crate::agent::hooks::{HookDecision, ToolHook};
use crate::tools::guardrails::{
    GuardrailConfig, GuardrailDecision, GuardrailSnapshot, ToolGuardrails,
};
use crate::tools::ToolResult;

/// The on-disk document: chat id hash → snapshot.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct StateFile {
    chats: HashMap<String, GuardrailSnapshot>,
}

/// Per-chat guardrail state that survives a restart.
pub struct GuardrailStore {
    config: GuardrailConfig,
    path: Option<PathBuf>,
    chats: RwLock<HashMap<String, ToolGuardrails>>,
}

impl GuardrailStore {
    /// Load state from `path`. A missing file is an empty store; a corrupt one
    /// is an empty store plus a warning — guardrail state is a safety net, and
    /// refusing to start because the net is torn would be worse than mending it.
    pub fn load(config: GuardrailConfig, path: Option<PathBuf>) -> Self {
        let mut chats = HashMap::new();
        if let Some(path) = path.as_deref() {
            match std::fs::read_to_string(path) {
                Ok(raw) => match serde_json::from_str::<StateFile>(&raw) {
                    Ok(state) => {
                        for (chat, snapshot) in state.chats {
                            chats.insert(chat, ToolGuardrails::restore(config.clone(), snapshot));
                        }
                    }
                    Err(e) => tracing::warn!("guardrail state at {path:?} unreadable: {e}"),
                },
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => tracing::warn!("guardrail state at {path:?} unreadable: {e}"),
            }
        }
        Self {
            config,
            path,
            chats: RwLock::new(chats),
        }
    }

    /// Chat ids are external input; they key a file, so they are hashed rather
    /// than trusted (`../` in a chat id must not reach a path).
    fn key(chat_id: &str) -> String {
        format!("{:x}", Sha256::digest(chat_id.as_bytes()))
    }

    /// Observe one completed tool call.
    pub async fn observe(
        &self,
        chat_id: &str,
        name: &str,
        arguments: &str,
        result: &ToolResult,
    ) -> GuardrailDecision {
        let key = Self::key(chat_id);
        let mut chats = self.chats.write().await;
        let rails = chats
            .entry(key)
            .or_insert_with(|| ToolGuardrails::new(self.config.clone()));
        rails.observe(name, arguments, &result.output, result.is_error)
    }

    /// Whether `name` has already failed enough times in this chat to be
    /// blocked. Only ever true when hard stops are configured.
    pub async fn is_blocked(&self, chat_id: &str, name: &str) -> Option<String> {
        if !self.config.hard_stop_enabled {
            return None;
        }
        let chats = self.chats.read().await;
        let rails = chats.get(&Self::key(chat_id))?;
        let streak = rails.error_streak(name);
        (streak >= self.config.exact_failure_block_after).then(|| {
            format!(
                "Tool '{name}' has failed {streak} times in a row in this chat — blocked. \
                 Try a different approach, or tell the operator what is broken."
            )
        })
    }

    /// A note for the start of a turn, when this chat left tools failing.
    pub async fn carried_note(&self, chat_id: &str) -> Option<String> {
        if !self.config.warnings_enabled {
            return None;
        }
        let chats = self.chats.read().await;
        let rails = chats.get(&Self::key(chat_id))?;
        let failing = rails.failing_tools(self.config.exact_failure_warn_after);
        if failing.is_empty() {
            return None;
        }
        let list = failing
            .iter()
            .map(|(tool, count)| format!("`{tool}` ({count}x)"))
            .collect::<Vec<_>>()
            .join(", ");
        Some(format!(
            "[guardrails] Still failing from earlier in this chat: {list}. \
             Do not simply retry the same call — change approach or say what is blocked."
        ))
    }

    /// Forget a chat's streaks (the operator says the underlying problem is fixed).
    pub async fn clear(&self, chat_id: &str) {
        self.chats.write().await.remove(&Self::key(chat_id));
        let _ = self.persist().await;
    }

    /// Write the state file, atomically: a crash mid-write must not leave a
    /// truncated file that the next start throws away.
    pub async fn persist(&self) -> anyhow::Result<()> {
        let Some(path) = self.path.as_deref() else {
            return Ok(());
        };
        let state = {
            let chats = self.chats.read().await;
            StateFile {
                chats: chats
                    .iter()
                    .map(|(chat, rails)| (chat.clone(), rails.snapshot()))
                    .collect(),
            }
        };
        let raw = serde_json::to_string_pretty(&state)?;
        write_atomic(path, raw.as_bytes())
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// The store, scoped to one chat, as a tool hook.
///
/// Hooks are registered on the runner and see no chat id, so this is built per
/// turn and appended to the turn's hook list rather than kept on the runner.
pub struct ChatGuardrailHook {
    store: Arc<GuardrailStore>,
    chat_id: String,
}

impl ChatGuardrailHook {
    pub fn new(store: Arc<GuardrailStore>, chat_id: impl Into<String>) -> Self {
        Self {
            store,
            chat_id: chat_id.into(),
        }
    }
}

#[async_trait::async_trait]
impl ToolHook for ChatGuardrailHook {
    async fn before_tool_use(&self, tool_name: &str, _arguments: &str) -> HookDecision {
        match self.store.is_blocked(&self.chat_id, tool_name).await {
            Some(reason) => HookDecision::Block(reason),
            None => HookDecision::Allow,
        }
    }

    async fn after_tool_result(&self, tool_name: &str, arguments: &str, result: &ToolResult) {
        match self
            .store
            .observe(&self.chat_id, tool_name, arguments, result)
            .await
        {
            GuardrailDecision::Proceed => {}
            GuardrailDecision::Warn(reason) | GuardrailDecision::Stop(reason) => {
                tracing::warn!("[guardrails] {}: {reason}", self.chat_id);
            }
        }
        if let Err(e) = self.store.persist().await {
            tracing::warn!("guardrail state not persisted: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hard_stop_config() -> GuardrailConfig {
        GuardrailConfig {
            hard_stop_enabled: true,
            exact_failure_block_after: 3,
            exact_failure_warn_after: 2,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn a_failing_tool_is_still_failing_after_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state").join("guardrails.json");

        let store = GuardrailStore::load(hard_stop_config(), Some(path.clone()));
        for _ in 0..3 {
            store
                .observe("chat-1", "shell", "{}", &ToolResult::error("boom"))
                .await;
        }
        store.persist().await.unwrap();
        assert!(store.is_blocked("chat-1", "shell").await.is_some());

        // A new process, same file.
        let reloaded = GuardrailStore::load(hard_stop_config(), Some(path));
        assert!(
            reloaded.is_blocked("chat-1", "shell").await.is_some(),
            "the streak must survive the restart"
        );
        assert!(
            reloaded.is_blocked("chat-2", "shell").await.is_none(),
            "another chat is not blocked by this one"
        );
        assert!(
            reloaded.is_blocked("chat-1", "edit").await.is_none(),
            "another tool is not blocked by this one"
        );
        let note = reloaded.carried_note("chat-1").await.expect("note");
        assert!(note.contains("shell"), "{note}");
    }

    #[tokio::test]
    async fn a_success_clears_the_streak_and_the_block() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("guardrails.json");
        let store = GuardrailStore::load(hard_stop_config(), Some(path.clone()));

        for _ in 0..3 {
            store
                .observe("chat-1", "shell", "{}", &ToolResult::error("boom"))
                .await;
        }
        store
            .observe("chat-1", "shell", "{}", &ToolResult::success("ok"))
            .await;
        store.persist().await.unwrap();

        let reloaded = GuardrailStore::load(hard_stop_config(), Some(path));
        assert!(reloaded.is_blocked("chat-1", "shell").await.is_none());
        assert!(reloaded.carried_note("chat-1").await.is_none());
    }

    #[tokio::test]
    async fn without_hard_stops_the_state_is_carried_but_nothing_is_blocked() {
        let store = GuardrailStore::load(GuardrailConfig::default(), None);
        for _ in 0..20 {
            store
                .observe("chat-1", "shell", "{}", &ToolResult::error("boom"))
                .await;
        }
        assert!(
            store.is_blocked("chat-1", "shell").await.is_none(),
            "default config warns, it does not block"
        );
        assert!(store.carried_note("chat-1").await.is_some());
    }

    #[tokio::test]
    async fn a_chat_id_cannot_escape_the_state_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("guardrails.json");
        let store = GuardrailStore::load(hard_stop_config(), Some(path.clone()));
        store
            .observe(
                "../../etc/passwd",
                "shell",
                "{}",
                &ToolResult::error("boom"),
            )
            .await;
        store.persist().await.unwrap();

        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(!raw.contains(".."), "{raw}");
        assert!(!raw.contains("passwd"), "{raw}");
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            1,
            "nothing was written outside the state file"
        );
    }

    #[tokio::test]
    async fn a_corrupt_state_file_starts_empty_instead_of_failing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("guardrails.json");
        std::fs::write(&path, "{not json").unwrap();
        let store = GuardrailStore::load(hard_stop_config(), Some(path));
        assert!(store.carried_note("chat-1").await.is_none());
    }

    #[tokio::test]
    async fn the_hook_blocks_a_tool_that_already_passed_the_threshold() {
        let store = Arc::new(GuardrailStore::load(hard_stop_config(), None));
        let hook = ChatGuardrailHook::new(Arc::clone(&store), "chat-1");

        for _ in 0..3 {
            assert!(matches!(
                hook.before_tool_use("shell", "{}").await,
                HookDecision::Allow
            ));
            hook.after_tool_result("shell", "{}", &ToolResult::error("boom"))
                .await;
        }
        assert!(matches!(
            hook.before_tool_use("shell", "{}").await,
            HookDecision::Block(_)
        ));
        assert!(
            matches!(
                hook.before_tool_use("edit", "{}").await,
                HookDecision::Allow
            ),
            "the block is per tool"
        );
    }
}
