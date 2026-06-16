//! Shared runtime policy controls for privileged capabilities.

use crate::config::PolicyConfig;

#[derive(Debug, Clone)]
pub struct ExecutionPolicy {
    pub allow_shell: bool,
    pub allow_dynamic_tools: bool,
    pub allow_plugin_shell: bool,
    pub allow_plugin_git: bool,
}

impl Default for ExecutionPolicy {
    fn default() -> Self {
        Self {
            allow_shell: true,
            allow_dynamic_tools: true,
            allow_plugin_shell: false,
            allow_plugin_git: false,
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
        }
    }

    pub fn deny(message: &str) -> anyhow::Result<crate::tools::ToolResult> {
        Ok(crate::tools::ToolResult::error(message))
    }
}
