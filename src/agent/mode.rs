//! Agent execution modes — coding mode, swarm mode, and plan approval.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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

/// A plan waiting for user approval before execution.
#[derive(Debug, Clone)]
pub struct PendingPlan {
    pub plan: String,
    pub original_message: String,
    pub preferred_model: String,
    pub created_at: Instant,
}

impl PendingPlan {
    const TTL: Duration = Duration::from_secs(30 * 60); // 30 minutes

    pub fn is_expired(&self) -> bool {
        self.created_at.elapsed() > Self::TTL
    }
}

pub type PendingPlans = Arc<Mutex<HashMap<String, PendingPlan>>>;

/// Returns true if `text` is an explicit approval ("go", "yes", "ok", etc.)
pub fn is_approval(text: &str) -> bool {
    let lower = text.trim().to_lowercase();
    matches!(
        lower.as_str(),
        "yes"
            | "y"
            | "go"
            | "ok"
            | "okay"
            | "proceed"
            | "approve"
            | "do it"
            | "looks good"
            | "lgtm"
            | "go ahead"
            | "continue"
            | "run it"
            | "execute"
    ) || lower.starts_with("yes,")
        || lower.starts_with("go ahead")
        || lower.starts_with("looks good")
        || lower.starts_with("lgtm")
}

/// Returns true if `text` is an explicit rejection ("no", "cancel", "abort", etc.)
pub fn is_rejection(text: &str) -> bool {
    let lower = text.trim().to_lowercase();
    matches!(
        lower.as_str(),
        "no" | "n" | "nope" | "cancel" | "abort" | "stop" | "nevermind" | "never mind"
    ) || lower.starts_with("no,")
        || lower.starts_with("cancel")
        || lower.starts_with("stop ")
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

    #[test]
    fn test_is_approval() {
        // Exact matches
        assert!(is_approval("yes"));
        assert!(is_approval("y"));
        assert!(is_approval("go"));
        assert!(is_approval("ok"));
        assert!(is_approval("okay"));
        assert!(is_approval("proceed"));
        assert!(is_approval("approve"));
        assert!(is_approval("do it"));
        assert!(is_approval("looks good"));
        assert!(is_approval("lgtm"));
        assert!(is_approval("go ahead"));
        assert!(is_approval("continue"));
        assert!(is_approval("run it"));
        assert!(is_approval("execute"));

        // Case insensitivity
        assert!(is_approval("YES"));
        assert!(is_approval("OkAy"));
        assert!(is_approval("PROCEED"));
        assert!(is_approval("LGTM"));

        // Leading/trailing whitespace
        assert!(is_approval("  yes  "));
        assert!(is_approval("\tgo ahead\n"));

        // Prefix matches
        assert!(is_approval("yes, do it"));
        assert!(is_approval("go ahead and run"));
        assert!(is_approval("looks good to me"));
        assert!(is_approval("lgtm, go for it"));

        // Negative cases
        assert!(!is_approval("no"));
        assert!(!is_approval("yep"));
        assert!(!is_approval("sure"));
        assert!(!is_approval(""));
        assert!(!is_approval("   "));
        assert!(!is_approval("go ahea")); // not an exact match or prefix
    }

    #[test]
    fn test_is_rejection() {
        // Exact matches
        assert!(is_rejection("no"));
        assert!(is_rejection("n"));
        assert!(is_rejection("nope"));
        assert!(is_rejection("cancel"));
        assert!(is_rejection("abort"));
        assert!(is_rejection("stop"));
        assert!(is_rejection("nevermind"));
        assert!(is_rejection("never mind"));

        // Case insensitivity
        assert!(is_rejection("NO"));
        assert!(is_rejection("Cancel"));
        assert!(is_rejection("ABORT"));
        assert!(is_rejection("STOP"));

        // Leading/trailing whitespace
        assert!(is_rejection("  no  "));
        assert!(is_rejection("\tabort\n"));

        // Prefix matches
        assert!(is_rejection("no, don't do that"));
        assert!(is_rejection("cancel the operation"));
        assert!(is_rejection("stop executing"));

        // Negative cases
        assert!(!is_rejection("yes"));
        assert!(!is_rejection("not really"));
        assert!(!is_rejection(""));
        assert!(!is_rejection("   "));
        assert!(!is_rejection("stopit")); // starts_with("stop ") has a space
    }
}
