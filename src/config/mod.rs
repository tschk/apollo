//! Configuration management.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub provider: ProviderConfig,
    pub embeddings: EmbeddingsConfig,
    pub agent: AgentConfig,
    pub model: String,
    pub system_prompt: String,
    pub workspace: PathBuf,
    pub storage: StorageConfig,
    pub runtime: RuntimeConfig,
    pub hosting: HostingConfig,
    pub observability: ObservabilityConfig,
    pub channel: ChannelConfig,
    pub gateway: GatewayConfig,
    pub policy: PolicyConfig,
    pub plugin_layer: PluginLayerConfig,
    pub group_chat: GroupChatConfig,
    pub toolsets: ToolsetConfig,
    pub memory: MemoryIdeasConfig,
    #[serde(default)]
    pub rs_gbrain: RsGbrainConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderConfig {
    pub name: String,
    pub api_key: Option<String>,
    pub base_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EmbeddingsConfig {
    pub enabled: bool,
    pub provider: String,
    pub api_key: Option<String>,
    pub model: Option<String>,
    pub base_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentConfig {
    /// Hard circuit breaker (absolute max execution rounds)
    pub max_rounds: usize,
    /// Max conversation history (prevents context overflow)
    pub max_history_messages: usize,
    /// Max chars for a single tool result
    pub max_tool_result_chars: usize,
    /// Max context chars before triggering mid-loop compaction
    pub max_context_chars: usize,
    /// Fast/cheap model for planning + summarization
    pub fast_model: String,
    /// Heavy model for complex coding/reasoning
    pub heavy_model: String,
    /// Per-tool allow/deny rules
    pub permissions: PermissionRulesConfig,
    /// Initial safety profile: `full`, `auto`, `prompt`, or `tools_only` (drives default `AgentMode`).
    pub permission_profile: String,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_rounds: 50,
            max_history_messages: 10,
            max_tool_result_chars: 20_000,
            max_context_chars: 150_000,
            fast_model: "claude-haiku-4-5-20251001".to_string(),
            heavy_model: "claude-sonnet-4-6".to_string(),
            permissions: PermissionRulesConfig::default(),
            permission_profile: "auto".to_string(),
        }
    }
}

/// Per-tool permission rules.
///
/// `deny` blocks matching tools outright (checked first).
/// `allow` restricts to only those tools when non-empty (allowlist mode).
/// Supports exact tool names and glob-style `*` wildcards.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PermissionRulesConfig {
    /// Tools that are always blocked (e.g. `["exec", "shell"]`).
    pub deny: Vec<String>,
    /// If non-empty, only these tools are allowed (allowlist mode).
    pub allow: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RuntimeConfig {
    pub kind: String, // "native", "docker"
    pub docker_image: Option<String>,
    pub memory_limit_mb: Option<u64>,
    pub state_path: Option<PathBuf>,
    pub self_update: SelfUpdateConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SelfUpdateConfig {
    pub enabled: bool,
    pub interval_secs: u64,
    pub remote: String,
    pub branch: String,
    pub restart_service: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct StorageConfig {
    pub backend: String, // "surreal"
    pub root: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HostingConfig {
    pub enabled: bool,
    pub tenant_root: PathBuf,
    pub session_timeout_minutes: u64,
    pub default_channel: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ObservabilityConfig {
    pub service_name: String,
    pub environment: String,
    pub json_logs: bool,
    pub trace_header_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ChannelConfig {
    pub kind: String, // "cli", "telegram", "discord", "websocket"
    pub token: Option<String>,
    /// When non-empty, only these Telegram (or channel) chat IDs may send inbound messages.
    pub allowed_chat_ids: Vec<String>,
    /// When non-empty, only these sender user IDs may send inbound messages.
    pub allowed_sender_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GatewayConfig {
    pub bind: String,
    pub auth_token: Option<String>,
    pub enable_admin_api: bool,
    pub request_body_limit_kb: usize,
    pub request_timeout_secs: u64,
    pub rate_limit_per_minute: usize,
    pub trusted_proxies: Vec<String>,
    pub allowed_origins: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PolicyConfig {
    pub allow_shell: bool,
    pub allow_dynamic_tools: bool,
    pub allow_plugin_shell: bool,
    pub allow_plugin_git: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PluginLayerConfig {
    pub enabled: bool,
    #[serde(default = "default_manifest_path")]
    pub manifest_path: PathBuf,
    /// Extra directories to scan for OpenClaw SKILL.md and Hermes plugin.json
    #[serde(default)]
    pub host_plugin_roots: Vec<PathBuf>,
    pub hook_events: Vec<String>,
    pub allow_core_fallback: bool,
    pub layered_overrides: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GroupChatConfig {
    pub enable_ambient_questions: bool,
    pub rolling_memory_namespace: String,
    pub rolling_memory_max_chars: usize,
    pub rolling_memory_recent_turns: usize,
    pub ambient_question_window: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ToolsetConfig {
    pub enabled: Vec<String>,
    pub disabled: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MemoryIdeasConfig {
    /// One logical user across channels (merged history when set).
    pub principal_id: Option<String>,
    /// Inject brief + recall gate + idea graph context each turn.
    pub inject_context: bool,
    pub graph_recall_limit: usize,
    /// Route heartbeat synthetic messages to this chat_id (e.g. telegram chat id).
    pub heartbeat_chat_id: Option<String>,
    /// After idle, expand open loops into dream nodes (graph).
    pub dream_on_heartbeat: bool,
}

impl Default for MemoryIdeasConfig {
    fn default() -> Self {
        Self {
            principal_id: None,
            inject_context: true,
            graph_recall_limit: 5,
            heartbeat_chat_id: None,
            dream_on_heartbeat: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RsGbrainConfig {
    pub enabled: bool,
    pub inject_brief: bool,
    /// Run rs_gbrain nightly dream on heartbeat tick (when memory.dream_on_heartbeat)
    pub dream_on_heartbeat: bool,
}

impl Default for RsGbrainConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            inject_brief: true,
            dream_on_heartbeat: true,
        }
    }
}

/// Apply a named onboarding permission profile to `cfg` (policy, toolsets, and `permission_profile`).
pub fn apply_permission_profile(cfg: &mut Config, profile: &str) {
    let p = profile.trim().to_ascii_lowercase().replace(['-', ' '], "_");
    match p.as_str() {
        "full" => {
            cfg.agent.permission_profile = "full".to_string();
            cfg.policy.allow_shell = true;
            cfg.policy.allow_dynamic_tools = true;
            cfg.toolsets = ToolsetConfig::default();
            cfg.agent.permissions = PermissionRulesConfig::default();
        }
        "auto" => {
            cfg.agent.permission_profile = "auto".to_string();
            cfg.policy = PolicyConfig::default();
            cfg.toolsets = ToolsetConfig::default();
            cfg.agent.permissions = PermissionRulesConfig::default();
        }
        "prompt" => {
            cfg.agent.permission_profile = "prompt".to_string();
            cfg.policy = PolicyConfig::default();
            cfg.toolsets = ToolsetConfig::default();
            cfg.agent.permissions = PermissionRulesConfig::default();
        }
        "tools_only" | "tools" => {
            cfg.agent.permission_profile = "tools_only".to_string();
            cfg.policy.allow_shell = false;
            cfg.policy.allow_dynamic_tools = false;
            cfg.toolsets.enabled = vec![
                "web".to_string(),
                "memory".to_string(),
                "sessions".to_string(),
            ];
            cfg.toolsets.disabled = vec![
                "browser".to_string(),
                "vibemania".to_string(),
                "create_tool".to_string(),
                "mcp".to_string(),
            ];
            cfg.agent.permissions = PermissionRulesConfig::default();
        }
        _ => {
            cfg.agent.permission_profile = "auto".to_string();
            cfg.policy = PolicyConfig::default();
            cfg.toolsets = ToolsetConfig::default();
            cfg.agent.permissions = PermissionRulesConfig::default();
        }
    }
}

impl Config {
    pub fn load(path: &str) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let config: Config = serde_json::from_str(&content)?;
        Ok(config)
    }

    pub fn default_config() -> Self {
        Self {
            provider: ProviderConfig::default(),
            embeddings: EmbeddingsConfig::default(),
            agent: AgentConfig::default(),
            model: "claude-sonnet-4-6".to_string(),
            system_prompt: "You are a helpful AI assistant.".to_string(),
            workspace: PathBuf::from("."),
            storage: StorageConfig::default(),
            runtime: RuntimeConfig::default(),
            hosting: HostingConfig::default(),
            observability: ObservabilityConfig::default(),
            channel: ChannelConfig::default(),
            gateway: GatewayConfig::default(),
            policy: PolicyConfig::default(),
            plugin_layer: PluginLayerConfig::default(),
            group_chat: GroupChatConfig::default(),
            toolsets: ToolsetConfig::default(),
            memory: MemoryIdeasConfig::default(),
            rs_gbrain: RsGbrainConfig::default(),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self::default_config()
    }
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            name: "anthropic".to_string(),
            api_key: None,
            base_url: None,
        }
    }
}

impl Default for EmbeddingsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: "noop".to_string(),
            api_key: None,
            model: None,
            base_url: None,
        }
    }
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            kind: "native".to_string(),
            docker_image: None,
            memory_limit_mb: None,
            state_path: None,
            self_update: SelfUpdateConfig::default(),
        }
    }
}

impl Default for SelfUpdateConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            interval_secs: 900,
            remote: "origin".to_string(),
            branch: "main".to_string(),
            restart_service: Some("unthinkclaw".to_string()),
        }
    }
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            backend: "surreal".to_string(),
            root: PathBuf::from(".unthinkclaw"),
        }
    }
}

impl Default for HostingConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            tenant_root: PathBuf::from(".unthinkclaw/tenants"),
            session_timeout_minutes: 120,
            default_channel: "gateway".to_string(),
        }
    }
}

impl Default for ObservabilityConfig {
    fn default() -> Self {
        Self {
            service_name: "unthinkclaw".to_string(),
            environment: "development".to_string(),
            json_logs: false,
            trace_header_name: "traceparent".to_string(),
        }
    }
}

impl Default for ChannelConfig {
    fn default() -> Self {
        Self {
            kind: "cli".to_string(),
            token: None,
            allowed_chat_ids: Vec::new(),
            allowed_sender_ids: Vec::new(),
        }
    }
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1:8080".to_string(),
            auth_token: None,
            enable_admin_api: false,
            request_body_limit_kb: 512,
            request_timeout_secs: 60,
            rate_limit_per_minute: 120,
            trusted_proxies: Vec::new(),
            allowed_origins: Vec::new(),
        }
    }
}

impl Default for PolicyConfig {
    fn default() -> Self {
        Self {
            allow_shell: true,
            allow_dynamic_tools: true,
            allow_plugin_shell: false,
            allow_plugin_git: false,
        }
    }
}

fn default_manifest_path() -> PathBuf {
    PathBuf::from("plugins/manifest.json")
}

impl Default for PluginLayerConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            manifest_path: default_manifest_path(),
            host_plugin_roots: Vec::new(),
            hook_events: vec![
                "before_message".to_string(),
                "after_message".to_string(),
                "before_tool".to_string(),
                "after_tool".to_string(),
            ],
            allow_core_fallback: true,
            layered_overrides: vec!["system_prompt".to_string(), "toolsets".to_string()],
        }
    }
}

impl Default for GroupChatConfig {
    fn default() -> Self {
        Self {
            enable_ambient_questions: true,
            rolling_memory_namespace: "group_memory".to_string(),
            rolling_memory_max_chars: 6_000,
            rolling_memory_recent_turns: 16,
            ambient_question_window: 24,
        }
    }
}

#[cfg(test)]
mod permission_profile_tests {
    use super::*;

    #[test]
    fn full_enables_shell_and_resets_toolsets() {
        let mut cfg = Config::default();
        cfg.policy.allow_shell = false;
        cfg.toolsets.enabled = vec!["browser".into()];
        apply_permission_profile(&mut cfg, "full");
        assert_eq!(cfg.agent.permission_profile, "full");
        assert!(cfg.policy.allow_shell);
        assert!(cfg.policy.allow_dynamic_tools);
        assert!(cfg.toolsets.enabled.is_empty());
    }

    #[test]
    fn tools_only_disables_shell_and_limits_toolsets() {
        let mut cfg = Config::default();
        apply_permission_profile(&mut cfg, "tools-only");
        assert_eq!(cfg.agent.permission_profile, "tools_only");
        assert!(!cfg.policy.allow_shell);
        assert!(!cfg.policy.allow_dynamic_tools);
        assert_eq!(
            cfg.toolsets.enabled,
            vec![
                "web".to_string(),
                "memory".to_string(),
                "sessions".to_string()
            ]
        );
        assert!(cfg.toolsets.disabled.contains(&"browser".to_string()));
    }

    #[test]
    fn unknown_profile_falls_back_to_auto_defaults() {
        let mut cfg = Config::default();
        cfg.policy.allow_shell = false;
        apply_permission_profile(&mut cfg, "nope");
        assert_eq!(cfg.agent.permission_profile, "auto");
        assert!(cfg.policy.allow_shell);
    }
}
