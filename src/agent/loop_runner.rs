//! Agent loop — the core execution engine.
//! Processes incoming messages, calls LLM, executes tools, sends responses.
//! Supports progress callbacks, self-healing, guardrails, compaction, and
//! lifecycle hooks.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::mpsc;
use tokio::sync::RwLock;

use crate::agent::hooks::{run_post_hooks, run_pre_hooks, HookDecision, ToolHook};
use crate::agent::stream::{emit, AgentStreamEvent};
use crate::agent::mode::{
    is_approval, is_rejection, AgentMode, NullChannel, PendingPlan, PendingPlans,
};
use crate::agent::compaction::{Compactor, ContextInfo, DefaultCompactor};
use crate::channels::{Channel, IncomingMessage, OutgoingMessage};
use crate::cost::{CostTracker, TokenUsage};
use crate::memory::MemoryBackend;
use crate::plugin::{HookManager, LifecycleEvent, PluginRegistry};
use crate::providers::{ChatMessage, ChatRequest, Provider};
use crate::skills;
use crate::tools::guardrails::{GuardrailDecision, ToolGuardrails};
use crate::tools::Tool;
use crate::trajectory::Trajectory;

/// Warn the LLM after this many identical tool calls (guardrails handle this now)
const LOOP_WARN_THRESHOLD: usize = 5;
/// Hard stop after this many identical consecutive tool calls (guardrails handle this now)
const LOOP_BREAK_THRESHOLD: usize = 8;

/// Agent execution state machine
#[derive(Debug, Clone, PartialEq)]
enum AgentState {
    Planning,
    Executing,
    Summarizing,
    Direct,
}

pub struct AgentRunner {
    provider: Arc<dyn Provider>,
    pub tools: Arc<RwLock<Vec<Arc<dyn Tool>>>>,
    memory: Arc<dyn MemoryBackend>,
    pub system_prompt: Arc<RwLock<String>>,
    model: std::sync::RwLock<String>,
    default_model: String,
    workspace: PathBuf,
    pub skills: Arc<RwLock<Vec<skills::Skill>>>,
    cost_tracker: Arc<CostTracker>,
    pub steering_queue: Arc<std::sync::Mutex<Vec<String>>>,
    pub agent_config: crate::config::AgentConfig,
    mode: Arc<std::sync::RwLock<AgentMode>>,
    #[cfg(feature = "swarm")]
    pub swarm: Arc<std::sync::RwLock<Option<Arc<crate::swarm::SwarmCoordinator>>>>,
    pending_plans: PendingPlans,
    hooks: Arc<std::sync::RwLock<Vec<Arc<dyn ToolHook>>>>,
    stream_sink: Arc<std::sync::RwLock<Option<crate::agent::stream::AgentStreamTx>>>,
    // ── New integrations ──
    /// Per-chat guardrail state
    guardrails: Arc<RwLock<HashMap<String, ToolGuardrails>>>,
    /// Pluggable compactor (default: DefaultCompactor)
    compactor: Option<Arc<dyn Compactor>>,
    /// Lifecycle hook manager (from plugin system)
    hook_manager: Arc<HookManager>,
    /// Plugin registry for lifecycle events
    plugin_registry: Arc<RwLock<PluginRegistry>>,
    /// Current trajectory being recorded (per chat)
    trajectories: Arc<RwLock<HashMap<String, Trajectory>>>,
}

impl AgentRunner {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        provider: Arc<dyn Provider>,
        tools: Vec<Arc<dyn Tool>>,
        memory: Arc<dyn MemoryBackend>,
        system_prompt: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        let model_str = model.into();
        Self {
            provider,
            tools: Arc::new(RwLock::new(tools)),
            memory,
            system_prompt: Arc::new(RwLock::new(system_prompt.into())),
            default_model: model_str.clone(),
            model: std::sync::RwLock::new(model_str),
            workspace: PathBuf::from("."),
            skills: Arc::new(RwLock::new(Vec::new())),
            cost_tracker: Arc::new(CostTracker::new()),
            steering_queue: Arc::new(std::sync::Mutex::new(Vec::new())),
            agent_config: crate::config::AgentConfig::default(),
            mode: Arc::new(std::sync::RwLock::new(AgentMode::default())),
            #[cfg(feature = "swarm")]
            swarm: Arc::new(std::sync::RwLock::new(None)),
            pending_plans: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            hooks: Arc::new(std::sync::RwLock::new(Vec::new())),
            stream_sink: Arc::new(std::sync::RwLock::new(None)),
            guardrails: Arc::new(RwLock::new(HashMap::new())),
            compactor: Some(Arc::new(DefaultCompactor::new())),
            hook_manager: Arc::new(HookManager::new()),
            plugin_registry: Arc::new(RwLock::new(PluginRegistry::new())),
            trajectories: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    // ── Existing setters ──

    pub fn set_stream_sink(&self, tx: Option<crate::agent::stream::AgentStreamTx>) {
        *self.stream_sink.write().unwrap() = tx;
    }

    pub fn stream_sink(&self) -> Option<crate::agent::stream::AgentStreamTx> {
        self.stream_sink.read().unwrap().clone()
    }

    #[cfg(feature = "swarm")]
    pub fn with_swarm(self, coordinator: Arc<crate::swarm::SwarmCoordinator>) -> Self {
        *self.swarm.write().unwrap() = Some(coordinator);
        self
    }

    pub fn with_config(mut self, config: crate::config::AgentConfig) -> Self {
        self.agent_config = config;
        self
    }

    pub fn with_mode(self, mode: AgentMode) -> Self {
        *self.mode.write().unwrap() = mode;
        self
    }

    pub async fn with_plugin_registry(self, registry: PluginRegistry) -> Self {
        *self.plugin_registry.write().await = registry;
        self
    }

    pub fn with_compactor(mut self, compactor: Arc<dyn Compactor>) -> Self {
        self.compactor = Some(compactor);
        self
    }

    pub fn get_mode(&self) -> AgentMode {
        self.mode.read().unwrap().clone()
    }

    pub fn set_mode(&self, mode: AgentMode) {
        *self.mode.write().unwrap() = mode;
    }

    pub fn mode_handle(&self) -> Arc<std::sync::RwLock<AgentMode>> {
        self.mode.clone()
    }

    pub fn add_hook(&self, hook: Arc<dyn ToolHook>) {
        self.hooks.write().unwrap().push(hook);
    }

    pub fn steer(&self, message: String) {
        self.steering_queue.lock().unwrap().push(message);
    }

    pub fn with_workspace(mut self, workspace: PathBuf) -> Self {
        self.workspace = workspace;
        self
    }

    pub async fn with_skills(self, skills: Vec<skills::Skill>) -> Self {
        *self.skills.write().await = skills;
        self
    }

    pub fn cost_tracker(&self) -> Arc<CostTracker> {
        self.cost_tracker.clone()
    }

    pub async fn get_cost_summary(&self) -> crate::cost::CostSummary {
        self.cost_tracker.summary().await
    }

    pub fn get_model(&self) -> String {
        self.model.read().unwrap().clone()
    }

    pub fn get_default_model(&self) -> &str {
        &self.default_model
    }

    pub fn set_model(&self, model: impl Into<String>) {
        *self.model.write().unwrap() = model.into();
    }

    pub fn reset_model(&self) {
        *self.model.write().unwrap() = self.default_model.clone();
    }

    pub async fn list_tools(&self) -> Vec<String> {
        self.tools
            .read()
            .await
            .iter()
            .map(|t| t.name().to_string())
            .collect()
    }

    pub async fn add_tool(&self, tool: Arc<dyn Tool>) {
        self.tools.write().await.push(tool);
    }

    /// Access the hook manager (for plugin tool registration)
    pub fn hook_manager(&self) -> &Arc<HookManager> {
        &self.hook_manager
    }

    /// Get the plugin registry
    pub fn plugin_registry(&self) -> &Arc<RwLock<PluginRegistry>> {
        &self.plugin_registry
    }

    // ── Deploy coding swarm ──

    pub async fn deploy_coding_swarm(
        self: Arc<Self>,
        tasks: Vec<String>,
        base_chat_id: &str,
        parallelism: usize,
    ) -> Vec<(String, String)> {
        #[cfg(feature = "swarm")]
        {
            let swarm_opt = {
                let s = self.swarm.read().unwrap();
                s.clone()
            };

            if let Some(coordinator) = swarm_opt {
                tracing::info!("Deploying swarm via SwarmCoordinator (lane-based)");
                return coordinator
                    .deploy_parallel_agents(self, tasks, base_chat_id, parallelism)
                    .await;
            }
        }

        tracing::info!("Deploying swarm via direct spawning (fallback)");
        let parallelism = parallelism.max(1);
        let mut all_results = Vec::new();

        for (chunk_idx, chunk) in tasks.chunks(parallelism).enumerate() {
            let handles: Vec<_> = chunk
                .iter()
                .enumerate()
                .map(|(i, task)| {
                    let runner = self.clone();
                    let chat_id = format!("{}_sw{}_{}", base_chat_id, chunk_idx, i);
                    let task = task.clone();
                    tokio::spawn(async move {
                        let msg = IncomingMessage {
                            id: format!("sw_{}_{}", chunk_idx, i),
                            sender_id: "swarm".to_string(),
                            sender_name: None,
                            chat_id,
                            text: task.clone(),
                            is_group: false,
                            reply_to: None,
                            timestamp: chrono::Utc::now(),
                        };
                        let null_ch = NullChannel::new("swarm");
                        let result = runner
                            .handle_message(&msg, &null_ch)
                            .await
                            .unwrap_or_else(|e| format!("⚠️ Agent error: {}", e));
                        (task, result)
                    })
                })
                .collect();

            for handle in handles {
                match handle.await {
                    Ok(result) => all_results.push(result),
                    Err(e) => tracing::warn!("Swarm worker panicked: {}", e),
                }
            }
        }

        all_results
    }

    // ── Run ──

    pub async fn run(&self, channel: &mut dyn Channel) -> anyhow::Result<()> {
        let mut rx = channel.start().await?;
        tracing::info!("Agent started on channel: {}", channel.name());

        while let Some(msg) = rx.recv().await {
            let _ = channel.send_typing(&msg.chat_id).await;

            match self.handle_message(&msg.clone(), channel).await {
                Ok(response) => {
                    if response.trim().is_empty() {
                        continue;
                    }
                    channel
                        .send(OutgoingMessage {
                            chat_id: msg.chat_id.clone(),
                            text: response,
                            reply_to: Some(msg.id.clone()),
                        })
                        .await?;
                }
                Err(e) => {
                    tracing::error!("Error handling message: {}", e);
                    channel
                        .send(OutgoingMessage {
                            chat_id: msg.chat_id,
                            text: format!("Error: {}", e),
                            reply_to: Some(msg.id),
                        })
                        .await?;
                }
            }
        }

        channel.stop().await?;
        Ok(())
    }

    pub async fn run_with_extra_rx(
        &self,
        channel: &mut dyn Channel,
        mut extra_rx: mpsc::Receiver<IncomingMessage>,
    ) -> anyhow::Result<()> {
        let mut rx = channel.start().await?;
        tracing::info!(
            "Agent started on channel: {} (with heartbeat)",
            channel.name()
        );

        loop {
            let msg = tokio::select! {
                Some(msg) = rx.recv() => msg,
                Some(msg) = extra_rx.recv() => msg,
                else => break,
            };

            let _ = channel.send_typing(&msg.chat_id).await;

            match self.handle_message(&msg, channel).await {
                Ok(response) => {
                    if msg.sender_id == "system" && response.contains("HEARTBEAT_OK") {
                        tracing::debug!("Heartbeat: agent responded OK, skipping output");
                        continue;
                    }
                    if response.trim().is_empty() {
                        continue;
                    }
                    channel
                        .send(OutgoingMessage {
                            chat_id: msg.chat_id.clone(),
                            text: response,
                            reply_to: Some(msg.id.clone()),
                        })
                        .await?;
                }
                Err(e) => {
                    tracing::error!("Error handling message: {}", e);
                    if msg.sender_id != "system" {
                        channel
                            .send(OutgoingMessage {
                                chat_id: msg.chat_id,
                                text: format!("Error: {}", e),
                                reply_to: Some(msg.id),
                            })
                            .await?;
                    }
                }
            }
        }

        channel.stop().await?;
        Ok(())
    }

    // ── Handle single message ──

    pub async fn handle_message(
        &self,
        msg: &IncomingMessage,
        channel: &dyn Channel,
    ) -> anyhow::Result<String> {
        let stream = self.stream_sink();
        emit(
            &stream,
            AgentStreamEvent::Status {
                message: "Thinking…".into(),
            },
        );

        // Emit lifecycle event: agent start
        self.hook_manager
            .emit(&LifecycleEvent::AgentStart(msg.chat_id.clone(), msg.text.clone()))
            .await;

        // Initialize per-chat guardrails
        {
            let mut gr = self.guardrails.write().await;
            gr.entry(msg.chat_id.clone())
                .or_insert_with(|| ToolGuardrails::new(crate::tools::guardrails::GuardrailConfig::default()));
        }

        // Initialize per-chat trajectory
        {
            let mut trajs = self.trajectories.write().await;
            if !trajs.contains_key(&msg.chat_id) {
                let mut t = Trajectory::new(
                    format!("traj_{}", chrono::Utc::now().timestamp()),
                    msg.chat_id.clone(),
                    self.get_model(),
                );
                t.metadata.insert("task".to_string(), msg.text.clone());
                trajs.insert(msg.chat_id.clone(), t);
            }
        }

        let draft_id: Option<String> = if channel.supports_draft_updates() {
            channel.send_draft(&msg.chat_id, "⏳").await.unwrap_or(None)
        } else {
            let _ = channel.send_typing(&msg.chat_id).await;
            None
        };

        if msg.is_group && !crate::context::should_respond(msg) {
            tracing::debug!(
                "Skipping ambient group message without assistant context: {}",
                msg.id
            );
            return Ok(String::new());
        }

        let (effective_text, resume_plan, resume_model) = {
            let mut plans = self.pending_plans.lock().unwrap();
            if let Some(pending) = plans.get(&msg.chat_id) {
                if pending.is_expired() {
                    plans.remove(&msg.chat_id);
                    (msg.text.clone(), None, None)
                } else if is_approval(&msg.text) {
                    let pending = plans.remove(&msg.chat_id).unwrap();
                    tracing::info!("Plan approved for chat_id={}", msg.chat_id);
                    (
                        pending.original_message,
                        Some(pending.plan),
                        Some(pending.preferred_model),
                    )
                } else if is_rejection(&msg.text) {
                    plans.remove(&msg.chat_id);
                    return Ok("Plan cancelled. What would you like to do instead?".to_string());
                } else {
                    plans.remove(&msg.chat_id);
                    (msg.text.clone(), None, None)
                }
            } else {
                (msg.text.clone(), None, None)
            }
        };
        let resuming_from_plan = resume_plan.is_some();

        let mode = self.get_mode();
        let hooks_snapshot: Vec<Arc<dyn ToolHook>> = self.hooks.read().unwrap().clone();

        // ── Build messages ──

        let system_prompt = self.system_prompt.read().await.clone();
        let mut messages = vec![ChatMessage::system(&system_prompt)];
        if let Some(guidance) = crate::context::routing_guidance(msg.is_group, channel.name()) {
            messages.push(ChatMessage::system(guidance));
        }

        if let Some(mode_prompt) = mode.system_prompt_injection() {
            messages.push(ChatMessage::system(mode_prompt));
        }

        // Skill injection with template preprocessing
        {
            let skills = self.skills.read().await;
            if let Some(skill) = skills::match_skill(&skills, &effective_text) {
                if let Some(content) = skills::load_skill_content(skill) {
                    let preprocessed = skills::preprocess_skill_content(
                        &content,
                        skill.location.parent(),
                        Some(&msg.chat_id),
                        Some(&self.workspace),
                    );
                    messages.push(ChatMessage::system(format!(
                        "# Active Skill: {}\n{}\n\nFollow the instructions above for this skill.",
                        skill.name, preprocessed
                    )));
                    tracing::info!("Skill matched: {} (preprocessed)", skill.name);
                }
            }
        }

        let history = self
            .memory
            .get_conversation_history(&msg.chat_id, self.agent_config.max_history_messages)
            .await?;
        for (role, content) in history {
            match role.as_str() {
                "user" => messages.push(ChatMessage::user(&content)),
                "assistant" => messages.push(ChatMessage::assistant(&content)),
                _ => {}
            }
        }

        messages.push(ChatMessage::user(&effective_text));

        let tool_specs: Vec<crate::tools::ToolSpec> =
            self.tools.read().await.iter().map(|t| t.spec()).collect();
        let tools_snapshot: Vec<Arc<dyn Tool>> = self.tools.read().await.iter().cloned().collect();
        let main_model = self.model.read().unwrap().clone();

        // ═══════════════════════════════════════════════════════
        // STATE MACHINE: Planning → Executing → Summarizing
        // ═══════════════════════════════════════════════════════

        let needs_tools = if resuming_from_plan {
            true
        } else {
            self.classify_request(&effective_text, &main_model).await
        };
        let mut state = if needs_tools {
            AgentState::Planning
        } else {
            AgentState::Direct
        };
        tracing::info!("Initial state: {:?}", state);

        let mut _plan: Option<String> = None;
        let mut execution_model = main_model.clone();

        if let Some(ref plan) = resume_plan {
            messages.push(ChatMessage::system(format!(
                "APPROVED EXECUTION PLAN (follow these steps):\n{}",
                plan
            )));
            execution_model = resume_model.unwrap_or_else(|| main_model.clone());
            state = AgentState::Executing;
            tracing::info!("Resuming with approved plan, model={}", execution_model);
        } else if state == AgentState::Planning {
            let swarm_model_choice = if mode.is_swarm() {
                "   - SWARM: for tasks that can be parallelized across multiple independent agents\n"
            } else {
                ""
            };
            let plan_prompt = format!(
                "You are a planning assistant. Analyze this request and output TWO things:\n\n\
                1. MODEL_CHOICE: Pick one execution model:\n\
                   - SONNET: for general tasks, file ops, web, simple edits, queries\n\
                   - OPUS: for complex coding, architecture, multi-file refactors, debugging hard bugs\n\
                   - VIBEMANIA: for building features, creating projects, coding tasks that need autonomous agents\n\
                {swarm_choice}\
                2. PLAN: A brief numbered step-by-step plan.\n\
                   - If VIBEMANIA: the plan should be a single step: delegate to vibemania/subspace with the goal\n\
                   - If SWARM: the plan should list each independent subtask for parallel agents\n\
                   - If SONNET/OPUS: list what tools to use and in what order\n\n\
                Format your response EXACTLY like:\n\
                MODEL_CHOICE: SONNET\n\
                PLAN:\n\
                1. step one\n\
                2. step two\n\n\
                Available tools: {tools}\n\n\
                User request: {request}",
                swarm_choice = swarm_model_choice,
                tools = tool_specs.iter().map(|t| format!("{} ({})", t.name, t.description.chars().take(50).collect::<String>())).collect::<Vec<_>>().join(", "),
                request = &effective_text
            );

            let plan_messages = [ChatMessage::user(&plan_prompt)];
            let plan_request = ChatRequest {
                messages: &plan_messages,
                tools: None,
                model: &self.agent_config.fast_model,
                temperature: 0.8,
                max_tokens: Some(500),
            };

            match self.provider.chat(&plan_request).await {
                Ok(resp) => {
                    let p = resp.text.unwrap_or_default();
                    tracing::info!("Plan: {}", &p[..p.len().min(300)]);

                    if let Some(usage) = &resp.usage {
                        let _ = self
                            .cost_tracker
                            .record(
                                &self.agent_config.fast_model,
                                TokenUsage {
                                    input_tokens: usage.input_tokens as usize,
                                    output_tokens: usage.output_tokens as usize,
                                    total_tokens: (usage.input_tokens + usage.output_tokens) as usize,
                                },
                            )
                            .await;
                    }

                    let p_upper = p.to_uppercase();
                    if p_upper.contains("MODEL_CHOICE: OPUS")
                        || p_upper.contains("MODEL_CHOICE:OPUS")
                    {
                        execution_model = self.agent_config.heavy_model.clone();
                        tracing::info!("Planner chose OPUS for execution");
                    } else if p_upper.contains("MODEL_CHOICE: SWARM")
                        || p_upper.contains("MODEL_CHOICE:SWARM")
                    {
                        tracing::info!("Planner chose SWARM — routing to coding_swarm tool");
                        messages.push(ChatMessage::system(
                            "IMPORTANT: This task can be parallelized. Use the `coding_swarm` tool \
                            to execute subtasks in parallel agents. \n\
                            Decompose the original goal into a list of independent tasks for the swarm."
                                .to_string(),
                        ));
                    } else if p_upper.contains("MODEL_CHOICE: VIBEMANIA")
                        || p_upper.contains("MODEL_CHOICE:VIBEMANIA")
                    {
                        tracing::info!("Planner chose VIBEMANIA — routing to vibemania tool");
                        messages.push(ChatMessage::system(
                            "IMPORTANT: This is a complex coding task. Use the `vibemania` tool \
                            to handle this autonomously. \n\
                            Do NOT write code yourself — delegate to vibemania."
                                .to_string(),
                        ));
                    }

                    if mode.prefer_heavy_model() && execution_model == main_model {
                        execution_model = self.agent_config.heavy_model.clone();
                        tracing::info!("Coding mode: upgraded to heavy model");
                    }

                    messages.push(ChatMessage::system(format!(
                        "EXECUTION PLAN (follow these steps):\n{}",
                        p
                    )));
                    _plan = Some(p.clone());
                    state = AgentState::Executing;

                    if mode.plan_approval() {
                        {
                            let mut plans = self.pending_plans.lock().unwrap();
                            plans.retain(|_, v| !v.is_expired());
                            plans.insert(
                                msg.chat_id.clone(),
                                PendingPlan {
                                    plan: p.clone(),
                                    original_message: effective_text.clone(),
                                    preferred_model: execution_model.clone(),
                                    created_at: std::time::Instant::now(),
                                },
                            );
                        }
                        let plan_response = format!(
                            "**Coding Plan**\n\n{}\n\n---\nReply **go** to execute or **cancel** to abort.",
                            p
                        );
                        if let Some(ref mid) = draft_id {
                            let _ = channel
                                .finalize_draft(&msg.chat_id, mid, &plan_response)
                                .await;
                            return Ok(String::new());
                        }
                        return Ok(plan_response);
                    }
                }
                Err(e) => {
                    tracing::warn!("Planning failed ({}), falling back to direct", e);
                    state = AgentState::Direct;
                }
            }
        }

        // ═══════════════════════════════════════════════════════
        // EXECUTION LOOP (with self-healing + guardrails)
        // ═══════════════════════════════════════════════════════

        let mut tool_call_history: Vec<String> = Vec::new();
        let mut compactions_done: usize = 0;
        // Self-healing tracking: per-tool consecutive error count for this execution
        let mut healing_attempts_left = 3;

        for round in 0..self.agent_config.max_rounds {
            // ── Steering queue ──
            {
                let mut queue = self.steering_queue.lock().unwrap();
                if !queue.is_empty() {
                    for steer_msg in queue.drain(..) {
                        tracing::info!("Steering: {}", &steer_msg[..steer_msg.len().min(80)]);
                        messages.push(ChatMessage::user(format!(
                            "⚡ STEERING — new instruction from user (prioritize this): {}",
                            steer_msg
                        )));
                    }
                }
            }

            // ── Context budget check with compactor ──
            let context_chars: usize = messages.iter().map(|m| m.content.len()).sum();
            let max_chars = self.agent_config.max_context_chars;
            if let Some(ref compactor) = self.compactor {
                let info = ContextInfo {
                    message_count: messages.len(),
                    total_chars: context_chars,
                    max_chars,
                    compactions_done,
                };
                if compactor.should_compress(&info) {
                    tracing::info!(
                        "Compacting at round {} ({} chars, {} msgs)",
                        round + 1,
                        context_chars,
                        messages.len()
                    );
                    let result = compactor
                        .compress(&messages, Some(&effective_text))
                        .await;
                    if result.did_compact {
                        messages = result.messages;
                        compactions_done += 1;
                    }
                }
            } else {
                // Fallback: original inline compaction
                if context_chars > max_chars {
                    tracing::info!(
                        "Compacting at round {} ({} chars)",
                        round + 1,
                        context_chars
                    );
                    messages = self.compact_messages(messages, &effective_text).await?;
                    compactions_done += 1;
                }
            }

            // ── Select model ──
            let (model, temperature) = match state {
                AgentState::Planning => (self.agent_config.fast_model.clone(), 0.8),
                AgentState::Executing => (execution_model.clone(), 0.2),
                AgentState::Summarizing => (self.agent_config.fast_model.clone(), 0.7),
                AgentState::Direct => (main_model.clone(), 0.7),
            };

            tracing::info!(
                "[{:?}] round {} {} msgs, ~{} chars, model={}",
                state,
                round + 1,
                messages.len(),
                messages.iter().map(|m| m.content.len()).sum::<usize>(),
                model
            );

            // ── LLM call ──
            let request = ChatRequest {
                messages: &messages,
                tools: if tool_specs.is_empty() || state == AgentState::Summarizing {
                    None
                } else {
                    Some(&tool_specs)
                },
                model: &model,
                temperature,
                max_tokens: Some(8192),
            };

            let response = self.provider.chat(&request).await?;

            if let Some(usage) = &response.usage {
                let _ = self
                    .cost_tracker
                    .record(
                        &model,
                        TokenUsage {
                            input_tokens: usage.input_tokens as usize,
                            output_tokens: usage.output_tokens as usize,
                            total_tokens: (usage.input_tokens + usage.output_tokens) as usize,
                        },
                    )
                    .await;
            }

            if !response.has_tool_calls() {
                let text = response.text.unwrap_or_default();

                match state {
                    AgentState::Executing => {
                        if round >= 3 {
                            tracing::info!(
                                "Execution done after {} rounds, summarizing",
                                round + 1
                            );
                            state = AgentState::Summarizing;
                            messages.push(ChatMessage::assistant(text));
                            messages.push(ChatMessage::user(
                                "Now provide a clean, concise final response to the user. \
                                Summarize what you did and the results. Be brief and direct."
                                    .to_string(),
                            ));
                            continue;
                        }
                        tracing::info!("Done after {} round(s) [{:?}]", round + 1, state);
                        self.finish_execution(msg, &text, &draft_id, channel).await?;
                        return Ok(String::new());
                    }
                    AgentState::Summarizing | AgentState::Direct | AgentState::Planning => {
                        tracing::info!("Done after {} round(s) [{:?}]", round + 1, state);
                        self.finish_execution(msg, &text, &draft_id, channel).await?;
                        return Ok(String::new());
                    }
                }
            }

            // ── TOOL EXECUTION ──

            // Legacy loop detection (backup for guardrails)
            for tc in &response.tool_calls {
                let hash = format!(
                    "{}:{}",
                    tc.name,
                    &tc.arguments[..tc.arguments.len().min(200)]
                );
                tool_call_history.push(hash);
            }
            if tool_call_history.len() >= LOOP_BREAK_THRESHOLD {
                let last = &tool_call_history[tool_call_history.len() - 1];
                let consecutive = tool_call_history
                    .iter()
                    .rev()
                    .take_while(|h| *h == last)
                    .count();
                if consecutive >= LOOP_BREAK_THRESHOLD {
                    tracing::warn!("Loop: {} identical calls, breaking", consecutive);
                    return Ok(format!(
                        "Loop detected ({} identical {} calls). Stopping.",
                        consecutive, response.tool_calls[0].name
                    ));
                }
                if consecutive >= LOOP_WARN_THRESHOLD {
                    messages.push(ChatMessage::user(
                        "WARNING: You're repeating tool calls. Stop and answer with what you have."
                            .to_string(),
                    ));
                }
            }

            if !channel.supports_draft_updates() {
                let _ = channel.send_typing(&msg.chat_id).await;
            }

            // Build assistant tool_use message
            {
                let mut content_blocks: Vec<serde_json::Value> = Vec::new();
                if let Some(text) = &response.text {
                    if !text.is_empty() {
                        content_blocks.push(serde_json::json!({
                            "type": "text", "text": text,
                        }));
                    }
                }
                for tc in &response.tool_calls {
                    content_blocks.push(serde_json::json!({
                        "type": "tool_use",
                        "id": &tc.id,
                        "name": &tc.name,
                        "input": serde_json::from_str::<serde_json::Value>(&tc.arguments).unwrap_or_default(),
                    }));
                }
                messages.push(ChatMessage {
                    role: "assistant_tool_use".to_string(),
                    content: String::new(),
                    tool_use_id: Some(serde_json::to_string(&content_blocks).unwrap_or_default()),
                });
            }

            tracing::info!(
                "Tools: {}",
                response
                    .tool_calls
                    .iter()
                    .map(|tc| tc.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );

            let mut progress_lines: Vec<String> = Vec::new();
            let mut tool_errors: Vec<String> = Vec::new();

            for tc in &response.tool_calls {
                let _ = self
                    .hook_manager
                    .emit(&LifecycleEvent::BeforeToolCall(
                        tc.name.clone(),
                        tc.arguments.clone(),
                    ))
                    .await;

                if let (true, Some(ref mid)) = (channel.supports_draft_updates(), &draft_id) {
                    let hint = extract_tool_hint(&tc.name, &tc.arguments);
                    let start_line = if hint.is_empty() {
                        format!("⏳ {}\n", tc.name)
                    } else {
                        format!("⏳ {}: {}\n", tc.name, hint)
                    };
                    progress_lines.push(start_line.clone());
                    let _ = channel
                        .update_draft_progress(&msg.chat_id, mid, &progress_lines.join(""))
                        .await;
                }

                emit(
                    &stream,
                    AgentStreamEvent::ToolStart {
                        name: tc.name.clone(),
                        hint: extract_tool_hint(&tc.name, &tc.arguments),
                    },
                );

                let started = std::time::Instant::now();

                // ── Plugin pre-tool check ──
                let pre_tool_decision = {
                    let registry = self.plugin_registry.read().await;
                    registry.check_pre_tool(&tc.name, &tc.arguments).await
                };

                // ── Run tool ──
                let result = match pre_tool_decision {
                    crate::plugin::HookDecision::Block(reason) => {
                        tracing::info!("Plugin blocked '{}': {}", tc.name, reason);
                        crate::tools::ToolResult::error(format!("Blocked by plugin: {}", reason))
                    }
                    crate::plugin::HookDecision::Allow => {
                        match run_pre_hooks(&hooks_snapshot, &tc.name, &tc.arguments).await {
                            HookDecision::Block(reason) => {
                                tracing::info!("Hook blocked '{}': {}", tc.name, reason);
                                crate::tools::ToolResult::error(format!("Blocked by policy: {}", reason))
                            }
                            HookDecision::Allow => {
                                if let Some(tool) = tools_snapshot.iter().find(|t| t.name() == tc.name) {
                                    match tool.execute(&tc.arguments).await {
                                        Ok(r) => r,
                                        Err(e) => {
                                            crate::tools::ToolResult::error(format!("Tool error: {}", e))
                                        }
                                    }
                                } else {
                                    crate::tools::ToolResult::error(format!("Unknown tool: {}", tc.name))
                                }
                            }
                        }
                    }
                };

                run_post_hooks(&hooks_snapshot, &tc.name, &tc.arguments, &result).await;

                // ── Post-tool lifecycle + guardrails ──
                let _ = self
                    .hook_manager
                    .emit(&LifecycleEvent::AfterToolCall(
                        tc.name.clone(),
                        tc.arguments.clone(),
                        result.clone(),
                    ))
                    .await;

                {
                    let mut gr = self.guardrails.write().await;
                    if let Some(g) = gr.get_mut(&msg.chat_id) {
                        let decision = g.observe(&tc.name, &tc.arguments, &result.output, result.is_error);
                        match decision {
                            GuardrailDecision::Stop(reason) => {
                                tracing::warn!("Guardrail stop: {}", reason);
                                self.finish_execution(msg, &reason, &draft_id, channel).await?;
                                return Ok(reason);
                            }
                            GuardrailDecision::Warn(warning) => {
                                tracing::warn!("Guardrail warn: {}", warning);
                                messages.push(ChatMessage::user(warning));
                            }
                            GuardrailDecision::Proceed => {}
                        }
                    }
                }

                // ── Post-tool plugin notification ──
                {
                    let registry = self.plugin_registry.read().await;
                    registry
                        .notify_post_tool(&tc.name, &tc.arguments, &result)
                        .await;
                }

                let elapsed = started.elapsed().as_secs();
                emit(
                    &stream,
                    AgentStreamEvent::ToolEnd {
                        name: tc.name.clone(),
                        ok: !result.is_error,
                        elapsed_secs: elapsed,
                    },
                );

                // ── Track trajectory ──
                {
                    let mut trajs = self.trajectories.write().await;
                    if let Some(t) = trajs.get_mut(&msg.chat_id) {
                        t.record_tool_step(
                            response.text.clone(),
                            tc.name.clone(),
                            tc.arguments.clone(),
                            result.output.clone(),
                            !result.is_error,
                        );
                    }
                }

                if let (true, Some(ref mid)) = (channel.supports_draft_updates(), &draft_id) {
                    let done_line = if result.is_error {
                        format!("❌ {} ({}s)\n", tc.name, elapsed)
                    } else {
                        format!("✅ {} ({}s)\n", tc.name, elapsed)
                    };
                    let prefix = format!("⏳ {}", tc.name);
                    if let Some(pos) = progress_lines.iter().rposition(|l| l.starts_with(&prefix)) {
                        progress_lines[pos] = done_line;
                    } else {
                        progress_lines.push(done_line);
                    }
                    let _ = channel
                        .update_draft_progress(&msg.chat_id, mid, &progress_lines.join(""))
                        .await;
                }

                // ── Self-healing: track tool errors for re-prompt ──
                if result.is_error {
                    tool_errors.push(format!(
                        "'{}' failed: {}",
                        tc.name,
                        &result.output[..result.output.len().min(200)]
                    ));
                }

                let truncated_output = if result.output.len() > self.agent_config.max_tool_result_chars {
                    format!(
                        "{}...\n⚠️ [Truncated {} → {} chars]",
                        &result.output[..self.agent_config.max_tool_result_chars],
                        result.output.len(),
                        self.agent_config.max_tool_result_chars
                    )
                } else {
                    result.output.clone()
                };

                messages.push(ChatMessage::tool_result(&tc.id, &truncated_output));
            }

            // ── Self-healing: if tools failed, re-prompt with error context ──
            if !tool_errors.is_empty() && healing_attempts_left > 0 {
                let error_summary = tool_errors.join("\n");
                tracing::info!(
                    "Self-healing: {} tool errors, {} attempts left",
                    tool_errors.len(),
                    healing_attempts_left
                );
                messages.push(ChatMessage::user(format!(
                    "The following tool call(s) failed:\n{}\n\n\
                    Please try a different approach. Previous steps are still valid — \
                    don't repeat what already worked unless needed.",
                    error_summary
                )));
                healing_attempts_left -= 1;
                // Reset guardrails per-turn state for next iteration
                {
                    let mut gr = self.guardrails.write().await;
                    if let Some(g) = gr.get_mut(&msg.chat_id) {
                        g.reset();
                    }
                }
                continue; // Re-prompt without incrementing the tool_call_history-based loop
            }

            if state == AgentState::Direct {
                state = AgentState::Executing;
            }
        }

        // ── Circuit breaker ──
        tracing::warn!(
            "Circuit breaker after {} rounds",
            self.agent_config.max_rounds
        );
        self.persist_conversation(
            msg,
            &format!("Hit {} rounds.", self.agent_config.max_rounds),
        )
        .await?;

        // Mark trajectory as unsuccessful
        {
            let mut trajs = self.trajectories.write().await;
            if let Some(t) = trajs.get_mut(&msg.chat_id) {
                t.success = false;
            }
        }

        let circuit_msg = format!(
            "⚠️ Hit {} rounds ({} compactions). Break into smaller tasks?",
            self.agent_config.max_rounds, compactions_done
        );
        if let Some(ref mid) = draft_id {
            let _ = channel
                .finalize_draft(&msg.chat_id, mid, &circuit_msg)
                .await;
            return Ok(String::new());
        }
        Ok(circuit_msg)
    }

    /// Finish execution — persist, emit events, finalize draft
    async fn finish_execution(
        &self,
        msg: &IncomingMessage,
        text: &str,
        draft_id: &Option<String>,
        channel: &dyn Channel,
    ) -> anyhow::Result<()> {
        self.persist_conversation(msg, text).await?;

        // Mark trajectory as successful, record final response
        {
            let mut trajs = self.trajectories.write().await;
            if let Some(t) = trajs.get_mut(&msg.chat_id) {
                t.success = true;
                t.record_response(text.to_string());
                t.iterations = t.tool_calls; // Approximate iterations as tool calls
            }
        }

        // Emit lifecycle event
        self.hook_manager
            .emit(&LifecycleEvent::AgentDone(
                msg.chat_id.clone(),
                text.to_string(),
            ))
            .await;

        let stream = self.stream_sink();
        emit(
            &stream,
            AgentStreamEvent::Done {
                response: text.to_string(),
            },
        );

        if let Some(ref mid) = draft_id {
            let _ = channel
                .finalize_draft(&msg.chat_id, mid, text)
                .await;
        }

        Ok(())
    }

    async fn classify_request(&self, text: &str, _model: &str) -> bool {
        if self.get_mode().always_plan() {
            return true;
        }

        let lower = text.to_lowercase();
        let word_count = text.split_whitespace().count();

        if word_count <= 5 {
            return false;
        }

        let tool_keywords = [
            "read ", "write ", "edit ", "create ", "build ", "fix ", "search ", "fetch ",
            "check ", "run ", "execute ", "install ", "deploy ", "find ", "list ", "show me ",
            "what's in ", "look at ", "file", "code", "commit", "git ", "grep", "curl",
        ];
        for kw in &tool_keywords {
            if lower.contains(kw) {
                return true;
            }
        }

        if word_count >= 15
            && (lower.contains('?') || lower.contains("can you") || lower.contains("please"))
        {
            return true;
        }

        false
    }

    async fn persist_conversation(
        &self,
        msg: &IncomingMessage,
        response: &str,
    ) -> anyhow::Result<()> {
        self.memory
            .store_conversation_batch(&[
                (&msg.chat_id, &msg.sender_id, "user", &msg.text),
                (&msg.chat_id, "assistant", "assistant", response),
            ])
            .await?;
        Ok(())
    }

    /// Compact conversation messages — fallback when no compactor is set
    async fn compact_messages(
        &self,
        messages: Vec<ChatMessage>,
        original_task: &str,
    ) -> anyhow::Result<Vec<ChatMessage>> {
        let info = ContextInfo {
            message_count: messages.len(),
            total_chars: messages.iter().map(|m| m.content.len()).sum(),
            max_chars: self.agent_config.max_context_chars,
            compactions_done: 0,
        };
        if let Some(ref compactor) = self.compactor {
            if compactor.should_compress(&info) {
                let result = compactor.compress(&messages, Some(original_task)).await;
                if result.did_compact {
                    tracing::info!(
                        "Compactor '{}' compressed {} → {} messages",
                        compactor.name(),
                        messages.len(),
                        result.messages.len()
                    );
                    return Ok(result.messages);
                }
            }
        }
        // Fallback: inline summarization via fast model
        let keep_recent = 6;
        if messages.len() <= keep_recent + 2 {
            return Ok(messages);
        }
        let system_msgs: Vec<&ChatMessage> =
            messages.iter().filter(|m| m.role == "system").collect();
        let non_system: Vec<&ChatMessage> =
            messages.iter().filter(|m| m.role != "system").collect();
        if non_system.len() <= keep_recent {
            return Ok(messages);
        }
        let (old_msgs, recent_msgs) = non_system.split_at(non_system.len() - keep_recent);
        let mut summary_input = String::new();
        for m in old_msgs {
            let role_label = match m.role.as_str() {
                "user" => "User",
                "assistant" | "assistant_tool_use" => "Assistant",
                "tool_result" => "Tool Result",
                _ => &m.role,
            };
            let content = if m.content.len() > 500 {
                format!("{}...", &m.content[..500])
            } else {
                m.content.clone()
            };
            summary_input.push_str(&format!("[{}]: {}\n", role_label, content));
        }
        let compaction_prompt = format!(
            "Summarize this conversation concisely. The original task was: \"{}\"\n\n\
            Focus on: what was accomplished, what tools were used, key results, and what's still pending.\n\n\
            Conversation:\n{}",
            original_task, summary_input
        );
        let compact_messages = [ChatMessage::user(&compaction_prompt)];
        let compact_request = ChatRequest {
            messages: &compact_messages,
            tools: None,
            model: &self.agent_config.fast_model,
            temperature: 0.3,
            max_tokens: Some(1000),
        };
        let summary = match self.provider.chat(&compact_request).await {
            Ok(resp) => {
                if let Some(usage) = &resp.usage {
                    let _ = self.cost_tracker.record(
                        &self.agent_config.fast_model,
                        TokenUsage {
                            input_tokens: usage.input_tokens as usize,
                            output_tokens: usage.output_tokens as usize,
                            total_tokens: (usage.input_tokens + usage.output_tokens) as usize,
                        },
                    ).await;
                }
                resp.text.unwrap_or_else(|| "Failed to summarize.".to_string())
            }
            Err(e) => {
                tracing::warn!("Compaction failed: {}, falling back to truncation", e);
                format!("[Previous {} messages truncated to save context]", old_msgs.len())
            }
        };
        let mut compacted = Vec::new();
        for sm in &system_msgs { compacted.push((*sm).clone()); }
        compacted.push(ChatMessage::user(format!(
            "[Conversation compacted — {} earlier messages summarized]\n\n{}",
            old_msgs.len(),
            summary
        )));
        compacted.push(ChatMessage::assistant("Understood, continuing from the summary.".to_string()));
        compacted.extend(recent_msgs.iter().map(|m| (*m).clone()));
        Ok(compacted)
    }

    // ── Trajectory access ──

    /// Get trajectory for a chat (for export)
    pub async fn get_trajectory(&self, chat_id: &str) -> Option<Trajectory> {
        let trajs = self.trajectories.read().await;
        trajs.get(chat_id).cloned()
    }

    /// Get all trajectories
    pub async fn get_all_trajectories(&self) -> Vec<Trajectory> {
        let trajs = self.trajectories.read().await;
        trajs.values().cloned().collect()
    }

    /// Save trajectory to disk
    pub async fn save_trajectory(&self, chat_id: &str, dir: &PathBuf) -> anyhow::Result<()> {
        if let Some(traj) = self.get_trajectory(chat_id).await {
            std::fs::create_dir_all(dir)?;
            let path = dir.join(format!("traj_{}.json", chat_id));
            traj.save_to_file(&path)?;
            tracing::info!("Trajectory saved: {:?}", path);
        }
        Ok(())
    }
}

// ── Helper ──

fn extract_tool_hint(name: &str, arguments: &str) -> String {
    let v: serde_json::Value = serde_json::from_str(arguments).unwrap_or_default();
    let hint = match name {
        "shell" | "bash" | "exec" => v
            .get("command")
            .or_else(|| v.get("cmd"))
            .and_then(|s| s.as_str()),
        "web_search" | "search" => v
            .get("query")
            .or_else(|| v.get("q"))
            .and_then(|s| s.as_str()),
        "web_fetch" | "fetch" => v.get("url").and_then(|s| s.as_str()),
        "file_ops" | "read" | "write" | "edit" => v
            .get("path")
            .or_else(|| v.get("file_path"))
            .and_then(|s| s.as_str()),
        "vibemania" => v.get("goal").and_then(|s| s.as_str()),
        _ => v
            .as_object()
            .and_then(|o| o.values().next())
            .and_then(|v| v.as_str()),
    };
    hint.map(|s| {
        let s = s.trim();
        if s.len() > 60 {
            format!("{}…", &s[..57])
        } else {
            s.to_string()
        }
    })
    .unwrap_or_default()
}


