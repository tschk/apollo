//! Toolset classification and filtering.

use crate::config::ToolsetConfig;

pub fn toolset_for_tool(name: &str) -> &'static str {
    match name {
        "exec" => "runtime",
        "Read" | "Write" | "Edit" => "fs",
        "web_search" | "web_fetch" | "browser" => "web",
        "memory_search" | "memory_get" | "session_search" | "brain_search" | "brain_query"
        | "brain_put" | "brain_get" => "memory",
        "list_models" | "doctor" => "sessions",
        "message" => "messaging",
        "skill_manager" => "skills",
        "mcp" | "create_tool" | "list_custom_tools" | "vibemania" => "advanced",
        _ => "misc",
    }
}

pub fn is_tool_enabled(name: &str, config: &ToolsetConfig) -> bool {
    let toolset = toolset_for_tool(name);
    let enabled = config.enabled.is_empty()
        || config
            .enabled
            .iter()
            .any(|entry| entry == name || entry == toolset);
    let disabled = config
        .disabled
        .iter()
        .any(|entry| entry == name || entry == toolset);
    enabled && !disabled
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toolset_filtering_allows_by_group() {
        let cfg = ToolsetConfig {
            enabled: vec!["memory".to_string()],
            disabled: Vec::new(),
        };
        assert!(is_tool_enabled("memory_search", &cfg));
        assert!(!is_tool_enabled("exec", &cfg));
    }
}
