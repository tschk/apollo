use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use std::path::Path;
use surrealdb::engine::local::RocksDb;
use surrealdb::Surreal;

use super::models::*;

/// Trait for swarm storage backends
#[async_trait]
pub trait SwarmStorage: Send + Sync {
    // === Tasks (legacy + team tasks) ===
    async fn store_task(&self, task: &super::Task) -> Result<()>;
    async fn get_task(&self, task_id: &str) -> Result<Option<super::Task>>;
    async fn list_pending_tasks(&self) -> Result<Vec<super::Task>>;
    async fn update_task_status(&self, task_id: &str, status: super::TaskStatus) -> Result<()>;

    // === Agents ===
    async fn register_agent(&self, agent: &super::AgentInfo) -> Result<()>;
    async fn get_agent(&self, agent_id: &str) -> Result<Option<super::AgentInfo>>;
    async fn get_agent_by_name(&self, name: &str) -> Result<Option<super::AgentInfo>>;
    async fn list_active_agents(&self) -> Result<Vec<super::AgentInfo>>;
    async fn list_all_agents(&self) -> Result<Vec<super::AgentInfo>>;
    async fn update_agent_heartbeat(&self, agent_id: &str) -> Result<()>;
    async fn update_agent_status(&self, agent_id: &str, status: &str) -> Result<()>;
    async fn upsert_agent(&self, agent: &super::AgentInfo) -> Result<()>;

    // === Agent Links (delegation permissions) ===
    async fn create_agent_link(&self, link: &AgentLink) -> Result<()>;
    async fn get_agent_links(&self, agent_id: &str) -> Result<Vec<AgentLink>>;
    async fn check_link_permission(&self, source: &str, target: &str) -> Result<Option<AgentLink>>;
    async fn delete_agent_link(&self, source: &str, target: &str) -> Result<()>;

    // === Delegation History ===
    async fn record_delegation(&self, record: &DelegationRecord) -> Result<()>;
    async fn get_delegation(&self, delegation_id: &str) -> Result<Option<DelegationRecord>>;
    async fn update_delegation_status(
        &self,
        delegation_id: &str,
        status: &str,
        result: Option<String>,
    ) -> Result<()>;
    async fn list_active_delegations(&self, agent_id: &str) -> Result<Vec<DelegationRecord>>;
    async fn count_active_delegations_for_link(&self, source: &str, target: &str) -> Result<usize>;
    async fn count_active_delegations_for_agent(&self, agent_id: &str) -> Result<usize>;

    // === Teams ===
    async fn create_team(&self, team: &Team) -> Result<()>;
    async fn get_team(&self, team_id: &str) -> Result<Option<Team>>;
    async fn get_team_by_name(&self, name: &str) -> Result<Option<Team>>;
    async fn list_teams(&self) -> Result<Vec<Team>>;
    async fn add_team_member(&self, member: &TeamMember) -> Result<()>;
    async fn get_team_members(&self, team_id: &str) -> Result<Vec<TeamMember>>;
    async fn list_team_members_for_teams(
        &self,
        team_ids: &[String],
    ) -> Result<std::collections::HashMap<String, Vec<TeamMember>>>;
    async fn remove_team_member(&self, team_id: &str, agent_id: &str) -> Result<()>;

    // === Team Tasks ===
    async fn create_team_task(&self, task: &TeamTask) -> Result<()>;
    async fn get_team_task(&self, task_id: &str) -> Result<Option<TeamTask>>;
    async fn list_team_tasks(&self, team_id: &str, status: Option<&str>) -> Result<Vec<TeamTask>>;
    async fn claim_team_task(&self, task_id: &str, agent_id: &str) -> Result<bool>;
    async fn complete_team_task(&self, task_id: &str, result: &str) -> Result<()>;
    async fn fail_team_task(&self, task_id: &str, error: &str) -> Result<()>;
    async fn get_blocked_tasks(&self, team_id: &str) -> Result<Vec<TeamTask>>;
    async fn get_ready_tasks(&self, team_id: &str) -> Result<Vec<TeamTask>>;
    async fn unblock_task(&self, task_id: &str) -> Result<()>;

    // === Team Messages ===
    async fn send_team_message(&self, msg: &TeamMessage) -> Result<()>;
    async fn get_team_messages(&self, team_id: &str, limit: usize) -> Result<Vec<TeamMessage>>;
    async fn get_unread_messages(&self, team_id: &str, agent_id: &str) -> Result<Vec<TeamMessage>>;
    async fn mark_message_read(&self, message_id: &str) -> Result<()>;

    // === Handoff Routes ===
    async fn set_handoff_route(&self, route: &HandoffRoute) -> Result<()>;
    async fn get_handoff_route(&self, channel: &str, chat_id: &str)
        -> Result<Option<HandoffRoute>>;
    async fn clear_handoff_route(&self, channel: &str, chat_id: &str) -> Result<()>;
    async fn list_handoff_routes(&self) -> Result<Vec<HandoffRoute>>;
}

/// SurrealDB backend (distributed state)
pub struct SurrealBackend {
    db: Surreal<surrealdb::engine::local::Db>,
}

impl SurrealBackend {
    pub async fn new<P: AsRef<Path>>(path: P) -> Result<Self> {
        let db = Surreal::new::<RocksDb>(path.as_ref()).await?;
        db.use_ns("claw").use_db("swarm").await?;

        // Initialize full schema
        db.query(SCHEMA_SQL).await?;

        Ok(Self { db })
    }

    /// Get raw DB handle for advanced queries
    pub fn db(&self) -> &Surreal<surrealdb::engine::local::Db> {
        &self.db
    }
}

const SCHEMA_SQL: &str = r#"
    -- Legacy tasks table (backward compat)
    DEFINE TABLE IF NOT EXISTS tasks SCHEMALESS;
    DEFINE FIELD IF NOT EXISTS task_id ON tasks TYPE string;
    DEFINE FIELD IF NOT EXISTS title ON tasks TYPE string;
    DEFINE FIELD IF NOT EXISTS description ON tasks TYPE string;
    DEFINE FIELD IF NOT EXISTS status ON tasks TYPE string;
    DEFINE FIELD IF NOT EXISTS priority ON tasks TYPE string;
    DEFINE FIELD IF NOT EXISTS assigned_to ON tasks TYPE option<string>;
    DEFINE FIELD IF NOT EXISTS created_at ON tasks TYPE string;
    DEFINE FIELD IF NOT EXISTS updated_at ON tasks TYPE string;
    DEFINE INDEX IF NOT EXISTS task_id_idx ON tasks FIELDS task_id UNIQUE;

    -- Agents table (extended)
    DEFINE TABLE IF NOT EXISTS agents SCHEMALESS;
    DEFINE FIELD IF NOT EXISTS agent_id ON agents TYPE string;
    DEFINE FIELD IF NOT EXISTS name ON agents TYPE string;
    DEFINE FIELD IF NOT EXISTS capabilities ON agents TYPE array;
    DEFINE FIELD IF NOT EXISTS status ON agents TYPE string;
    DEFINE FIELD IF NOT EXISTS last_heartbeat ON agents TYPE string;
    DEFINE FIELD IF NOT EXISTS model ON agents TYPE option<string>;
    DEFINE FIELD IF NOT EXISTS tools ON agents TYPE option<array>;
    DEFINE FIELD IF NOT EXISTS max_concurrent ON agents TYPE int DEFAULT 5;
    DEFINE FIELD IF NOT EXISTS settings ON agents TYPE option<object>;
    DEFINE INDEX IF NOT EXISTS agent_id_idx ON agents FIELDS agent_id UNIQUE;
    DEFINE INDEX IF NOT EXISTS agent_name_idx ON agents FIELDS name UNIQUE;

    -- Agent links (delegation permissions)
    DEFINE TABLE IF NOT EXISTS agent_links SCHEMALESS;
    DEFINE FIELD IF NOT EXISTS link_id ON agent_links TYPE string;
    DEFINE FIELD IF NOT EXISTS source_agent_id ON agent_links TYPE string;
    DEFINE FIELD IF NOT EXISTS target_agent_id ON agent_links TYPE string;
    DEFINE FIELD IF NOT EXISTS direction ON agent_links TYPE string;
    DEFINE FIELD IF NOT EXISTS max_concurrent ON agent_links TYPE int DEFAULT 3;
    DEFINE FIELD IF NOT EXISTS settings ON agent_links TYPE option<object>;
    DEFINE FIELD IF NOT EXISTS created_at ON agent_links TYPE string;
    DEFINE INDEX IF NOT EXISTS link_id_idx ON agent_links FIELDS link_id UNIQUE;
    DEFINE INDEX IF NOT EXISTS link_pair_idx ON agent_links FIELDS source_agent_id, target_agent_id UNIQUE;

    -- Delegation history
    DEFINE TABLE IF NOT EXISTS delegation_history SCHEMALESS;
    DEFINE FIELD IF NOT EXISTS delegation_id ON delegation_history TYPE string;
    DEFINE FIELD IF NOT EXISTS source_agent_id ON delegation_history TYPE string;
    DEFINE FIELD IF NOT EXISTS target_agent_id ON delegation_history TYPE string;
    DEFINE FIELD IF NOT EXISTS task ON delegation_history TYPE string;
    DEFINE FIELD IF NOT EXISTS mode ON delegation_history TYPE string;
    DEFINE FIELD IF NOT EXISTS status ON delegation_history TYPE string;
    DEFINE FIELD IF NOT EXISTS result ON delegation_history TYPE option<string>;
    DEFINE FIELD IF NOT EXISTS context ON delegation_history TYPE option<string>;
    DEFINE FIELD IF NOT EXISTS created_at ON delegation_history TYPE string;
    DEFINE FIELD IF NOT EXISTS completed_at ON delegation_history TYPE option<datetime>;
    DEFINE INDEX IF NOT EXISTS delegation_id_idx ON delegation_history FIELDS delegation_id UNIQUE;
    DEFINE INDEX IF NOT EXISTS delegation_source_idx ON delegation_history FIELDS source_agent_id;
    DEFINE INDEX IF NOT EXISTS delegation_target_idx ON delegation_history FIELDS target_agent_id;

    -- Teams
    DEFINE TABLE IF NOT EXISTS teams SCHEMALESS;
    DEFINE FIELD IF NOT EXISTS team_id ON teams TYPE string;
    DEFINE FIELD IF NOT EXISTS name ON teams TYPE string;
    DEFINE FIELD IF NOT EXISTS lead_agent_id ON teams TYPE string;
    DEFINE FIELD IF NOT EXISTS status ON teams TYPE string;
    DEFINE FIELD IF NOT EXISTS settings ON teams TYPE option<object>;
    DEFINE FIELD IF NOT EXISTS created_at ON teams TYPE string;
    DEFINE INDEX IF NOT EXISTS team_id_idx ON teams FIELDS team_id UNIQUE;
    DEFINE INDEX IF NOT EXISTS team_name_idx ON teams FIELDS name UNIQUE;

    -- Team members
    DEFINE TABLE IF NOT EXISTS team_members SCHEMALESS;
    DEFINE FIELD IF NOT EXISTS team_id ON team_members TYPE string;
    DEFINE FIELD IF NOT EXISTS agent_id ON team_members TYPE string;
    DEFINE FIELD IF NOT EXISTS role ON team_members TYPE string;
    DEFINE FIELD IF NOT EXISTS joined_at ON team_members TYPE string;
    DEFINE INDEX IF NOT EXISTS team_member_idx ON team_members FIELDS team_id, agent_id UNIQUE;

    -- Team tasks (with dependency tracking)
    DEFINE TABLE IF NOT EXISTS team_tasks SCHEMALESS;
    DEFINE FIELD IF NOT EXISTS task_id ON team_tasks TYPE string;
    DEFINE FIELD IF NOT EXISTS team_id ON team_tasks TYPE string;
    DEFINE FIELD IF NOT EXISTS subject ON team_tasks TYPE string;
    DEFINE FIELD IF NOT EXISTS description ON team_tasks TYPE option<string>;
    DEFINE FIELD IF NOT EXISTS status ON team_tasks TYPE string;
    DEFINE FIELD IF NOT EXISTS owner_agent_id ON team_tasks TYPE option<string>;
    DEFINE FIELD IF NOT EXISTS blocked_by ON team_tasks TYPE array DEFAULT [];
    DEFINE FIELD IF NOT EXISTS priority ON team_tasks TYPE int DEFAULT 0;
    DEFINE FIELD IF NOT EXISTS result ON team_tasks TYPE option<string>;
    DEFINE FIELD IF NOT EXISTS error ON team_tasks TYPE option<string>;
    DEFINE FIELD IF NOT EXISTS created_at ON team_tasks TYPE string;
    DEFINE FIELD IF NOT EXISTS updated_at ON team_tasks TYPE string;
    DEFINE INDEX IF NOT EXISTS team_task_id_idx ON team_tasks FIELDS task_id UNIQUE;
    DEFINE INDEX IF NOT EXISTS team_task_team_idx ON team_tasks FIELDS team_id;
    DEFINE INDEX IF NOT EXISTS team_task_status_idx ON team_tasks FIELDS team_id, status;

    -- Team messages (mailbox)
    DEFINE TABLE IF NOT EXISTS team_messages SCHEMALESS;
    DEFINE FIELD IF NOT EXISTS message_id ON team_messages TYPE string;
    DEFINE FIELD IF NOT EXISTS team_id ON team_messages TYPE string;
    DEFINE FIELD IF NOT EXISTS from_agent_id ON team_messages TYPE string;
    DEFINE FIELD IF NOT EXISTS to_agent_id ON team_messages TYPE option<string>;
    DEFINE FIELD IF NOT EXISTS content ON team_messages TYPE string;
    DEFINE FIELD IF NOT EXISTS message_type ON team_messages TYPE string;
    DEFINE FIELD IF NOT EXISTS read ON team_messages TYPE bool DEFAULT false;
    DEFINE FIELD IF NOT EXISTS created_at ON team_messages TYPE string;
    DEFINE INDEX IF NOT EXISTS team_msg_id_idx ON team_messages FIELDS message_id UNIQUE;
    DEFINE INDEX IF NOT EXISTS team_msg_team_idx ON team_messages FIELDS team_id;

    -- Handoff routes (conversation routing override)
    DEFINE TABLE IF NOT EXISTS handoff_routes SCHEMALESS;
    DEFINE FIELD IF NOT EXISTS route_id ON handoff_routes TYPE string;
    DEFINE FIELD IF NOT EXISTS channel ON handoff_routes TYPE string;
    DEFINE FIELD IF NOT EXISTS chat_id ON handoff_routes TYPE string;
    DEFINE FIELD IF NOT EXISTS from_agent_key ON handoff_routes TYPE string;
    DEFINE FIELD IF NOT EXISTS to_agent_key ON handoff_routes TYPE string;
    DEFINE FIELD IF NOT EXISTS context ON handoff_routes TYPE option<string>;
    DEFINE FIELD IF NOT EXISTS created_at ON handoff_routes TYPE string;
    DEFINE INDEX IF NOT EXISTS handoff_route_idx ON handoff_routes FIELDS channel, chat_id UNIQUE;
"#;

#[async_trait]
impl SwarmStorage for SurrealBackend {
    // === Tasks ===
    async fn store_task(&self, task: &super::Task) -> Result<()> {
        let _: Option<super::Task> = self
            .db
            .create(("tasks", &*task.task_id))
            .content(task.clone())
            .await?;
        Ok(())
    }

    async fn get_task(&self, task_id: &str) -> Result<Option<super::Task>> {
        let mut result = self
            .db
            .query("SELECT * FROM tasks WHERE task_id = $task_id LIMIT 1")
            .bind(("task_id", task_id.to_string()))
            .await?;
        let tasks: Vec<super::Task> = result.take(0)?;
        Ok(tasks.into_iter().next())
    }

    async fn list_pending_tasks(&self) -> Result<Vec<super::Task>> {
        let mut result = self.db
            .query("SELECT * FROM tasks WHERE status = 'pending' ORDER BY priority DESC, created_at ASC")
            .await?;
        Ok(result.take(0)?)
    }

    async fn update_task_status(&self, task_id: &str, status: super::TaskStatus) -> Result<()> {
        self.db
            .query("UPDATE tasks SET status = $status, updated_at = time::now() WHERE task_id = $task_id")
            .bind(("task_id", task_id.to_string()))
            .bind(("status", status.to_string()))
            .await?;
        Ok(())
    }

    // === Agents ===
    async fn register_agent(&self, agent: &super::AgentInfo) -> Result<()> {
        let _: Option<super::AgentInfo> = self
            .db
            .create(("agents", &*agent.agent_id))
            .content(agent.clone())
            .await?;
        Ok(())
    }

    async fn get_agent(&self, agent_id: &str) -> Result<Option<super::AgentInfo>> {
        let mut result = self
            .db
            .query("SELECT * FROM agents WHERE agent_id = $agent_id LIMIT 1")
            .bind(("agent_id", agent_id.to_string()))
            .await?;
        let agents: Vec<super::AgentInfo> = result.take(0)?;
        Ok(agents.into_iter().next())
    }

    async fn get_agent_by_name(&self, name: &str) -> Result<Option<super::AgentInfo>> {
        let mut result = self
            .db
            .query("SELECT * FROM agents WHERE name = $name LIMIT 1")
            .bind(("name", name.to_string()))
            .await?;
        let agents: Vec<super::AgentInfo> = result.take(0)?;
        Ok(agents.into_iter().next())
    }

    async fn list_active_agents(&self) -> Result<Vec<super::AgentInfo>> {
        let mut result = self
            .db
            .query("SELECT * FROM agents WHERE status = 'active'")
            .await?;
        Ok(result.take(0)?)
    }

    async fn list_all_agents(&self) -> Result<Vec<super::AgentInfo>> {
        let mut result = self
            .db
            .query("SELECT * FROM agents ORDER BY name ASC")
            .await?;
        Ok(result.take(0)?)
    }

    async fn update_agent_heartbeat(&self, agent_id: &str) -> Result<()> {
        self.db
            .query("UPDATE agents SET last_heartbeat = time::now() WHERE agent_id = $agent_id")
            .bind(("agent_id", agent_id.to_string()))
            .await?;
        Ok(())
    }

    async fn update_agent_status(&self, agent_id: &str, status: &str) -> Result<()> {
        self.db
            .query("UPDATE agents SET status = $status WHERE agent_id = $agent_id")
            .bind(("agent_id", agent_id.to_string()))
            .bind(("status", status.to_string()))
            .await?;
        Ok(())
    }

    async fn upsert_agent(&self, agent: &super::AgentInfo) -> Result<()> {
        let _: Option<super::AgentInfo> = self
            .db
            .upsert(("agents", &*agent.agent_id))
            .content(agent.clone())
            .await?;
        Ok(())
    }

    // === Agent Links ===
    async fn create_agent_link(&self, link: &AgentLink) -> Result<()> {
        let _: Option<AgentLink> = self
            .db
            .create(("agent_links", &*link.link_id))
            .content(link.clone())
            .await?;
        Ok(())
    }

    async fn get_agent_links(&self, agent_id: &str) -> Result<Vec<AgentLink>> {
        let mut result = self.db
            .query("SELECT * FROM agent_links WHERE source_agent_id = $id OR (target_agent_id = $id AND direction = 'bidirectional')")
            .bind(("id", agent_id.to_string()))
            .await?;
        Ok(result.take(0)?)
    }

    async fn check_link_permission(&self, source: &str, target: &str) -> Result<Option<AgentLink>> {
        let mut result = self.db
            .query(r#"
                SELECT * FROM agent_links WHERE
                    (source_agent_id = $source AND target_agent_id = $target AND direction IN ['outbound', 'bidirectional'])
                    OR
                    (source_agent_id = $target AND target_agent_id = $source AND direction = 'bidirectional')
                LIMIT 1
            "#)
            .bind(("source", source.to_string()))
            .bind(("target", target.to_string()))
            .await?;
        let links: Vec<AgentLink> = result.take(0)?;
        Ok(links.into_iter().next())
    }

    async fn delete_agent_link(&self, source: &str, target: &str) -> Result<()> {
        self.db
            .query("DELETE FROM agent_links WHERE source_agent_id = $source AND target_agent_id = $target")
            .bind(("source", source.to_string()))
            .bind(("target", target.to_string()))
            .await?;
        Ok(())
    }

    // === Delegation History ===
    async fn record_delegation(&self, record: &DelegationRecord) -> Result<()> {
        let _: Option<DelegationRecord> = self
            .db
            .create(("delegation_history", &*record.delegation_id))
            .content(record.clone())
            .await?;
        Ok(())
    }

    async fn get_delegation(&self, delegation_id: &str) -> Result<Option<DelegationRecord>> {
        let mut result = self
            .db
            .query("SELECT * FROM delegation_history WHERE delegation_id = $id LIMIT 1")
            .bind(("id", delegation_id.to_string()))
            .await?;
        let records: Vec<DelegationRecord> = result.take(0)?;
        Ok(records.into_iter().next())
    }

    async fn update_delegation_status(
        &self,
        delegation_id: &str,
        status: &str,
        result: Option<String>,
    ) -> Result<()> {
        self.db
            .query("UPDATE delegation_history SET status = $status, result = $result, completed_at = time::now() WHERE delegation_id = $id")
            .bind(("id", delegation_id.to_string()))
            .bind(("status", status.to_string()))
            .bind(("result", result))
            .await?;
        Ok(())
    }

    async fn list_active_delegations(&self, agent_id: &str) -> Result<Vec<DelegationRecord>> {
        let mut result = self.db
            .query("SELECT * FROM delegation_history WHERE (source_agent_id = $id OR target_agent_id = $id) AND status IN ['pending', 'running']")
            .bind(("id", agent_id.to_string()))
            .await?;
        Ok(result.take(0)?)
    }

    async fn count_active_delegations_for_link(&self, source: &str, target: &str) -> Result<usize> {
        let mut result = self.db
            .query("SELECT count() AS total FROM delegation_history WHERE source_agent_id = $source AND target_agent_id = $target AND status IN ['pending', 'running'] GROUP ALL")
            .bind(("source", source.to_string()))
            .bind(("target", target.to_string()))
            .await?;
        #[derive(Deserialize)]
        struct Count {
            total: usize,
        }
        let counts: Vec<Count> = result.take(0)?;
        Ok(counts.first().map(|c| c.total).unwrap_or(0))
    }

    async fn count_active_delegations_for_agent(&self, agent_id: &str) -> Result<usize> {
        let mut result = self.db
            .query("SELECT count() AS total FROM delegation_history WHERE target_agent_id = $id AND status IN ['pending', 'running'] GROUP ALL")
            .bind(("id", agent_id.to_string()))
            .await?;
        #[derive(Deserialize)]
        struct Count {
            total: usize,
        }
        let counts: Vec<Count> = result.take(0)?;
        Ok(counts.first().map(|c| c.total).unwrap_or(0))
    }

    // === Teams ===
    async fn create_team(&self, team: &Team) -> Result<()> {
        let _: Option<Team> = self
            .db
            .create(("teams", &*team.team_id))
            .content(team.clone())
            .await?;
        Ok(())
    }

    async fn get_team(&self, team_id: &str) -> Result<Option<Team>> {
        let mut result = self
            .db
            .query("SELECT * FROM teams WHERE team_id = $id LIMIT 1")
            .bind(("id", team_id.to_string()))
            .await?;
        let teams: Vec<Team> = result.take(0)?;
        Ok(teams.into_iter().next())
    }

    async fn get_team_by_name(&self, name: &str) -> Result<Option<Team>> {
        let mut result = self
            .db
            .query("SELECT * FROM teams WHERE name = $name LIMIT 1")
            .bind(("name", name.to_string()))
            .await?;
        let teams: Vec<Team> = result.take(0)?;
        Ok(teams.into_iter().next())
    }

    async fn list_teams(&self) -> Result<Vec<Team>> {
        let mut result = self
            .db
            .query("SELECT * FROM teams ORDER BY created_at DESC")
            .await?;
        Ok(result.take(0)?)
    }

    async fn add_team_member(&self, member: &TeamMember) -> Result<()> {
        let key = format!("{}_{}", member.team_id, member.agent_id);
        let _: Option<TeamMember> = self
            .db
            .create(("team_members", &*key))
            .content(member.clone())
            .await?;
        Ok(())
    }

    async fn get_team_members(&self, team_id: &str) -> Result<Vec<TeamMember>> {
        let mut result = self
            .db
            .query("SELECT * FROM team_members WHERE team_id = $id")
            .bind(("id", team_id.to_string()))
            .await?;
        Ok(result.take(0)?)
    }

    async fn list_team_members_for_teams(
        &self,
        team_ids: &[String],
    ) -> Result<std::collections::HashMap<String, Vec<TeamMember>>> {
        use std::collections::HashMap;
        if team_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let mut result = self
            .db
            .query("SELECT * FROM team_members WHERE team_id IN $ids")
            .bind(("ids", team_ids.to_vec()))
            .await?;
        let members: Vec<TeamMember> = result.take(0)?;
        let mut map: HashMap<String, Vec<TeamMember>> = HashMap::new();
        for m in members {
            map.entry(m.team_id.clone()).or_default().push(m);
        }
        Ok(map)
    }

    async fn remove_team_member(&self, team_id: &str, agent_id: &str) -> Result<()> {
        self.db
            .query("DELETE FROM team_members WHERE team_id = $team_id AND agent_id = $agent_id")
            .bind(("team_id", team_id.to_string()))
            .bind(("agent_id", agent_id.to_string()))
            .await?;
        Ok(())
    }

    // === Team Tasks ===
    async fn create_team_task(&self, task: &TeamTask) -> Result<()> {
        let _: Option<TeamTask> = self
            .db
            .create(("team_tasks", &*task.task_id))
            .content(task.clone())
            .await?;
        Ok(())
    }

    async fn get_team_task(&self, task_id: &str) -> Result<Option<TeamTask>> {
        let mut result = self
            .db
            .query("SELECT * FROM team_tasks WHERE task_id = $id LIMIT 1")
            .bind(("id", task_id.to_string()))
            .await?;
        let tasks: Vec<TeamTask> = result.take(0)?;
        Ok(tasks.into_iter().next())
    }

    async fn list_team_tasks(&self, team_id: &str, status: Option<&str>) -> Result<Vec<TeamTask>> {
        if let Some(s) = status {
            let mut result = self.db
                .query("SELECT * FROM team_tasks WHERE team_id = $team_id AND status = $status ORDER BY priority DESC, created_at ASC")
                .bind(("team_id", team_id.to_string()))
                .bind(("status", s.to_string()))
                .await?;
            Ok(result.take(0)?)
        } else {
            let mut result = self.db
                .query("SELECT * FROM team_tasks WHERE team_id = $team_id ORDER BY priority DESC, created_at ASC")
                .bind(("team_id", team_id.to_string()))
                .await?;
            Ok(result.take(0)?)
        }
    }

    async fn claim_team_task(&self, task_id: &str, agent_id: &str) -> Result<bool> {
        // Conditional update — only claims if status is 'pending'
        let mut result = self.db
            .query("UPDATE team_tasks SET status = 'claimed', owner_agent_id = $agent_id, updated_at = time::now() WHERE task_id = $task_id AND status = 'pending'")
            .bind(("task_id", task_id.to_string()))
            .bind(("agent_id", agent_id.to_string()))
            .await?;
        let updated: Vec<TeamTask> = result.take(0)?;
        Ok(!updated.is_empty())
    }

    async fn complete_team_task(&self, task_id: &str, result_text: &str) -> Result<()> {
        self.db
            .query("UPDATE team_tasks SET status = 'done', result = $result, updated_at = time::now() WHERE task_id = $task_id")
            .bind(("task_id", task_id.to_string()))
            .bind(("result", result_text.to_string()))
            .await?;
        Ok(())
    }

    async fn fail_team_task(&self, task_id: &str, error: &str) -> Result<()> {
        self.db
            .query("UPDATE team_tasks SET status = 'failed', error = $error, updated_at = time::now() WHERE task_id = $task_id")
            .bind(("task_id", task_id.to_string()))
            .bind(("error", error.to_string()))
            .await?;
        Ok(())
    }

    async fn get_blocked_tasks(&self, team_id: &str) -> Result<Vec<TeamTask>> {
        let mut result = self.db
            .query("SELECT * FROM team_tasks WHERE team_id = $team_id AND status = 'blocked' ORDER BY priority DESC")
            .bind(("team_id", team_id.to_string()))
            .await?;
        Ok(result.take(0)?)
    }

    async fn get_ready_tasks(&self, team_id: &str) -> Result<Vec<TeamTask>> {
        // Tasks that are pending and have no incomplete blockers
        let mut result = self.db
            .query(r#"
                SELECT * FROM team_tasks WHERE team_id = $team_id AND status = 'pending'
                AND (array::len(blocked_by) = 0 OR array::len(
                    (SELECT task_id FROM team_tasks WHERE task_id IN $parent.blocked_by AND status != 'done')
                ) = 0)
                ORDER BY priority DESC, created_at ASC
            "#)
            .bind(("team_id", team_id.to_string()))
            .await?;
        Ok(result.take(0)?)
    }

    async fn unblock_task(&self, task_id: &str) -> Result<()> {
        self.db
            .query("UPDATE team_tasks SET status = 'pending', updated_at = time::now() WHERE task_id = $id AND status = 'blocked'")
            .bind(("id", task_id.to_string()))
            .await?;
        Ok(())
    }

    // === Team Messages ===
    async fn send_team_message(&self, msg: &TeamMessage) -> Result<()> {
        let _: Option<TeamMessage> = self
            .db
            .create(("team_messages", &*msg.message_id))
            .content(msg.clone())
            .await?;
        Ok(())
    }

    async fn get_team_messages(&self, team_id: &str, limit: usize) -> Result<Vec<TeamMessage>> {
        let mut result = self.db
            .query("SELECT * FROM team_messages WHERE team_id = $team_id ORDER BY created_at DESC LIMIT $limit")
            .bind(("team_id", team_id.to_string()))
            .bind(("limit", limit))
            .await?;
        Ok(result.take(0)?)
    }

    async fn get_unread_messages(&self, team_id: &str, agent_id: &str) -> Result<Vec<TeamMessage>> {
        let mut result = self.db
            .query("SELECT * FROM team_messages WHERE team_id = $team_id AND (to_agent_id = $agent_id OR to_agent_id IS NONE) AND read = false ORDER BY created_at ASC")
            .bind(("team_id", team_id.to_string()))
            .bind(("agent_id", agent_id.to_string()))
            .await?;
        Ok(result.take(0)?)
    }

    async fn mark_message_read(&self, message_id: &str) -> Result<()> {
        self.db
            .query("UPDATE team_messages SET read = true WHERE message_id = $id")
            .bind(("id", message_id.to_string()))
            .await?;
        Ok(())
    }

    // === Handoff Routes ===
    async fn set_handoff_route(&self, route: &HandoffRoute) -> Result<()> {
        // Upsert: delete existing route for this channel+chat_id, then create
        self.db
            .query("DELETE FROM handoff_routes WHERE channel = $channel AND chat_id = $chat_id")
            .bind(("channel", route.channel.clone()))
            .bind(("chat_id", route.chat_id.clone()))
            .await?;
        let _: Option<HandoffRoute> = self
            .db
            .create(("handoff_routes", &*route.route_id))
            .content(route.clone())
            .await?;
        Ok(())
    }

    async fn get_handoff_route(
        &self,
        channel: &str,
        chat_id: &str,
    ) -> Result<Option<HandoffRoute>> {
        let mut result = self.db
            .query("SELECT * FROM handoff_routes WHERE channel = $channel AND chat_id = $chat_id LIMIT 1")
            .bind(("channel", channel.to_string()))
            .bind(("chat_id", chat_id.to_string()))
            .await?;
        let routes: Vec<HandoffRoute> = result.take(0)?;
        Ok(routes.into_iter().next())
    }

    async fn clear_handoff_route(&self, channel: &str, chat_id: &str) -> Result<()> {
        self.db
            .query("DELETE FROM handoff_routes WHERE channel = $channel AND chat_id = $chat_id")
            .bind(("channel", channel.to_string()))
            .bind(("chat_id", chat_id.to_string()))
            .await?;
        Ok(())
    }

    async fn list_handoff_routes(&self) -> Result<Vec<HandoffRoute>> {
        let mut result = self
            .db
            .query("SELECT * FROM handoff_routes ORDER BY created_at DESC")
            .await?;
        Ok(result.take(0)?)
    }
}

/// RocksDB cache (local hot data)
pub struct RocksCache {
    db: rocksdb::DB,
}

impl RocksCache {
    pub fn new<P: AsRef<Path>>(path: P) -> Result<Self> {
        let mut opts = rocksdb::Options::default();
        opts.create_if_missing(true);
        opts.create_missing_column_families(true);

        let db = rocksdb::DB::open_cf(
            &opts,
            path,
            vec!["embeddings", "chunks", "sticker_cache", "agent_cache"],
        )?;

        Ok(Self { db })
    }

    pub fn get(&self, cf: &str, key: &[u8]) -> Result<Option<Vec<u8>>> {
        let cf_handle = self
            .db
            .cf_handle(cf)
            .ok_or_else(|| anyhow::anyhow!("Column family {} not found", cf))?;
        Ok(self.db.get_cf(cf_handle, key)?)
    }

    pub fn put(&self, cf: &str, key: &[u8], value: &[u8]) -> Result<()> {
        let cf_handle = self
            .db
            .cf_handle(cf)
            .ok_or_else(|| anyhow::anyhow!("Column family {} not found", cf))?;
        self.db.put_cf(cf_handle, key, value)?;
        Ok(())
    }

    pub fn delete(&self, cf: &str, key: &[u8]) -> Result<()> {
        let cf_handle = self
            .db
            .cf_handle(cf)
            .ok_or_else(|| anyhow::anyhow!("Column family {} not found", cf))?;
        self.db.delete_cf(cf_handle, key)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_rockscache_operations() {
        let dir = tempdir().unwrap();
        let cache = RocksCache::new(dir.path()).unwrap();

        let cf = "embeddings";
        let key = b"test_key";
        let value = b"test_value";

        // Test getting non-existent key
        assert_eq!(cache.get(cf, key).unwrap(), None);

        // Test putting a value
        cache.put(cf, key, value).unwrap();

        // Test getting the value
        assert_eq!(cache.get(cf, key).unwrap().unwrap(), value);

        // Test overwriting the value
        let new_value = b"new_test_value";
        cache.put(cf, key, new_value).unwrap();
        assert_eq!(cache.get(cf, key).unwrap().unwrap(), new_value);

        // Test deleting the value
        cache.delete(cf, key).unwrap();
        assert_eq!(cache.get(cf, key).unwrap(), None);
    }

    #[test]
    fn test_rockscache_invalid_cf() {
        let dir = tempdir().unwrap();
        let cache = RocksCache::new(dir.path()).unwrap();

        let cf = "invalid_cf";
        let key = b"test_key";
        let value = b"test_value";

        assert!(cache.get(cf, key).is_err());
        assert!(cache.put(cf, key, value).is_err());
        assert!(cache.delete(cf, key).is_err());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_rockscache_operations() {
        let dir = tempdir().unwrap();
        let cache = RocksCache::new(dir.path()).unwrap();

        let cf = "embeddings";
        let key = b"test_key";
        let value = b"test_value";

        // Test getting non-existent key
        assert_eq!(cache.get(cf, key).unwrap(), None);

        // Test putting a value
        cache.put(cf, key, value).unwrap();

        // Test getting the value
        assert_eq!(cache.get(cf, key).unwrap().unwrap(), value);

        // Test overwriting the value
        let new_value = b"new_test_value";
        cache.put(cf, key, new_value).unwrap();
        assert_eq!(cache.get(cf, key).unwrap().unwrap(), new_value);

        // Test deleting the value
        cache.delete(cf, key).unwrap();
        assert_eq!(cache.get(cf, key).unwrap(), None);
    }

    #[test]
    fn test_rockscache_invalid_cf() {
        let dir = tempdir().unwrap();
        let cache = RocksCache::new(dir.path()).unwrap();

        let cf = "invalid_cf";
        let key = b"test_key";
        let value = b"test_value";

        assert!(cache.get(cf, key).is_err());
        assert!(cache.put(cf, key, value).is_err());
        assert!(cache.delete(cf, key).is_err());
    }
}
