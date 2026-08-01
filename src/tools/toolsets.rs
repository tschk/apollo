//! Toolset classification and filtering.

use crate::config::ToolsetConfig;

pub const CORE_TOOLSET_GROUPS: &[&str] = &["runtime", "fs", "memory", "sessions", "misc"];

pub fn toolset_for_tool(name: &str) -> &'static str {
    match name {
        // Delegating a task to the worker runs commands on this machine, so it
        // belongs with `exec` rather than in the "misc" catch-all.
        "exec" | "telekinesis" | "build_runner" => "runtime",
        "Read" | "Write" | "Edit" => "fs",
        "web_search" | "web_fetch" | "browser" => "web",
        "memory_search" | "memory_get" | "session_search" | "brain_search" | "brain_query"
        | "brain_put" | "brain_get" => "memory",
        "doctor" => "sessions",
        "message" => "messaging",
        "skill_manager" => "skills",
        "praefectus" => "desktop",
        "mcp" | "create_tool" | "list_custom_tools" | "vibemania" => "advanced",
        "generate_image" | "text_to_speech" | "speech_to_text" => "media",
        _ => "misc",
    }
}

pub fn expand_package(name: &str) -> Vec<&'static str> {
    match name.trim().to_ascii_lowercase().as_str() {
        "web" => vec!["web"],
        "browser" => vec!["browser"],
        "skills" => vec!["skills"],
        "advanced" => vec!["advanced"],
        "desktop" => vec!["desktop"],
        "media" => vec!["media"],
        "apollo-live" | "live" => vec!["web", "browser", "skills", "advanced", "desktop", "media"],
        "core" | "default" => CORE_TOOLSET_GROUPS.to_vec(),
        _ => vec![],
    }
}

pub fn apply_package_manifest(toolsets: &mut ToolsetConfig, packages: &[String]) {
    if packages.is_empty() {
        return;
    }
    for g in CORE_TOOLSET_GROUPS {
        let s = (*g).to_string();
        if !toolsets.enabled.contains(&s) {
            toolsets.enabled.push(s);
        }
    }
    for pkg in packages {
        for g in expand_package(pkg) {
            let s = g.to_string();
            if !toolsets.enabled.contains(&s) {
                toolsets.enabled.push(s);
            }
        }
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

    #[test]
    fn package_live_enables_web() {
        let mut cfg = ToolsetConfig {
            enabled: vec![],
            disabled: Vec::new(),
        };
        apply_package_manifest(&mut cfg, &["live".to_string()]);
        assert!(is_tool_enabled("web_search", &cfg));
        assert!(cfg.enabled.iter().any(|e| e == "runtime"));
    }
}
