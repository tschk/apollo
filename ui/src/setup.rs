//! First-run setup for the desktop app: the choices the onboarding screens
//! collect, and writing them where the `apollo` CLI will find them.
//!
//! This mirrors `apollo init` (src/setup.rs in the agent crate) rather than
//! linking it, so the UI keeps building without SurrealDB/RocksDB:
//!
//! - `<workspace>/apollo.json` — provider, model, workspace, permission
//!   profile. Never carries the key (`provider.api_key` stays `null`).
//! - `<workspace>/.env` — the provider key, under the variable apollo's
//!   credential detection reads for that provider. Written owner-only.
//! - `~/.apollo/desktop.json` — "onboarding done" plus the workspace to open,
//!   so a relaunch goes straight to the main window. No secrets.
//!
//! The key is held in [`Secret`], whose `Debug` is redacted, and is never
//! printed, logged or echoed back to the screen.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// A model provider the onboarding offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderInfo {
    /// `provider.name` in apollo.json.
    pub id: &'static str,
    pub label: &'static str,
    /// Environment variable apollo reads the key from. `None`: no key needed.
    pub env_var: Option<&'static str>,
    /// Pre-filled model. Matches `providers::defaults` where apollo has one.
    pub default_model: &'static str,
    pub blurb: &'static str,
}

/// Providers apollo constructs directly and whose key its credential
/// detection (`bootstrap::catalog_env_key`) picks up from the environment.
///
/// OpenAI's key goes in `OPENAI_API_KEY`, which apollo also treats as "use
/// OpenAI" — so it is only ever written for the OpenAI entry. Writing it for
/// another provider would silently switch that config to OpenAI.
pub const PROVIDERS: &[ProviderInfo] = &[
    ProviderInfo {
        id: "openrouter",
        label: "OpenRouter",
        env_var: Some("OPENROUTER_API_KEY"),
        default_model: "z-ai/glm-5.2",
        blurb: "one key, many models",
    },
    ProviderInfo {
        id: "openai",
        label: "OpenAI",
        env_var: Some("OPENAI_API_KEY"),
        default_model: "gpt-5.4",
        blurb: "gpt models",
    },
    ProviderInfo {
        id: "xai",
        label: "xAI",
        env_var: Some("XAI_API_KEY"),
        default_model: "grok-build-0.1",
        blurb: "grok models",
    },
    ProviderInfo {
        id: "gemini",
        label: "Gemini",
        env_var: Some("GEMINI_API_KEY"),
        default_model: "gemini-3.1-pro-preview",
        blurb: "ai studio key",
    },
    ProviderInfo {
        id: "deepseek",
        label: "DeepSeek",
        env_var: Some("DEEPSEEK_API_KEY"),
        default_model: "deepseek-v4-pro",
        blurb: "platform key",
    },
    ProviderInfo {
        id: "moonshot",
        label: "Moonshot",
        env_var: Some("MOONSHOT_API_KEY"),
        default_model: "kimi-k3",
        blurb: "kimi models",
    },
    ProviderInfo {
        id: "ollama",
        label: "Ollama",
        env_var: None,
        default_model: "llama3.2",
        blurb: "local models, no key",
    },
];

pub fn provider(id: &str) -> Option<&'static ProviderInfo> {
    PROVIDERS.iter().find(|p| p.id == id)
}

/// A permission profile, as `apollo init` offers them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProfileInfo {
    pub id: &'static str,
    pub label: &'static str,
    pub detail: &'static str,
}

pub const PROFILES: &[ProfileInfo] = &[
    ProfileInfo {
        id: "auto",
        label: "auto",
        detail: "default heuristics, shell enabled — recommended",
    },
    ProfileInfo {
        id: "prompt",
        label: "prompt",
        detail: "approve plans before tools run",
    },
    ProfileInfo {
        id: "tools_only",
        label: "tools only",
        detail: "web, memory and sessions — no shell, no file writes",
    },
    ProfileInfo {
        id: "full",
        label: "full",
        detail: "autonomous — shell, dynamic tools and computer use on",
    },
];

/// A credential that must never reach a log, a panic message or the screen.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
    pub fn push_str(&mut self, s: &str) {
        self.0.push_str(s);
    }
    pub fn pop(&mut self) {
        self.0.pop();
    }
    pub fn clear(&mut self) {
        self.0.clear();
    }
    pub fn is_empty(&self) -> bool {
        self.0.trim().is_empty()
    }
    /// What the masked field shows: one dot per character, capped, so the
    /// length of a long key is not revealed either.
    pub fn masked(&self) -> String {
        let n = self.0.chars().count();
        if n == 0 {
            return String::new();
        }
        "•".repeat(n.min(24))
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_empty() {
            "Secret(<empty>)"
        } else {
            "Secret(<redacted>)"
        })
    }
}

/// Everything the onboarding collects.
#[derive(Debug, Clone)]
pub struct SetupChoices {
    pub provider: &'static ProviderInfo,
    pub api_key: Secret,
    pub model: String,
    pub workspace: PathBuf,
    pub profile: &'static ProfileInfo,
}

/// Where each piece landed, for the summary screen.
#[derive(Debug, Clone)]
pub struct WrittenSetup {
    pub config_path: PathBuf,
    pub env_path: Option<PathBuf>,
}

/// Reject a key that could not survive a round trip through `.env`.
pub fn validate_key(provider: &ProviderInfo, key: &Secret) -> Result<(), String> {
    if provider.env_var.is_none() {
        return Ok(());
    }
    let key = key.expose().trim();
    if key.is_empty() {
        return Err(format!("{} needs an API key", provider.label));
    }
    if key
        .chars()
        .any(|c| c.is_control() || c == '"' || c == '\\' || c.is_whitespace())
    {
        return Err("that key contains characters an API key never has".into());
    }
    Ok(())
}

pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

pub fn default_workspace() -> PathBuf {
    home_dir()
        .map(|h| h.join("apollo"))
        .unwrap_or_else(|| PathBuf::from("apollo"))
}

/// Expand a leading `~/` the way a user typing a path expects.
pub fn expand_path(input: &str) -> PathBuf {
    let input = input.trim();
    if input == "~" {
        return home_dir().unwrap_or_else(|| PathBuf::from("."));
    }
    if let Some(rest) = input.strip_prefix("~/") {
        if let Some(home) = home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(input)
}

/// Show `$HOME/...` as `~/...`.
pub fn display_path(path: &Path) -> String {
    if let Some(home) = home_dir() {
        if let Ok(rest) = path.strip_prefix(&home) {
            return if rest.as_os_str().is_empty() {
                "~".into()
            } else {
                format!("~/{}", rest.display())
            };
        }
    }
    path.display().to_string()
}

/// Write `apollo.json` and `.env` into the chosen workspace.
///
/// An existing `apollo.json` is merged into, not replaced, so re-running the
/// onboarding over a configured workspace keeps its channels, prompt and
/// plugins. An existing `.env` keeps every line but the one being set.
pub fn write_setup(choices: &SetupChoices) -> Result<WrittenSetup, String> {
    validate_key(choices.provider, &choices.api_key)?;
    let workspace = &choices.workspace;
    std::fs::create_dir_all(workspace)
        .map_err(|e| format!("could not create {}: {e}", workspace.display()))?;
    let workspace = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.clone());

    let config_path = workspace.join("apollo.json");
    let mut config = match std::fs::read_to_string(&config_path) {
        Ok(text) => serde_json::from_str::<serde_json::Value>(&text)
            .map_err(|e| format!("{} is not valid JSON: {e}", config_path.display()))?,
        Err(_) => serde_json::json!({}),
    };
    apply_choices(&mut config, choices, &workspace);
    let json = serde_json::to_string_pretty(&config).map_err(|e| e.to_string())?;
    // The agent sandbox treats apollo.json as a credential file.
    write_private(&config_path, &json)?;

    let env_path = match choices.provider.env_var {
        Some(var) => {
            let path = workspace.join(".env");
            let existing = std::fs::read_to_string(&path).unwrap_or_default();
            let content = upsert_env_line(&existing, var, choices.api_key.expose().trim());
            write_private(&path, &content)?;
            Some(path)
        }
        None => {
            let path = workspace.join(".env");
            let existing = std::fs::read_to_string(&path).unwrap_or_default();
            let content = upsert_env_line(&existing, "OLLAMA_BASE_URL", "http://localhost:11434");
            write_private(&path, &content)?;
            Some(path)
        }
    };

    Ok(WrittenSetup {
        config_path,
        env_path,
    })
}

/// Fold the choices into an apollo.json value. Mirrors `apollo init` and
/// `config::apply_permission_profile`.
pub fn apply_choices(config: &mut serde_json::Value, choices: &SetupChoices, workspace: &Path) {
    use serde_json::{json, Value};
    if !config.is_object() {
        *config = json!({});
    }
    let root = config.as_object_mut().expect("object");

    let provider = root.entry("provider").or_insert_with(|| json!({}));
    if !provider.is_object() {
        *provider = json!({});
    }
    let provider = provider.as_object_mut().expect("object");
    provider.insert("name".into(), json!(choices.provider.id));
    // Secrets stay in .env.
    provider.insert("api_key".into(), Value::Null);
    provider.insert(
        "base_url".into(),
        if choices.provider.id == "ollama" {
            json!("http://localhost:11434")
        } else {
            Value::Null
        },
    );

    let model = if choices.model.trim().is_empty() {
        choices.provider.default_model.to_string()
    } else {
        choices.model.trim().to_string()
    };
    root.insert("model".into(), json!(model));
    root.insert("workspace".into(), json!(workspace));

    let agent = root.entry("agent").or_insert_with(|| json!({}));
    if !agent.is_object() {
        *agent = json!({});
    }
    let agent = agent.as_object_mut().expect("object");
    agent.insert("permission_profile".into(), json!(choices.profile.id));
    agent.remove("permissions");

    match choices.profile.id {
        "full" => {
            root.insert(
                "policy".into(),
                json!({"allow_shell": true, "allow_dynamic_tools": true, "allow_computer_use": true}),
            );
            root.insert("toolsets".into(), json!({}));
        }
        "tools_only" => {
            root.insert(
                "policy".into(),
                json!({"allow_shell": false, "allow_dynamic_tools": false, "allow_computer_use": false}),
            );
            root.insert(
                "toolsets".into(),
                json!({
                    "enabled": ["web", "memory", "sessions"],
                    "disabled": ["browser", "vibemania", "create_tool", "mcp"]
                }),
            );
        }
        _ => {
            root.insert("policy".into(), json!({}));
            root.insert("toolsets".into(), json!({}));
        }
    }
}

/// Set `KEY="value"` in a dotenv file body, keeping every other line.
pub fn upsert_env_line(existing: &str, key: &str, value: &str) -> String {
    let line = format!("{key}=\"{value}\"");
    let mut out = Vec::new();
    let mut replaced = false;
    for l in existing.lines() {
        let trimmed = l.trim_start().trim_start_matches("export ").trim_start();
        let is_key = trimmed
            .split_once('=')
            .map(|(k, _)| k.trim() == key)
            .unwrap_or(false);
        if is_key {
            if !replaced {
                out.push(line.clone());
                replaced = true;
            }
        } else {
            out.push(l.to_string());
        }
    }
    if !replaced {
        out.push(line);
    }
    let mut body = out.join("\n");
    body.push('\n');
    body
}

fn write_private(path: &Path, content: &str) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .map_err(|e| format!("could not write {}: {e}", path.display()))?;
        // `mode` only applies on create; tighten a file that already existed.
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("could not restrict {}: {e}", path.display()))?;
        file.write_all(content.as_bytes())
            .map_err(|e| format!("could not write {}: {e}", path.display()))
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, content)
            .map_err(|e| format!("could not write {}: {e}", path.display()))
    }
}

/// `~/.apollo/desktop.json`: whether onboarding finished and what to open.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct DesktopState {
    pub onboarded: bool,
    pub workspace: Option<PathBuf>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub permission_profile: Option<String>,
}

pub fn desktop_state_path() -> Option<PathBuf> {
    home_dir().map(|h| h.join(".apollo").join("desktop.json"))
}

impl DesktopState {
    pub fn load() -> Option<Self> {
        Self::load_from(&desktop_state_path()?)
    }

    pub fn load_from(path: &Path) -> Option<Self> {
        let text = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&text).ok()
    }

    pub fn save(&self) -> Result<PathBuf, String> {
        let path = desktop_state_path().ok_or("HOME is not set")?;
        self.save_to(&path)?;
        Ok(path)
    }

    pub fn save_to(&self, path: &Path) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(path, json).map_err(|e| format!("could not write {}: {e}", path.display()))
    }

    /// Onboarding is done and the workspace it set up still has a config.
    pub fn ready(&self) -> Option<&Path> {
        let ws = self.workspace.as_deref()?;
        (self.onboarded && ws.join("apollo.json").is_file()).then_some(ws)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn choices(dir: &Path, provider_id: &str, key: &str, profile: &str) -> SetupChoices {
        SetupChoices {
            provider: provider(provider_id).unwrap(),
            api_key: Secret::new(key),
            model: String::new(),
            workspace: dir.to_path_buf(),
            profile: PROFILES.iter().find(|p| p.id == profile).unwrap(),
        }
    }

    #[test]
    fn secret_debug_is_redacted() {
        let s = Secret::new("sk-live-abcdef");
        let shown = format!(
            "{s:?} {:?}",
            choices(Path::new("/x"), "openai", "sk-live-abcdef", "auto")
        );
        assert!(!shown.contains("sk-live"), "{shown}");
        assert_eq!(s.masked(), "•".repeat(14));
    }

    #[test]
    fn writes_config_without_the_key_and_env_owner_only() {
        let dir = tempfile::tempdir().unwrap();
        let written =
            write_setup(&choices(dir.path(), "openrouter", "sk-or-test123", "auto")).unwrap();
        let config = std::fs::read_to_string(&written.config_path).unwrap();
        assert!(!config.contains("sk-or-test123"));
        let v: serde_json::Value = serde_json::from_str(&config).unwrap();
        assert_eq!(v["provider"]["name"], "openrouter");
        assert!(v["provider"]["api_key"].is_null());
        assert_eq!(v["model"], "z-ai/glm-5.2");
        assert_eq!(v["agent"]["permission_profile"], "auto");

        let env_path = written.env_path.unwrap();
        let env = std::fs::read_to_string(&env_path).unwrap();
        assert_eq!(env, "OPENROUTER_API_KEY=\"sk-or-test123\"\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for p in [&env_path, &written.config_path] {
                let mode = std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
                assert_eq!(mode, 0o600, "{}", p.display());
            }
        }
    }

    #[test]
    fn merges_existing_config_and_env() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("apollo.json"),
            r#"{"system_prompt":"keep me","channel":{"kind":"telegram"},"agent":{"max_rounds":7}}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join(".env"),
            "APOLLO_TELEGRAM_TOKEN=\"t\"\nXAI_API_KEY=\"old\"\n",
        )
        .unwrap();
        write_setup(&choices(dir.path(), "xai", "xai-new", "tools_only")).unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.path().join("apollo.json")).unwrap())
                .unwrap();
        assert_eq!(v["system_prompt"], "keep me");
        assert_eq!(v["channel"]["kind"], "telegram");
        assert_eq!(v["agent"]["max_rounds"], 7);
        assert_eq!(v["agent"]["permission_profile"], "tools_only");
        assert_eq!(v["policy"]["allow_shell"], false);
        assert_eq!(v["toolsets"]["enabled"][0], "web");
        let env = std::fs::read_to_string(dir.path().join(".env")).unwrap();
        assert_eq!(
            env,
            "APOLLO_TELEGRAM_TOKEN=\"t\"\nXAI_API_KEY=\"xai-new\"\n"
        );
    }

    #[test]
    fn ollama_needs_no_key() {
        let dir = tempfile::tempdir().unwrap();
        let written = write_setup(&choices(dir.path(), "ollama", "", "auto")).unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(written.config_path).unwrap()).unwrap();
        assert_eq!(v["provider"]["base_url"], "http://localhost:11434");
    }

    #[test]
    fn rejects_missing_or_malformed_keys() {
        let p = provider("openai").unwrap();
        assert!(validate_key(p, &Secret::new("")).is_err());
        assert!(validate_key(p, &Secret::new("sk-\"x")).is_err());
        assert!(validate_key(p, &Secret::new("sk abc")).is_err());
        assert!(validate_key(p, &Secret::new("sk-abc")).is_ok());
    }

    #[test]
    fn only_openai_writes_openai_api_key() {
        // apollo reads OPENAI_API_KEY as "switch to OpenAI".
        for p in PROVIDERS {
            if p.id != "openai" {
                assert_ne!(p.env_var, Some("OPENAI_API_KEY"), "{}", p.id);
            }
        }
    }

    #[test]
    fn desktop_state_round_trips_and_requires_a_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state/desktop.json");
        let ws = dir.path().join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        let state = DesktopState {
            onboarded: true,
            workspace: Some(ws.clone()),
            ..Default::default()
        };
        state.save_to(&path).unwrap();
        let loaded = DesktopState::load_from(&path).unwrap();
        assert_eq!(loaded, state);
        assert!(loaded.ready().is_none(), "no apollo.json yet");
        std::fs::write(ws.join("apollo.json"), "{}").unwrap();
        assert_eq!(loaded.ready(), Some(ws.as_path()));
    }
}
