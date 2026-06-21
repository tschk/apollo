use super::delegation::DelegationManager;
use super::handoff::HandoffManager;
use super::scheduler::ConcurrencyScheduler;
use super::team::TeamManager;
use super::{AgentCapability, AgentInfo, SwarmStorage, Task, TaskPriority, TaskStatus};
use anyhow::Result;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Swarm coordinator — central orchestrator for multi-agent operations.
/// Owns delegation, team, handoff managers, and concurrency scheduler.
pub struct SwarmCoordinator {
    storage: Arc<dyn SwarmStorage>,
    message_queue: Arc<RwLock<Vec<String>>>,
    pub delegation: DelegationManager,
    pub teams: TeamManager,
    pub handoffs: HandoffManager,
    pub scheduler: Arc<ConcurrencyScheduler>,
}

impl SwarmCoordinator {
    pub fn new(storage: Arc<dyn SwarmStorage>) -> Self {
        let scheduler = Arc::new(ConcurrencyScheduler::new());
        Self {
            delegation: DelegationManager::new(storage.clone()),
            teams: TeamManager::new(storage.clone()),
            handoffs: HandoffManager::new(storage.clone()),
            scheduler: scheduler.clone(),
            storage,
            message_queue: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Initialize — sync in-memory state from storage
    pub async fn init(&self) -> Result<()> {
        self.delegation.sync_counts().await?;
        Ok(())
    }

    /// Get storage reference
    pub fn storage(&self) -> &Arc<dyn SwarmStorage> {
        &self.storage
    }

    /// Submit a task to the swarm
    pub async fn submit_task(
        &self,
        title: String,
        description: String,
        priority: TaskPriority,
    ) -> Result<String> {
        let task = Task::new(title, description, priority);
        let task_id = task.task_id.clone();
        self.storage.store_task(&task).await?;
        Ok(task_id)
    }

    /// Assign task to an agent based on capabilities
    pub async fn assign_task(&self, task_id: &str) -> Result<Option<String>> {
        let _task = match self.storage.get_task(task_id).await? {
            Some(t) => t,
            None => return Ok(None),
        };

        let agents = self.storage.list_active_agents().await?;
        if let Some(agent) = agents.first() {
            self.storage
                .update_task_status(task_id, TaskStatus::Running)
                .await?;
            return Ok(Some(agent.agent_id.clone()));
        }

        Ok(None)
    }

    /// Register a new agent with extended info
    pub async fn register_agent(
        &self,
        name: String,
        capabilities: Vec<AgentCapability>,
        model: Option<String>,
        tools: Option<Vec<String>>,
    ) -> Result<String> {
        let mut agent = AgentInfo::new(name, capabilities);
        if let Some(m) = model {
            agent = agent.with_model(m);
        }
        if let Some(t) = tools {
            agent = agent.with_tools(t);
        }
        let agent_id = agent.agent_id.clone();
        self.storage.register_agent(&agent).await?;
        Ok(agent_id)
    }

    /// Update agent heartbeat
    pub async fn heartbeat(&self, agent_id: &str) -> Result<()> {
        self.storage.update_agent_heartbeat(agent_id).await
    }

    /// Queue a message for steering
    pub async fn queue_message(&self, message: String) {
        let mut queue = self.message_queue.write().await;
        queue.push(message);
    }

    /// Get next queued message
    pub async fn pop_message(&self) -> Option<String> {
        let mut queue = self.message_queue.write().await;
        if !queue.is_empty() {
            Some(queue.remove(0))
        } else {
            None
        }
    }

    /// List all pending tasks
    pub async fn list_pending_tasks(&self) -> Result<Vec<Task>> {
        self.storage.list_pending_tasks().await
    }

    /// List all active agents
    pub async fn list_active_agents(&self) -> Result<Vec<AgentInfo>> {
        self.storage.list_active_agents().await
    }

    /// List all agents
    pub async fn list_all_agents(&self) -> Result<Vec<AgentInfo>> {
        self.storage.list_all_agents().await
    }

    /// Get agent by name
    pub async fn get_agent_by_name(&self, name: &str) -> Result<Option<AgentInfo>> {
        self.storage.get_agent_by_name(name).await
    }

    /// Deploy parallel headless agent workers (unified from AgentRunner).
    ///
    /// Each task runs in an isolated context (shared provider + tools).
    /// Uses the concurrency scheduler to manage slots in the Delegate lane.
    pub async fn deploy_parallel_agents(
        &self,
        runner: Arc<crate::agent::AgentRunner>,
        tasks: Vec<String>,
        base_chat_id: &str,
        parallelism: usize,
    ) -> Vec<(String, String)> {
        let parallelism = parallelism.max(1);
        let mut all_results = Vec::new();

        for (chunk_idx, chunk) in tasks.chunks(parallelism).enumerate() {
            let handles: Vec<_> = chunk
                .iter()
                .enumerate()
                .map(|(i, task)| {
                    let coordinator = Arc::new(self.clone_for_worker());
                    let runner = runner.clone();
                    let chat_id = format!("{}_sw{}_{}", base_chat_id, chunk_idx, i);
                    let task = task.clone();

                    tokio::spawn(async move {
                        // Acquire a slot in the Delegate lane
                        let slot_id = match coordinator
                            .scheduler
                            .acquire_slot("swarm_worker", super::scheduler::Lane::Delegate, &task)
                            .await
                        {
                            Some(id) => id,
                            None => {
                                return (task, "⚠️ Swarm lane full, task deferred.".to_string())
                            }
                        };

                        let msg = crate::channels::IncomingMessage {
                            id: format!("sw_{}_{}", chunk_idx, i),
                            sender_id: "swarm".to_string(),
                            sender_name: None,
                            chat_id,
                            text: task.clone(),
                            is_group: false,
                            reply_to: None,
                            timestamp: chrono::Utc::now(),
                        };
                        let null_ch = crate::agent::mode::NullChannel::new("swarm");

                        let result = runner
                            .handle_message(&msg, &null_ch)
                            .await
                            .unwrap_or_else(|e| format!("⚠️ Agent error: {}", e));

                        coordinator.scheduler.release_slot(&slot_id).await;
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

    /// Internal helper to clone for worker tasks (storage is Arc, others are new/shared)
    fn clone_for_worker(&self) -> Self {
        Self {
            storage: self.storage.clone(),
            message_queue: self.message_queue.clone(),
            delegation: DelegationManager::new(self.storage.clone()),
            teams: TeamManager::new(self.storage.clone()),
            handoffs: HandoffManager::new(self.storage.clone()),
            scheduler: self.scheduler.clone(), // Use shared Arc
        }
    }
}
