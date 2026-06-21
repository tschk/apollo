//! Tool hook system — PreToolUse / PostToolUse event interception.
//!
//! Hooks run synchronously in the tool execution path. A hook can allow,
//! block, or (future) modify a tool call. Multiple hooks are checked in
//! registration order; the first Block wins.

use std::sync::Arc;

use async_trait::async_trait;

use crate::tools::ToolResult;

/// Decision returned by a hook's `before_tool_use` method.
#[derive(Debug)]
pub enum HookDecision {
    Allow,
    Block(String), // reason shown to the LLM as a tool error
}

/// Hook that runs before and after every tool execution.
#[async_trait]
pub trait ToolHook: Send + Sync {
    /// Called before the tool executes. Return `Block` to skip execution.
    async fn before_tool_use(&self, tool_name: &str, arguments: &str) -> HookDecision;

    /// Called after the tool completes (whether success or error).
    async fn after_tool_result(&self, _tool_name: &str, _arguments: &str, _result: &ToolResult) {}
}

/// Enforces `allow` / `deny` rules from `PermissionRulesConfig`.
///
/// Deny is checked before allow. Patterns support a single trailing `*` wildcard.
pub struct PermissionHook {
    deny: Vec<String>,
    allow: Vec<String>,
}

impl PermissionHook {
    pub fn new(deny: Vec<String>, allow: Vec<String>) -> Self {
        Self { deny, allow }
    }

    fn matches(pattern: &str, tool_name: &str) -> bool {
        if let Some(prefix) = pattern.strip_suffix('*') {
            tool_name.starts_with(prefix)
        } else {
            pattern == tool_name
        }
    }
}

#[async_trait]
impl ToolHook for PermissionHook {
    async fn before_tool_use(&self, tool_name: &str, _arguments: &str) -> HookDecision {
        // Deny list checked first
        for pattern in &self.deny {
            if Self::matches(pattern, tool_name) {
                return HookDecision::Block(format!(
                    "Tool '{}' is blocked by permission rules (deny list).",
                    tool_name
                ));
            }
        }

        // Allow list: if non-empty, tool must match at least one pattern
        if !self.allow.is_empty() {
            let allowed = self.allow.iter().any(|p| Self::matches(p, tool_name));
            if !allowed {
                return HookDecision::Block(format!(
                    "Tool '{}' is not in the allow list.",
                    tool_name
                ));
            }
        }

        HookDecision::Allow
    }
}

/// Logging hook — emits a tracing event for every tool call and result.
pub struct LoggingHook;

#[async_trait]
impl ToolHook for LoggingHook {
    async fn before_tool_use(&self, tool_name: &str, arguments: &str) -> HookDecision {
        let preview: String = arguments.chars().take(120).collect();
        tracing::debug!("→ tool:{} args:{}", tool_name, preview);
        HookDecision::Allow
    }

    async fn after_tool_result(&self, tool_name: &str, _arguments: &str, result: &ToolResult) {
        tracing::debug!(
            "← tool:{} is_error:{} len:{}",
            tool_name,
            result.is_error,
            result.output.len()
        );
    }
}

/// Helper: run all registered hooks before a tool call.
/// Returns `HookDecision::Block` on first blocking hook; otherwise `Allow`.
pub async fn run_pre_hooks(
    hooks: &[Arc<dyn ToolHook>],
    tool_name: &str,
    arguments: &str,
) -> HookDecision {
    for hook in hooks {
        match hook.before_tool_use(tool_name, arguments).await {
            HookDecision::Block(reason) => return HookDecision::Block(reason),
            HookDecision::Allow => {}
        }
    }
    HookDecision::Allow
}

/// Helper: run all registered hooks after a tool call.
pub async fn run_post_hooks(
    hooks: &[Arc<dyn ToolHook>],
    tool_name: &str,
    arguments: &str,
    result: &ToolResult,
) {
    for hook in hooks {
        hook.after_tool_result(tool_name, arguments, result).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn permission_hook_deny() {
        let hook = PermissionHook::new(vec!["exec".to_string()], vec![]);
        assert!(matches!(
            hook.before_tool_use("exec", "{}").await,
            HookDecision::Block(_)
        ));
        assert!(matches!(
            hook.before_tool_use("web_search", "{}").await,
            HookDecision::Allow
        ));
    }

    #[tokio::test]
    async fn permission_hook_allow_list() {
        let hook = PermissionHook::new(
            vec![],
            vec!["web_search".to_string(), "file_ops".to_string()],
        );
        assert!(matches!(
            hook.before_tool_use("web_search", "{}").await,
            HookDecision::Allow
        ));
        assert!(matches!(
            hook.before_tool_use("exec", "{}").await,
            HookDecision::Block(_)
        ));
    }

    #[tokio::test]
    async fn permission_hook_wildcard() {
        let hook = PermissionHook::new(vec!["web_*".to_string()], vec![]);
        assert!(matches!(
            hook.before_tool_use("web_search", "{}").await,
            HookDecision::Block(_)
        ));
        assert!(matches!(
            hook.before_tool_use("web_fetch", "{}").await,
            HookDecision::Block(_)
        ));
        assert!(matches!(
            hook.before_tool_use("exec", "{}").await,
            HookDecision::Allow
        ));
    }

    #[tokio::test]
    async fn run_pre_hooks_first_block_wins() {
        let hooks: Vec<Arc<dyn ToolHook>> = vec![
            Arc::new(PermissionHook::new(vec!["exec".to_string()], vec![])),
            Arc::new(PermissionHook::new(vec!["web_search".to_string()], vec![])),
        ];
        // exec is blocked by first hook
        assert!(matches!(
            run_pre_hooks(&hooks, "exec", "{}").await,
            HookDecision::Block(_)
        ));
        // web_search passes first hook, blocked by second
        assert!(matches!(
            run_pre_hooks(&hooks, "web_search", "{}").await,
            HookDecision::Block(_)
        ));
        // file_ops passes both
        assert!(matches!(
            run_pre_hooks(&hooks, "file_ops", "{}").await,
            HookDecision::Allow
        ));
    }
}
