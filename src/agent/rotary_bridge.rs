//! Rotary (rx4) bridge — adapts apollo's types to rx4's agent harness.
//!
//! This module provides:
//! - `RotaryProviderAdapter`: wraps an apollo `Provider` as an `rx4::Provider`
//!   so rx4's `Agent` loop can use apollo's existing provider backends.
//! - `register_apollo_tools`: registers apollo's `Tool` trait objects
//!   into rx4's `ToolRegistry` via boxed closures.
//! - `chat_message_to_rx4` / `rx4_message_to_chat`: type translators between
//!   apollo's `ChatMessage` and rx4's `Message`.
//! - `RotaryAgentBridge`: wraps an `rx4::Agent`, wiring up provider, tools,
//!   system prompt, and providing a `run_prompt` method that the outer
//!   apollo shell (channels, swarm, cron, heartbeat) can call.
//!
//! The bridge delegates the core agent loop to rx4 while keeping apollo's
//! unique features (channels, swarm, cron, heartbeat, autonomous mode,
//! plugins, MCP) as the outer shell.

use std::sync::Arc;

use rx4::provider::{
    Message, Provider as Rx4Provider, ProviderError as Rx4ProviderError, Role, StreamEvent,
};
use rx4::{RecoveryAction, RecoveryKind, SpillStatus};

use crate::agent::hooks::{run_post_hooks, run_pre_hooks, HookDecision, ToolHook};
use crate::agent::stream::{emit, AgentStreamEvent, AgentStreamTx};
use crate::cost::{ContextSnapshot, CostTracker, TokenUsage};
use crate::plugin::{HookManager, LifecycleEvent, PluginRegistry};
use crate::providers::{ChatMessage, ChatRequest, Provider as UnthinkclawProvider};
use crate::tools::pty_worker::PtyWorker;
use crate::tools::{Tool as UnthinkclawTool, ToolResult as UnthinkclawToolResult, ToolSpec};
use crate::trajectory::{Trajectory, TrajectoryStep};

pub fn runtime_pty_worker(tools: &[Arc<dyn UnthinkclawTool>]) -> Arc<PtyWorker> {
    tools
        .iter()
        .find_map(|tool| tool.pty_worker())
        .unwrap_or_else(|| Arc::new(PtyWorker::new()))
}

/// Everything a tool call must be wrapped in.
///
/// The same sequence runs around every tool: the `BeforeToolCall` lifecycle
/// event, a `ToolStart` stream event, plugin then policy pre-checks,
/// execution, the post hooks, the `AfterToolCall` lifecycle event, plugin
/// notification, and a `ToolEnd` stream event. That sequence lives once, in
/// `execute_tool_with_hooks`; this type carries the collaborators it needs.
#[derive(Clone, Default)]
pub struct ToolHookContext {
    hooks: Vec<Arc<dyn ToolHook>>,
    plugins: Option<Arc<tokio::sync::RwLock<PluginRegistry>>>,
    hook_manager: Option<Arc<HookManager>>,
    stream: Option<AgentStreamTx>,
}

impl ToolHookContext {
    pub fn new(
        hooks: Vec<Arc<dyn ToolHook>>,
        plugins: Option<Arc<tokio::sync::RwLock<PluginRegistry>>>,
    ) -> Self {
        Self {
            hooks,
            plugins,
            hook_manager: None,
            stream: None,
        }
    }

    /// Attach the lifecycle hook manager, so plugins observing tool calls see
    /// them under either engine.
    pub fn with_hook_manager(mut self, hook_manager: Arc<HookManager>) -> Self {
        self.hook_manager = Some(hook_manager);
        self
    }

    /// Attach the turn's stream sink, so a WS client sees tool progress under
    /// either engine.
    pub fn with_stream(mut self, stream: Option<AgentStreamTx>) -> Self {
        self.stream = stream;
        self
    }

    async fn emit_lifecycle(&self, event: LifecycleEvent) {
        if let Some(manager) = &self.hook_manager {
            manager.emit(&event).await;
        }
    }

    /// Run the pre-tool checks. `Block` means the tool must not execute.
    pub async fn check_pre_tool(&self, name: &str, arguments: &str) -> HookDecision {
        if let Some(plugins) = &self.plugins {
            let registry = plugins.read().await;
            if let HookDecision::Block(reason) = registry.check_pre_tool(name, arguments).await {
                return HookDecision::Block(format!("Blocked by plugin: {reason}"));
            }
        }
        match run_pre_hooks(&self.hooks, name, arguments).await {
            HookDecision::Block(reason) => {
                HookDecision::Block(format!("Blocked by policy: {reason}"))
            }
            HookDecision::Allow => HookDecision::Allow,
        }
    }

    /// Notify the post-tool hooks and plugins.
    pub async fn notify_post_tool(
        &self,
        name: &str,
        arguments: &str,
        result: &UnthinkclawToolResult,
    ) {
        run_post_hooks(&self.hooks, name, arguments, result).await;
        self.emit_lifecycle(LifecycleEvent::AfterToolCall(
            name.to_string(),
            arguments.to_string(),
            result.clone(),
        ))
        .await;
        if let Some(plugins) = &self.plugins {
            let registry = plugins.read().await;
            registry.notify_post_tool(name, arguments, result).await;
        }
    }
}

/// Run one tool call with every hook and event both engines owe it.
///
/// This is the single place the ordering exists. `tool` is `None` when the
/// model named a tool that is not registered; the pre-checks still run, so a
/// policy that blocks an unknown name is honoured before that is reported.
pub async fn execute_tool_with_hooks(
    ctx: &ToolHookContext,
    name: &str,
    arguments: &str,
    tool: Option<&Arc<dyn UnthinkclawTool>>,
) -> UnthinkclawToolResult {
    ctx.emit_lifecycle(LifecycleEvent::BeforeToolCall(
        name.to_string(),
        arguments.to_string(),
    ))
    .await;
    emit(
        &ctx.stream,
        AgentStreamEvent::ToolStart {
            name: name.to_string(),
            hint: crate::agent::loop_runner::extract_tool_hint(name, arguments),
        },
    );

    let started = std::time::Instant::now();
    let result = match ctx.check_pre_tool(name, arguments).await {
        HookDecision::Block(reason) => {
            tracing::info!("blocked '{}': {}", name, reason);
            UnthinkclawToolResult::error(reason)
        }
        HookDecision::Allow => match tool {
            Some(tool) => match tool.execute(arguments).await {
                Ok(result) => result,
                Err(e) => UnthinkclawToolResult::error(crate::redaction::redact_text(&format!(
                    "Tool error: {e}"
                ))),
            },
            None => UnthinkclawToolResult::error(format!("Unknown tool: {name}")),
        },
    };

    ctx.notify_post_tool(name, arguments, &result).await;

    emit(
        &ctx.stream,
        AgentStreamEvent::ToolEnd {
            name: name.to_string(),
            ok: !result.is_error,
            elapsed_secs: started.elapsed().as_secs(),
        },
    );

    result
}

#[derive(Debug, Default)]
pub struct Rx4TrajectoryRecorder {
    pending: Option<(String, String)>,
    steps: Vec<TrajectoryStep>,
    iterations: usize,
}

impl Rx4TrajectoryRecorder {
    pub fn on_event(&mut self, event: &rx4::Event) {
        record_rx4_event(self, event);
    }

    pub fn take_steps(&mut self) -> (Vec<TrajectoryStep>, usize) {
        (std::mem::take(&mut self.steps), self.iterations)
    }
}

pub fn record_rx4_event(recorder: &mut Rx4TrajectoryRecorder, event: &rx4::Event) {
    match event {
        rx4::Event::TurnStart { turn } => {
            recorder.iterations = *turn;
        }
        rx4::Event::ToolExecutionStart(call) => {
            recorder.pending = Some((call.name.clone(), call.arguments.clone()));
        }
        rx4::Event::ToolExecutionEnd(result) => {
            let (action, action_args) = recorder
                .pending
                .take()
                .unwrap_or_else(|| ("tool".to_string(), String::new()));
            recorder.steps.push(TrajectoryStep {
                step: recorder.steps.len() + 1,
                thought: None,
                action: Some(action),
                action_args: Some(action_args),
                observation: Some(result.content.clone()),
                response: None,
                success: !result.is_error,
            });
            if result.spill.is_none() {
                if let Some(locator) = spill_locator(&result.content) {
                    record_spill_notice(recorder, locator);
                }
            }
        }
        rx4::Event::GuardrailWarning { tool, reason }
        | rx4::Event::GuardrailStop { tool, reason } => {
            recorder.steps.push(TrajectoryStep {
                step: recorder.steps.len() + 1,
                thought: None,
                action: Some(tool.clone()),
                action_args: None,
                observation: Some(reason.clone()),
                response: None,
                success: false,
            });
        }
        rx4::Event::Error(message) => {
            recorder.steps.push(TrajectoryStep {
                step: recorder.steps.len() + 1,
                thought: None,
                action: Some("error".to_string()),
                action_args: None,
                observation: Some(message.clone()),
                response: None,
                success: false,
            });
        }
        rx4::Event::BudgetExceeded { reason } => {
            recorder.steps.push(TrajectoryStep {
                step: recorder.steps.len() + 1,
                thought: None,
                action: Some("budget".to_string()),
                action_args: None,
                observation: Some(reason.clone()),
                response: None,
                success: false,
            });
        }
        rx4::Event::RetryReason {
            retry_reason,
            layer,
        } => {
            recorder.steps.push(TrajectoryStep {
                step: recorder.steps.len() + 1,
                thought: None,
                action: Some("retry".to_string()),
                action_args: Some(layer.clone()),
                observation: Some(retry_reason.clone()),
                response: None,
                success: false,
            });
        }
        rx4::Event::Recovery { action, reason } => {
            record_recovery_kind(recorder, *action, reason);
        }
        rx4::Event::ToolSpill {
            status,
            locator,
            original_bytes,
        } => {
            record_tool_spill(recorder, *status, locator, *original_bytes);
        }
        rx4::Event::ProcessStart {
            process_id,
            program,
        } => {
            recorder.steps.push(TrajectoryStep {
                step: recorder.steps.len() + 1,
                thought: None,
                action: Some("process_start".to_string()),
                action_args: Some(process_id.clone()),
                observation: Some(program.clone()),
                response: None,
                success: true,
            });
        }
        rx4::Event::ProcessEnd {
            process_id,
            exit_code,
        } => {
            recorder.steps.push(TrajectoryStep {
                step: recorder.steps.len() + 1,
                thought: None,
                action: Some("process_end".to_string()),
                action_args: Some(process_id.clone()),
                observation: Some(exit_code.map(|code| code.to_string()).unwrap_or_default()),
                response: None,
                success: !matches!(exit_code, Some(code) if *code != 0),
            });
        }
        rx4::Event::ProcessStdin { process_id, bytes } => {
            recorder.steps.push(TrajectoryStep {
                step: recorder.steps.len() + 1,
                thought: None,
                action: Some("stdin".to_string()),
                action_args: Some(process_id.clone()),
                observation: Some(bytes.to_string()),
                response: None,
                success: true,
            });
        }
        rx4::Event::RequestPermissions { tool, paths } => {
            recorder.steps.push(TrajectoryStep {
                step: recorder.steps.len() + 1,
                thought: None,
                action: Some("permissions".to_string()),
                action_args: Some(tool.clone()),
                observation: Some(paths.join(",")),
                response: None,
                success: true,
            });
        }
        rx4::Event::PatchHunk { path, hunk } => {
            recorder.steps.push(TrajectoryStep {
                step: recorder.steps.len() + 1,
                thought: None,
                action: Some("patch".to_string()),
                action_args: Some(path.clone()),
                observation: Some(hunk.clone()),
                response: None,
                success: true,
            });
        }
        rx4::Event::SelfHealing {
            attempt,
            max_attempts,
            errors,
        } => {
            recorder.steps.push(TrajectoryStep {
                step: recorder.steps.len() + 1,
                thought: None,
                action: Some("recovery".to_string()),
                action_args: Some(format!("{attempt}/{max_attempts}")),
                observation: Some(errors.join("; ")),
                response: None,
                success: false,
            });
        }
        _ => {}
    }
}

pub fn record_recovery_action(
    recorder: &mut Rx4TrajectoryRecorder,
    action: &RecoveryAction,
    source: &str,
) {
    let stuck = source == "stuck_tool";
    let (name, args, observation, success) = match action {
        RecoveryAction::Prefill(text) => {
            if stuck {
                (
                    "stuck_tool",
                    Some("prefill".to_string()),
                    Some(text.clone()),
                    true,
                )
            } else {
                (
                    "prefill",
                    Some(source.to_string()),
                    Some(text.clone()),
                    true,
                )
            }
        }
        RecoveryAction::Nudge(text) => {
            if stuck {
                (
                    "stuck_tool",
                    Some("nudge".to_string()),
                    Some(text.clone()),
                    true,
                )
            } else {
                ("nudge", Some(source.to_string()), Some(text.clone()), true)
            }
        }
        RecoveryAction::Retry => {
            if stuck {
                ("stuck_tool", Some("retry".to_string()), None, false)
            } else {
                ("retry", Some(source.to_string()), None, false)
            }
        }
        RecoveryAction::Halt(reason) => {
            if stuck {
                (
                    "stuck_tool",
                    Some("halt".to_string()),
                    Some(reason.clone()),
                    false,
                )
            } else {
                (
                    "halt",
                    Some(source.to_string()),
                    Some(reason.clone()),
                    false,
                )
            }
        }
    };
    recorder.steps.push(TrajectoryStep {
        step: recorder.steps.len() + 1,
        thought: None,
        action: Some(name.to_string()),
        action_args: args,
        observation,
        response: None,
        success,
    });
}

pub fn record_recovery_kind(
    recorder: &mut Rx4TrajectoryRecorder,
    action: RecoveryKind,
    reason: &str,
) {
    let (name, success) = match action {
        RecoveryKind::Prefill => ("prefill", true),
        RecoveryKind::Nudge => ("nudge", true),
        RecoveryKind::Retry => ("retry", false),
        RecoveryKind::Halt => ("halt", false),
    };
    recorder.steps.push(TrajectoryStep {
        step: recorder.steps.len() + 1,
        thought: None,
        action: Some(name.to_string()),
        action_args: None,
        observation: (!reason.is_empty()).then(|| reason.to_string()),
        response: None,
        success,
    });
}

pub fn record_tool_spill(
    recorder: &mut Rx4TrajectoryRecorder,
    status: SpillStatus,
    locator: impl Into<String>,
    original_bytes: usize,
) {
    let (args, success) = match status {
        SpillStatus::Inline => ("inline", true),
        SpillStatus::Spilled => ("spilled", true),
        SpillStatus::SpillFailed => ("spill_failed", false),
    };
    recorder.steps.push(TrajectoryStep {
        step: recorder.steps.len() + 1,
        thought: None,
        action: Some("spill".to_string()),
        action_args: Some(format!("{args}:{original_bytes}")),
        observation: Some(locator.into()),
        response: None,
        success,
    });
}

pub fn record_spill_notice(recorder: &mut Rx4TrajectoryRecorder, locator: impl Into<String>) {
    recorder.steps.push(TrajectoryStep {
        step: recorder.steps.len() + 1,
        thought: None,
        action: Some("spill".to_string()),
        action_args: None,
        observation: Some(locator.into()),
        response: None,
        success: true,
    });
}

pub fn record_failure_notice(recorder: &mut Rx4TrajectoryRecorder, message: impl Into<String>) {
    recorder.steps.push(TrajectoryStep {
        step: recorder.steps.len() + 1,
        thought: None,
        action: Some("failure".to_string()),
        action_args: None,
        observation: Some(message.into()),
        response: None,
        success: false,
    });
}

pub fn record_rx4_event_value(recorder: &mut Rx4TrajectoryRecorder, value: &serde_json::Value) {
    let Some(ty) = value.get("type").and_then(|v| v.as_str()) else {
        return;
    };
    match ty {
        "Prefill" => {
            record_recovery_action(
                recorder,
                &RecoveryAction::Prefill(json_text(value, &["text", "message"])),
                recovery_source(value),
            );
        }
        "Nudge" => {
            record_recovery_action(
                recorder,
                &RecoveryAction::Nudge(json_text(value, &["text", "message"])),
                recovery_source(value),
            );
        }
        "StuckTool" | "StuckToolRecovery" => {
            record_recovery_action(recorder, &parse_recovery_action_field(value), "stuck_tool");
        }
        "Recovery" => {
            let reason = json_text(value, &["reason", "text", "message"]);
            if let Some(kind) = value
                .get("action")
                .and_then(|v| v.as_str())
                .and_then(recovery_kind_from_name)
            {
                record_recovery_kind(recorder, kind, &reason);
            } else {
                record_recovery_action(
                    recorder,
                    &parse_recovery_action_field(value),
                    recovery_source(value),
                );
            }
        }
        "Spill" | "SpillNotice" | "ToolSpill" => {
            if let Some(status) = value.get("status").and_then(|v| v.as_str()) {
                let status = match status {
                    "inline" => SpillStatus::Inline,
                    "spill_failed" => SpillStatus::SpillFailed,
                    _ => SpillStatus::Spilled,
                };
                let original_bytes = value
                    .get("original_bytes")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as usize;
                record_tool_spill(
                    recorder,
                    status,
                    json_text(value, &["locator", "path", "observation"]),
                    original_bytes,
                );
            } else {
                record_spill_notice(
                    recorder,
                    json_text(value, &["locator", "path", "observation"]),
                );
            }
        }
        "Failure" | "FailureNotice" | "ToolFailure" => {
            record_failure_notice(recorder, json_text(value, &["message", "reason", "error"]));
        }
        _ => {}
    }
}

fn recovery_source(value: &serde_json::Value) -> &'static str {
    let source = value
        .get("source")
        .and_then(|v| v.as_str())
        .or_else(|| value.get("kind").and_then(|v| v.as_str()))
        .unwrap_or("empty_turn");
    if source.eq_ignore_ascii_case("stuck_tool") || source.eq_ignore_ascii_case("stuck-tool") {
        "stuck_tool"
    } else {
        "empty_turn"
    }
}

fn json_text(value: &serde_json::Value, keys: &[&str]) -> String {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(|v| v.as_str()))
        .unwrap_or("")
        .to_string()
}

fn parse_recovery_action_field(value: &serde_json::Value) -> RecoveryAction {
    let text = json_text(value, &["text", "reason", "message"]);
    if let Some(action) = value.get("action") {
        if let Some(name) = action.as_str() {
            return recovery_action_from_name(name, text);
        }
        if let Some(obj) = action.as_object() {
            if let Some(prefill) = obj.get("Prefill").and_then(|v| v.as_str()) {
                return RecoveryAction::Prefill(prefill.to_string());
            }
            if let Some(nudge) = obj.get("Nudge").and_then(|v| v.as_str()) {
                return RecoveryAction::Nudge(nudge.to_string());
            }
            if obj.contains_key("Retry") {
                return RecoveryAction::Retry;
            }
            if let Some(halt) = obj.get("Halt").and_then(|v| v.as_str()) {
                return RecoveryAction::Halt(halt.to_string());
            }
            if let Some(ty) = obj.get("type").and_then(|v| v.as_str()) {
                let inner = json_text(&serde_json::Value::Object(obj.clone()), &["text", "reason"]);
                return recovery_action_from_name(ty, inner);
            }
        }
    }
    recovery_action_from_name(
        value
            .get("recovery")
            .and_then(|v| v.as_str())
            .unwrap_or("Nudge"),
        text,
    )
}

fn recovery_action_from_name(name: &str, text: String) -> RecoveryAction {
    match name {
        "Prefill" | "prefill" => RecoveryAction::Prefill(text),
        "Nudge" | "nudge" => RecoveryAction::Nudge(text),
        "Retry" | "retry" => RecoveryAction::Retry,
        "Halt" | "halt" => RecoveryAction::Halt(text),
        _ => RecoveryAction::Nudge(text),
    }
}

fn recovery_kind_from_name(name: &str) -> Option<RecoveryKind> {
    match name {
        "Prefill" | "prefill" => Some(RecoveryKind::Prefill),
        "Nudge" | "nudge" => Some(RecoveryKind::Nudge),
        "Retry" | "retry" => Some(RecoveryKind::Retry),
        "Halt" | "halt" => Some(RecoveryKind::Halt),
        _ => None,
    }
}

fn spill_locator(content: &str) -> Option<&str> {
    let marker = "[truncated, full output at ";
    let start = content.find(marker)? + marker.len();
    let rest = content.get(start..)?;
    let end = rest.find(']')?;
    let locator = rest[..end].trim();
    (!locator.is_empty()).then_some(locator)
}

pub fn apply_recorded_steps(trajectory: &mut Trajectory, recorder: &mut Rx4TrajectoryRecorder) {
    let (steps, iterations) = recorder.take_steps();
    trajectory.absorb_recorded_steps(steps, iterations);
}

// ── Message translation ──────────────────────────────────────────────────

/// Convert an apollo `ChatMessage` to an rx4 `Message`.
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
        tool_calls: Vec::new(),
    }
}

/// Convert an rx4 `Message` back to an apollo `ChatMessage`.
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

// ── Provider adapter ─────────────────────────────────────────────────────

/// Adapter that wraps an apollo `Provider` and implements rx4's `Provider`
/// trait. This lets rx4's `Agent` loop use apollo's existing provider
/// backends (Anthropic, OpenAI-compat, Ollama, Copilot) without modification.
///
/// rx4's `Provider` trait is streaming-based (`stream()`), while apollo's
/// is request-response (`chat()`). This adapter bridges the gap by calling
/// apollo's `chat()` and wrapping the result in a single-element stream.
pub struct RotaryProviderAdapter {
    inner: Arc<dyn UnthinkclawProvider>,
    id: String,
    name: String,
    cost_tracker: Option<Arc<CostTracker>>,
}

impl RotaryProviderAdapter {
    pub fn new(
        provider: Arc<dyn UnthinkclawProvider>,
        cost_tracker: Option<Arc<CostTracker>>,
    ) -> Self {
        let id = provider.name().to_string();
        let name = format!("apollo-{}", provider.name());
        Self {
            inner: provider,
            id,
            name,
            cost_tracker,
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
        _reasoning_effort: Option<&str>,
    ) -> Result<rx4::provider::StreamResult, Rx4ProviderError> {
        // Translate rx4 messages to apollo ChatMessages
        let mut chat_messages: Vec<ChatMessage> = Vec::new();

        // rx4 passes system prompt separately; apollo includes it in messages
        if let Some(sys) = system {
            chat_messages.push(ChatMessage::system(sys));
        }

        for msg in messages {
            chat_messages.push(rx4_message_to_chat(msg));
        }

        // Convert rx4 tool definitions to apollo ToolSpecs
        let tool_specs: Vec<ToolSpec> = tools
            .iter()
            .filter_map(|t| {
                let name = t.get("name")?.as_str()?.to_string();
                let description = t
                    .get("description")
                    .and_then(|d| d.as_str())
                    .unwrap_or("")
                    .to_string();
                let parameters = t
                    .get("parameters")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
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

        if let Some(tracker) = &self.cost_tracker {
            let system_chars = system
                .as_ref()
                .map(|value| value.chars().count())
                .unwrap_or(0);
            let history_chars = messages
                .iter()
                .map(|message| message.content.chars().count())
                .sum::<usize>();
            let tool_chars = tools
                .iter()
                .map(|tool| {
                    serde_json::to_string(tool)
                        .unwrap_or_default()
                        .chars()
                        .count()
                })
                .sum::<usize>();
            tracker
                .record_context(ContextSnapshot {
                    system_chars,
                    history_chars,
                    tool_chars,
                    estimated_input_tokens: (system_chars + history_chars + tool_chars).div_ceil(4),
                })
                .await;
        }

        let response = self
            .inner
            .chat(&request)
            .await
            .map_err(|e| Rx4ProviderError::Api(e.to_string()))?;

        if let (Some(tracker), Some(usage)) = (&self.cost_tracker, response.usage.as_ref()) {
            let _ = tracker
                .record(
                    model,
                    TokenUsage {
                        input_tokens: usage.input_tokens as usize,
                        output_tokens: usage.output_tokens as usize,
                        total_tokens: usage.input_tokens as usize + usage.output_tokens as usize,
                    },
                )
                .await;
        }

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

/// Register apollo's `Tool` trait objects into rx4's `ToolRegistry`.
///
/// Each apollo tool is wrapped in a boxed closure that captures the
/// `Arc<dyn Tool>` and calls its `execute()` method. The closure is registered
/// via `ToolDefinition::new_boxed()`, which uses `ToolExecutor::Boxed`.
///
/// Tool effects are classified based on the tool name using rx4's
/// `classify_tool()` guardrail function — idempotent tools get `ToolEffect::Read`,
/// mutating tools get `ToolEffect::Write`.
pub fn register_apollo_tools(
    registry: &mut rx4::ToolRegistry,
    tools: &[Arc<dyn UnthinkclawTool>],
    hook_ctx: &ToolHookContext,
) {
    use rx4::guardrails::classify_tool;
    use rx4::{ToolDefinition, ToolEffect, ToolExecuteBox};

    for tool in tools {
        let spec = tool.spec();
        let name = spec.name.clone();
        let description = spec.description.clone();
        let parameters_json = serde_json::to_string(&spec.parameters).unwrap_or_default();

        let tool_clone = Arc::clone(tool);
        let hook_ctx = hook_ctx.clone();
        let tool_name = name.clone();
        let execute: ToolExecuteBox = Box::new(move |_ctx, args| {
            let tool = Arc::clone(&tool_clone);
            let hook_ctx = hook_ctx.clone();
            let tool_name = tool_name.clone();
            Box::pin(async move {
                let result =
                    execute_tool_with_hooks(&hook_ctx, &tool_name, &args, Some(&tool)).await;

                rx4::ToolResult {
                    id: String::new(),
                    content: result.output,
                    is_error: result.is_error,
                    error_kind: None,
                    spill: None,
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
    /// rx4 auto-compaction threshold. `0` leaves compaction off; a non-zero
    /// value is forwarded to `Agent::auto_compact_after`.
    pub auto_compact_after: usize,
    /// Optional tracker used for provider usage and context-shape telemetry.
    pub cost_tracker: Option<Arc<CostTracker>>,
    /// Pre/post tool hooks, so rx4 enforces the same permissions as the
    /// legacy loop.
    pub hook_ctx: ToolHookContext,
}

fn model_registry_for(provider: &dyn UnthinkclawProvider, model: &str) -> rx4::ModelRegistry {
    let mut registry = rx4::ModelRegistry::new();
    let capabilities = provider.capabilities();
    let mut info = rx4::ModelInfo::new(
        provider.name(),
        model,
        capabilities.max_context.max(128_000) as usize,
        8_192,
    );
    info.supports_tools = capabilities.native_tools;
    info.supports_vision = capabilities.vision;
    registry.register(info);
    registry
}

/// Bridge that wraps an `rx4::Agent` and provides a simplified interface for
/// apollo's outer shell to use.
///
/// The bridge handles:
/// - Creating and configuring the rx4::Agent (provider, tools, system prompt)
/// - Translating messages between apollo and rx4 types
/// - Running prompts through rx4's agent loop
///
/// Unthinkclaw's unique features (channels, swarm, cron, heartbeat, autonomous
/// mode, plugins) remain in the outer shell and call `run_prompt()` on this
/// bridge to execute agent turns.
pub struct RotaryAgentBridge {
    agent: rx4::Agent,
    hook_ctx: ToolHookContext,
    /// Conversation messages maintained in rx4 format (per-session)
    messages: Vec<Message>,
    pty: Option<Arc<PtyWorker>>,
}

impl RotaryAgentBridge {
    /// Build a new bridge from the given configuration.
    pub fn new(config: RotaryBridgeConfig) -> Self {
        Self::new_with_model_registry(config, rx4::ModelRegistry::new())
    }

    /// Build a bridge with model metadata owned by the embedding consumer.
    /// Passing an empty registry preserves the provider-capability fallback.
    pub fn new_with_model_registry(
        config: RotaryBridgeConfig,
        model_registry: rx4::ModelRegistry,
    ) -> Self {
        let rx4_provider = Arc::new(RotaryProviderAdapter::new(
            Arc::clone(&config.provider),
            config.cost_tracker,
        ));

        let mut agent = rx4::Agent::new();
        let model_registry = if model_registry.is_empty() {
            model_registry_for(config.provider.as_ref(), &config.model)
        } else {
            model_registry
        };
        agent.set_model_registry(model_registry);
        agent.set_model(&config.model);
        agent.set_system_prompt(&config.system_prompt);
        agent.set_provider(rx4_provider);
        agent.set_workspace_root(&config.workspace);
        agent.max_tool_iterations = config.max_tool_iterations;
        // rx4 leaves `auto_compact_after` at `0` by default, which disables
        // compaction. Forward the configured threshold so a non-zero value
        // turns rx4's auto-compact on.
        agent.auto_compact_after = config.auto_compact_after;

        // apollo, not rx4, is the authorization authority here.
        //
        // `rx4::Policy` defaults to `workspace_write()`, which asks for
        // approval before running a tool it does not recognise. With no
        // approver attached that resolves to a denial, so leaving the default
        // in place means *no apollo tool can ever run under this engine* — the
        // turn completes with every call reporting "approval required".
        //
        // Authorization instead happens one layer in, inside the closure
        // `register_apollo_tools` installs: `execute_tool_with_hooks` runs
        // apollo's `PermissionHook` and the plugin pre-tool hooks, which are
        // driven by apollo's own permission profile and mode. Handing rx4
        // `full_access` makes it defer to that single gate rather than
        // second-guessing it with a policy apollo never configured.
        agent.set_policy(rx4::Policy::full_access());

        // Register apollo's tools into rx4's tool registry
        let mut tool_registry = rx4::ToolRegistry::new();
        register_apollo_tools(&mut tool_registry, &config.tools, &config.hook_ctx);
        agent.tools = Arc::new(tool_registry);

        Self {
            agent,
            hook_ctx: config.hook_ctx,
            messages: Vec::new(),
            pty: Some(runtime_pty_worker(&config.tools)),
        }
    }

    pub fn with_pty_worker(mut self, worker: Arc<PtyWorker>) -> Self {
        self.pty = Some(worker);
        self
    }

    pub fn pty_worker(&self) -> Option<Arc<PtyWorker>> {
        self.pty.clone()
    }

    pub async fn write_stdin(&self, process_id: &str, data: &[u8]) -> anyhow::Result<()> {
        let worker = self
            .pty
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("pty worker is not attached"))?;
        worker.write_stdin(process_id, data).await?;
        let _ = self.agent.write_stdin(process_id, data);
        Ok(())
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

    pub fn subscribe_trajectory(&mut self, recorder: Arc<std::sync::Mutex<Rx4TrajectoryRecorder>>) {
        self.agent.subscribe(move |event| {
            recorder.lock().unwrap().on_event(event);
        });
    }

    /// Run a single user prompt through the rx4 agent loop.
    ///
    /// This delegates the core agent loop (LLM calls, tool execution, turn
    /// cycling) to rx4::Agent. The caller (apollo's channel/swarm/cron
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
    /// prompt. This is used when apollo's memory backend provides
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
        if let Some(registry) = Arc::get_mut(&mut self.agent.tools) {
            register_apollo_tools(registry, tools, &self.hook_ctx);
        } else {
            tracing::warn!("cannot register rx4 tools while the registry is shared");
        }
    }

    /// Get the list of registered tool names.
    pub fn list_tools(&self) -> Vec<String> {
        self.agent
            .tools
            .definitions()
            .iter()
            .filter_map(|d| {
                d.get("name")
                    .and_then(|n| n.as_str())
                    .map(|s| s.to_string())
            })
            .collect()
    }

    /// Compact the conversation context (delegates to rx4's compact).
    pub fn compact(&mut self, reason: &str) {
        self.agent.compact(reason);
    }

    /// Give rx4 a shared handle on the message buffer.
    ///
    /// rx4 0.5.0 keeps `Agent::messages` behind an `Arc`, so a host can append
    /// to the conversation while `prompt()` is still running and the next tool
    /// iteration will see it. This is what apollo's steering queue needs: a
    /// message that arrives mid-turn is pushed here rather than queued until
    /// the turn ends.
    pub fn messages_handle(&self) -> Arc<parking_lot::RwLock<Vec<Message>>> {
        self.agent.messages_handle()
    }

    /// Load rx4's `SkillEngine` over apollo's skill directories and hand it to
    /// the agent, which runs its background skill reviewer after each prompt.
    ///
    /// This is additive to apollo's own `skills` module: rx4's engine does not
    /// perform apollo's template-variable substitution or inline shell
    /// expansion, so it supplements rather than replaces `skills::match_skill`.
    pub fn enable_skill_engine(&mut self, workspace: &std::path::Path) {
        let mut engine = build_rx4_skill_engine(workspace);
        if let Err(error) = engine.load() {
            tracing::warn!("rx4 skill engine load failed, leaving it unset: {error}");
            return;
        }
        self.agent.set_skill_engine(engine);
    }

    /// Attach an rx4 `GraphMemory` rooted at the workspace.
    ///
    /// rx4 extracts concepts, decisions and patterns from the conversation
    /// after each prompt and adds them to the graph. `auto_dream` additionally
    /// runs one consolidation pass per prompt.
    pub fn enable_graph_memory(&mut self, workspace: &std::path::Path, auto_dream: bool) {
        self.agent
            .set_graph_memory(rx4::GraphMemory::from_workspace(workspace));
        self.agent.enable_auto_dream(auto_dream);
    }
}

// ── Skill bridge ─────────────────────────────────────────────────────────

/// Build an `rx4::SkillEngine` configured with apollo's skill directories.
///
/// Unthinkclaw discovers skills from 3 directories:
/// 1. `~/.npm-global/lib/node_modules/openclaw/skills` (legacy)
/// 2. `~/.openclaw/workspace/skills` (shared workspace skills)
/// 3. `{workspace}/.apollo/skills` (project-local managed skills)
///
/// This maps to rx4's `SkillEngine` with the primary dir set to the managed
/// skills directory and the other two as `extra_dirs`.
///
/// After calling this, use `engine.load()` to populate skills from disk,
/// then `engine.search()` for keyword matching (replaces apollo's
/// `match_skill()`).
///
/// Note: apollo's template variable substitution and inline shell
/// preprocessing (`preprocess_skill_content`) are not part of rx4's
/// SkillEngine and remain in apollo's `skills` module. Use
/// `skills::preprocess_skill_content()` on the matched skill's instructions
/// before injecting into the system prompt.
pub fn build_rx4_skill_engine(workspace: &std::path::Path) -> rx4::SkillEngine {
    let home = dirs::home_dir().unwrap_or_default();

    // Primary dir: managed skills in the workspace
    let managed_dir = workspace.join(".apollo/skills");

    let mut engine = rx4::SkillEngine::new(managed_dir);

    // Extra dirs: legacy openclaw skills and shared workspace skills
    let openclaw_skills = home.join(".npm-global/lib/node_modules/openclaw/skills");
    engine.add_extra_dir(openclaw_skills);

    let shared_skills = home.join(".openclaw/workspace/skills");
    engine.add_extra_dir(shared_skills);

    engine
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
        assert!(
            engine.skills_dir().exists()
                || engine.skills_dir() == tmp.path().join(".apollo/skills")
        );
    }

    struct RecordingTool {
        ran: Arc<std::sync::atomic::AtomicBool>,
    }

    #[async_trait::async_trait]
    impl UnthinkclawTool for RecordingTool {
        fn name(&self) -> &str {
            "exec"
        }

        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "exec".to_string(),
                description: "test tool".to_string(),
                parameters: serde_json::json!({"type": "object"}),
            }
        }

        async fn execute(&self, _arguments: &str) -> anyhow::Result<UnthinkclawToolResult> {
            self.ran.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(UnthinkclawToolResult::success("ran"))
        }
    }

    async fn run_exec_through_rx4(hook_ctx: ToolHookContext) -> (rx4::ToolResult, bool) {
        let ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let tool: Arc<dyn UnthinkclawTool> = Arc::new(RecordingTool {
            ran: Arc::clone(&ran),
        });
        let mut registry = rx4::ToolRegistry::new();
        register_apollo_tools(&mut registry, &[tool], &hook_ctx);

        let ctx = Arc::new(rx4::ToolContext::new("."));
        let result = registry
            .execute("exec", &ctx, r#"{"command":"rm -rf /"}"#)
            .await
            .expect("tool registered");
        (result, ran.load(std::sync::atomic::Ordering::SeqCst))
    }

    #[tokio::test]
    async fn rx4_bridge_enforces_blocking_hooks() {
        let hook: Arc<dyn ToolHook> = Arc::new(crate::agent::hooks::PermissionHook::new(
            vec!["exec".to_string()],
            vec![],
        ));
        let (result, ran) = run_exec_through_rx4(ToolHookContext::new(vec![hook], None)).await;
        assert!(result.is_error, "blocked tool must report an error");
        assert!(
            result.content.contains("Blocked by policy"),
            "unexpected content: {}",
            result.content
        );
        assert!(!ran, "a blocked tool must not execute under rx4");
    }

    #[tokio::test]
    async fn rx4_bridge_allows_unblocked_tools() {
        let (result, ran) = run_exec_through_rx4(ToolHookContext::default()).await;
        assert!(!result.is_error);
        assert_eq!(result.content, "ran");
        assert!(ran);
    }

    #[tokio::test]
    async fn rx4_bridge_enforces_plugin_pre_tool_block() {
        let mut registry = PluginRegistry::new();
        registry.register_pre_tool_hook(Arc::new(BlockingPluginHook));
        let ctx = ToolHookContext::new(
            Vec::new(),
            Some(Arc::new(tokio::sync::RwLock::new(registry))),
        );
        let (result, ran) = run_exec_through_rx4(ctx).await;
        assert!(result.is_error);
        assert!(
            result.content.contains("Blocked by plugin"),
            "unexpected content: {}",
            result.content
        );
        assert!(!ran);
    }

    struct BlockingPluginHook;

    #[async_trait::async_trait]
    impl crate::plugin::PreToolHook for BlockingPluginHook {
        fn name(&self) -> &str {
            "blocking-test-hook"
        }

        async fn before_tool_call(&self, _name: &str, _arguments: &str) -> HookDecision {
            HookDecision::Block("plugin says no".to_string())
        }
    }

    /// Records the lifecycle events a plugin would see.
    struct RecordingLifecycleHook {
        seen: Arc<std::sync::Mutex<Vec<String>>>,
    }

    #[async_trait::async_trait]
    impl crate::plugin::LifecycleHook for RecordingLifecycleHook {
        fn name(&self) -> &str {
            "recording-lifecycle"
        }

        async fn on_event(&self, event: &LifecycleEvent) -> anyhow::Result<()> {
            let label = match event {
                LifecycleEvent::BeforeToolCall(name, _) => format!("before:{name}"),
                LifecycleEvent::AfterToolCall(name, _, _) => format!("after:{name}"),
                other => format!("other:{other:?}"),
            };
            self.seen.lock().unwrap().push(label);
            Ok(())
        }
    }

    fn stream_labels(
        rx: &mut tokio::sync::mpsc::UnboundedReceiver<AgentStreamEvent>,
    ) -> Vec<String> {
        let mut labels = Vec::new();
        while let Ok(event) = rx.try_recv() {
            labels.push(match event {
                AgentStreamEvent::ToolStart { name, .. } => format!("tool_start:{name}"),
                AgentStreamEvent::ToolEnd { name, ok, .. } => format!("tool_end:{name}:{ok}"),
                other => format!("other:{other:?}"),
            });
        }
        labels
    }

    /// Build a context that records everything a plugin or WS client sees.
    fn recording_context() -> (
        ToolHookContext,
        Arc<std::sync::Mutex<Vec<String>>>,
        tokio::sync::mpsc::UnboundedReceiver<AgentStreamEvent>,
    ) {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut manager = HookManager::new();
        manager.register_lifecycle(Arc::new(RecordingLifecycleHook {
            seen: Arc::clone(&seen),
        }));
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let ctx = ToolHookContext::default()
            .with_hook_manager(Arc::new(manager))
            .with_stream(Some(tx));
        (ctx, seen, rx)
    }

    /// The rx4 registry closure and a direct call must produce the same hooks
    /// and stream events for a tool call. Both reach the tool through
    /// `execute_tool_with_hooks`; this fails if either side stops doing so,
    /// which is how rx4 previously lost `BeforeToolCall` and the
    /// `ToolStart`/`ToolEnd` progress events.
    #[tokio::test]
    async fn both_engines_emit_the_same_hooks_and_events() {
        let args = r#"{"command":"ls"}"#;

        // rx4: the tool runs inside the registry closure.
        let (ctx, rx4_seen, mut rx4_stream) = recording_context();
        let ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let tool: Arc<dyn UnthinkclawTool> = Arc::new(RecordingTool {
            ran: Arc::clone(&ran),
        });
        let mut registry = rx4::ToolRegistry::new();
        register_apollo_tools(&mut registry, &[Arc::clone(&tool)], &ctx);
        let tool_ctx = Arc::new(rx4::ToolContext::new("."));
        registry
            .execute("exec", &tool_ctx, args)
            .await
            .expect("tool registered");
        let rx4_events = rx4_seen.lock().unwrap().clone();
        let rx4_stream_events = stream_labels(&mut rx4_stream);

        // Direct: the shared path called directly.
        let (ctx, legacy_seen, mut legacy_stream) = recording_context();
        execute_tool_with_hooks(&ctx, "exec", args, Some(&tool)).await;
        let legacy_events = legacy_seen.lock().unwrap().clone();
        let legacy_stream_events = stream_labels(&mut legacy_stream);

        assert_eq!(
            rx4_events, legacy_events,
            "the paths disagree on lifecycle hooks"
        );
        assert_eq!(
            rx4_stream_events, legacy_stream_events,
            "the paths disagree on stream events"
        );
        assert_eq!(legacy_events, vec!["before:exec", "after:exec"]);
        assert_eq!(
            legacy_stream_events,
            vec!["tool_start:exec", "tool_end:exec:true"]
        );
    }

    #[tokio::test]
    async fn a_blocked_tool_still_reports_start_and_end() {
        let hook: Arc<dyn ToolHook> = Arc::new(crate::agent::hooks::PermissionHook::new(
            vec!["exec".to_string()],
            vec![],
        ));
        let (ctx, seen, mut stream) = recording_context();
        let ctx = ToolHookContext::new(vec![hook], None)
            .with_hook_manager(Arc::new({
                let mut manager = HookManager::new();
                manager.register_lifecycle(Arc::new(RecordingLifecycleHook {
                    seen: Arc::clone(&seen),
                }));
                manager
            }))
            .with_stream(ctx.stream.clone());
        let ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let tool: Arc<dyn UnthinkclawTool> = Arc::new(RecordingTool {
            ran: Arc::clone(&ran),
        });
        let result = execute_tool_with_hooks(&ctx, "exec", "{}", Some(&tool)).await;
        assert!(result.is_error);
        assert!(!ran.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(
            stream_labels(&mut stream),
            vec!["tool_start:exec", "tool_end:exec:false"],
            "a blocked call must still open and close its progress line"
        );
        assert_eq!(
            seen.lock().unwrap().clone(),
            vec!["before:exec", "after:exec"]
        );
    }

    #[test]
    fn rx4_events_become_trajectory_steps() {
        let mut recorder = Rx4TrajectoryRecorder::default();
        recorder.on_event(&rx4::Event::TurnStart { turn: 2 });
        recorder.on_event(&rx4::Event::ToolExecutionStart(rx4::ToolCall {
            id: "c1".into(),
            name: "probe".into(),
            arguments: r#"{"path":"hello.txt"}"#.into(),
        }));
        recorder.on_event(&rx4::Event::ToolExecutionEnd(rx4::ToolResult {
            id: "c1".into(),
            content: "hello".into(),
            is_error: false,
            error_kind: None,
            spill: None,
        }));
        recorder.on_event(&rx4::Event::GuardrailStop {
            tool: "exec".into(),
            reason: "loop".into(),
        });
        recorder.on_event(&rx4::Event::RetryReason {
            retry_reason: "sandbox deny".into(),
            layer: "NestedFs".into(),
        });
        recorder.on_event(&rx4::Event::ProcessStdin {
            process_id: "p1".into(),
            bytes: 4,
        });
        recorder.on_event(&rx4::Event::RequestPermissions {
            tool: "write".into(),
            paths: vec!["src/lib.rs".into()],
        });
        recorder.on_event(&rx4::Event::PatchHunk {
            path: "src/lib.rs".into(),
            hunk: "@@ -1 +1 @@".into(),
        });
        recorder.on_event(&rx4::Event::SelfHealing {
            attempt: 1,
            max_attempts: 3,
            errors: vec!["timeout".into()],
        });
        let (steps, iterations) = recorder.take_steps();
        assert_eq!(iterations, 2);
        assert_eq!(steps.len(), 7);
        assert_eq!(steps[0].action.as_deref(), Some("probe"));
        assert!(steps[0].success);
        assert_eq!(steps[1].action.as_deref(), Some("exec"));
        assert!(!steps[1].success);
        assert_eq!(steps[2].action.as_deref(), Some("retry"));
        assert_eq!(steps[2].action_args.as_deref(), Some("NestedFs"));
        assert_eq!(steps[3].action.as_deref(), Some("stdin"));
        assert_eq!(steps[4].action.as_deref(), Some("permissions"));
        assert_eq!(steps[5].action.as_deref(), Some("patch"));
        assert_eq!(steps[6].action.as_deref(), Some("recovery"));
    }

    #[test]
    fn recovery_actions_become_trajectory_steps() {
        let mut recorder = Rx4TrajectoryRecorder::default();
        record_recovery_action(&mut recorder, &rx4::recover_empty_turn(0, 3), "empty_turn");
        record_recovery_action(&mut recorder, &rx4::recover_empty_turn(1, 3), "empty_turn");
        record_recovery_action(&mut recorder, &RecoveryAction::Retry, "empty_turn");
        record_recovery_action(&mut recorder, &rx4::recover_empty_turn(2, 3), "empty_turn");
        record_recovery_action(&mut recorder, &rx4::recover_stuck_tool(0, 3), "stuck_tool");
        record_recovery_action(&mut recorder, &RecoveryAction::Retry, "stuck_tool");
        record_recovery_action(&mut recorder, &rx4::recover_stuck_tool(2, 3), "stuck_tool");
        let (steps, _) = recorder.take_steps();
        let actions: Vec<_> = steps
            .iter()
            .map(|step| {
                (
                    step.action.as_deref(),
                    step.action_args.as_deref(),
                    step.success,
                )
            })
            .collect();
        assert_eq!(
            actions,
            [
                (Some("prefill"), Some("empty_turn"), true),
                (Some("nudge"), Some("empty_turn"), true),
                (Some("retry"), Some("empty_turn"), false),
                (Some("halt"), Some("empty_turn"), false),
                (Some("stuck_tool"), Some("nudge"), true),
                (Some("stuck_tool"), Some("retry"), false),
                (Some("stuck_tool"), Some("halt"), false),
            ]
        );
    }

    #[test]
    fn typed_recovery_spill_and_process_events_become_trajectory_steps() {
        let mut recorder = Rx4TrajectoryRecorder::default();
        recorder.on_event(&rx4::Event::Recovery {
            action: RecoveryKind::Prefill,
            reason: "Continue from where you left off.".into(),
        });
        recorder.on_event(&rx4::Event::Recovery {
            action: RecoveryKind::Nudge,
            reason: "Your last turn was empty.".into(),
        });
        recorder.on_event(&rx4::Event::Recovery {
            action: RecoveryKind::Retry,
            reason: String::new(),
        });
        recorder.on_event(&rx4::Event::Recovery {
            action: RecoveryKind::Halt,
            reason: "empty turn limit reached (2/3)".into(),
        });
        recorder.on_event(&rx4::Event::ToolSpill {
            status: SpillStatus::Spilled,
            locator: "file://spill.txt".into(),
            original_bytes: 20_000,
        });
        recorder.on_event(&rx4::Event::ToolSpill {
            status: SpillStatus::SpillFailed,
            locator: String::new(),
            original_bytes: 20,
        });
        recorder.on_event(&rx4::Event::ProcessStart {
            process_id: "p1".into(),
            program: "cat".into(),
        });
        recorder.on_event(&rx4::Event::ProcessEnd {
            process_id: "p1".into(),
            exit_code: Some(0),
        });
        recorder.on_event(&rx4::Event::ProcessEnd {
            process_id: "p2".into(),
            exit_code: Some(1),
        });
        let (steps, _) = recorder.take_steps();
        let actions: Vec<_> = steps
            .iter()
            .map(|step| {
                (
                    step.action.as_deref(),
                    step.action_args.as_deref(),
                    step.observation.as_deref(),
                    step.success,
                )
            })
            .collect();
        assert_eq!(
            actions,
            [
                (
                    Some("prefill"),
                    None,
                    Some("Continue from where you left off."),
                    true
                ),
                (Some("nudge"), None, Some("Your last turn was empty."), true),
                (Some("retry"), None, None, false),
                (
                    Some("halt"),
                    None,
                    Some("empty turn limit reached (2/3)"),
                    false
                ),
                (
                    Some("spill"),
                    Some("spilled:20000"),
                    Some("file://spill.txt"),
                    true
                ),
                (Some("spill"), Some("spill_failed:20"), Some(""), false),
                (Some("process_start"), Some("p1"), Some("cat"), true),
                (Some("process_end"), Some("p1"), Some("0"), true),
                (Some("process_end"), Some("p2"), Some("1"), false),
            ]
        );
    }

    #[test]
    fn forthcoming_typed_event_values_become_trajectory_steps() {
        let mut recorder = Rx4TrajectoryRecorder::default();
        record_rx4_event_value(
            &mut recorder,
            &serde_json::json!({"type": "Prefill", "text": "continue"}),
        );
        record_rx4_event_value(
            &mut recorder,
            &serde_json::json!({"type": "Nudge", "text": "say something"}),
        );
        record_rx4_event_value(
            &mut recorder,
            &serde_json::json!({
                "type": "StuckTool",
                "action": "Nudge",
                "text": "change args"
            }),
        );
        record_rx4_event_value(
            &mut recorder,
            &serde_json::json!({
                "type": "Recovery",
                "action": "halt",
                "reason": "empty turn limit reached"
            }),
        );
        record_rx4_event_value(
            &mut recorder,
            &serde_json::json!({"type": "Spill", "locator": "file://spill.txt"}),
        );
        record_rx4_event_value(
            &mut recorder,
            &serde_json::json!({"type": "FailureNotice", "message": "tool blew up"}),
        );
        let (steps, _) = recorder.take_steps();
        let actions: Vec<_> = steps
            .iter()
            .map(|step| {
                (
                    step.action.as_deref(),
                    step.action_args.as_deref(),
                    step.observation.as_deref(),
                    step.success,
                )
            })
            .collect();
        assert_eq!(
            actions,
            [
                (Some("prefill"), Some("empty_turn"), Some("continue"), true),
                (
                    Some("nudge"),
                    Some("empty_turn"),
                    Some("say something"),
                    true
                ),
                (Some("stuck_tool"), Some("nudge"), Some("change args"), true),
                (Some("halt"), None, Some("empty turn limit reached"), false),
                (Some("spill"), None, Some("file://spill.txt"), true),
                (Some("failure"), None, Some("tool blew up"), false),
            ]
        );
    }
}
