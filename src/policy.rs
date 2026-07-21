//! Shared runtime policy controls for privileged capabilities.

use crate::config::PolicyConfig;

#[derive(Debug, Clone)]
pub struct ExecutionPolicy {
    pub allow_shell: bool,
    pub allow_dynamic_tools: bool,
    pub allow_plugin_shell: bool,
    pub allow_plugin_git: bool,
    pub allow_computer_use: bool,
}

impl Default for ExecutionPolicy {
    fn default() -> Self {
        Self {
            allow_shell: true,
            allow_dynamic_tools: true,
            allow_plugin_shell: true,
            allow_plugin_git: true,
            allow_computer_use: true,
        }
    }
}

impl ExecutionPolicy {
    pub fn from_config(config: &PolicyConfig) -> Self {
        Self {
            allow_shell: config.allow_shell,
            allow_dynamic_tools: config.allow_dynamic_tools,
            allow_plugin_shell: config.allow_plugin_shell,
            allow_plugin_git: config.allow_plugin_git,
            allow_computer_use: config.allow_computer_use,
        }
    }

    pub fn deny(message: &str) -> anyhow::Result<crate::tools::ToolResult> {
        Ok(crate::tools::ToolResult::error(message))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_execution_policy_defaults_to_allow_shell() {
        let policy = ExecutionPolicy::default();
        assert!(policy.allow_shell);
    }

    #[test]
    fn test_execution_policy_defaults_to_allow_dynamic_tools() {
        let policy = ExecutionPolicy::default();
        assert!(policy.allow_dynamic_tools);
    }

    #[test]
    fn test_execution_policy_defaults_to_allow_computer_use() {
        let policy = ExecutionPolicy::default();
        assert!(policy.allow_computer_use);
    }
}
