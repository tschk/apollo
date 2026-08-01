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
    pub observability: ObservabilityConfig,
    pub channel: ChannelConfig,
    pub policy: PolicyConfig,
    pub plugin_layer: PluginLayerConfig,
    pub group_chat: GroupChatConfig,
    pub toolsets: ToolsetConfig,
    pub memory: MemoryIdeasConfig,
    #[serde(default)]
    pub zkr: ZkrConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderConfig {
    pub name: String,
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    /// Let the provider run web search on its own infrastructure instead of
    /// apollo's `web_search` tool. Anthropic only; billed by the provider.
    pub native_web_search: bool,
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
    /// rx4 auto-compaction threshold. `0` leaves compaction off (the bridge
    /// default); a non-zero value is forwarded to `Agent::auto_compact_after`,
    /// which rx4 interprets as an estimated-token cutoff before each prompt.
    pub auto_compact_after: usize,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_rounds: 50,
            max_history_messages: 10,
            max_tool_result_chars: 20_000,
            max_context_chars: 150_000,
            fast_model: "gpt-5.4-mini".to_string(),
            heavy_model: "gpt-5.5".to_string(),
            permissions: PermissionRulesConfig::default(),
            permission_profile: "auto".to_string(),
            auto_compact_after: 0,
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
pub struct PolicyConfig {
    pub allow_shell: bool,
    pub allow_dynamic_tools: bool,
    pub allow_plugin_shell: bool,
    pub allow_plugin_git: bool,
    pub allow_computer_use: bool,
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
pub struct ZkrConfig {
    pub enabled: bool,
    pub database: PathBuf,
    pub tenant_id: String,
    pub person_id: String,
    pub auto_capture: bool,
    pub inject_recall: bool,
    pub recall_limit: u32,
    pub self_improve: bool,
}

impl Default for ZkrConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            database: PathBuf::from(".apollo/zkr.db"),
            tenant_id: "apollo".to_string(),
            person_id: "local".to_string(),
            auto_capture: true,
            inject_recall: true,
            recall_limit: 5,
            self_improve: true,
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
            cfg.policy.allow_computer_use = true;
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
            cfg.policy.allow_computer_use = false;
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

/// Leaf field names whose values must never be printed.
const SECRET_LEAF_KEYS: &[&str] = &["api_key", "token", "secret", "password"];

/// Whether a leaf field name holds a credential.
pub fn is_secret_key(leaf: &str) -> bool {
    let leaf = leaf.to_ascii_lowercase();
    SECRET_LEAF_KEYS
        .iter()
        .any(|s| leaf == *s || leaf.ends_with(&format!("_{s}")))
}

/// Replace every credential-bearing leaf with a fixed mask, in place.
///
/// Empty strings and nulls stay as they are: "unset" is not a secret, and
/// showing a mask for one would be a lie.
pub fn mask_secrets(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map.iter_mut() {
                if is_secret_key(key) {
                    if let serde_json::Value::String(s) = child {
                        if !s.is_empty() {
                            *child = serde_json::Value::String("********".to_string());
                            continue;
                        }
                    }
                }
                mask_secrets(child);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                mask_secrets(item);
            }
        }
        _ => {}
    }
}

fn lookup<'a>(root: &'a serde_json::Value, key: &str) -> anyhow::Result<&'a serde_json::Value> {
    let mut current = root;
    let mut walked: Vec<&str> = Vec::new();
    for segment in key.split('.') {
        let object = current.as_object().ok_or_else(|| {
            anyhow::anyhow!(
                "unknown config key `{key}`: `{}` is not a section",
                walked.join(".")
            )
        })?;
        current = object.get(segment).ok_or_else(|| {
            let mut names: Vec<&str> = object.keys().map(|k| k.as_str()).collect();
            names.sort_unstable();
            anyhow::anyhow!(
                "unknown config key `{key}`. Available under `{}`: {}",
                if walked.is_empty() {
                    "<root>".to_string()
                } else {
                    walked.join(".")
                },
                names.join(", ")
            )
        })?;
        walked.push(segment);
    }
    Ok(current)
}

fn coerce(existing: &serde_json::Value, input: &str) -> anyhow::Result<serde_json::Value> {
    use serde_json::Value;
    match existing {
        Value::String(_) => Ok(Value::String(input.to_string())),
        Value::Bool(_) => input
            .parse::<bool>()
            .map(Value::Bool)
            .map_err(|_| anyhow::anyhow!("expected `true` or `false`, got `{input}`")),
        Value::Number(n) => {
            if n.is_f64() {
                input
                    .parse::<f64>()
                    .ok()
                    .and_then(serde_json::Number::from_f64)
                    .map(Value::Number)
                    .ok_or_else(|| anyhow::anyhow!("expected a number, got `{input}`"))
            } else {
                input
                    .parse::<i64>()
                    .map(|v| Value::Number(v.into()))
                    .map_err(|_| anyhow::anyhow!("expected an integer, got `{input}`"))
            }
        }
        Value::Array(_) | Value::Object(_) => serde_json::from_str(input)
            .map_err(|e| anyhow::anyhow!("expected JSON matching the existing value: {e}")),
        Value::Null => Ok(serde_json::from_str(input).unwrap_or(Value::String(input.to_string()))),
    }
}

fn assign(root: &mut serde_json::Value, key: &str, value: serde_json::Value) {
    let segments: Vec<&str> = key.split('.').collect();
    let mut current = root;
    for segment in &segments[..segments.len() - 1] {
        if !current.is_object() {
            *current = serde_json::Value::Object(serde_json::Map::new());
        }
        current = current
            .as_object_mut()
            .expect("object")
            .entry((*segment).to_string())
            .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    }
    if !current.is_object() {
        *current = serde_json::Value::Object(serde_json::Map::new());
    }
    current
        .as_object_mut()
        .expect("object")
        .insert(segments[segments.len() - 1].to_string(), value);
}

impl Config {
    /// Read a dotted path (`agent.max_rounds`, `provider.name`, `model`) out of the
    /// effective configuration.
    pub fn get_path(&self, key: &str) -> anyhow::Result<serde_json::Value> {
        if key.trim().is_empty() {
            anyhow::bail!("empty config key");
        }
        let root = serde_json::to_value(self)?;
        lookup(&root, key).cloned()
    }

    /// Coerce `value` to the type the schema already uses at `key`, apply it,
    /// and round-trip the result through `Config` so an invalid write is
    /// rejected before it can reach disk.
    ///
    /// Returns the updated config and the JSON value that was written, so a
    /// caller can splice just that path into the on-disk file.
    pub fn set_path(&self, key: &str, value: &str) -> anyhow::Result<(Config, serde_json::Value)> {
        if key.trim().is_empty() {
            anyhow::bail!("empty config key");
        }
        let mut root = serde_json::to_value(self)?;
        let existing = lookup(&root, key)?;
        let coerced = coerce(existing, value)
            .map_err(|e| anyhow::anyhow!("invalid value for `{key}`: {e}"))?;
        assign(&mut root, key, coerced.clone());
        let updated: Config = serde_json::from_value(root)
            .map_err(|e| anyhow::anyhow!("invalid value for `{key}`: {e}"))?;
        Ok((updated, coerced))
    }

    /// Splice a validated value into a raw config document, leaving every other
    /// key in the file untouched.
    pub fn splice_into_raw(raw: &mut serde_json::Value, key: &str, value: serde_json::Value) {
        assign(raw, key, value);
    }

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
            model: "gpt-5.5".to_string(),
            system_prompt: "You are a helpful AI assistant.".to_string(),
            workspace: PathBuf::from("."),
            storage: StorageConfig::default(),
            runtime: RuntimeConfig::default(),
            observability: ObservabilityConfig::default(),
            channel: ChannelConfig::default(),
            policy: PolicyConfig::default(),
            plugin_layer: PluginLayerConfig::default(),
            group_chat: GroupChatConfig::default(),
            toolsets: ToolsetConfig::default(),
            memory: MemoryIdeasConfig::default(),
            zkr: ZkrConfig::default(),
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
            name: "chatgpt".to_string(),
            api_key: None,
            base_url: None,
            native_web_search: false,
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
            restart_service: Some("apollo".to_string()),
        }
    }
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            backend: "surreal".to_string(),
            root: PathBuf::from(".apollo"),
        }
    }
}

impl Default for ObservabilityConfig {
    fn default() -> Self {
        Self {
            service_name: "apollo".to_string(),
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

impl Default for PolicyConfig {
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
mod config_path_tests {
    use super::*;

    #[test]
    fn get_reads_nested_and_top_level_keys() {
        let cfg = Config::default_config();
        assert_eq!(cfg.get_path("model").unwrap(), cfg.model.as_str());
        assert_eq!(cfg.get_path("provider.name").unwrap(), "chatgpt");
        assert_eq!(cfg.get_path("agent.max_rounds").unwrap(), 50);
    }

    #[test]
    fn set_updates_strings_bools_and_numbers() {
        let cfg = Config::default_config();
        let (cfg, _) = cfg.set_path("policy.allow_shell", "false").unwrap();
        assert!(!cfg.policy.allow_shell);
        let (cfg, written) = cfg.set_path("agent.max_rounds", "12").unwrap();
        assert_eq!(cfg.agent.max_rounds, 12);
        assert_eq!(written, 12);
    }

    #[test]
    fn set_fills_an_optional_field_that_was_null() {
        let cfg = Config::default_config();
        let (cfg, _) = cfg
            .set_path("provider.base_url", "http://localhost:11434")
            .unwrap();
        assert_eq!(
            cfg.provider.base_url.as_deref(),
            Some("http://localhost:11434")
        );
    }

    #[test]
    fn unknown_keys_are_rejected_with_the_available_names() {
        let cfg = Config::default_config();
        let err = cfg.set_path("agent.nope", "1").unwrap_err().to_string();
        assert!(err.contains("unknown config key `agent.nope`"), "{err}");
        assert!(err.contains("max_rounds"), "{err}");

        let err = cfg.get_path("not_a_section.x").unwrap_err().to_string();
        assert!(err.contains("unknown config key"), "{err}");
    }

    #[test]
    fn wrong_typed_values_are_rejected_before_they_reach_disk() {
        let cfg = Config::default_config();
        let err = cfg
            .set_path("agent.max_rounds", "abc")
            .unwrap_err()
            .to_string();
        assert!(err.contains("expected an integer"), "{err}");

        let err = cfg
            .set_path("policy.allow_shell", "yes-please")
            .unwrap_err()
            .to_string();
        assert!(err.contains("expected `true` or `false`"), "{err}");
    }

    #[test]
    fn secrets_are_masked_and_other_values_are_not() {
        let mut cfg = Config::default_config();
        cfg.provider.api_key = Some("sk-ant-secret-value".to_string());
        cfg.channel.token = Some("123:telegram-secret".to_string());
        let mut value = serde_json::to_value(&cfg).unwrap();
        mask_secrets(&mut value);
        let rendered = serde_json::to_string(&value).unwrap();
        assert!(!rendered.contains("sk-ant-secret-value"), "{rendered}");
        assert!(!rendered.contains("telegram-secret"), "{rendered}");
        assert_eq!(value["provider"]["api_key"], "********");
        assert_eq!(value["channel"]["token"], "********");
        assert_eq!(value["provider"]["name"], "chatgpt");
    }

    #[test]
    fn splicing_preserves_unrelated_keys_in_the_file() {
        let mut raw: serde_json::Value =
            serde_json::from_str(r#"{"model":"m","custom":{"kept":true}}"#).unwrap();
        Config::splice_into_raw(&mut raw, "agent.max_rounds", serde_json::json!(12));
        assert_eq!(raw["custom"]["kept"], true);
        assert_eq!(raw["model"], "m");
        assert_eq!(raw["agent"]["max_rounds"], 12);
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
