//! Concurrency manager — lane-based scheduling, active delegation tracking,
//! limit enforcement, and deadlock detection.
//!
//! Lanes:
//! - Main: primary agent interactions
//! - Delegate: delegated tasks from other agents
//! - Cron: scheduled background tasks

use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Execution lane types
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Lane {
    Main,
    Delegate,
    Cron,
}

impl std::fmt::Display for Lane {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Lane::Main => write!(f, "main"),
            Lane::Delegate => write!(f, "delegate"),
            Lane::Cron => write!(f, "cron"),
        }
    }
}

/// Lane configuration
#[derive(Debug, Clone)]
pub struct LaneConfig {
    pub max_concurrent: usize,
    pub priority: u8,
}

impl Default for LaneConfig {
    fn default() -> Self {
        Self {
            max_concurrent: 1,
            priority: 0,
        }
    }
}

/// Active execution slot
#[derive(Debug, Clone)]
pub struct ExecutionSlot {
    pub slot_id: String,
    pub agent_id: String,
    pub lane: Lane,
    pub description: String,
    pub started_at: DateTime<Utc>,
    /// Optional: agent this task is waiting on
    pub waiting_on: Option<String>,
}

/// Concurrency scheduler
pub struct ConcurrencyScheduler {
    lane_configs: HashMap<Lane, LaneConfig>,
    active_slots: Arc<RwLock<Vec<ExecutionSlot>>>,
    /// Tracks which agents are waiting on which other agents (for deadlock detection)
    wait_graph: Arc<RwLock<HashMap<String, String>>>,
}

impl Default for ConcurrencyScheduler {
    fn default() -> Self {
        Self::new()
    }
}

impl ConcurrencyScheduler {
    pub fn new() -> Self {
        let mut configs = HashMap::new();
        configs.insert(
            Lane::Main,
            LaneConfig {
                max_concurrent: 3,
                priority: 2,
            },
        );
        configs.insert(
            Lane::Delegate,
            LaneConfig {
                max_concurrent: 5,
                priority: 1,
            },
        );
        configs.insert(
            Lane::Cron,
            LaneConfig {
                max_concurrent: 2,
                priority: 0,
            },
        );

        Self {
            lane_configs: configs,
            active_slots: Arc::new(RwLock::new(Vec::new())),
            wait_graph: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Configure a lane
    pub fn configure_lane(&mut self, lane: Lane, config: LaneConfig) {
        self.lane_configs.insert(lane, config);
    }

    /// Try to acquire an execution slot. Returns None if lane is full.
    pub async fn acquire_slot(
        &self,
        agent_id: &str,
        lane: Lane,
        description: &str,
    ) -> Option<String> {
        let config = self.lane_configs.get(&lane)?;
        let mut slots = self.active_slots.write().await;

        let lane_count = slots.iter().filter(|s| s.lane == lane).count();
        if lane_count >= config.max_concurrent {
            return None;
        }

        let slot_id = uuid::Uuid::new_v4().to_string();
        slots.push(ExecutionSlot {
            slot_id: slot_id.clone(),
            agent_id: agent_id.to_string(),
            lane,
            description: description.to_string(),
            started_at: Utc::now(),
            waiting_on: None,
        });

        Some(slot_id)
    }

    /// Release an execution slot
    pub async fn release_slot(&self, slot_id: &str) {
        let mut slots = self.active_slots.write().await;
        slots.retain(|s| s.slot_id != slot_id);

        // Clean up wait graph
        let mut graph = self.wait_graph.write().await;
        graph.retain(|_, v| v != slot_id);
    }

    /// Mark that an agent is waiting on another agent (for delegation)
    pub async fn set_waiting(&self, slot_id: &str, waiting_on_agent: &str) {
        let mut slots = self.active_slots.write().await;
        if let Some(slot) = slots.iter_mut().find(|s| s.slot_id == slot_id) {
            slot.waiting_on = Some(waiting_on_agent.to_string());
        }

        let mut graph = self.wait_graph.write().await;
        graph.insert(slot_id.to_string(), waiting_on_agent.to_string());
    }

    /// Clear waiting status
    pub async fn clear_waiting(&self, slot_id: &str) {
        let mut slots = self.active_slots.write().await;
        if let Some(slot) = slots.iter_mut().find(|s| s.slot_id == slot_id) {
            slot.waiting_on = None;
        }

        let mut graph = self.wait_graph.write().await;
        graph.remove(slot_id);
    }

    /// Detect deadlocks in the wait graph
    /// Returns cycles as Vec<Vec<String>> (each cycle is a list of agent IDs)
    pub async fn detect_deadlocks(&self) -> Vec<Vec<String>> {
        let slots = self.active_slots.read().await;

        // Build agent -> waiting_on_agent map
        let wait_map: HashMap<String, String> = slots
            .iter()
            .filter_map(|slot| {
                slot.waiting_on
                    .as_ref()
                    .map(|waiting_on| (slot.agent_id.clone(), waiting_on.clone()))
            })
            .collect();

        find_cycles(&wait_map)
    }

    /// Get current state for monitoring
    pub async fn get_status(&self) -> SchedulerStatus {
        let slots = self.active_slots.read().await;
        let mut lane_usage = HashMap::new();

        for (lane, config) in &self.lane_configs {
            let active = slots.iter().filter(|s| s.lane == *lane).count();
            lane_usage.insert(*lane, (active, config.max_concurrent));
        }

        drop(slots); // Release lock before calling detect_deadlocks
        let deadlocks = self.detect_deadlocks().await;

        SchedulerStatus {
            lane_usage,
            active_slots: self.active_slots.read().await.clone(),
            deadlocks,
        }
    }

    /// Get active slot count for a lane
    pub async fn lane_count(&self, lane: Lane) -> usize {
        let slots = self.active_slots.read().await;
        slots.iter().filter(|s| s.lane == lane).count()
    }
}

/// Find cycles in a wait graph using DFS
fn find_cycles(wait_map: &HashMap<String, String>) -> Vec<Vec<String>> {
    let mut cycles = Vec::new();
    let mut visited = std::collections::HashSet::new();
    let mut path = Vec::new();
    let mut path_set = std::collections::HashSet::new();

    for start in wait_map.keys() {
        if visited.contains(start) {
            continue;
        }

        path.clear();
        path_set.clear();

        path.push(start);
        path_set.insert(start);
        let mut current = start;

        while let Some(next) = wait_map.get(current) {
            if path_set.contains(&next) {
                // Found a cycle
                let cycle_start = path.iter().position(|&p| p == next).unwrap();
                cycles.push(path[cycle_start..].iter().map(|&s| s.clone()).collect());
                break;
            }
            if visited.contains(&next) {
                break;
            }
            path.push(next);
            path_set.insert(next);
            current = next;
        }

        for p in &path {
            visited.insert(*p);
        }
    }

    cycles
}

/// Scheduler status for monitoring
#[derive(Debug)]
pub struct SchedulerStatus {
    pub lane_usage: HashMap<Lane, (usize, usize)>, // (active, max)
    pub active_slots: Vec<ExecutionSlot>,
    pub deadlocks: Vec<Vec<String>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_cycles_no_cycles() {
        let mut wait_map = HashMap::new();
        wait_map.insert("A".to_string(), "B".to_string());
        wait_map.insert("B".to_string(), "C".to_string());

        let cycles = find_cycles(&wait_map);
        assert!(cycles.is_empty());
    }

    #[test]
    fn test_find_cycles_single_cycle() {
        let mut wait_map = HashMap::new();
        wait_map.insert("A".to_string(), "B".to_string());
        wait_map.insert("B".to_string(), "C".to_string());
        wait_map.insert("C".to_string(), "A".to_string());

        let cycles = find_cycles(&wait_map);
        assert_eq!(cycles.len(), 1);
        let cycle = &cycles[0];
        assert_eq!(cycle.len(), 3);
        assert!(cycle.contains(&"A".to_string()));
        assert!(cycle.contains(&"B".to_string()));
        assert!(cycle.contains(&"C".to_string()));
    }

    #[test]
    fn test_find_cycles_disconnected_cycles() {
        let mut wait_map = HashMap::new();
        // Cycle 1
        wait_map.insert("A".to_string(), "B".to_string());
        wait_map.insert("B".to_string(), "A".to_string());
        // Cycle 2
        wait_map.insert("C".to_string(), "D".to_string());
        wait_map.insert("D".to_string(), "E".to_string());
        wait_map.insert("E".to_string(), "C".to_string());

        let cycles = find_cycles(&wait_map);
        assert_eq!(cycles.len(), 2);
    }
}
