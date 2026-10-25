//! Delegation system — named agents delegate to other named agents.
//!
//! Permission system: agent_links table (outbound/inbound/bidirectional)
//! Concurrency limits: per-link + per-agent
//! Modes: sync (wait for result) vs async (announce later)

use anyhow::{bail, Result};
use std::sync::Arc;
use tokio::sync::RwLock;

use super::models::{AgentLink, DelegationMode, DelegationRecord, LinkDirection};
use super::storage::SwarmStorage;

/// Delegation manager — handles inter-agent delegation
pub struct DelegationManager {
    storage: Arc<dyn SwarmStorage>,
    /// In-memory tracking of active delegations (fast path)
    active_counts: Arc<RwLock<ActiveCounts>>,
}

/// In-memory concurrency counters for fast checking
struct ActiveCounts {
    /// (source_id, target_id) -> count
    per_link: std::collections::HashMap<(String, String), u32>,
    /// agent_id -> count of incoming delegations
    per_agent: std::collections::HashMap<String, u32>,
}

impl ActiveCounts {
    fn new() -> Self {
        Self {
            per_link: std::collections::HashMap::new(),
            per_agent: std::collections::HashMap::new(),
        }
    }

    fn increment(&mut self, source: &str, target: &str) {
        *self
            .per_link
            .entry((source.to_string(), target.to_string()))
            .or_insert(0) += 1;
        *self.per_agent.entry(target.to_string()).or_insert(0) += 1;
    }

    fn decrement(&mut self, source: &str, target: &str) {
        if let Some(c) = self
            .per_link
            .get_mut(&(source.to_string(), target.to_string()))
        {
            *c = c.saturating_sub(1);
        }
        if let Some(c) = self.per_agent.get_mut(target) {
            *c = c.saturating_sub(1);
        }
    }

    fn link_count(&self, source: &str, target: &str) -> u32 {
        self.per_link
            .get(&(source.to_string(), target.to_string()))
            .copied()
            .unwrap_or(0)
    }

    fn agent_count(&self, agent: &str) -> u32 {
        self.per_agent.get(agent).copied().unwrap_or(0)
    }
}

impl DelegationManager {
    pub fn new(storage: Arc<dyn SwarmStorage>) -> Self {
        Self {
            storage,
            active_counts: Arc::new(RwLock::new(ActiveCounts::new())),
        }
    }

    /// Create a link between two agents (permission to delegate)
    pub async fn create_link(
        &self,
        source_id: &str,
        target_id: &str,
        direction: LinkDirection,
        max_concurrent: u32,
    ) -> Result<AgentLink> {
        // Verify both agents exist
        if self.storage.get_agent(source_id).await?.is_none() {
            bail!("Source agent '{}' not found", source_id);
        }
        if self.storage.get_agent(target_id).await?.is_none() {
            bail!("Target agent '{}' not found", target_id);
        }

        let link = AgentLink::new(source_id.to_string(), target_id.to_string(), direction)
            .with_max_concurrent(max_concurrent);
        self.storage.create_agent_link(&link).await?;
        Ok(link)
    }

    /// Delete a link between two agents
    pub async fn delete_link(&self, source_id: &str, target_id: &str) -> Result<()> {
        self.storage.delete_agent_link(source_id, target_id).await
    }

    /// Delegate a task from source agent to target agent
    pub async fn delegate(
        &self,
        source_name: &str,
        target_name: &str,
        task: &str,
        mode: DelegationMode,
        context: Option<String>,
    ) -> Result<DelegationRecord> {
        // Resolve agent names to IDs
        let source = self
            .storage
            .get_agent_by_name(source_name)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Source agent '{}' not found", source_name))?;
        let target = self
            .storage
            .get_agent_by_name(target_name)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Target agent '{}' not found", target_name))?;

        // Check permission
        let link = self
            .storage
            .check_link_permission(&source.agent_id, &target.agent_id)
            .await?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "No delegation permission from '{}' to '{}'",
                    source_name,
                    target_name
                )
            })?;

        // Check per-link concurrency
        let counts = self.active_counts.read().await;
        let link_count = counts.link_count(&source.agent_id, &target.agent_id);
        if link_count >= link.max_concurrent {
            bail!(
                "Per-link concurrency limit reached ({}/{}) for {} -> {}",
                link_count,
                link.max_concurrent,
                source_name,
                target_name
            );
        }

        // Check per-agent concurrency
        let agent = self
            .storage
            .get_agent(&target.agent_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Target agent vanished"))?;
        let max_agent = agent.max_concurrent.unwrap_or(5) as u32;
        let agent_count = counts.agent_count(&target.agent_id);
        if agent_count >= max_agent {
            bail!(
                "Per-agent concurrency limit reached ({}/{}) for '{}'",
                agent_count,
                max_agent,
                target_name
            );
        }
        drop(counts);

        // Create delegation record
        let mut record = DelegationRecord::new(
            source.agent_id.clone(),
            target.agent_id.clone(),
            task.to_string(),
            mode,
        );
        if let Some(ctx) = context {
            record = record.with_context(ctx);
        }
        record.status = "running".to_string();

        self.storage.record_delegation(&record).await?;

        // Update in-memory counts
        let mut counts = self.active_counts.write().await;
        counts.increment(&source.agent_id, &target.agent_id);

        Ok(record)
    }

    /// Complete a delegation
    pub async fn complete_delegation(&self, delegation_id: &str, result: String) -> Result<()> {
        let record = self
            .storage
            .get_delegation(delegation_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Delegation '{}' not found", delegation_id))?;

        self.storage
            .update_delegation_status(delegation_id, "completed", Some(result))
            .await?;

        let mut counts = self.active_counts.write().await;
        counts.decrement(&record.source_agent_id, &record.target_agent_id);

        Ok(())
    }

    /// Fail a delegation
    pub async fn fail_delegation(&self, delegation_id: &str, error: String) -> Result<()> {
        let record = self
            .storage
            .get_delegation(delegation_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Delegation '{}' not found", delegation_id))?;

        self.storage
            .update_delegation_status(delegation_id, "failed", Some(error))
            .await?;

        let mut counts = self.active_counts.write().await;
        counts.decrement(&record.source_agent_id, &record.target_agent_id);

        Ok(())
    }

    /// Cancel a delegation
    pub async fn cancel_delegation(&self, delegation_id: &str) -> Result<()> {
        let record = self
            .storage
            .get_delegation(delegation_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Delegation '{}' not found", delegation_id))?;

        self.storage
            .update_delegation_status(delegation_id, "cancelled", None)
            .await?;

        let mut counts = self.active_counts.write().await;
        counts.decrement(&record.source_agent_id, &record.target_agent_id);

        Ok(())
    }

    /// List active delegations for an agent
    pub async fn list_active(&self, agent_id: &str) -> Result<Vec<DelegationRecord>> {
        self.storage.list_active_delegations(agent_id).await
    }

    /// Get links for an agent
    pub async fn get_links(&self, agent_id: &str) -> Result<Vec<AgentLink>> {
        self.storage.get_agent_links(agent_id).await
    }

    /// Sync in-memory counts with storage (call on startup)
    pub async fn sync_counts(&self) -> Result<()> {
        let delegations = self.storage.list_all_active_delegations().await?;
        let mut counts = self.active_counts.write().await;
        *counts = ActiveCounts::new();

        for d in delegations {
            counts.increment(&d.source_agent_id, &d.target_agent_id);
        }

        Ok(())
    }
}
