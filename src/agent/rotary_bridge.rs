//! Rotary (rx4) bridge — adapts unthinkclaw's types to rx4's agent harness.
//!
//! This module provides:
//! - `RotaryProviderAdapter`: wraps an unthinkclaw `Provider` as an `rx4::Provider`
//!   so rx4's `Agent` loop can use unthinkclaw's existing provider backends.
//! - `register_unthinkclaw_tools`: registers unthinkclaw's `Tool` trait objects
//!   into rx4's `ToolRegistry` via boxed closures.
//! - `chat_message_to_rx4` / `rx4_message_to_chat`: type translators between
//!   unthinkclaw's `ChatMessage` and rx4's `Message`.
//! - `RotaryAgentBridge`: wraps an `rx4::Agent`, wiring up provider, tools,
//!   system prompt, and providing a `run_prompt` method that the outer
//!   unthinkclaw shell (channels, swarm, cron, heartbeat) can call.
//!
//! The bridge is designed to be incremental. The existing `AgentRunner` loop
//! remains available; this bridge provides an alternative execution path that
//! delegates the core agent loop to rx4 while keeping unthinkclaw's unique
//! features (channels, swarm, cron, heartbeat, autonomous mode, plugins, MCP)
//! as the outer shell.

use std::sync::Arc;

use rx4::provider::{Message, Provider as Rx4Provider, ProviderError as Rx4ProviderError, Role, StreamEvent};

use crate::providers::{ChatMessage, ChatRequest, Provider as UnthinkclawProvider};
use crate::tools::{Tool as UnthinkclawTool, ToolSpec};

// ── Message translation ──────────────────────────────────────────────────

/// Convert an unthinkclaw `ChatMessage` to an rx4 `Message`.
pub fn chat_message_to_rx4(msg: &ChatMessage) -> Message {
    let role = match msg.role.as_str() {
        "system" => Role::System,
        "user" => Role::User,
        "assistant" | "assistant_tool_use" => Role::Assistant,
        "tool_result" => Role::Tool,
        _ => Role::User,
    };
    Message {
        role,
        content: msg.content.clone(),
        tool_call_id: msg.tool_use_id.clone(),
    }
}

/// Convert an rx4 `Message` back to an unthinkclaw `ChatMessage`.
pub fn rx4_message_to_chat(msg: &Message) -> ChatMessage {
    let role = match msg.role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool_result",
    };
    ChatMessage {
        role: role.to_string(),
        content: msg.content.clone(),
        tool_use_id: msg.tool_call_id.clone(),
    }
}

/// Convert a slice of unthinkclaw `ChatMessage`s to rx4 `Message`s.
pub fn chat_messages_to_rx4(messages: &[ChatMessage]) -> Vec<Message> {
    messages.iter().map(chat_message_to_rx4).collect()
}

/// Convert unthinkclaw `ToolSpec`s to rx4 tool definitions (JSON array).
pub fn tool_specs_to_rx4_json(specs: &[ToolSpec]) -> Vec<serde_json::Value> {
    specs
        .iter()
        .map(|s| {
            serde_json::json!({
                "name": s.name,
                "description": s.description,
                "parameters": s.parameters,
            })
        })
        .collect()
}

// ── Provider adapter ─────────────────────────────────────────────────────

/// Adapter that wraps an unthinkclaw `Provider` and implements rx4's `Provider`
/// trait. This lets rx4's `Agent` loop use unthinkclaw's existing provider
/// backends (Anthropic, OpenAI-compat, Ollama, Copilot) without modification.
///
/// rx4's `Provider` trait is streaming-based (`stream()`), while unthinkclaw's
/// is request-response (`chat()`). This adapter bridges the gap by calling
/// unthinkclaw's `chat()` and wrapping the result in a single-element stream.
pub struct RotaryProviderAdapter {
    inner: Arc<dyn UnthinkclawProvider>,
    id: String,
    name: String,
}

impl RotaryProviderAdapter {
    pub fn new(provider: Arc<dyn UnthinkclawProvider>) -> Self {
        let id = provider.name().to_string();
        let name = format!("unthinkclaw-{}", provider.name());
        Self {
            inner: provider,
            id,
            name,
        }
    }
}

#[async_trait::async_trait]
impl Rx4Provider for RotaryProviderAdapter {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    async fn stream(
        &self,
        messages: &[Message],
        system: &Option<String>,
        model: &str,
        tools: &[serde_json::Value],
    ) -> Result<rx4::provider::StreamResult, Rx4ProviderError> {
        // Translate rx4 messages to unthinkclaw ChatMessages
        let mut chat_messages: Vec<ChatMessage> = Vec::new();

        // rx4 passes system prompt separately; unthinkclaw includes it in messages
        if let Some(sys) = system {
            chat_messages.push(ChatMessage::system(sys));
        }

        for msg in messages {
            chat_messages.push(rx4_message_to_chat(msg));
        }

        // Convert rx4 tool definitions to unthinkclaw ToolSpecs
        let tool_specs: Vec<ToolSpec> = tools
            .iter()
            .filter_map(|t| {
                let name = t.get("name")?.as_str()?.to_string();
                let description = t
                    .get("description")
                    .and_then(|d| d.as_str())
                    .unwrap_or("")
                    .to_string();
                let parameters = t.get("parameters").cloned().unwrap_or(serde_json::Value::Null);
                Some(ToolSpec {
                    name,
                    description,
                    parameters,
                })
            })
            .collect();

        let tool_refs: &[ToolSpec] = if tool_specs.is_empty() {
            &[]
        } else {
            // Safety: tool_specs lives for the duration of this call
            // This is a workaround for the lifetime constraint in ChatRequest
            &tool_specs
        };

        let request = ChatRequest {
            messages: &chat_messages,
            tools: if tool_refs.is_empty() {
                None
            } else {
                Some(tool_refs)
            },
            model,
            temperature: 0.7,
            max_tokens: Some(8192),
        };

        let response = self
            .inner
            .chat(&request)
            .await
            .map_err(|e| Rx4ProviderError::Api(e.to_string()))?;

        // Build a stream that emits the response as events
        let text = response.text.unwrap_or_default();
        let tool_calls = response.tool_calls;

        // Create a single-shot stream
        let events: Vec<Result<StreamEvent, Rx4ProviderError>> = {
            let mut evs = Vec::new();
            if !text.is_empty() {
                evs.push(Ok(StreamEvent::Delta(text)));
            }
            for tc in tool_calls {
                evs.push(Ok(StreamEvent::ToolCall(rx4::ToolCall {
                    id: tc.id,
                    name: tc.name,
                    arguments: tc.arguments,
                })));
            }
            evs.push(Ok(StreamEvent::Done));
            evs
        };

        // Return a stream that yields the pre-computed events
        use futures_util::stream;
        Ok(Box::new(Box::pin(stream::iter(events))))
    }
}

// ── Tool registration ────────────────────────────────────────────────────

/// Register unthinkclaw's `Tool` trait objects into rx4's `ToolRegistry`.
///
/// Each unthinkclaw tool is wrapped in a boxed closure that captures the
/// `Arc<dyn Tool>` and calls its `execute()` method. The closure is registered
/// via `ToolDefinition::new_boxed()`, which uses `ToolExecutor::Boxed`.
///
/// Tool effects are classified based on the tool name using rx4's
/// `classify_tool()` guardrail function — idempotent tools get `ToolEffect::Read`,
/// mutating tools get `ToolEffect::Write`.
pub fn register_unthinkclaw_tools(
    registry: &mut rx4::ToolRegistry,
    tools: &[Arc<dyn UnthinkclawTool>],
) {
    use rx4::guardrails::classify_tool;
    use rx4::{ToolDefinition, ToolEffect, ToolExecuteBox};

    for tool in tools {
        let spec = tool.spec();
        let name = spec.name.clone();
        let description = spec.description.clone();
        let parameters_json = serde_json::to_string(&spec.parameters).unwrap_or_default();

        let tool_clone = Arc::clone(tool);
        let execute: ToolExecuteBox = Box::new(move |_ctx, args| {
            let tool = Arc::clone(&tool_clone);
            Box::pin(async move {
                match tool.execute(&args).await {
                    Ok(result) => rx4::ToolResult {
                        id: String::new(),
                        content: result.output,
                        is_error: result.is_error,
                    },
                    Err(e) => rx4::ToolResult {
                        id: String::new(),
                        content: format!("Tool error: {e}"),
                        is_error: true,
                    },
                }
            })
        });

        let effect = match classify_tool(&name) {
            rx4::guardrails::ToolClass::Idempotent => ToolEffect::Read,
            rx4::guardrails::ToolClass::Mutating => ToolEffect::Write,
        };

        registry.register(
            ToolDefinition::new_boxed(name, description, parameters_json, execute)
                .with_effect(effect),
        );
    }
}

// ── Agent bridge ─────────────────────────────────────────────────────────

/// Configuration for building a `RotaryAgentBridge`.
pub struct RotaryBridgeConfig {
    pub provider: Arc<dyn UnthinkclawProvider>,
    pub tools: Vec<Arc<dyn UnthinkclawTool>>,
    pub system_prompt: String,
    pub model: String,
    pub workspace: std::path::PathBuf,
    pub max_tool_iterations: usize,
}

/// Bridge that wraps an `rx4::Agent` and provides a simplified interface for
/// unthinkclaw's outer shell to use.
///
/// The bridge handles:
/// - Creating and configuring the rx4::Agent (provider, tools, system prompt)
/// - Translating messages between unthinkclaw and rx4 types
/// - Running prompts through rx4's agent loop
///
/// Unthinkclaw's unique features (channels, swarm, cron, heartbeat, autonomous
/// mode, plugins) remain in the outer shell and call `run_prompt()` on this
/// bridge to execute agent turns.
pub struct RotaryAgentBridge {
    agent: rx4::Agent,
    /// Conversation messages maintained in rx4 format (per-session)
    messages: Vec<Message>,
}

impl RotaryAgentBridge {
    /// Build a new bridge from the given configuration.
    pub fn new(config: RotaryBridgeConfig) -> Self {
        let rx4_provider = Arc::new(RotaryProviderAdapter::new(config.provider));

        let mut agent = rx4::Agent::new();
        agent.set_model(&config.model);
        agent.set_system_prompt(&config.system_prompt);
        agent.set_provider(rx4_provider);
        agent.set_workspace_root(&config.workspace);
        agent.max_tool_iterations = config.max_tool_iterations;

        // Register unthinkclaw's tools into rx4's tool registry
        register_unthinkclaw_tools(&mut agent.tools, &config.tools);

        // Use rx4's guardrails for loop detection (replaces unthinkclaw's
        // ToolGuardrails in the main loop path)
        // rx4's Agent already has built-in tool caching and effect classification

        Self {
            agent,
            messages: Vec::new(),
        }
    }

    /// Get a reference to the inner rx4::Agent (for advanced configuration).
    pub fn agent(&self) -> &rx4::Agent {
        &self.agent
    }

    /// Get a mutable reference to the inner rx4::Agent.
    pub fn agent_mut(&mut self) -> &mut rx4::Agent {
        &mut self.agent
    }

    /// Clear the conversation history.
    pub fn clear_messages(&mut self) {
        self.messages.clear();
        self.agent.clear_messages();
    }

    /// Get the number of messages in the conversation.
    pub fn message_count(&self) -> usize {
        self.messages.len()
    }

    /// Set the model for the agent.
    pub fn set_model(&mut self, model: &str) {
        self.agent.set_model(model);
    }

    /// Set the system prompt.
    pub fn set_system_prompt(&mut self, prompt: &str) {
        self.agent.set_system_prompt(prompt);
    }

    /// Set the workspace root.
    pub fn set_workspace_root(&mut self, path: &std::path::Path) {
        self.agent.set_workspace_root(path);
    }

    /// Set the scope (e.g., Coding, Research, Ask).
    pub fn set_scope(&mut self, scope: rx4::Scope) {
        self.agent.set_scope(scope);
    }

    /// Add a subscriber to receive agent events (tool calls, deltas, etc.).
    pub fn subscribe(&mut self, callback: impl Fn(&rx4::Event) + Send + Sync + 'static) {
        self.agent.subscribe(callback);
    }

    /// Run a single user prompt through the rx4 agent loop.
    ///
    /// This delegates the core agent loop (LLM calls, tool execution, turn
    /// cycling) to rx4::Agent. The caller (unthinkclaw's channel/swarm/cron
    /// shell) is responsible for:
    /// - Receiving the user message from a channel
    /// - Calling this method with the prompt text
    /// - Sending the final response back through the channel
    ///
    /// Returns the final assistant response text.
    pub async fn run_prompt(&mut self, prompt: &str) -> anyhow::Result<String> {
        // Track the last assistant message for the return value
        let last_response = Arc::new(parking_lot::RwLock::new(String::new()));
        let last_response_clone = Arc::clone(&last_response);

        self.agent.subscribe(move |event| {
            if let rx4::Event::MessageEnd {
                content,
                role: Role::Assistant,
            } = event
            {
                *last_response_clone.write() = content.clone();
            }
        });

        self.agent.prompt(prompt).await?;

        let response = last_response.read().clone();
        Ok(response)
    }

    /// Run a prompt with pre-loaded conversation history.
    ///
    /// The history is loaded into rx4's message buffer before running the
    /// prompt. This is used when unthinkclaw's memory backend provides
    /// conversation history for a chat session.
    pub async fn run_prompt_with_history(
        &mut self,
        prompt: &str,
        history: &[ChatMessage],
    ) -> anyhow::Result<String> {
        // Load history into rx4's message buffer
        self.agent.clear_messages();
        for msg in history {
            let rx4_msg = chat_message_to_rx4(msg);
            // rx4's messages are stored internally; we push them via the
            // messages RwLock
            self.agent.messages.write().push(rx4_msg);
        }

        self.run_prompt(prompt).await
    }

    /// Register additional tools at runtime.
    pub fn register_tools(&mut self, tools: &[Arc<dyn UnthinkclawTool>]) {
        register_unthinkclaw_tools(&mut self.agent.tools, tools);
    }

    /// Get the list of registered tool names.
    pub fn list_tools(&self) -> Vec<String> {
        self.agent
            .tools
            .definitions()
            .iter()
            .filter_map(|d| d.get("name").and_then(|n| n.as_str()).map(|s| s.to_string()))
            .collect()
    }

    /// Compact the conversation context (delegates to rx4's compact).
    pub fn compact(&mut self, reason: &str) {
        self.agent.compact(reason);
    }
}

// ── Skill bridge ─────────────────────────────────────────────────────────

/// Build an `rx4::SkillEngine` configured with unthinkclaw's skill directories.
///
/// Unthinkclaw discovers skills from 3 directories:
/// 1. `~/.npm-global/lib/node_modules/openclaw/skills` (legacy)
/// 2. `~/.openclaw/workspace/skills` (shared workspace skills)
/// 3. `{workspace}/.unthinkclaw/skills` (project-local managed skills)
///
/// This maps to rx4's `SkillEngine` with the primary dir set to the managed
/// skills directory and the other two as `extra_dirs`.
///
/// After calling this, use `engine.load()` to populate skills from disk,
/// then `engine.search()` for keyword matching (replaces unthinkclaw's
/// `match_skill()`).
///
/// Note: unthinkclaw's template variable substitution and inline shell
/// preprocessing (`preprocess_skill_content`) are not part of rx4's
/// SkillEngine and remain in unthinkclaw's `skills` module. Use
/// `skills::preprocess_skill_content()` on the matched skill's instructions
/// before injecting into the system prompt.
pub fn build_rx4_skill_engine(workspace: &std::path::Path) -> rx4::SkillEngine {
    let home = dirs::home_dir().unwrap_or_default();

    // Primary dir: managed skills in the workspace
    let managed_dir = workspace.join(".unthinkclaw/skills");

    let mut engine = rx4::SkillEngine::new(managed_dir);

    // Extra dirs: legacy openclaw skills and shared workspace skills
    let openclaw_skills = home.join(".npm-global/lib/node_modules/openclaw/skills");
    engine.add_extra_dir(openclaw_skills);

    let shared_skills = home.join(".openclaw/workspace/skills");
    engine.add_extra_dir(shared_skills);

    engine
}

/// Match a skill using rx4's SkillEngine keyword search.
///
/// This replaces unthinkclaw's `skills::match_skill()` when using the rx4
/// bridge path. Returns the best-matching skill's name and instructions
/// (raw, unpreprocessed).
///
/// The caller should preprocess the instructions using
/// `unthinkclaw::skills::preprocess_skill_content()` before injecting
/// into the system prompt, as rx4's SkillEngine does not perform template
/// variable substitution or inline shell expansion.
pub fn match_skill_via_rx4(
    engine: &rx4::SkillEngine,
    user_message: &str,
) -> Option<(String, String)> {
    let results = engine.search(user_message);
    if results.is_empty() {
        return None;
    }

    // Pick the first result (rx4's search returns matches sorted by relevance)
    let skill = results[0];
    Some((skill.name.clone(), skill.instructions.clone()))
}

/// Discover skills using rx4's SkillEngine, returning unthinkclaw-compatible
/// Skill structs for backward compatibility with existing code that expects
/// the `Vec<skills::Skill>` type.
///
/// This loads skills from disk via rx4's SkillEngine (which handles both
/// JSON and SKILL.md formats with YAML frontmatter), then converts them to
/// unthinkclaw's Skill type.
pub fn discover_skills_via_rx4(
    workspace: &std::path::Path,
) -> Vec<crate::skills::Skill> {
    let mut engine = build_rx4_skill_engine(workspace);
    if engine.load().is_err() {
        tracing::warn!("rx4 SkillEngine load failed, returning empty skill list");
        return Vec::new();
    }

    engine
        .list()
        .into_iter()
        .map(|rx4_skill| {
            let location = engine
                .skills_dir()
                .join(format!("{}.json", rx4_skill.id));
            crate::skills::Skill {
                name: rx4_skill.name.clone(),
                description: rx4_skill.description.clone(),
                location,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chat_message_to_rx4_system() {
        let msg = ChatMessage::system("hello");
        let rx4_msg = chat_message_to_rx4(&msg);
        assert_eq!(rx4_msg.role, Role::System);
        assert_eq!(rx4_msg.content, "hello");
    }

    #[test]
    fn test_chat_message_to_rx4_user() {
        let msg = ChatMessage::user("test");
        let rx4_msg = chat_message_to_rx4(&msg);
        assert_eq!(rx4_msg.role, Role::User);
        assert_eq!(rx4_msg.content, "test");
    }

    #[test]
    fn test_chat_message_to_rx4_tool_result() {
        let msg = ChatMessage::tool_result("tc_123", "result text");
        let rx4_msg = chat_message_to_rx4(&msg);
        assert_eq!(rx4_msg.role, Role::Tool);
        assert_eq!(rx4_msg.content, "result text");
        assert_eq!(rx4_msg.tool_call_id.as_deref(), Some("tc_123"));
    }

    #[test]
    fn test_rx4_message_to_chat() {
        let msg = Message::assistant("hello back");
        let chat_msg = rx4_message_to_chat(&msg);
        assert_eq!(chat_msg.role, "assistant");
        assert_eq!(chat_msg.content, "hello back");
    }

    #[test]
    fn test_tool_specs_to_rx4_json() {
        let specs = vec![ToolSpec {
            name: "shell".to_string(),
            description: "Run shell commands".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        }];
        let json = tool_specs_to_rx4_json(&specs);
        assert_eq!(json.len(), 1);
        assert_eq!(json[0]["name"], "shell");
    }

    #[test]
    fn test_roundtrip_translation() {
        let original = ChatMessage::user("roundtrip test");
        let rx4_msg = chat_message_to_rx4(&original);
        let back = rx4_message_to_chat(&rx4_msg);
        assert_eq!(back.role, "user");
        assert_eq!(back.content, "roundtrip test");
    }

    #[test]
    fn test_build_rx4_skill_engine() {
        // Just verify it doesn't panic with a temp dir
        let tmp = tempfile::tempdir().unwrap();
        let engine = build_rx4_skill_engine(tmp.path());
        assert!(engine.skills_dir().exists() || engine.skills_dir() == tmp.path().join(".unthinkclaw/skills"));
    }

    #[test]
    fn test_match_skill_via_rx4_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let mut engine = build_rx4_skill_engine(tmp.path());
        let _ = engine.load();
        // No skills in empty dir, should return None
        let result = match_skill_via_rx4(&engine, "test query");
        assert!(result.is_none());
    }
}
