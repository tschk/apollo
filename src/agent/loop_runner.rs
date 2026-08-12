//! Agent loop — the core execution engine.
//! Processes incoming messages, calls LLM, executes tools, sends responses.
//! Supports progress callbacks, lifecycle hooks, and trajectory recording.
//!
//! The loop itself is owned by the rx4 (rotary) harness; apollo owns everything
//! around it — system prompt, skill injection, conversation history, memory
//! recall, tool set, persistence, and lifecycle hooks.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use sha2::{Digest, Sha256};
use tokio::sync::mpsc;
use tokio::sync::RwLock;

use crate::agent::hooks::ToolHook;
use crate::agent::mode::{AgentMode, NullChannel};
use crate::agent::stream::{emit, AgentStreamEvent};
use crate::channels::{Channel, Delivery, IncomingMessage, OutgoingMessage};
use crate::cost::CostTracker;
use crate::memory::MemoryBackend;
use crate::plugin::{HookManager, LifecycleEvent, PluginRegistry};
use crate::providers::{ChatMessage, Provider};
use crate::skills;
use crate::text::truncate_chars;
use crate::tools::Tool;
use crate::trajectory::Trajectory;

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
    hooks: Arc<std::sync::RwLock<Vec<Arc<dyn ToolHook>>>>,
    stream_sink: Arc<std::sync::RwLock<Option<crate::agent::stream::AgentStreamTx>>>,
    /// Lifecycle hook manager (from plugin system)
    hook_manager: Arc<HookManager>,
    /// Plugin registry for lifecycle events
    plugin_registry: Arc<RwLock<PluginRegistry>>,
    /// Current trajectory being recorded (per chat)
    trajectories: Arc<RwLock<HashMap<String, Trajectory>>>,
    /// Whether this runner may read or persist conversational memory.
    memory_enabled: bool,
    memory_ideas: crate::config::MemoryIdeasConfig,
    group_chat: crate::config::GroupChatConfig,
    #[cfg(feature = "zkr-memory")]
    zkr: Option<Arc<crate::memory::zkr::ZkrStore>>,
    #[cfg(feature = "zkr-memory")]
    zkr_config: crate::config::ZkrConfig,
    session_note_workspace: Option<PathBuf>,
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
            hooks: Arc::new(std::sync::RwLock::new(Vec::new())),
            stream_sink: Arc::new(std::sync::RwLock::new(None)),
            hook_manager: Arc::new(HookManager::new()),
            plugin_registry: Arc::new(RwLock::new(PluginRegistry::new())),
            trajectories: Arc::new(RwLock::new(HashMap::new())),
            memory_enabled: true,
            memory_ideas: crate::config::MemoryIdeasConfig::default(),
            group_chat: crate::config::GroupChatConfig::default(),
            #[cfg(feature = "zkr-memory")]
            zkr: None,
            #[cfg(feature = "zkr-memory")]
            zkr_config: crate::config::ZkrConfig::default(),
            session_note_workspace: None,
        }
    }

    // ── Existing setters ──

    pub fn set_stream_sink(&self, tx: Option<crate::agent::stream::AgentStreamTx>) {
        *self.stream_sink.write().unwrap() = tx;
    }

    /// The sink events go to: the per-turn sink of the current task if one is
    /// scoped, otherwise the process-wide sink.
    pub fn stream_sink(&self) -> Option<crate::agent::stream::AgentStreamTx> {
        crate::agent::stream::current_turn_sink()
            .or_else(|| self.stream_sink.read().unwrap().clone())
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
        // Plugin tools join the agent's tool list here. Registering a tool and
        // never exposing it is the failure this closes: the plugin API
        // accepted it, the log said so, and the agent could not call it.
        {
            let mut tools = self.tools.write().await;
            for tool in registry.tools() {
                if tools.iter().any(|t| t.name() == tool.name()) {
                    tracing::warn!(
                        "plugin tool '{}' shadows a built-in of the same name; keeping the built-in",
                        tool.name()
                    );
                    continue;
                }
                tools.push(Arc::clone(tool));
            }
        }
        *self.plugin_registry.write().await = registry;
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
        self.session_note_workspace = Some(workspace.clone());
        self.workspace = workspace;
        self
    }

    pub fn with_memory_ideas(mut self, cfg: crate::config::MemoryIdeasConfig) -> Self {
        self.memory_ideas = cfg;
        self
    }

    /// Enable or disable conversational memory for this runner.
    ///
    /// Restricted automation uses this to keep prior conversations and
    /// experiment output out of the model context and persistent stores.
    pub fn with_memory_enabled(mut self, enabled: bool) -> Self {
        self.memory_enabled = enabled;
        self
    }

    pub fn with_group_chat(mut self, cfg: crate::config::GroupChatConfig) -> Self {
        self.group_chat = cfg;
        self
    }

    #[cfg(feature = "zkr-memory")]
    pub fn with_zkr(
        mut self,
        store: Option<Arc<crate::memory::zkr::ZkrStore>>,
        cfg: crate::config::ZkrConfig,
    ) -> Self {
        self.zkr = store;
        self.zkr_config = cfg;
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

    /// The name of the provider currently backing model calls.
    pub fn provider_name(&self) -> &str {
        self.provider.name()
    }

    /// The conversation store, for callers that need to read or clear history.
    pub fn memory(&self) -> &Arc<dyn MemoryBackend> {
        &self.memory
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

    pub async fn run_with_runtime_rx(
        &self,
        channel: &mut dyn Channel,
        mut extra_rx: mpsc::Receiver<IncomingMessage>,
        mut cron_rx: mpsc::Receiver<crate::cron_scheduler::DueJob>,
        scheduler: Arc<crate::cron_scheduler::CronScheduler>,
    ) -> anyhow::Result<()> {
        let mut rx = channel.start().await?;
        loop {
            enum RuntimeInput {
                Message(IncomingMessage),
                Cron(crate::cron_scheduler::DueJob),
            }
            let input = tokio::select! {
                Some(msg) = rx.recv() => RuntimeInput::Message(msg),
                Some(msg) = extra_rx.recv() => RuntimeInput::Message(msg),
                Some(job) = cron_rx.recv() => RuntimeInput::Cron(job),
                else => break,
            };
            match input {
                RuntimeInput::Message(msg) => {
                    let _ = channel.send_typing(&msg.chat_id).await;
                    match self.handle_message(&msg, channel).await {
                        Ok(response) if !response.trim().is_empty() => {
                            channel
                                .send(OutgoingMessage {
                                    chat_id: msg.chat_id,
                                    text: response,
                                    reply_to: Some(msg.id),
                                })
                                .await?;
                        }
                        Ok(_) => {}
                        Err(error) if msg.sender_id != "system" => {
                            channel
                                .send(OutgoingMessage {
                                    chat_id: msg.chat_id,
                                    text: format!("Error: {error}"),
                                    reply_to: Some(msg.id),
                                })
                                .await?;
                        }
                        Err(error) => tracing::error!("Error handling message: {error}"),
                    }
                }
                RuntimeInput::Cron(due) => {
                    let job = due.job;
                    let job_id = job.id.clone().unwrap_or_default();
                    let run_token = job.run_token.clone().unwrap_or_default();
                    if job.channel != channel.name() {
                        scheduler.release_run(&job_id, &run_token).await?;
                        continue;
                    }
                    let msg = IncomingMessage {
                        id: format!("cron-{job_id}"),
                        sender_id: "scheduler".to_string(),
                        sender_name: Some("Scheduler".to_string()),
                        chat_id: job.chat_id.clone(),
                        text: job.task.clone(),
                        is_group: false,
                        reply_to: None,
                        timestamp: chrono::Utc::now(),
                    };
                    match self
                        .handle_message_with_model(
                            &msg,
                            channel,
                            (!job.model.is_empty()).then_some(job.model.as_str()),
                        )
                        .await
                    {
                        Ok(response) => {
                            if !response.trim().is_empty() {
                                if let Err(error) = channel
                                    .send(OutgoingMessage {
                                        chat_id: job.chat_id,
                                        text: response,
                                        reply_to: None,
                                    })
                                    .await
                                {
                                    scheduler
                                        .fail_run(&job_id, &run_token, &error.to_string())
                                        .await?;
                                    continue;
                                }
                            }
                            scheduler
                                .mark_run(&job_id, &run_token, &job.schedule)
                                .await?;
                        }
                        Err(error) => {
                            scheduler
                                .fail_run(&job_id, &run_token, &error.to_string())
                                .await?
                        }
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
        self.handle_message_with_model(msg, channel, None).await
    }

    pub async fn handle_message_with_model(
        &self,
        msg: &IncomingMessage,
        channel: &dyn Channel,
        model: Option<&str>,
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
            .emit(&LifecycleEvent::AgentStart(
                msg.chat_id.clone(),
                msg.text.clone(),
            ))
            .await;

        // Initialize per-chat trajectory
        {
            let mut trajs = self.trajectories.write().await;
            if !trajs.contains_key(&msg.chat_id) {
                let t = Trajectory::new(
                    format!("traj_{}", chrono::Utc::now().timestamp()),
                    msg.chat_id.clone(),
                    self.get_model(),
                );
                trajs.insert(msg.chat_id.clone(), t);
            }
        }

        let delivery = Delivery::open(channel, &msg.chat_id, "⏳").await;
        if delivery.draft().is_none() {
            let _ = channel.send_typing(&msg.chat_id).await;
        }

        if msg.is_group && !crate::context::should_respond(msg) {
            tracing::debug!(
                "Skipping ambient group message without assistant context: {}",
                msg.id
            );
            return Ok(String::new());
        }

        let effective_text = msg.text.clone();
        let mode = self.get_mode();

        // ── Build messages ──

        let base_prompt = self.system_prompt.read().await.clone();
        #[cfg(feature = "zkr-memory")]
        let system_prompt = if self.memory_enabled && self.zkr_config.self_improve {
            if let Some(store) = &self.zkr {
                match store.augment_prompt(&effective_text, &base_prompt).await {
                    Ok(augmented) => augmented,
                    Err(error) => {
                        tracing::warn!("self-improve augmentation failed: {error}");
                        base_prompt
                    }
                }
            } else {
                base_prompt
            }
        } else {
            base_prompt
        };
        #[cfg(not(feature = "zkr-memory"))]
        let system_prompt = base_prompt;
        let mut messages = vec![ChatMessage::system(&system_prompt)];
        if let Some(guidance) = crate::context::routing_guidance(msg.is_group, channel.name()) {
            messages.push(ChatMessage::system(guidance));
        }

        if let Some(mode_prompt) = mode.system_prompt_injection() {
            messages.push(ChatMessage::system(mode_prompt));
        }

        // Skill injection with template preprocessing
        {
            let matched = {
                let skills = self.skills.read().await;
                skills::match_skill(&skills, &effective_text)
                    .map(|skill| (skill.name.clone(), skill.location.clone()))
            };
            if let Some((skill_name, location)) = matched {
                let chat_id = msg.chat_id.clone();
                let workspace = self.workspace.clone();
                let preprocessed = tokio::task::spawn_blocking(move || {
                    let content = std::fs::read_to_string(&location).ok()?;
                    Some(skills::preprocess_skill_content(
                        &content,
                        location.parent(),
                        Some(&chat_id),
                        Some(&workspace),
                    ))
                })
                .await?;
                if let Some(preprocessed) = preprocessed {
                    messages.push(ChatMessage::system(format!(
                        "# Active Skill: {}\n{}\n\nFollow the instructions above for this skill.",
                        skill_name, preprocessed
                    )));
                    tracing::info!("Skill matched: {} (preprocessed)", skill_name);
                }
            }
        }
        if self.memory_enabled && msg.is_group {
            if let Some(group_memory) = self.load_group_memory(&msg.chat_id).await? {
                if !group_memory.trim().is_empty() {
                    messages.push(ChatMessage::system(crate::context::group_memory_prompt(
                        &msg.chat_id,
                        &group_memory,
                    )));
                }
            }
        }

        if self.memory_enabled {
            let history = crate::memory::context_inject::merged_history(
                &self.memory,
                &msg.chat_id,
                self.memory_ideas.principal_id.as_deref(),
                self.agent_config.max_history_messages,
            )
            .await?;
            for (role, content) in history {
                match role.as_str() {
                    "user" => messages.push(ChatMessage::user(&content)),
                    "assistant" => messages.push(ChatMessage::assistant(&content)),
                    _ => {}
                }
            }
        }

        let mut user_turn = effective_text.clone();
        if self.memory_enabled && self.memory_ideas.inject_context {
            let blocks = crate::memory::context_inject::personal_context_blocks(
                &self.memory,
                crate::memory::context_inject::InjectConfig {
                    workspace: &self.workspace,
                    principal_id: self.memory_ideas.principal_id.as_deref(),
                    graph_recall_limit: self.memory_ideas.graph_recall_limit,
                },
                &effective_text,
            )
            .await;
            if !blocks.is_empty() {
                user_turn = format!("{user_turn}\n\n{}", blocks.join("\n\n"));
            }
        }
        #[cfg(feature = "zkr-memory")]
        if self.memory_enabled && self.zkr_config.inject_recall {
            if let Some(store) = &self.zkr {
                match store
                    .context(&effective_text, self.zkr_config.recall_limit)
                    .await
                {
                    Ok(Some(context)) => user_turn = format!("{user_turn}\n\n{context}"),
                    Ok(None) => {}
                    Err(error) => tracing::warn!("zkr recall failed: {error}"),
                }
            }
        }
        messages.push(ChatMessage::user(&user_turn));

        let tools_snapshot: Vec<Arc<dyn Tool>> = self.tools.read().await.iter().cloned().collect();
        let main_model = model
            .map(str::to_string)
            .unwrap_or_else(|| self.model.read().unwrap().clone());

        // ── rx4 engine ──
        // Context assembly above stays apollo's; from here rx4 owns the loop.
        let text = self
            .run_via_rotary(&messages, &tools_snapshot, &main_model)
            .await?;
        self.finish_execution(msg, &text, &delivery).await
    }

    /// Run one turn through the rx4 (rotary) harness.
    ///
    /// apollo keeps ownership of everything around the loop — system prompt,
    /// skill injection, conversation history, memory recall, tool set — and
    /// hands rx4 the assembled conversation. rx4 owns model calls and tool
    /// cycling from there.
    ///
    /// The bridge is built per turn so each chat gets an isolated message
    /// buffer; registration is in-memory and does no I/O.
    async fn run_via_rotary(
        &self,
        messages: &[ChatMessage],
        tools: &[Arc<dyn Tool>],
        model: &str,
    ) -> anyhow::Result<String> {
        use crate::agent::rotary_bridge::{RotaryAgentBridge, RotaryBridgeConfig};

        // rx4 takes the system prompt out of band, so collapse apollo's system
        // messages (base prompt, mode injection, skills, group memory) into one.
        let system_prompt = messages
            .iter()
            .filter(|m| m.role == "system")
            .map(|m| m.content.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");

        let mut history: Vec<ChatMessage> = messages
            .iter()
            .filter(|m| m.role != "system")
            .cloned()
            .collect();
        let prompt = history
            .pop()
            .ok_or_else(|| anyhow::anyhow!("no user turn to run through rx4"))?;

        // Apollo owns provider/model selection. Supply the selected model's
        // current provider metadata to rx4 rather than asking rx4 for a
        // built-in catalog.
        let capabilities = self.provider.capabilities();
        let mut model_info = rx4::ModelInfo::new(
            self.provider.name(),
            model,
            capabilities.max_context.max(128_000) as usize,
            8_192,
        );
        model_info.supports_tools = capabilities.native_tools;
        model_info.supports_vision = capabilities.vision;
        let model_registry = rx4::ModelRegistry::from_models([model_info]);

        let mut bridge = RotaryAgentBridge::new_with_model_registry(
            RotaryBridgeConfig {
                provider: Arc::clone(&self.provider),
                tools: tools.to_vec(),
                system_prompt,
                model: model.to_string(),
                workspace: self.workspace.clone(),
                max_tool_iterations: self.agent_config.max_rounds,
                auto_compact_after: self.agent_config.auto_compact_after,
                cost_tracker: Some(Arc::clone(&self.cost_tracker)),
                // Both engines must run the same hooks and emit the same events.
                hook_ctx: crate::agent::rotary_bridge::ToolHookContext::new(
                    self.hooks.read().unwrap().clone(),
                    Some(Arc::clone(&self.plugin_registry)),
                )
                .with_hook_manager(Arc::clone(&self.hook_manager))
                .with_stream(self.stream_sink()),
            },
            model_registry,
        );

        // ── Steering queue ──
        // rx4's `messages_handle()` exposes the shared message buffer the tool
        // loop reads at the top of every iteration, so a message pushed here
        // while `prompt()` is running is visible to the next tool cycle. This
        // mirrors the legacy loop's per-round steering drain.
        let messages_handle = bridge.messages_handle();
        let steering_queue = Arc::clone(&self.steering_queue);
        let prompt_fut = bridge.run_prompt_with_history(&prompt.content, &history);
        tokio::pin!(prompt_fut);

        loop {
            tokio::select! {
                biased;
                result = &mut prompt_fut => {
                    return result;
                }
                _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {
                    let mut queue = steering_queue.lock().unwrap();
                    if !queue.is_empty() {
                        for steer_msg in queue.drain(..) {
                            tracing::info!(
                                chars = steer_msg.chars().count(),
                                "Steering message queued"
                            );
                            messages_handle.write().push(rx4::provider::Message::user(
                                format!(
                                    "⚡ STEERING — new instruction from user (prioritize this): {}",
                                    steer_msg
                                ),
                            ));
                        }
                    }
                }
            }
        }
    }

    /// Finish execution — persist, emit events, finalize draft
    async fn finish_execution(
        &self,
        msg: &IncomingMessage,
        text: &str,
        delivery: &Delivery<'_>,
    ) -> anyhow::Result<String> {
        if self.memory_enabled {
            self.persist_conversation(msg, text).await?;
        }

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
        if self.memory_enabled {
            if let Some(ws) = &self.session_note_workspace {
                let preview: String = text.chars().take(200).collect();
                if !preview.is_empty() {
                    let _ = crate::memory::session_note::append_session_note(
                        ws,
                        &msg.chat_id,
                        &preview,
                    );
                }
            }
        }

        let stream = self.stream_sink();
        emit(
            &stream,
            AgentStreamEvent::Done {
                response: text.to_string(),
            },
        );

        // Draft-capable channels already have the text on screen; returning it
        // as well would post it twice. Every other channel relies on the
        // returned string being the reply.
        let delivered = delivery.deliver(&msg.chat_id, text).await?;

        #[cfg(feature = "zkr-memory")]
        if self.memory_enabled && self.zkr_config.self_improve {
            if let Some(store) = &self.zkr {
                let _ = store
                    .record_reflection(&msg.text, "agent turn", text, "completed")
                    .await;
            }
        }

        Ok(delivered)
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
        #[cfg(feature = "zkr-memory")]
        if self.zkr_config.auto_capture {
            if let Some(store) = &self.zkr {
                if let Err(error) = store
                    .capture_turn(
                        &msg.chat_id,
                        &msg.id,
                        &msg.text,
                        response,
                        msg.timestamp.timestamp(),
                    )
                    .await
                {
                    tracing::warn!("zkr turn capture failed: {error}");
                }
            }
        }
        if msg.is_group {
            self.update_group_memory(msg, response).await?;
        }
        Ok(())
    }

    async fn load_group_memory(&self, chat_id: &str) -> anyhow::Result<Option<String>> {
        let key = crate::context::group_memory_key(chat_id);
        Ok(self
            .memory
            .recall(&self.group_chat.rolling_memory_namespace, &key)
            .await?
            .map(|entry| entry.value))
    }

    async fn update_group_memory(
        &self,
        msg: &IncomingMessage,
        response: &str,
    ) -> anyhow::Result<()> {
        let existing = self
            .load_group_memory(&msg.chat_id)
            .await?
            .unwrap_or_default();
        let updated = Self::rolling_group_memory(
            &existing,
            msg,
            response,
            self.group_chat.rolling_memory_max_chars,
        );
        let key = crate::context::group_memory_key(&msg.chat_id);
        self.memory
            .store(
                &self.group_chat.rolling_memory_namespace,
                &key,
                &updated,
                None,
            )
            .await?;
        Ok(())
    }

    fn rolling_group_memory(
        existing: &str,
        msg: &IncomingMessage,
        response: &str,
        max_chars: usize,
    ) -> String {
        let sender = msg
            .sender_name
            .as_deref()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or(&msg.sender_id);
        let mut text = String::new();
        if !existing.trim().is_empty() {
            text.push_str(existing.trim());
            text.push_str("\n\n");
        }
        text.push_str(&format!("[{sender}] user: {}\n", msg.text.trim()));
        text.push_str(&format!("assistant: {}", response.trim()));
        if text.chars().count() <= max_chars {
            return text;
        }
        let tail: String = text
            .chars()
            .rev()
            .take(max_chars)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        if let Some(idx) = tail.find('\n') {
            tail[idx + 1..].to_string()
        } else {
            tail
        }
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
    pub async fn save_trajectory(&self, chat_id: &str, dir: &Path) -> anyhow::Result<()> {
        if let Some(traj) = self.get_trajectory(chat_id).await {
            let path = dir.join(trajectory_filename(chat_id));
            traj.save_to_file(&path)?;
            tracing::info!("Trajectory saved: {:?}", path);
        }
        Ok(())
    }
}

// ── Helper ──

fn trajectory_filename(chat_id: &str) -> String {
    format!("traj_{:x}.json", Sha256::digest(chat_id.as_bytes()))
}

pub(crate) fn extract_tool_hint(name: &str, arguments: &str) -> String {
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
        if s.chars().count() > 60 {
            format!("{}…", truncate_chars(s, 57))
        } else {
            s.to_string()
        }
    })
    .unwrap_or_default()
}

#[cfg(test)]
mod retry_tests {
    use std::path::Path;

    use super::{trajectory_filename, truncate_chars};

    #[test]
    fn truncating_multibyte_tool_output_does_not_panic() {
        // Byte-slicing this at 200 would land mid-sequence and panic.
        let output = "日本語".repeat(200);
        assert_eq!(truncate_chars(&output, 200).chars().count(), 200);
        assert_eq!(truncate_chars("hi", 200), "hi");
        assert_eq!(truncate_chars("héllo", 2), "hé");
    }

    #[test]
    fn trajectory_filename_confines_external_chat_ids() {
        let filename = trajectory_filename("x/../../outside");
        assert_eq!(
            Path::new(&filename).parent(),
            Some(Path::new("")),
            "trajectory filename must be one path component"
        );
        assert_eq!(filename.len(), "traj_".len() + 64 + ".json".len());
        assert!(filename.starts_with("traj_"));
        assert!(filename.ends_with(".json"));
        assert!(!filename.contains(".."));
        assert!(!filename.contains('/'));
        assert!(!filename.contains('\\'));
    }
}
