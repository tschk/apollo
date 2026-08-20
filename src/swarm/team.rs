//! Team system — shared task boards with atomic claiming, dependency tracking, and mailbox.
//!
//! Teams enable structured collaboration between agents:
//! - Shared task board with priority + dependencies
//! - Atomic task claiming (no double-assignment)
//! - Dependency resolution (blocked_by)
//! - Team mailbox for peer messaging

use anyhow::{bail, Result};
use std::sync::Arc;

use super::models::{Team, TeamMember, TeamMessage, TeamTask};
use super::storage::SwarmStorage;

/// Team manager — handles team lifecycle and task coordination
pub struct TeamManager {
    storage: Arc<dyn SwarmStorage>,
}

impl TeamManager {
    pub fn new(storage: Arc<dyn SwarmStorage>) -> Self {
        Self { storage }
    }

    // === Team Lifecycle ===

    /// Create a new team with a lead agent
    pub async fn create_team(&self, name: &str, lead_agent_id: &str) -> Result<Team> {
        // Verify lead agent exists
        if self.storage.get_agent(lead_agent_id).await?.is_none() {
            bail!("Lead agent '{}' not found", lead_agent_id);
        }

        // Check name uniqueness
        if self.storage.get_team_by_name(name).await?.is_some() {
            bail!("Team '{}' already exists", name);
        }

        let team = Team::new(name.to_string(), lead_agent_id.to_string());
        self.storage.create_team(&team).await?;

        // Add lead as team member with "lead" role
        let member = TeamMember::new(
            team.team_id.clone(),
            lead_agent_id.to_string(),
            "lead".to_string(),
        );
        self.storage.add_team_member(&member).await?;

        Ok(team)
    }

    /// Add a member to a team
    pub async fn add_member(&self, team_id: &str, agent_id: &str, role: &str) -> Result<()> {
        if self.storage.get_team(team_id).await?.is_none() {
            bail!("Team '{}' not found", team_id);
        }
        if self.storage.get_agent(agent_id).await?.is_none() {
            bail!("Agent '{}' not found", agent_id);
        }

        let member = TeamMember::new(team_id.to_string(), agent_id.to_string(), role.to_string());
        self.storage.add_team_member(&member).await?;
        Ok(())
    }

    /// Remove a member from a team
    pub async fn remove_member(&self, team_id: &str, agent_id: &str) -> Result<()> {
        self.storage.remove_team_member(team_id, agent_id).await
    }

    /// List team members
    pub async fn list_members(&self, team_id: &str) -> Result<Vec<TeamMember>> {
        self.storage.get_team_members(team_id).await
    }

    /// Member counts for many teams in one query (avoids N+1 listing).
    pub async fn member_counts_for(
        &self,
        team_ids: &[String],
    ) -> Result<std::collections::HashMap<String, usize>> {
        let grouped = self.storage.list_team_members_for_teams(team_ids).await?;
        Ok(grouped
            .into_iter()
            .map(|(id, members)| (id, members.len()))
            .collect())
    }

    /// Get team by name
    pub async fn get_team_by_name(&self, name: &str) -> Result<Option<Team>> {
        self.storage.get_team_by_name(name).await
    }

    /// List all teams
    pub async fn list_teams(&self) -> Result<Vec<Team>> {
        self.storage.list_teams().await
    }

    // === Task Board ===

    async fn has_unresolved_dependencies(
        &self,
        blocked_by: &[String],
        ignore_task_id: Option<&str>,
    ) -> Result<bool> {
        for blocker_id in blocked_by {
            if Some(blocker_id.as_str()) == ignore_task_id {
                continue;
            }
            if let Some(blocker) = self.storage.get_team_task(blocker_id).await? {
                if blocker.status != "done" {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    /// Create a task on the team board
    pub async fn create_task(
        &self,
        team_id: &str,
        subject: &str,
        description: Option<&str>,
        priority: i32,
        blocked_by: Vec<String>,
    ) -> Result<TeamTask> {
        if self.storage.get_team(team_id).await?.is_none() {
            bail!("Team '{}' not found", team_id);
        }

        // Verify blocker tasks exist
        if !blocked_by.is_empty() {
            let found_tasks = self.storage.get_team_tasks_by_ids(&blocked_by).await?;
            let found_ids: std::collections::HashSet<_> =
                found_tasks.into_iter().map(|t| t.task_id).collect();
            for blocker_id in &blocked_by {
                if !found_ids.contains(blocker_id) {
                    bail!("Blocker task '{}' not found", blocker_id);
                }
            }
        }

        let mut task = TeamTask::new(team_id.to_string(), subject.to_string())
            .with_priority(priority)
            .with_blocked_by(blocked_by);

        if let Some(desc) = description {
            task = task.with_description(desc.to_string());
        }

        self.storage.create_team_task(&task).await?;
        Ok(task)
    }

    /// Atomically claim a task (returns false if already claimed)
    pub async fn claim_task(&self, task_id: &str, agent_id: &str) -> Result<bool> {
        // Verify agent is a member of the task's team
        let task = self
            .storage
            .get_team_task(task_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Task '{}' not found", task_id))?;

        let members = self.storage.get_team_members(&task.team_id).await?;
        if !members.iter().any(|m| m.agent_id == agent_id) {
            bail!(
                "Agent '{}' is not a member of team '{}'",
                agent_id,
                task.team_id
            );
        }

        // Check if task is blocked
        if task.status == "blocked"
            && self
                .has_unresolved_dependencies(&task.blocked_by, None)
                .await?
        {
            bail!("Task '{}' is blocked by incomplete dependencies", task_id);
        }

        self.storage.claim_team_task(task_id, agent_id).await
    }

    /// Complete a task with a result
    pub async fn complete_task(&self, task_id: &str, result: &str) -> Result<()> {
        self.storage.complete_team_task(task_id, result).await?;

        // Check if completing this task unblocks others
        let task = self
            .storage
            .get_team_task(task_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Task '{}' not found after completion", task_id))?;

        let blocked = self.storage.get_blocked_tasks(&task.team_id).await?;
        for bt in blocked {
            if bt.blocked_by.contains(&task_id.to_string())
                && !self
                    .has_unresolved_dependencies(&bt.blocked_by, Some(task_id))
                    .await?
            {
                // Unblock the task (move from blocked to pending)
                self.storage.unblock_task(&bt.task_id).await?;
            }
        }

        Ok(())
    }

    /// Fail a task
    pub async fn fail_task(&self, task_id: &str, error: &str) -> Result<()> {
        self.storage.fail_team_task(task_id, error).await
    }

    /// List tasks for a team, optionally filtered by status
    pub async fn list_tasks(&self, team_id: &str, status: Option<&str>) -> Result<Vec<TeamTask>> {
        self.storage.list_team_tasks(team_id, status).await
    }

    /// Get tasks that are ready to be claimed (no unresolved dependencies)
    pub async fn get_ready_tasks(&self, team_id: &str) -> Result<Vec<TeamTask>> {
        self.storage.get_ready_tasks(team_id).await
    }

    /// Search tasks by keyword
    pub async fn search_tasks(&self, team_id: &str, query: &str) -> Result<Vec<TeamTask>> {
        let all = self.storage.list_team_tasks(team_id, None).await?;
        let q = query.to_lowercase();
        Ok(all
            .into_iter()
            .filter(|t| {
                t.subject.to_lowercase().contains(&q)
                    || t.description
                        .as_deref()
                        .unwrap_or("")
                        .to_lowercase()
                        .contains(&q)
            })
            .collect())
    }

    // === Mailbox ===

    /// Send a message to the team (broadcast or directed)
    pub async fn send_message(
        &self,
        team_id: &str,
        from_agent_id: &str,
        content: &str,
        to_agent_id: Option<&str>,
        message_type: &str,
    ) -> Result<TeamMessage> {
        let mut msg = TeamMessage::new(
            team_id.to_string(),
            from_agent_id.to_string(),
            content.to_string(),
        )
        .with_type(message_type.to_string());
        if let Some(to) = to_agent_id {
            msg = msg.directed(to.to_string());
        }
        self.storage.send_team_message(&msg).await?;
        Ok(msg)
    }

    /// Read team messages
    pub async fn get_messages(&self, team_id: &str, limit: usize) -> Result<Vec<TeamMessage>> {
        self.storage.get_team_messages(team_id, limit).await
    }

    /// Get unread messages for an agent
    pub async fn get_unread(&self, team_id: &str, agent_id: &str) -> Result<Vec<TeamMessage>> {
        self.storage.get_unread_messages(team_id, agent_id).await
    }

    /// Mark a message as read
    pub async fn mark_read(&self, message_id: &str) -> Result<()> {
        self.storage.mark_message_read(message_id).await
    }
}
