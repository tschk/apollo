use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Agent link — permission for delegation between agents
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentLink {
    pub link_id: String,
    pub source_agent_id: String,
    pub target_agent_id: String,
    /// "outbound" | "inbound" | "bidirectional"
    pub direction: LinkDirection,
    pub max_concurrent: u32,
    pub settings: Option<serde_json::Value>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LinkDirection {
    Outbound,
    Inbound,
    Bidirectional,
}

impl std::fmt::Display for LinkDirection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LinkDirection::Outbound => write!(f, "outbound"),
            LinkDirection::Inbound => write!(f, "inbound"),
            LinkDirection::Bidirectional => write!(f, "bidirectional"),
        }
    }
}

impl AgentLink {
    pub fn new(source: String, target: String, direction: LinkDirection) -> Self {
        Self {
            link_id: uuid::Uuid::new_v4().to_string(),
            source_agent_id: source,
            target_agent_id: target,
            direction,
            max_concurrent: 3,
            settings: None,
            created_at: Utc::now(),
        }
    }

    pub fn with_max_concurrent(mut self, max: u32) -> Self {
        self.max_concurrent = max;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_link_builder() {
        let link = AgentLink::new(
            "source1".to_string(),
            "target1".to_string(),
            LinkDirection::Outbound,
        )
        .with_max_concurrent(5);
        assert_eq!(link.source_agent_id, "source1");
        assert_eq!(link.target_agent_id, "target1");
        assert_eq!(link.direction, LinkDirection::Outbound);
        assert_eq!(link.max_concurrent, 5);
    }

    #[test]
    fn test_delegation_record_builder() {
        let record = DelegationRecord::new(
            "source1".to_string(),
            "target1".to_string(),
            "task1".to_string(),
            DelegationMode::Async,
        )
        .with_context("context1".to_string());

        assert_eq!(record.source_agent_id, "source1");
        assert_eq!(record.target_agent_id, "target1");
        assert_eq!(record.task, "task1");
        assert_eq!(record.mode, DelegationMode::Async);
        assert_eq!(record.context, Some("context1".to_string()));
    }

    #[test]
    fn test_team_task_builder() {
        // Test with blocks
        let task = TeamTask::new("team1".to_string(), "subject1".to_string())
            .with_description("desc1".to_string())
            .with_priority(10)
            .with_blocked_by(vec!["task2".to_string()]);

        assert_eq!(task.team_id, "team1");
        assert_eq!(task.subject, "subject1");
        assert_eq!(task.description, Some("desc1".to_string()));
        assert_eq!(task.priority, 10);
        assert_eq!(task.blocked_by, vec!["task2".to_string()]);
        assert_eq!(task.status, "blocked");

        // Test without blocks (status should remain pending)
        let task2 =
            TeamTask::new("team1".to_string(), "subject1".to_string()).with_blocked_by(vec![]);
        assert_eq!(task2.status, "pending");
    }

    #[test]
    fn test_team_message_builder() {
        let msg = TeamMessage::new(
            "team1".to_string(),
            "from1".to_string(),
            "content1".to_string(),
        )
        .directed("to1".to_string())
        .with_type("alert".to_string());

        assert_eq!(msg.team_id, "team1");
        assert_eq!(msg.from_agent_id, "from1");
        assert_eq!(msg.content, "content1");
        assert_eq!(msg.to_agent_id, Some("to1".to_string()));
        assert_eq!(msg.message_type, "alert");
    }

    #[test]
    fn test_handoff_route_builder() {
        let route = HandoffRoute::new(
            "channel1".to_string(),
            "chat1".to_string(),
            "from_key1".to_string(),
            "to_key1".to_string(),
        )
        .with_context("ctx1".to_string());

        assert_eq!(route.channel, "channel1");
        assert_eq!(route.chat_id, "chat1");
        assert_eq!(route.from_agent_key, "from_key1");
        assert_eq!(route.to_agent_key, "to_key1");
        assert_eq!(route.context, Some("ctx1".to_string()));
    }
}

/// Delegation record — tracks a delegation request and its outcome
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegationRecord {
    pub delegation_id: String,
    pub source_agent_id: String,
    pub target_agent_id: String,
    pub task: String,
    /// "sync" | "async"
    pub mode: DelegationMode,
    /// "pending" | "running" | "completed" | "failed" | "cancelled"
    pub status: String,
    pub result: Option<String>,
    pub context: Option<String>,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DelegationMode {
    Sync,
    Async,
}

impl std::fmt::Display for DelegationMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DelegationMode::Sync => write!(f, "sync"),
            DelegationMode::Async => write!(f, "async"),
        }
    }
}

impl DelegationRecord {
    pub fn new(source: String, target: String, task: String, mode: DelegationMode) -> Self {
        Self {
            delegation_id: uuid::Uuid::new_v4().to_string(),
            source_agent_id: source,
            target_agent_id: target,
            task,
            mode,
            status: "pending".to_string(),
            result: None,
            context: None,
            created_at: Utc::now(),
            completed_at: None,
        }
    }

    pub fn with_context(mut self, context: String) -> Self {
        self.context = Some(context);
        self
    }
}

/// Team
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Team {
    pub team_id: String,
    pub name: String,
    pub lead_agent_id: String,
    /// "active" | "paused" | "disbanded"
    pub status: String,
    pub settings: Option<serde_json::Value>,
    pub created_at: DateTime<Utc>,
}

impl Team {
    pub fn new(name: String, lead_agent_id: String) -> Self {
        Self {
            team_id: uuid::Uuid::new_v4().to_string(),
            name,
            lead_agent_id,
            status: "active".to_string(),
            settings: None,
            created_at: Utc::now(),
        }
    }
}

/// Team member
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamMember {
    pub team_id: String,
    pub agent_id: String,
    /// "lead" | "member" | "observer"
    pub role: String,
    pub joined_at: DateTime<Utc>,
}

impl TeamMember {
    pub fn new(team_id: String, agent_id: String, role: String) -> Self {
        Self {
            team_id,
            agent_id,
            role,
            joined_at: Utc::now(),
        }
    }
}

/// Team task with dependency tracking
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamTask {
    pub task_id: String,
    pub team_id: String,
    pub subject: String,
    pub description: Option<String>,
    /// "pending" | "claimed" | "done" | "failed" | "blocked"
    pub status: String,
    pub owner_agent_id: Option<String>,
    /// Task IDs that must complete before this task can start
    pub blocked_by: Vec<String>,
    pub priority: i32,
    pub result: Option<String>,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl TeamTask {
    pub fn new(team_id: String, subject: String) -> Self {
        let now = Utc::now();
        Self {
            task_id: uuid::Uuid::new_v4().to_string(),
            team_id,
            subject,
            description: None,
            status: "pending".to_string(),
            owner_agent_id: None,
            blocked_by: vec![],
            priority: 0,
            result: None,
            error: None,
            created_at: now,
            updated_at: now,
        }
    }

    pub fn with_description(mut self, desc: String) -> Self {
        self.description = Some(desc);
        self
    }

    pub fn with_priority(mut self, priority: i32) -> Self {
        self.priority = priority;
        self
    }

    pub fn with_blocked_by(mut self, deps: Vec<String>) -> Self {
        self.blocked_by = deps;
        if !self.blocked_by.is_empty() {
            self.status = "blocked".to_string();
        }
        self
    }
}

/// Team message (mailbox)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamMessage {
    pub message_id: String,
    pub team_id: String,
    pub from_agent_id: String,
    /// None = broadcast to all team members
    pub to_agent_id: Option<String>,
    pub content: String,
    /// "chat" | "status_update" | "task_result" | "alert"
    pub message_type: String,
    pub read: bool,
    pub created_at: DateTime<Utc>,
}

impl TeamMessage {
    pub fn new(team_id: String, from: String, content: String) -> Self {
        Self {
            message_id: uuid::Uuid::new_v4().to_string(),
            team_id,
            from_agent_id: from,
            to_agent_id: None,
            content,
            message_type: "chat".to_string(),
            read: false,
            created_at: Utc::now(),
        }
    }

    pub fn directed(mut self, to: String) -> Self {
        self.to_agent_id = Some(to);
        self
    }

    pub fn with_type(mut self, msg_type: String) -> Self {
        self.message_type = msg_type;
        self
    }
}

/// Handoff route — overrides message routing
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandoffRoute {
    pub route_id: String,
    pub channel: String,
    pub chat_id: String,
    pub from_agent_key: String,
    pub to_agent_key: String,
    pub context: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl HandoffRoute {
    pub fn new(channel: String, chat_id: String, from: String, to: String) -> Self {
        Self {
            route_id: uuid::Uuid::new_v4().to_string(),
            channel,
            chat_id,
            from_agent_key: from,
            to_agent_key: to,
            context: None,
            created_at: Utc::now(),
        }
    }

    pub fn with_context(mut self, context: String) -> Self {
        self.context = Some(context);
        self
    }
}
