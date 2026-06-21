//! Plugin system — JSON-RPC 2.0 interface + lifecycle hooks.
//!
//! Two extension surfaces:
//! 1. JSON-RPC methods (existing) — regular plugin methods
//! 2. Lifecycle hooks — intercept tool calls, session events, etc.

use crate::tools::ToolResult;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

// ── Core Plugin trait ─────────────────────────────────────────────────────

/// Plugin trait — implement this to extend aclaw with JSON-RPC methods
#[async_trait]
pub trait Plugin: Send + Sync {
    /// Plugin name (e.g., "ai", "remote", "tools", "vibemania", "git")
    fn name(&self) -> &str;

    /// Plugin version
    fn version(&self) -> &str;

    /// List available methods this plugin provides
    fn methods(&self) -> Vec<MethodSpec>;

    /// Execute a method (JSON-RPC style)
    async fn call(&self, method: &str, params: Value) -> Result<Value, PluginError>;

    /// Called after registration — gives the plugin a chance to register tools and hooks.
    /// Default implementation does nothing.
    async fn on_register(&self, _ctx: &mut PluginContext) {}
}

/// Method specification
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MethodSpec {
    pub name: String,
    pub description: String,
    pub params: HashMap<String, String>,
    pub returns: String,
}

/// Plugin error
#[derive(Debug, Serialize, Deserialize)]
pub struct PluginError {
    pub code: i32,
    pub message: String,
    pub data: Option<Value>,
}

impl PluginError {
    pub fn new(code: i32, message: &str) -> Self {
        Self {
            code,
            message: message.to_string(),
            data: None,
        }
    }
}

// ── Lifecycle Hook System ─────────────────────────────────────────────────

/// Events that lifecycle hooks can intercept
#[derive(Debug, Clone)]
pub enum LifecycleEvent {
    /// Before a tool executes (tool_name, arguments_json)
    BeforeToolCall(String, String),
    /// After a tool completes (tool_name, arguments_json, result)
    AfterToolCall(String, String, ToolResult),
    /// A conversation session started (session_id)
    SessionStart(String),
    /// A conversation session ended (session_id)
    SessionEnd(String),
    /// Agent loop started (chat_id, message)
    AgentStart(String, String),
    /// Agent loop completed (chat_id, response)
    AgentDone(String, String),
}

// Re-export hook decision types from agent hooks
pub use crate::agent::hooks::HookDecision;

/// A lifecycle hook — registered by plugins or core
#[async_trait]
pub trait LifecycleHook: Send + Sync {
    fn name(&self) -> &str;

    /// Called on any lifecycle event. Return Ok(()) to continue, Err to abort (tool-call only).
    async fn on_event(&self, event: &LifecycleEvent) -> anyhow::Result<()>;
}

/// Hook that can block tool execution based on custom logic
#[async_trait]
pub trait PreToolHook: Send + Sync {
    fn name(&self) -> &str;
    async fn before_tool_call(&self, name: &str, arguments: &str) -> HookDecision;
}

/// Hook that observes tool results (logging, metrics, auditing)
#[async_trait]
pub trait PostToolHook: Send + Sync {
    fn name(&self) -> &str;
    async fn after_tool_call(&self, name: &str, arguments: &str, result: &ToolResult);
}

/// Central hook manager — dispatches events to all registered hooks
#[derive(Default)]
pub struct HookManager {
    lifecycle_hooks: Vec<Arc<dyn LifecycleHook>>,
    pre_hooks: Vec<Arc<dyn PreToolHook>>,
    post_hooks: Vec<Arc<dyn PostToolHook>>,
}

impl HookManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_lifecycle(&mut self, hook: Arc<dyn LifecycleHook>) {
        self.lifecycle_hooks.push(hook);
    }

    pub fn register_pre_tool(&mut self, hook: Arc<dyn PreToolHook>) {
        self.pre_hooks.push(hook);
    }

    pub fn register_post_tool(&mut self, hook: Arc<dyn PostToolHook>) {
        self.post_hooks.push(hook);
    }

    /// Fire event to all lifecycle hooks
    pub async fn emit(&self, event: &LifecycleEvent) {
        for hook in &self.lifecycle_hooks {
            if let Err(e) = hook.on_event(event).await {
                tracing::warn!(
                    "LifecycleHook '{}' error on {:?}: {}",
                    hook.name(),
                    event,
                    e
                );
            }
        }
    }

    /// Run pre-tool hooks — first Block wins
    pub async fn check_pre_tool(&self, name: &str, arguments: &str) -> HookDecision {
        for hook in &self.pre_hooks {
            match hook.before_tool_call(name, arguments).await {
                HookDecision::Block(reason) => return HookDecision::Block(reason),
                HookDecision::Allow => {}
            }
        }
        HookDecision::Allow
    }

    /// Run post-tool hooks
    pub async fn notify_post_tool(&self, name: &str, arguments: &str, result: &ToolResult) {
        for hook in &self.post_hooks {
            hook.after_tool_call(name, arguments, result).await;
        }
    }
}

/// Plugin context — passed to plugins during registration, allows tool registration
#[derive(Default)]
pub struct PluginContext {
    /// Tools registered by plugins
    pub tools: Vec<Arc<dyn crate::tools::Tool>>,
    /// Hook manager for lifecycle hooks
    pub hooks: HookManager,
}

impl PluginContext {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a tool that the agent can call
    pub fn register_tool(&mut self, tool: Arc<dyn crate::tools::Tool>) {
        self.tools.push(tool);
    }

    /// Register a lifecycle hook
    pub fn register_lifecycle_hook(&mut self, hook: Arc<dyn LifecycleHook>) {
        self.hooks.register_lifecycle(hook);
    }

    /// Register a pre-tool hook
    pub fn register_pre_tool_hook(&mut self, hook: Arc<dyn PreToolHook>) {
        self.hooks.register_pre_tool(hook);
    }

    /// Register a post-tool hook
    pub fn register_post_tool_hook(&mut self, hook: Arc<dyn PostToolHook>) {
        self.hooks.register_post_tool(hook);
    }
}

// ── Plugin Registry ───────────────────────────────────────────────────────

/// Plugin registry — manage installed plugins
pub struct PluginRegistry {
    plugins: HashMap<String, Arc<dyn Plugin>>,
    hooks: HookManager,
}

impl PluginRegistry {
    pub fn new() -> Self {
        Self {
            plugins: HashMap::new(),
            hooks: HookManager::new(),
        }
    }

    /// Log discovered OpenClaw/Hermes plugins from workspace (host-agnostic).
    pub fn ingest_host_plugins(&mut self, workspace: &std::path::Path, extra: &[PathBuf]) {
        let found = crate::plugin_hosts::discover_host_plugins(workspace, extra);
        for p in found {
            tracing::info!(
                "[plugin-host] {:?} {} {:?}",
                p.kind,
                p.name.as_deref().unwrap_or("?"),
                p.path
            );
        }
    }

    /// Register a plugin and call its on_register with a PluginContext
    pub async fn register(&mut self, plugin: Arc<dyn Plugin>) {
        let name = plugin.name().to_string();
        let mut ctx = PluginContext::default();
        plugin.on_register(&mut ctx).await;
        // Register any tools the plugin exposed during on_register
        for tool in ctx.tools {
            tracing::info!("[plugin] '{}' registered tool: {}", name, tool.name());
        }
        // Merge hooks
        self.hooks.lifecycle_hooks.extend(ctx.hooks.lifecycle_hooks);
        self.hooks.pre_hooks.extend(ctx.hooks.pre_hooks);
        self.hooks.post_hooks.extend(ctx.hooks.post_hooks);
        self.plugins.insert(name, plugin);
    }

    /// Register a plugin without async (for tests, legacy)
    pub fn register_sync(&mut self, plugin: Arc<dyn Plugin>) {
        self.plugins.insert(plugin.name().to_string(), plugin);
    }

    /// Register a lifecycle hook directly (not from a plugin)
    pub fn register_hook(&mut self, hook: Arc<dyn LifecycleHook>) {
        self.hooks.register_lifecycle(hook);
    }

    /// Access the hook manager
    pub fn hooks(&self) -> &HookManager {
        &self.hooks
    }

    /// Call a plugin method
    pub async fn call(
        &self,
        plugin: &str,
        method: &str,
        params: Value,
    ) -> Result<Value, PluginError> {
        let p = self
            .plugins
            .get(plugin)
            .ok_or_else(|| PluginError::new(-32601, "Plugin not found"))?;
        p.call(method, params).await
    }

    /// List all plugins
    pub fn list(&self) -> Vec<String> {
        self.plugins.keys().cloned().collect()
    }

    /// Get plugin info
    pub fn info(&self, name: &str) -> Option<PluginInfo> {
        self.plugins.get(name).map(|p| PluginInfo {
            name: p.name().to_string(),
            version: p.version().to_string(),
            methods: p.methods(),
        })
    }

    /// Emit lifecycle event to all registered hooks
    pub async fn emit(&self, event: &LifecycleEvent) {
        self.hooks.emit(event).await;
    }

    /// Run pre-tool checks
    pub async fn check_pre_tool(&self, name: &str, arguments: &str) -> HookDecision {
        self.hooks.check_pre_tool(name, arguments).await
    }

    /// Notify post-tool hooks
    pub async fn notify_post_tool(&self, name: &str, arguments: &str, result: &ToolResult) {
        self.hooks.notify_post_tool(name, arguments, result).await;
    }
}

impl Default for PluginRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ── Plugin Info ───────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct PluginInfo {
    pub name: String,
    pub version: String,
    pub methods: Vec<MethodSpec>,
}

// ── Built-in lifecycle hooks ──────────────────────────────────────────────

/// Append short session notes on agent completion.
pub struct SessionNoteLifecycleHook {
    workspace: std::path::PathBuf,
}

impl SessionNoteLifecycleHook {
    pub fn new(workspace: std::path::PathBuf) -> Self {
        Self { workspace }
    }
}

#[async_trait]
impl LifecycleHook for SessionNoteLifecycleHook {
    fn name(&self) -> &str {
        "session_note"
    }

    async fn on_event(&self, event: &LifecycleEvent) -> anyhow::Result<()> {
        if let LifecycleEvent::AgentDone(chat_id, response) = event {
            let preview: String = response.chars().take(200).collect();
            if !preview.is_empty() {
                let _ = crate::memory::session_note::append_session_note(
                    &self.workspace,
                    chat_id,
                    &preview,
                );
            }
        }
        Ok(())
    }
}

/// Logging hook — traces every lifecycle event
pub struct LoggingLifecycleHook;

#[async_trait]
impl LifecycleHook for LoggingLifecycleHook {
    fn name(&self) -> &str {
        "logging"
    }

    async fn on_event(&self, event: &LifecycleEvent) -> anyhow::Result<()> {
        match event {
            LifecycleEvent::BeforeToolCall(name, args) => {
                let preview: String = args.chars().take(80).collect();
                tracing::debug!("[hook] before_tool {} args:{}", name, preview);
            }
            LifecycleEvent::AfterToolCall(name, _args, result) => {
                tracing::debug!(
                    "[hook] after_tool {} is_error:{} len:{}",
                    name,
                    result.is_error,
                    result.output.len()
                );
            }
            LifecycleEvent::SessionStart(id) => {
                tracing::info!("[hook] session_start {}", id);
            }
            LifecycleEvent::SessionEnd(id) => {
                tracing::info!("[hook] session_end {}", id);
            }
            LifecycleEvent::AgentStart(chat_id, msg) => {
                let preview: String = msg.chars().take(80).collect();
                tracing::debug!("[hook] agent_start {} msg:{}", chat_id, preview);
            }
            LifecycleEvent::AgentDone(chat_id, response) => {
                let preview: String = response.chars().take(80).collect();
                tracing::debug!("[hook] agent_done {} response:{}", chat_id, preview);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct TestPlugin;

    #[async_trait]
    impl Plugin for TestPlugin {
        fn name(&self) -> &str {
            "test"
        }

        fn version(&self) -> &str {
            "0.1.0"
        }

        fn methods(&self) -> Vec<MethodSpec> {
            vec![MethodSpec {
                name: "echo".to_string(),
                description: "Echo input".to_string(),
                params: HashMap::new(),
                returns: "object".to_string(),
            }]
        }

        async fn call(&self, method: &str, params: Value) -> Result<Value, PluginError> {
            match method {
                "echo" => Ok(json!({ "result": params })),
                _ => Err(PluginError::new(-32601, "Method not found")),
            }
        }
    }

    #[tokio::test]
    async fn test_plugin_call() {
        let mut registry = PluginRegistry::new();
        registry.register_sync(Arc::new(TestPlugin));

        let result = registry
            .call("test", "echo", json!({ "code": "fn main() {}" }))
            .await
            .unwrap();

        assert!(result.get("result").is_some());
    }

    #[tokio::test]
    async fn test_hook_manager_emit() {
        let mut manager = HookManager::new();
        manager.register_lifecycle(Arc::new(LoggingLifecycleHook));
        let _ = manager.check_pre_tool("test", "{}").await;
        manager
            .notify_post_tool("test", "{}", &ToolResult::success("ok"))
            .await;
        // No crash = success
    }

    #[tokio::test]
    async fn test_plugin_context_register_tool() {
        let mut ctx = PluginContext::new();
        // Just verify no crash
        ctx.register_lifecycle_hook(Arc::new(LoggingLifecycleHook));
        assert!(ctx.hooks.lifecycle_hooks.len() == 1);
    }
}
