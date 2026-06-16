//! Tool call guardrails — loop detection, failure counting, idempotent/mutating classification.
//!
//! Port from hermes-agent `tool_guardrails.py`. Tracks per-turn tool-call observations
//! and returns guardrail decisions.
//!
//! pontytail: single-threaded, in-memory history. Per-chat isolation is handled by
//! the HashMap<String, ToolGuardrails> in loop_runner.

use std::time::Instant;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Tools safe to retry (read-only, no side effects)
pub const IDEMPOTENT_TOOLS: &[&str] = &[
    "read",
    "file_ops",
    "web_search",
    "web_fetch",
    "search_files",
    "session_status",
    "list_models",
    "memory_search",
    "tool_search",
    "brief",
    "sleep_tool",
];

/// Tools that mutate state (dangerous to repeat blindly)
pub const MUTATING_TOOLS: &[&str] = &[
    "shell",
    "write",
    "edit",
    "vibemania",
    "coding_swarm",
    "message",
    "todo_write",
    "cron_tool",
    "config_tool",
    "mode_switch",
    "skill_manager",
];

/// A single tool call record for guardrail analysis.
#[derive(Debug, Clone)]
pub struct ToolCallRecord {
    pub name: String,
    pub arguments_hash: String,
    pub timestamp: Instant,
    pub is_error: bool,
}

/// Guardrail configuration thresholds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuardrailConfig {
    pub warnings_enabled: bool,
    pub hard_stop_enabled: bool,
    /// Consecutive errors on same tool before warning
    pub exact_failure_warn_after: usize,
    /// Consecutive errors on same tool before blocking
    pub exact_failure_block_after: usize,
    /// Repeated identical calls before warning
    pub same_tool_failure_warn_after: usize,
    /// Repeated identical calls before halting
    pub same_tool_failure_halt_after: usize,
    /// No-progress warnings
    pub no_progress_warn_after: usize,
    /// No-progress hard stop
    pub no_progress_block_after: usize,
}

impl Default for GuardrailConfig {
    fn default() -> Self {
        Self {
            warnings_enabled: true,
            hard_stop_enabled: false,
            exact_failure_warn_after: 2,
            exact_failure_block_after: 5,
            same_tool_failure_warn_after: 3,
            same_tool_failure_halt_after: 8,
            no_progress_warn_after: 2,
            no_progress_block_after: 5,
        }
    }
}

/// Decision returned by the guardrail after observing a tool call.
#[derive(Debug, Clone)]
pub enum GuardrailDecision {
    /// Proceed normally
    Proceed,
    /// Warn the LLM but allow execution
    Warn(String),
    /// Hard stop - return this response to the user
    Stop(String),
}

/// Tool call guardrails — pure analysis, no side effects.
pub struct ToolGuardrails {
    config: GuardrailConfig,
    history: Vec<ToolCallRecord>,
    identical_call_count: usize,
    last_call_identity: Option<(String, String)>,
}

impl ToolGuardrails {
    pub fn new(config: GuardrailConfig) -> Self {
        Self {
            config,
            history: Vec::with_capacity(64),
            identical_call_count: 0,
            last_call_identity: None,
        }
    }

    /// Observe a tool call and return the guardrail decision.
    pub fn observe(&mut self, name: &str, args: &str, _output: &str, is_error: bool) -> GuardrailDecision {
        let hash = hash_args(args);
        let identity = (name.to_string(), hash.clone());

        // Track identical consecutive calls
        if Some(&identity) == self.last_call_identity.as_ref() {
            self.identical_call_count += 1;
        } else {
            self.identical_call_count = 0;
            self.last_call_identity = Some(identity);
        }

        // Loop detection: identical consecutive calls
        if self.identical_call_count >= self.config.same_tool_failure_halt_after
            && self.config.hard_stop_enabled
        {
            tracing::warn!(
                "[guardrails] Stop: {} called identically {} times",
                name,
                self.identical_call_count + 1
            );
            return GuardrailDecision::Stop(format!(
                "Same tool call '{}' repeated {} times — loop detected. Breaking.",
                name,
                self.identical_call_count + 1
            ));
        }
        if self.config.warnings_enabled
            && self.identical_call_count >= self.config.same_tool_failure_warn_after
        {
            tracing::warn!(
                "[guardrails] Warn: {} repeated {} times",
                name,
                self.identical_call_count + 1
            );
            return GuardrailDecision::Warn(format!(
                "WARNING: You're calling '{}' identically ({}x). Stop and answer with what you have.",
                name,
                self.identical_call_count + 1
            ));
        }

        // Error counting
        if is_error {
            let error_count = self
                .history
                .iter()
                .rev()
                .take_while(|r| r.name == name && r.is_error)
                .count()
                + 1;

            if self.config.hard_stop_enabled
                && error_count >= self.config.exact_failure_block_after
            {
                tracing::warn!(
                    "[guardrails] Stop: {} failed {} consecutive times",
                    name,
                    error_count
                );
                return GuardrailDecision::Stop(format!(
                    "Tool '{}' failed {} consecutive times — blocking further attempts.",
                    name,
                    error_count
                ));
            }
            if self.config.warnings_enabled
                && error_count >= self.config.exact_failure_warn_after
            {
                tracing::warn!(
                    "[guardrails] Warn: {} failed {} times",
                    name,
                    error_count
                );
                return GuardrailDecision::Warn(format!(
                    "WARNING: Tool '{}' keeps failing ({} consecutive errors). Try a different approach.",
                    name,
                    error_count
                ));
            }
        }

        // Record the call
        self.history.push(ToolCallRecord {
            name: name.to_string(),
            arguments_hash: hash,
            timestamp: Instant::now(),
            is_error,
        });

        GuardrailDecision::Proceed
    }

    /// Check if a tool is idempotent (safe to retry).
    pub fn is_idempotent(name: &str) -> bool {
        IDEMPOTENT_TOOLS.contains(&name)
    }

    /// Check if a tool is mutating (has side effects).
    pub fn is_mutating(name: &str) -> bool {
        MUTATING_TOOLS.contains(&name)
    }

    /// Reset for a new turn.
    pub fn reset(&mut self) {
        self.history.clear();
        self.identical_call_count = 0;
        self.last_call_identity = None;
    }
}

fn hash_args(args: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(args.as_bytes());
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_allow_first_call() {
        let mut g = ToolGuardrails::new(GuardrailConfig::default());
        let decision = g.observe("read", r#"{"path":"test"}"#, "", false);
        assert!(matches!(decision, GuardrailDecision::Proceed));
    }

    #[test]
    fn test_loop_warn() {
        let mut config = GuardrailConfig::default();
        config.same_tool_failure_warn_after = 3;
        let mut g = ToolGuardrails::new(config);

        for _ in 0..3 {
            let _ = g.observe("shell", r#"{"command":"ls"}"#, "", false);
        }
        let decision = g.observe("shell", r#"{"command":"ls"}"#, "", false);
        assert!(matches!(decision, GuardrailDecision::Warn(_)));
    }

    #[test]
    fn test_loop_stop() {
        let mut config = GuardrailConfig::default();
        config.hard_stop_enabled = true;
        config.same_tool_failure_halt_after = 3;
        let mut g = ToolGuardrails::new(config);

        for _ in 0..3 {
            let _ = g.observe("shell", r#"{"command":"ls"}"#, "", false);
        }
        let decision = g.observe("shell", r#"{"command":"ls"}"#, "", false);
        assert!(matches!(decision, GuardrailDecision::Stop(_)));
    }

    #[test]
    fn test_reset() {
        let mut config = GuardrailConfig::default();
        config.same_tool_failure_warn_after = 3;
        let mut g = ToolGuardrails::new(config);

        for _ in 0..4 {
            let _ = g.observe("read", r#"{"path":"x"}"#, "", false);
        }

        g.reset();
        let decision = g.observe("read", r#"{"path":"x"}"#, "", false);
        assert!(matches!(decision, GuardrailDecision::Proceed));
    }

    #[test]
    fn test_different_args_no_loop() {
        let mut config = GuardrailConfig::default();
        config.same_tool_failure_warn_after = 3;
        let mut g = ToolGuardrails::new(config);

        let _ = g.observe("shell", r#"{"command":"ls"}"#, "", false);
        let decision = g.observe("shell", r#"{"command":"pwd"}"#, "", false);
        assert!(matches!(decision, GuardrailDecision::Proceed));
    }

    #[test]
    fn test_error_detection() {
        let mut config = GuardrailConfig::default();
        config.exact_failure_warn_after = 2;
        let mut g = ToolGuardrails::new(config);

        let _ = g.observe("shell", r#"{"command":"ls"}"#, "", true);
        let decision = g.observe("shell", r#"{"command":"ls"}"#, "", true);
        assert!(matches!(decision, GuardrailDecision::Warn(_)));
    }

    #[test]
    fn test_idempotent_classification() {
        assert!(ToolGuardrails::is_idempotent("read"));
        assert!(ToolGuardrails::is_idempotent("web_search"));
        assert!(!ToolGuardrails::is_idempotent("shell"));
        assert!(ToolGuardrails::is_mutating("shell"));
        assert!(ToolGuardrails::is_mutating("write"));
        assert!(!ToolGuardrails::is_mutating("read"));
    }
}
