//! Integration tests for the multi-agent swarm system.
//! Run with: cargo test --features swarm --test swarm_integration

#![cfg(feature = "swarm")]

use apollo::swarm::models::*;
use apollo::swarm::scheduler::{ConcurrencyScheduler, Lane};
use apollo::swarm::{
    AgentCapability, SurrealBackend, SwarmCoordinator, SwarmStorage, TaskPriority,
};
use std::sync::Arc;

async fn setup_storage() -> Arc<dyn SwarmStorage> {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("test_swarm.db");
    // Leak the tempdir so it doesn't get cleaned up while storage is active
    std::mem::forget(tmp);
    Arc::new(SurrealBackend::new(&path).await.unwrap())
}

async fn setup_coordinator() -> SwarmCoordinator {
    let storage = setup_storage().await;
    let coordinator = SwarmCoordinator::new(storage);
    coordinator.init().await.unwrap();
    coordinator
}

// === Agent Registry ===

#[tokio::test]
async fn test_register_and_list_agents() {
    let coordinator = setup_coordinator().await;

    let id1 = coordinator
        .register_agent(
            "coder".to_string(),
            vec![AgentCapability::Coding],
            Some("claude-sonnet-4-5".to_string()),
            Some(vec!["exec".to_string(), "Read".to_string()]),
        )
        .await
        .unwrap();

    let _id2 = coordinator
        .register_agent(
            "researcher".to_string(),
            vec![AgentCapability::Research],
            Some("claude-sonnet-4-5".to_string()),
            None,
        )
        .await
        .unwrap();

    let agents = coordinator.list_all_agents().await.unwrap();
    assert_eq!(agents.len(), 2);

    let coder = coordinator
        .get_agent_by_name("coder")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(coder.agent_id, id1);
    assert_eq!(coder.model, Some("claude-sonnet-4-5".to_string()));
    assert!(coder.tools.is_some());
}

// === Delegation ===

#[tokio::test]
async fn test_delegation_permission_check() {
    let coordinator = setup_coordinator().await;

    let id1 = coordinator
        .register_agent(
            "alice".to_string(),
            vec![AgentCapability::Coding],
            None,
            None,
        )
        .await
        .unwrap();
    let id2 = coordinator
        .register_agent(
            "bob".to_string(),
            vec![AgentCapability::Research],
            None,
            None,
        )
        .await
        .unwrap();

    // No link yet — delegation should fail
    let result = coordinator
        .delegation
        .delegate(
            "alice",
            "bob",
            "research this topic",
            DelegationMode::Sync,
            None,
        )
        .await;
    assert!(result.is_err());

    // Create link
    coordinator
        .delegation
        .create_link(&id1, &id2, LinkDirection::Outbound, 3)
        .await
        .unwrap();

    // Now delegation should succeed
    let record = coordinator
        .delegation
        .delegate(
            "alice",
            "bob",
            "research this topic",
            DelegationMode::Sync,
            None,
        )
        .await
        .unwrap();
    assert_eq!(record.status, "running");

    // Complete it
    coordinator
        .delegation
        .complete_delegation(&record.delegation_id, "Found the answer".to_string())
        .await
        .unwrap();

    let active = coordinator.delegation.list_active(&id1).await.unwrap();
    assert_eq!(active.len(), 0);
}

#[tokio::test]
async fn test_delegation_concurrency_limit() {
    let coordinator = setup_coordinator().await;

    let id1 = coordinator
        .register_agent(
            "sender".to_string(),
            vec![AgentCapability::Coding],
            None,
            None,
        )
        .await
        .unwrap();
    let id2 = coordinator
        .register_agent(
            "receiver".to_string(),
            vec![AgentCapability::Research],
            None,
            None,
        )
        .await
        .unwrap();

    // Create link with max_concurrent = 2
    coordinator
        .delegation
        .create_link(&id1, &id2, LinkDirection::Outbound, 2)
        .await
        .unwrap();

    // First two should succeed
    let _d1 = coordinator
        .delegation
        .delegate("sender", "receiver", "task 1", DelegationMode::Async, None)
        .await
        .unwrap();
    let _d2 = coordinator
        .delegation
        .delegate("sender", "receiver", "task 2", DelegationMode::Async, None)
        .await
        .unwrap();

    // Third should fail (concurrency limit)
    let result = coordinator
        .delegation
        .delegate("sender", "receiver", "task 3", DelegationMode::Async, None)
        .await;
    assert!(result.is_err());
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("concurrency limit"));
}

// === Teams ===

#[tokio::test]
async fn test_team_creation_and_membership() {
    let coordinator = setup_coordinator().await;

    let lead_id = coordinator
        .register_agent(
            "lead".to_string(),
            vec![AgentCapability::Coding],
            None,
            None,
        )
        .await
        .unwrap();
    let member_id = coordinator
        .register_agent(
            "worker".to_string(),
            vec![AgentCapability::Testing],
            None,
            None,
        )
        .await
        .unwrap();

    let team = coordinator
        .teams
        .create_team("security-audit", &lead_id)
        .await
        .unwrap();

    coordinator
        .teams
        .add_member(&team.team_id, &member_id, "member")
        .await
        .unwrap();

    let members = coordinator.teams.list_members(&team.team_id).await.unwrap();
    assert_eq!(members.len(), 2); // lead + worker

    let teams = coordinator.teams.list_teams().await.unwrap();
    assert_eq!(teams.len(), 1);
    assert_eq!(teams[0].name, "security-audit");
}

#[tokio::test]
async fn test_team_task_claiming() {
    let coordinator = setup_coordinator().await;

    let lead_id = coordinator
        .register_agent(
            "lead2".to_string(),
            vec![AgentCapability::Coding],
            None,
            None,
        )
        .await
        .unwrap();
    let worker_id = coordinator
        .register_agent(
            "worker2".to_string(),
            vec![AgentCapability::Coding],
            None,
            None,
        )
        .await
        .unwrap();

    let team = coordinator
        .teams
        .create_team("dev-team", &lead_id)
        .await
        .unwrap();
    coordinator
        .teams
        .add_member(&team.team_id, &worker_id, "member")
        .await
        .unwrap();

    // Create task
    let task = coordinator
        .teams
        .create_task(
            &team.team_id,
            "Fix auth bug",
            Some("Critical auth bypass"),
            5,
            vec![],
        )
        .await
        .unwrap();

    // Claim it
    let claimed = coordinator
        .teams
        .claim_task(&task.task_id, &worker_id)
        .await
        .unwrap();
    assert!(claimed);

    // Second claim should fail (already claimed)
    let claimed2 = coordinator
        .teams
        .claim_task(&task.task_id, &lead_id)
        .await
        .unwrap();
    assert!(!claimed2);

    // Complete it
    coordinator
        .teams
        .complete_task(&task.task_id, "Fixed the bug")
        .await
        .unwrap();
}

#[tokio::test]
async fn test_team_task_dependencies() {
    let coordinator = setup_coordinator().await;

    let lead_id = coordinator
        .register_agent(
            "lead3".to_string(),
            vec![AgentCapability::Coding],
            None,
            None,
        )
        .await
        .unwrap();

    let team = coordinator
        .teams
        .create_team("build-team", &lead_id)
        .await
        .unwrap();

    // Create task A
    let task_a = coordinator
        .teams
        .create_task(&team.team_id, "Build foundation", None, 5, vec![])
        .await
        .unwrap();

    // Create task B blocked by A
    let task_b = coordinator
        .teams
        .create_task(
            &team.team_id,
            "Build walls",
            None,
            3,
            vec![task_a.task_id.clone()],
        )
        .await
        .unwrap();

    assert_eq!(task_b.status, "blocked");

    // Trying to claim B should fail
    let result = coordinator
        .teams
        .claim_task(&task_b.task_id, &lead_id)
        .await;
    assert!(result.is_err());

    // Complete A — B should auto-unblock
    coordinator
        .teams
        .claim_task(&task_a.task_id, &lead_id)
        .await
        .unwrap();
    coordinator
        .teams
        .complete_task(&task_a.task_id, "Foundation done")
        .await
        .unwrap();

    // Now B should be claimable
    let claimed = coordinator
        .teams
        .claim_task(&task_b.task_id, &lead_id)
        .await
        .unwrap();
    assert!(claimed);
}

// === Team Messages ===

#[tokio::test]
async fn test_team_messaging() {
    let coordinator = setup_coordinator().await;

    let id1 = coordinator
        .register_agent(
            "alice2".to_string(),
            vec![AgentCapability::Coding],
            None,
            None,
        )
        .await
        .unwrap();
    let id2 = coordinator
        .register_agent(
            "bob2".to_string(),
            vec![AgentCapability::Research],
            None,
            None,
        )
        .await
        .unwrap();

    let team = coordinator
        .teams
        .create_team("chat-team", &id1)
        .await
        .unwrap();
    coordinator
        .teams
        .add_member(&team.team_id, &id2, "member")
        .await
        .unwrap();

    // Broadcast
    coordinator
        .teams
        .send_message(&team.team_id, &id1, "Hello team!", None, "chat")
        .await
        .unwrap();

    // Directed
    coordinator
        .teams
        .send_message(&team.team_id, &id1, "Hey Bob", Some(&id2), "chat")
        .await
        .unwrap();

    let messages = coordinator
        .teams
        .get_messages(&team.team_id, 10)
        .await
        .unwrap();
    assert_eq!(messages.len(), 2);

    let unread = coordinator
        .teams
        .get_unread(&team.team_id, &id2)
        .await
        .unwrap();
    assert!(!unread.is_empty());
}

// === Handoff ===

#[tokio::test]
async fn test_handoff_routing() {
    let coordinator = setup_coordinator().await;

    let _id1 = coordinator
        .register_agent(
            "agent_a".to_string(),
            vec![AgentCapability::Coding],
            None,
            None,
        )
        .await
        .unwrap();
    let _id2 = coordinator
        .register_agent(
            "agent_b".to_string(),
            vec![AgentCapability::Communication],
            None,
            None,
        )
        .await
        .unwrap();

    // Handoff
    let route = coordinator
        .handoffs
        .handoff(
            "telegram",
            "12345",
            "agent_a",
            "agent_b",
            Some("User needs help with billing".to_string()),
        )
        .await
        .unwrap();
    assert_eq!(route.to_agent_key, "agent_b");

    // Resolve
    let resolved = coordinator
        .handoffs
        .resolve_agent("telegram", "12345", "agent_a")
        .await
        .unwrap();
    assert_eq!(resolved, "agent_b");

    // No handoff for different chat
    let resolved2 = coordinator
        .handoffs
        .resolve_agent("telegram", "99999", "agent_a")
        .await
        .unwrap();
    assert_eq!(resolved2, "agent_a");

    // Return control
    coordinator
        .handoffs
        .return_control("telegram", "12345")
        .await
        .unwrap();
    let resolved3 = coordinator
        .handoffs
        .resolve_agent("telegram", "12345", "agent_a")
        .await
        .unwrap();
    assert_eq!(resolved3, "agent_a");
}

// === Concurrency Scheduler ===

#[tokio::test]
async fn test_scheduler_lanes() {
    let scheduler = ConcurrencyScheduler::new();

    // Acquire slots
    let s1 = scheduler
        .acquire_slot("agent1", Lane::Main, "handle message")
        .await;
    assert!(s1.is_some());

    let s2 = scheduler
        .acquire_slot("agent2", Lane::Main, "handle message 2")
        .await;
    assert!(s2.is_some());

    let s3 = scheduler
        .acquire_slot("agent3", Lane::Main, "handle message 3")
        .await;
    assert!(s3.is_some());

    // 4th should fail (Main lane max = 3)
    let s4 = scheduler
        .acquire_slot("agent4", Lane::Main, "handle message 4")
        .await;
    assert!(s4.is_none());

    // Release one
    scheduler.release_slot(&s1.unwrap()).await;

    // Now should succeed
    let s5 = scheduler
        .acquire_slot("agent4", Lane::Main, "handle message 4")
        .await;
    assert!(s5.is_some());
}

#[tokio::test]
async fn test_deadlock_detection() {
    let scheduler = ConcurrencyScheduler::new();

    // Create circular wait: A waits on B, B waits on A
    let slot_a = scheduler
        .acquire_slot("agent_a", Lane::Delegate, "task a")
        .await
        .unwrap();
    let slot_b = scheduler
        .acquire_slot("agent_b", Lane::Delegate, "task b")
        .await
        .unwrap();

    scheduler.set_waiting(&slot_a, "agent_b").await;
    scheduler.set_waiting(&slot_b, "agent_a").await;

    let deadlocks = scheduler.detect_deadlocks().await;
    assert!(!deadlocks.is_empty(), "Should detect circular dependency");
}

// === Legacy Task Queue ===

#[tokio::test]
async fn test_legacy_task_submission() {
    let coordinator = setup_coordinator().await;

    let task_id = coordinator
        .submit_task(
            "Test task".to_string(),
            "Do something".to_string(),
            TaskPriority::High,
        )
        .await
        .unwrap();

    let tasks = coordinator.list_pending_tasks().await.unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].task_id, task_id);
}
