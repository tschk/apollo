//! Agent execution modes — coding mode, swarm mode.

use std::path::PathBuf;

use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::channels::{Channel, IncomingMessage, OutgoingMessage};

/// Agent execution mode.
#[derive(Debug, Clone, Default)]
pub enum AgentMode {
    /// Auto: heuristic-based classification (default).
    ///
    /// Short conversational messages skip planning. Longer or tool-requiring
    /// messages go through the Planning → Executing → Summarizing state machine.
    #[default]
    Auto,

    /// Bypass all permission checks and approval steps — agent runs fully autonomously.
    ///
    /// Equivalent to Claude Code's `--dangerously-skip-permissions`. The agent:
    /// - Never pauses for plan approval
    /// - Executes all tool calls without confirmation
    /// - Defaults to the heavy model (Opus) for maximum capability
    /// - Always plans before executing
    ///
    /// Only enable this in trusted, sandboxed environments or when you explicitly
    /// want autonomous unattended execution.
    BypassPermissions,

    /// Coding mode: optimized for software development.
    ///
    /// - Always plans before executing
    /// - Prefers the heavy model (Opus) for execution
    /// - Injects coding-specific rules into the system prompt
    /// - When `plan_approval` is enabled, previews the plan to the user
    ///   and waits for explicit confirmation before executing
    Coding {
        plan_approval: bool,
        project_path: Option<PathBuf>,
    },

    /// Swarm mode: deploy parallel coding agents.
    ///
    /// The planner decomposes the task, then spawns up to `parallelism`
    /// concurrent AgentRunner workers. Results are merged into a summary.
    Swarm { parallelism: usize },
}

/// Maps stored `agent.permission_profile` to the initial runtime mode.
pub fn agent_mode_from_permission_profile(profile: &str) -> AgentMode {
    let p = profile.trim().to_ascii_lowercase().replace(['-', ' '], "_");
    match p.as_str() {
        "full" => AgentMode::BypassPermissions,
        "prompt" => AgentMode::Coding {
            plan_approval: true,
            project_path: None,
        },
        _ => AgentMode::Auto,
    }
}

impl AgentMode {
    pub fn is_coding(&self) -> bool {
        matches!(self, Self::Coding { .. })
    }

    pub fn is_swarm(&self) -> bool {
        matches!(self, Self::Swarm { .. })
    }

    /// Whether all permission checks and approval steps should be skipped.
    /// True for `BypassPermissions` and `Coding { plan_approval: false }`.
    pub fn bypass_permissions(&self) -> bool {
        matches!(self, Self::BypassPermissions)
    }

    /// Whether to show a plan preview and wait for user approval before executing.
    /// Always false in `BypassPermissions` mode even if `plan_approval` was set.
    pub fn plan_approval(&self) -> bool {
        if self.bypass_permissions() {
            return false;
        }
        matches!(
            self,
            Self::Coding {
                plan_approval: true,
                ..
            }
        )
    }

    pub fn swarm_parallelism(&self) -> usize {
        match self {
            Self::Swarm { parallelism } => *parallelism,
            _ => 1,
        }
    }

    /// System prompt section to inject when this mode is active.
    pub fn system_prompt_injection(&self) -> Option<&'static str> {
        match self {
            Self::BypassPermissions => Some(BYPASS_PERMISSIONS_PROMPT),
            Self::Coding { .. } => Some(CODING_MODE_PROMPT),
            Self::Swarm { .. } => Some(SWARM_MODE_PROMPT),
            Self::Auto => None,
        }
    }

    /// Whether to prefer the heavy model for execution.
    pub fn prefer_heavy_model(&self) -> bool {
        matches!(
            self,
            Self::BypassPermissions | Self::Coding { .. } | Self::Swarm { .. }
        )
    }

    /// Whether to always plan (skip the conversational shortcut).
    pub fn always_plan(&self) -> bool {
        matches!(
            self,
            Self::BypassPermissions | Self::Coding { .. } | Self::Swarm { .. }
        )
    }
}

/// No-op channel for headless swarm worker agents.
/// All operations are discarded — only the return value of `handle_message` is used.
pub struct NullChannel {
    name: String,
}

impl NullChannel {
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }
}

#[async_trait]
impl Channel for NullChannel {
    fn name(&self) -> &str {
        &self.name
    }

    async fn start(&mut self) -> anyhow::Result<mpsc::Receiver<IncomingMessage>> {
        let (_tx, rx) = mpsc::channel(1);
        Ok(rx) // tx dropped immediately — receiver never yields messages
    }

    async fn send(&self, _message: OutgoingMessage) -> anyhow::Result<Option<String>> {
        Ok(None)
    }

    async fn stop(&mut self) -> anyhow::Result<()> {
        Ok(())
    }
}

const BYPASS_PERMISSIONS_PROMPT: &str = "\
## Autonomous Mode (Bypass Permissions)

You are running with all permission checks disabled. You have full autonomy to:
- Execute shell commands without confirmation
- Read and write any files in the workspace
- Make and push commits autonomously
- Install dependencies and run builds

Operate decisively. Do not ask for permission before taking actions. \
Complete the task end-to-end and report what was done.";

const CODING_MODE_PROMPT: &str = "\
## Coding Mode Active

You are in coding mode. Follow these rules:
- Read files before editing them
- Make surgical, minimal changes — don't reformat code you don't touch
- After edits, verify correctness (cargo build, npm test, or equivalent)
- Prefer targeted `edit` operations over full file rewrites
- For multi-file changes: enumerate all affected files first, then execute in order
- Diagnose root causes before retrying failures
- Commit at natural checkpoints with clear, descriptive messages
- Never break working code — test incrementally";

const SWARM_MODE_PROMPT: &str = "\
## Swarm Deployment Mode Active

You are coordinating parallel coding agents. Follow these rules:
- Decompose the goal into independent, non-overlapping subtasks
- Assign each agent a clear, bounded scope (a file, module, or feature)
- Avoid shared-state conflicts between agents
- Validate and integrate each agent's output before merging
- Reassign or handle failed tasks yourself
- Report the outcome of each agent with status and a brief summary";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_mode_from_permission_profile() {
        assert!(matches!(
            agent_mode_from_permission_profile("full"),
            AgentMode::BypassPermissions
        ));
        assert!(matches!(
            agent_mode_from_permission_profile("  FULL  "),
            AgentMode::BypassPermissions
        ));
        assert!(matches!(
            agent_mode_from_permission_profile("prompt"),
            AgentMode::Coding {
                plan_approval: true,
                project_path: None
            }
        ));
        assert!(matches!(
            agent_mode_from_permission_profile(" ProMpT\n"),
            AgentMode::Coding {
                plan_approval: true,
                project_path: None
            }
        ));
        assert!(matches!(
            agent_mode_from_permission_profile(""),
            AgentMode::Auto
        ));
        assert!(matches!(
            agent_mode_from_permission_profile("unknown-profile"),
            AgentMode::Auto
        ));
    }
}
