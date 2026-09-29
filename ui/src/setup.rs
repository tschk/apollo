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
//! - `~/.apollo/desktop.json` — "onboarding done", simple/advanced mode and
//!   the instances (each a config dir + workspace), so a relaunch goes
//!   straight to the main window. No secrets.
//!
//! An instance scoped to a folder keeps its config in that folder, exactly
//! where `apollo init` puts it. A "works everywhere" instance has no single
//! folder: its workspace is `$HOME` and its config lives in
//! `~/.apollo/instances/<id>/`.
//!
//! The key is held in [`Secret`], whose `Debug` is redacted, and is never
//! printed, logged or echoed back to the screen.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// How a provider authenticates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::enum_variant_names)] // variants name the auth methods
pub enum Auth {
    /// Browser sign-in; tokens go to the shared rs_ai credential store.
    OAuth(crate::oauth::OAuthKind),
    /// An API key, written to `.env` under this variable.
    ApiKey(&'static str),
    /// Nothing to enter (Ollama).
    Local,
    /// Any OpenAI-compatible endpoint: base URL + optional key + model.
    /// The key goes to `.env` as [`CUSTOM_KEY_VAR`].
    Custom,
}

/// The variable apollo reads the configured provider's key from when it is
/// not one of the catalog providers (see `bootstrap::explicit_provider_key`).
pub const CUSTOM_KEY_VAR: &str = "APOLLO_PROVIDER_API_KEY";

/// A model provider the onboarding offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderInfo {
    /// `provider.name` in apollo.json.
    pub id: &'static str,
    pub label: &'static str,
    pub auth: Auth,
    /// Pre-filled model. Matches `providers::defaults` where apollo has one.
    pub default_model: &'static str,
    pub blurb: &'static str,
    /// `rs_ai_providers::catalog` id, for the model list (empty: none).
    pub catalog: &'static str,
    /// Written to `provider.base_url` for catalog providers apollo runs
    /// through its generic OpenAI-compatible client.
    pub base_url: Option<&'static str>,
}

impl ProviderInfo {
    /// Environment variable the key is written under, if any.
    pub fn env_var(&self) -> Option<&'static str> {
        match self.auth {
            Auth::ApiKey(var) => Some(var),
            Auth::Custom => Some(CUSTOM_KEY_VAR),
            Auth::OAuth(_) | Auth::Local => None,
        }
    }
    pub fn is_custom(&self) -> bool {
        matches!(self.auth, Auth::Custom)
    }
}

/// Sign-in providers, pinned at the top of the provider step.
pub const OAUTH_PROVIDERS: &[ProviderInfo] = &[
    ProviderInfo {
        id: "chatgpt",
        label: "ChatGPT",
        auth: Auth::OAuth(crate::oauth::OAuthKind::ChatGpt),
        default_model: "gpt-5.6",
        blurb: "sign in with your chatgpt plan",
        catalog: "openai",
        base_url: None,
    },
    ProviderInfo {
        id: "github-copilot",
        label: "GitHub Copilot",
        auth: Auth::OAuth(crate::oauth::OAuthKind::Copilot),
        default_model: "gpt-5.4",
        blurb: "sign in with github",
        catalog: "github-copilot",
        base_url: None,
    },
    ProviderInfo {
        id: "claude",
        label: "Claude",
        auth: Auth::OAuth(crate::oauth::OAuthKind::Claude),
        default_model: "claude-sonnet-5",
        blurb: "sign in with your claude plan",
        catalog: "anthropic",
        base_url: None,
    },
];

/// Everything else, behind the dropdown — built from the rs_ai catalog.
pub use crate::catalog::PROVIDERS;

pub fn provider(id: &str) -> Option<&'static ProviderInfo> {
    if id == "custom" || id.starts_with("custom-") {
        return PROVIDERS.iter().find(|p| p.is_custom());
    }
    OAUTH_PROVIDERS
        .iter()
        .chain(PROVIDERS.iter())
        .find(|p| p.id == id)
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

/// Where an instance works.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// One folder; the config sits in it, like `apollo init`.
    Folder(PathBuf),
    /// No single folder: workspace `$HOME`, config under
    /// `~/.apollo/instances/<id>/`.
    Everywhere,
}

/// Everything the onboarding collects.
#[derive(Debug, Clone)]
pub struct SetupChoices {
    pub instance_id: String,
    pub name: String,
    pub provider: &'static ProviderInfo,
    pub api_key: Secret,
    /// Custom endpoints only.
    pub base_url: String,
    /// Custom endpoints only: what the user called it.
    pub custom_name: String,
    pub model: String,
    pub scope: Scope,
    pub profile: &'static ProfileInfo,
    /// Written to apollo.json `system_prompt`. Empty keeps the existing one.
    pub system_prompt: String,
    /// `agent.reasoning_effort`: low, medium, high, or xhigh.
    pub effort: String,
}

impl SetupChoices {
    pub fn workspace(&self) -> PathBuf {
        match &self.scope {
            Scope::Folder(p) => p.clone(),
            Scope::Everywhere => home_dir().unwrap_or_else(|| PathBuf::from(".")),
        }
    }

    pub fn config_dir(&self) -> PathBuf {
        match &self.scope {
            Scope::Folder(p) => p.clone(),
            Scope::Everywhere => instances_root().join(&self.instance_id),
        }
    }

    /// `provider.name` in apollo.json. A custom endpoint is named after
    /// what the user called it (`custom-<slug>`).
    pub fn provider_name(&self) -> String {
        if !self.provider.is_custom() {
            return self.provider.id.to_string();
        }
        let slug = instance_id(&self.custom_name, &[]);
        if self.custom_name.trim().is_empty() || slug == "apollo" {
            "custom".into()
        } else {
            format!("custom-{slug}")
        }
    }

    pub fn resolved_model(&self) -> String {
        if self.model.trim().is_empty() {
            self.provider.default_model.to_string()
        } else {
            self.model.trim().to_string()
        }
    }
}

pub fn instances_root() -> PathBuf {
    home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".apollo")
        .join("instances")
}

/// A filesystem-safe id from a display name, unique among `taken`.
pub fn instance_id(name: &str, taken: &[String]) -> String {
    let mut base: String = name
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    while base.contains("--") {
        base = base.replace("--", "-");
    }
    let base = base.trim_matches('-');
    let base = if base.is_empty() { "apollo" } else { base }.to_string();
    if !taken.contains(&base) {
        return base;
    }
    (2..)
        .map(|n| format!("{base}-{n}"))
        .find(|id| !taken.contains(id))
        .expect("unbounded")
}

/// Where each piece landed, for the summary screen.
#[derive(Debug, Clone)]
pub struct WrittenSetup {
    pub config_dir: PathBuf,
    pub config_path: PathBuf,
    pub env_path: Option<PathBuf>,
    pub workspace: PathBuf,
}

/// Reject a key that could not survive a round trip through `.env`.
pub fn validate_key(provider: &ProviderInfo, key: &Secret) -> Result<(), String> {
    let key = key.expose().trim();
    match provider.auth {
        Auth::ApiKey(_) if key.is_empty() => {
            return Err(format!("{} needs an API key", provider.label))
        }
        Auth::ApiKey(_) | Auth::Custom => {}
        Auth::OAuth(_) | Auth::Local => return Ok(()),
    }
    if key
        .chars()
        .any(|c| c.is_control() || c == '"' || c == '\\' || c.is_whitespace())
    {
        return Err("that key contains characters an API key never has".into());
    }
    Ok(())
}

/// A custom endpoint needs an http(s) base URL and a model name.
pub fn validate_custom(base_url: &str, model: &str) -> Result<(), String> {
    let url = base_url.trim();
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err("base url must start with http:// or https://".into());
    }
    if url.chars().any(|c| c.is_whitespace() || c == '"') {
        return Err("base url cannot contain spaces or quotes".into());
    }
    if model.trim().is_empty() {
        return Err("a custom endpoint needs a model name".into());
    }
    Ok(())
}

/// `$HOME`, or `%USERPROFILE%` on Windows where HOME is usually unset.
pub fn home_dir() -> Option<PathBuf> {
    ["HOME", "USERPROFILE"]
        .iter()
        .find_map(|var| std::env::var_os(var).filter(|h| !h.is_empty()))
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

/// Write `apollo.json` and `.env` into the instance's config dir.
///
/// An existing `apollo.json` is merged into, not replaced, so re-running the
/// onboarding over a configured workspace keeps its channels, prompt and
/// plugins. An existing `.env` keeps every line but the ones being set.
pub fn write_setup(choices: &SetupChoices) -> Result<WrittenSetup, String> {
    validate_key(choices.provider, &choices.api_key)?;
    if choices.provider.is_custom() {
        validate_custom(&choices.base_url, &choices.model)?;
    }
    let workspace = choices.workspace();
    let config_dir = choices.config_dir();
    for dir in [&workspace, &config_dir] {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    }
    let workspace = workspace.canonicalize().unwrap_or(workspace);
    let config_dir = config_dir.canonicalize().unwrap_or(config_dir);

    let config_path = config_dir.join("apollo.json");
    let mut config = match std::fs::read_to_string(&config_path) {
        Ok(text) => serde_json::from_str::<serde_json::Value>(&text)
            .map_err(|e| format!("{} is not valid JSON: {e}", config_path.display()))?,
        Err(_) => serde_json::json!({}),
    };
    apply_choices(&mut config, choices, &workspace);
    let json = serde_json::to_string_pretty(&config).map_err(|e| e.to_string())?;
    // The agent sandbox treats apollo.json as a credential file.
    write_private(&config_path, &json)?;

    let env_path = config_dir.join(".env");
    let mut env = std::fs::read_to_string(&env_path).unwrap_or_default();
    // A leftover custom-endpoint key would override whatever provider is
    // chosen now, since apollo reads it first.
    if choices.provider.env_var() != Some(CUSTOM_KEY_VAR) {
        env = remove_env_line(&env, CUSTOM_KEY_VAR);
    }
    match choices.provider.auth {
        Auth::ApiKey(var) => env = upsert_env_line(&env, var, choices.api_key.expose().trim()),
        Auth::Custom if !choices.api_key.is_empty() => {
            env = upsert_env_line(&env, CUSTOM_KEY_VAR, choices.api_key.expose().trim())
        }
        Auth::Custom => env = remove_env_line(&env, CUSTOM_KEY_VAR),
        Auth::Local => env = upsert_env_line(&env, "OLLAMA_BASE_URL", "http://localhost:11434"),
        // OAuth tokens live in the shared rs_ai credential store.
        Auth::OAuth(_) => {}
    }
    let env_path = if env.trim().is_empty() && !env_path.exists() {
        None
    } else {
        write_private(&env_path, &env)?;
        Some(env_path)
    };

    Ok(WrittenSetup {
        config_dir,
        config_path,
        env_path,
        workspace,
    })
}

/// rx4's reasoning levels. The label is what the desktop shows.
pub const EFFORTS: &[(&str, &str)] = &[
    ("low", "low"),
    ("medium", "medium"),
    ("high", "high"),
    ("xhigh", "max"),
];

pub fn normalize_effort(value: &str) -> Option<&'static str> {
    match value.trim() {
        "low" => Some("low"),
        "medium" => Some("medium"),
        "high" => Some("high"),
        "xhigh" | "max" => Some("xhigh"),
        _ => None,
    }
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
    provider.insert("name".into(), json!(choices.provider_name()));
    // Secrets stay in .env.
    provider.insert("api_key".into(), Value::Null);
    provider.insert(
        "base_url".into(),
        match choices.provider.auth {
            Auth::Local => json!("http://localhost:11434"),
            Auth::Custom => json!(choices.base_url.trim().trim_end_matches('/')),
            _ => choices.provider.base_url.map_or(Value::Null, |u| json!(u)),
        },
    );

    root.insert("model".into(), json!(choices.resolved_model()));
    root.insert("workspace".into(), json!(workspace));
    let effort = normalize_effort(&choices.effort).unwrap_or("medium");
    let instructions = choices.system_prompt.trim();
    if !instructions.is_empty() {
        root.insert("system_prompt".into(), json!(instructions));
    }

    apply_profile(config, choices.profile.id);
    if let Some(agent) = config.get_mut("agent").and_then(|a| a.as_object_mut()) {
        agent.insert("reasoning_effort".into(), json!(effort));
    }
}

/// Set the permission profile the way `config::apply_permission_profile`
/// does: profile name plus the policy/toolsets it implies.
pub fn apply_profile(config: &mut serde_json::Value, profile: &str) {
    use serde_json::json;
    if !config.is_object() {
        *config = json!({});
    }
    let root = config.as_object_mut().expect("object");
    let agent = root.entry("agent").or_insert_with(|| json!({}));
    if !agent.is_object() {
        *agent = json!({});
    }
    let agent = agent.as_object_mut().expect("object");
    agent.insert("permission_profile".into(), json!(profile));
    agent.remove("permissions");

    match profile {
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

/// Read, change and rewrite (0600) an instance's apollo.json.
pub fn update_config(
    path: &Path,
    change: impl FnOnce(&mut serde_json::Value),
) -> Result<serde_json::Value, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("could not read {}: {e}", path.display()))?;
    let mut value: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| format!("{} is not valid JSON: {e}", path.display()))?;
    change(&mut value);
    let json = serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?;
    write_private(path, &json)?;
    Ok(value)
}

pub fn read_config(path: &Path) -> serde_json::Value {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| serde_json::json!({}))
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

/// Drop `KEY=` lines from a dotenv body.
pub fn remove_env_line(existing: &str, key: &str) -> String {
    let kept: Vec<&str> = existing
        .lines()
        .filter(|l| {
            let t = l.trim_start().trim_start_matches("export ").trim_start();
            t.split_once('=')
                .map(|(k, _)| k.trim() != key)
                .unwrap_or(true)
        })
        .collect();
    if kept.is_empty() {
        return String::new();
    }
    let mut body = kept.join("\n");
    body.push('\n');
    body
}

pub(crate) fn write_private(path: &Path, content: &str) -> Result<(), String> {
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

/// How much of the app is on screen.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Chat and an instance switcher, nothing else.
    #[default]
    Simple,
    /// Instance roster, tools, permissions, model params and logs.
    Advanced,
}

// ── Roster helpers (pin, duplicate) and .env inspection ─────────────────────

/// Pinned instances first, otherwise in list order (a stable sort, so
/// pinning never reshuffles the rest of the roster).
pub fn roster_order(instances: &[Instance]) -> Vec<&Instance> {
    let mut ordered: Vec<&Instance> = instances.iter().collect();
    ordered.sort_by_key(|i| !i.pinned);
    ordered
}

/// Parse one dotenv line into (name, non-empty value), skipping comments
/// and empty assignments. The value is compared, never returned.
fn env_line(l: &str) -> Option<(&str, bool)> {
    let t = l.trim_start().trim_start_matches("export ").trim_start();
    if t.starts_with('#') {
        return None;
    }
    let (k, v) = t.split_once('=')?;
    let value = v.trim().trim_matches('"').trim();
    Some((k.trim(), !value.is_empty()))
}

/// Does the dotenv at `path` set `var` to a non-empty value? The value
/// itself is never read out of this function.
pub fn env_has(path: &Path, var: &str) -> bool {
    std::fs::read_to_string(path)
        .map(|text| {
            text.lines()
                .any(|l| env_line(l).is_some_and(|(k, set)| k == var && set))
        })
        .unwrap_or(false)
}

/// Variable NAMES of every configured `*_API_KEY` / `*_TOKEN` entry in the
/// dotenv at `path` — names only, so the UI can list credentials without
/// ever holding a value.
pub fn configured_keys(env_path: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(env_path) else {
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::new();
    for l in text.lines() {
        let Some((name, set)) = env_line(l) else {
            continue;
        };
        if set
            && (name.ends_with("_API_KEY") || name.ends_with("_TOKEN"))
            && !out.iter().any(|n| n == name)
        {
            out.push(name.to_string());
        }
    }
    out
}

/// Copy an instance as "<name> copy": its `apollo.json` and `.env` land in
/// a fresh config dir under `~/.apollo/instances/<new-id>/`, owner-only for
/// the `.env`. Nothing in the source is touched.
///
/// A folder-scoped instance keeps its workspace but is detached from the
/// folder's config (the copy no longer lives in it); an "everywhere"
/// instance stays everywhere. The copy is never pinned.
pub fn duplicate_instance(state: &DesktopState, id: &str) -> Result<Instance, String> {
    let src = state
        .instance(id)
        .ok_or_else(|| format!("no instance \"{id}\""))?;
    let name = format!("{} copy", src.name);
    let new_id = instance_id(&name, &state.ids());
    let config_dir = instances_root().join(&new_id);
    std::fs::create_dir_all(&config_dir)
        .map_err(|e| format!("could not create {}: {e}", config_dir.display()))?;

    let config = std::fs::read_to_string(src.config_path())
        .map_err(|e| format!("could not read {}: {e}", src.config_path().display()))?;
    write_private(&config_dir.join("apollo.json"), &config)?;
    if let Ok(env) = std::fs::read_to_string(src.env_path()) {
        write_private(&config_dir.join(".env"), &env)?;
    }

    Ok(Instance {
        id: new_id,
        name,
        everywhere: src.everywhere,
        workspace: src.workspace.clone(),
        config_dir,
        provider: src.provider.clone(),
        model: src.model.clone(),
        permission_profile: src.permission_profile.clone(),
        color: src.color,
        pinned: false,
    })
}

/// One apollo agent the app knows about.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Instance {
    pub id: String,
    pub name: String,
    /// `true`: works everywhere (workspace `$HOME`).
    #[serde(default)]
    pub everywhere: bool,
    pub workspace: PathBuf,
    /// Holds `apollo.json` and `.env`; the chat runs with this as its cwd.
    pub config_dir: PathBuf,
    pub provider: String,
    pub model: String,
    pub permission_profile: String,
    /// Avatar color for the roster, as a 0xRRGGBB the UI picks from a
    /// swatch row. `None` keeps the default look.
    #[serde(default)]
    pub color: Option<u32>,
    /// Pinned instances sort first in the roster.
    #[serde(default)]
    pub pinned: bool,
}

impl Instance {
    pub fn from_setup(choices: &SetupChoices, written: &WrittenSetup) -> Self {
        Self {
            id: choices.instance_id.clone(),
            name: choices.name.trim().to_string(),
            everywhere: choices.scope == Scope::Everywhere,
            workspace: written.workspace.clone(),
            config_dir: written.config_dir.clone(),
            provider: choices.provider_name(),
            model: choices.resolved_model(),
            permission_profile: choices.profile.id.to_string(),
            color: None,
            pinned: false,
        }
    }

    pub fn config_path(&self) -> PathBuf {
        self.config_dir.join("apollo.json")
    }

    /// The `.env` next to `apollo.json`, where this instance's keys live.
    pub fn env_path(&self) -> PathBuf {
        self.config_dir.join(".env")
    }

    pub fn scope_label(&self) -> String {
        if self.everywhere {
            "everywhere".into()
        } else {
            display_path(&self.workspace)
        }
    }
}

/// `~/.apollo/desktop.json`: onboarding state, mode and instances.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct DesktopState {
    pub onboarded: bool,
    #[serde(default)]
    pub mode: Mode,
    #[serde(default)]
    pub active: Option<String>,
    #[serde(default)]
    pub instances: Vec<Instance>,
    /// Pre-instances layout (a single workspace); migrated on load.
    #[serde(default, skip_serializing)]
    workspace: Option<PathBuf>,
    #[serde(default, skip_serializing)]
    provider: Option<String>,
    #[serde(default, skip_serializing)]
    model: Option<String>,
    #[serde(default, skip_serializing)]
    permission_profile: Option<String>,
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
        let mut state: Self = serde_json::from_str(&text).ok()?;
        state.migrate();
        Some(state)
    }

    /// Turn a single-workspace state into one instance.
    fn migrate(&mut self) {
        if !self.instances.is_empty() {
            return;
        }
        if let Some(ws) = self.workspace.take() {
            self.instances.push(Instance {
                id: "apollo".into(),
                name: "apollo".into(),
                everywhere: false,
                config_dir: ws.clone(),
                workspace: ws,
                provider: self.provider.take().unwrap_or_default(),
                model: self.model.take().unwrap_or_default(),
                permission_profile: self
                    .permission_profile
                    .take()
                    .unwrap_or_else(|| "auto".into()),
                color: None,
                pinned: false,
            });
            self.active = Some("apollo".into());
        }
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

    pub fn ids(&self) -> Vec<String> {
        self.instances.iter().map(|i| i.id.clone()).collect()
    }

    pub fn instance(&self, id: &str) -> Option<&Instance> {
        self.instances.iter().find(|i| i.id == id)
    }

    /// Add or replace an instance and make it active.
    pub fn upsert(&mut self, instance: Instance) {
        self.active = Some(instance.id.clone());
        match self.instances.iter_mut().find(|i| i.id == instance.id) {
            Some(existing) => *existing = instance,
            None => self.instances.push(instance),
        }
    }

    /// Another instance already keeps its config in `dir`.
    pub fn config_dir_owner(&self, dir: &Path, except: &str) -> Option<&Instance> {
        let dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
        self.instances.iter().find(|i| {
            i.id != except
                && i.config_dir
                    .canonicalize()
                    .unwrap_or_else(|_| i.config_dir.clone())
                    == dir
        })
    }

    /// Onboarding is done and the active instance still has a config.
    pub fn ready(&self) -> Option<&Instance> {
        if !self.onboarded {
            return None;
        }
        let active = self
            .active
            .as_deref()
            .and_then(|id| self.instance(id))
            .or_else(|| self.instances.first())?;
        active.config_path().is_file().then_some(active)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn choices(dir: &Path, provider_id: &str, key: &str, profile: &str) -> SetupChoices {
        SetupChoices {
            instance_id: "t".into(),
            name: "t".into(),
            provider: provider(provider_id).unwrap(),
            api_key: Secret::new(key),
            base_url: String::new(),
            custom_name: String::new(),
            model: String::new(),
            scope: Scope::Folder(dir.to_path_buf()),
            profile: PROFILES.iter().find(|p| p.id == profile).unwrap(),
            system_prompt: String::new(),
            effort: "medium".into(),
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
            "APOLLO_TELEGRAM_TOKEN=\"t\"\nXAI_API_KEY=\"old\"\nAPOLLO_PROVIDER_API_KEY=\"stale\"\n",
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
        // The stale custom key is gone: apollo would read it before XAI_API_KEY.
        let env = std::fs::read_to_string(dir.path().join(".env")).unwrap();
        assert_eq!(
            env,
            "APOLLO_TELEGRAM_TOKEN=\"t\"\nXAI_API_KEY=\"xai-new\"\n"
        );
    }

    #[test]
    fn custom_endpoint_keeps_its_key_in_env() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = choices(dir.path(), "custom", "sk-gw-1", "auto");
        c.base_url = "https://llm.example.com/v1/".into();
        c.model = "my-model".into();
        let written = write_setup(&c).unwrap();
        let config = std::fs::read_to_string(&written.config_path).unwrap();
        assert!(!config.contains("sk-gw-1"));
        let v: serde_json::Value = serde_json::from_str(&config).unwrap();
        assert_eq!(v["provider"]["name"], "custom");
        assert_eq!(v["provider"]["base_url"], "https://llm.example.com/v1");
        assert_eq!(v["model"], "my-model");
        let env = std::fs::read_to_string(written.env_path.unwrap()).unwrap();
        assert_eq!(env, "APOLLO_PROVIDER_API_KEY=\"sk-gw-1\"\n");
    }

    #[test]
    fn custom_endpoint_validation() {
        assert!(validate_custom("llm.example.com", "m").is_err());
        assert!(validate_custom("https://x/v1", "").is_err());
        assert!(validate_custom("https://x/v1", "m").is_ok());
        // A keyless local endpoint is fine.
        assert!(validate_key(provider("custom").unwrap(), &Secret::new("")).is_ok());
    }

    #[test]
    fn oauth_writes_no_key_and_no_env() {
        let dir = tempfile::tempdir().unwrap();
        let written = write_setup(&choices(dir.path(), "chatgpt", "", "auto")).unwrap();
        assert!(written.env_path.is_none());
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(written.config_path).unwrap()).unwrap();
        assert_eq!(v["provider"]["name"], "chatgpt");
        assert_eq!(v["model"], "gpt-5.6");
    }

    #[test]
    fn everywhere_scope_uses_home_and_a_private_config_dir() {
        let home = tempfile::tempdir().unwrap();
        temp_env::with_var("HOME", Some(home.path()), || {
            let mut c = choices(Path::new("/unused"), "ollama", "", "auto");
            c.scope = Scope::Everywhere;
            c.instance_id = "global".into();
            let written = write_setup(&c).unwrap();
            let home = home.path().canonicalize().unwrap();
            assert_eq!(written.workspace, home);
            assert_eq!(written.config_dir, home.join(".apollo/instances/global"));
            let v: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(written.config_path).unwrap())
                    .unwrap();
            assert_eq!(v["workspace"], serde_json::json!(home));
        });
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
        for p in OAUTH_PROVIDERS.iter().chain(PROVIDERS.iter()) {
            if p.id != "openai" {
                assert_ne!(p.env_var(), Some("OPENAI_API_KEY"), "{}", p.id);
            }
        }
    }

    #[test]
    fn generic_catalog_provider_writes_base_url_and_shared_key() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = choices(dir.path(), "zai-coding-plan", "zk-test-1234", "auto");
        c.model = "glm-5.1".into();
        let written = write_setup(&c).unwrap();
        let cfg = read_config(&written.config_path);
        assert_eq!(cfg["provider"]["name"], "zai-coding-plan");
        assert_eq!(
            cfg["provider"]["base_url"],
            "https://api.z.ai/api/coding/paas/v4"
        );
        assert!(cfg["provider"]["api_key"].is_null());
        let env = std::fs::read_to_string(written.env_path.unwrap()).unwrap();
        assert!(env.contains("APOLLO_PROVIDER_API_KEY=\"zk-test-1234\""));

        // Switching to a native provider drops the shared key.
        let c = choices(dir.path(), "openrouter", "or-test-1234", "auto");
        let written = write_setup(&c).unwrap();
        let env = std::fs::read_to_string(written.env_path.unwrap()).unwrap();
        assert!(!env.contains("APOLLO_PROVIDER_API_KEY"));
        assert!(env.contains("OPENROUTER_API_KEY=\"or-test-1234\""));
        let cfg = read_config(&written.config_path);
        assert!(cfg["provider"]["base_url"].is_null());
    }

    #[test]
    fn custom_endpoint_is_named_after_the_user() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = choices(dir.path(), "custom", "", "auto");
        assert_eq!(c.provider_name(), "custom");
        c.custom_name = "Work Gateway".into();
        c.base_url = "https://llm.example.com/v1/".into();
        c.model = "m1".into();
        assert_eq!(c.provider_name(), "custom-work-gateway");
        let written = write_setup(&c).unwrap();
        let cfg = read_config(&written.config_path);
        assert_eq!(cfg["provider"]["name"], "custom-work-gateway");
        assert_eq!(cfg["provider"]["base_url"], "https://llm.example.com/v1");
        assert_eq!(provider("custom-work-gateway").unwrap().id, "custom");
    }

    #[test]
    fn instance_ids_are_slugged_and_unique() {
        assert_eq!(instance_id("Work  Bot!", &[]), "work-bot");
        assert_eq!(instance_id("", &[]), "apollo");
        let taken = vec!["apollo".to_string(), "apollo-2".to_string()];
        assert_eq!(instance_id("apollo", &taken), "apollo-3");
    }

    #[test]
    fn desktop_state_round_trips_and_requires_a_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state/desktop.json");
        let ws = dir.path().join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        let mut state = DesktopState {
            onboarded: true,
            mode: Mode::Advanced,
            ..Default::default()
        };
        state.upsert(Instance {
            id: "a".into(),
            name: "a".into(),
            everywhere: false,
            workspace: ws.clone(),
            config_dir: ws.clone(),
            provider: "gemini".into(),
            model: "m".into(),
            permission_profile: "auto".into(),
            color: None,
            pinned: false,
        });
        state.save_to(&path).unwrap();
        let loaded = DesktopState::load_from(&path).unwrap();
        assert_eq!(loaded, state);
        assert!(loaded.ready().is_none(), "no apollo.json yet");
        std::fs::write(ws.join("apollo.json"), "{}").unwrap();
        assert_eq!(loaded.ready().map(|i| i.id.as_str()), Some("a"));
        assert!(loaded.config_dir_owner(&ws, "b").is_some());
        assert!(loaded.config_dir_owner(&ws, "a").is_none());
    }

    #[test]
    fn single_workspace_state_migrates_to_an_instance() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("desktop.json");
        std::fs::write(
            &path,
            r#"{"onboarded":true,"workspace":"/w","provider":"gemini","model":"m","permission_profile":"auto"}"#,
        )
        .unwrap();
        let state = DesktopState::load_from(&path).unwrap();
        assert_eq!(state.mode, Mode::Simple);
        assert_eq!(state.instances.len(), 1);
        assert_eq!(state.instances[0].config_dir, PathBuf::from("/w"));
        assert_eq!(state.active.as_deref(), Some("apollo"));
    }

    #[test]
    fn roster_order_pins_first_and_is_otherwise_stable() {
        let inst = |id: &str, pinned: bool| Instance {
            id: id.into(),
            name: id.into(),
            everywhere: false,
            workspace: PathBuf::new(),
            config_dir: PathBuf::new(),
            provider: String::new(),
            model: String::new(),
            permission_profile: "auto".into(),
            color: None,
            pinned,
        };
        let list = vec![inst("a", false), inst("b", true), inst("c", false)];
        let order: Vec<&str> = roster_order(&list).iter().map(|i| i.id.as_str()).collect();
        assert_eq!(order, ["b", "a", "c"]);
        // Several pinned keep their relative order.
        let list = vec![inst("a", false), inst("b", true), inst("c", true)];
        let order: Vec<&str> = roster_order(&list).iter().map(|i| i.id.as_str()).collect();
        assert_eq!(order, ["b", "c", "a"]);
    }

    #[test]
    fn env_has_requires_a_non_empty_value() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(".env");
        std::fs::write(
            &p,
            "XAI_API_KEY=\"xai-live\"\nEMPTY=\"\"\nexport GROQ_API_KEY=\"g\"\n#COMMENTED=\"c\"\nBARE=\n",
        )
        .unwrap();
        assert!(env_has(&p, "XAI_API_KEY"));
        assert!(env_has(&p, "GROQ_API_KEY"));
        assert!(!env_has(&p, "EMPTY"));
        assert!(!env_has(&p, "BARE"));
        assert!(!env_has(&p, "COMMENTED"));
        assert!(!env_has(&p, "MISSING"));
        assert!(!env_has(&dir.path().join("no.env"), "XAI_API_KEY"));
    }

    #[test]
    fn configured_keys_lists_names_and_never_values() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(".env");
        std::fs::write(
            &p,
            concat!(
                "ANTHROPIC_API_KEY=\"sk-super-secret-9\"\n",
                "GITHUB_TOKEN=\"gh-not-for-screen\"\n",
                "APOLLO_PROVIDER_API_KEY=\"zk-also-secret\"\n",
                "EMPTY_API_KEY=\"\"\n",
                "SOME_OTHER_VAR=\"not-a-credential\"\n",
                "#COMMENT_TOKEN=\"nope\"\n",
            ),
        )
        .unwrap();
        let keys = configured_keys(&p);
        assert_eq!(
            keys,
            vec![
                "ANTHROPIC_API_KEY".to_string(),
                "GITHUB_TOKEN".to_string(),
                "APOLLO_PROVIDER_API_KEY".to_string(),
            ]
        );
        // The proof the task asks for: no value ever leaves this function.
        let shown = keys.join(" ");
        assert!(!shown.contains("secret"), "{shown}");
        assert!(!shown.contains("not-for-screen"), "{shown}");
        assert!(configured_keys(&dir.path().join("missing.env")).is_empty());
    }

    #[test]
    fn duplicate_instance_copies_files_under_a_new_id() {
        let home = tempfile::tempdir().unwrap();
        temp_env::with_var("HOME", Some(home.path()), || {
            let folder = tempfile::tempdir().unwrap();
            std::fs::write(
                folder.path().join("apollo.json"),
                r#"{"provider":{"name":"xai"},"model":"grok","workspace":"/w"}"#,
            )
            .unwrap();
            std::fs::write(folder.path().join(".env"), "XAI_API_KEY=\"xai-1\"\n").unwrap();
            let mut state = DesktopState {
                onboarded: true,
                ..Default::default()
            };
            state.upsert(Instance {
                id: "t".into(),
                name: "t".into(),
                everywhere: false,
                workspace: folder.path().to_path_buf(),
                config_dir: folder.path().to_path_buf(),
                provider: "xai".into(),
                model: "grok".into(),
                permission_profile: "auto".into(),
                color: Some(0x2550eb),
                pinned: true,
            });

            let copy = duplicate_instance(&state, "t").unwrap();
            assert_eq!(copy.id, "t-copy");
            assert_eq!(copy.name, "t copy");
            assert!(!copy.everywhere, "folder-scoped copy is detached");
            assert_eq!(copy.workspace, folder.path(), "workspace is kept");
            let home = home.path().canonicalize().unwrap();
            assert_eq!(
                copy.config_dir,
                home.join(".apollo").join("instances").join("t-copy")
            );
            assert_eq!(
                std::fs::read_to_string(copy.config_path()).unwrap(),
                r#"{"provider":{"name":"xai"},"model":"grok","workspace":"/w"}"#
            );
            assert_eq!(
                std::fs::read_to_string(copy.env_path()).unwrap(),
                "XAI_API_KEY=\"xai-1\"\n"
            );
            assert_eq!(copy.color, Some(0x2550eb));
            assert!(!copy.pinned, "the copy starts unpinned");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = std::fs::metadata(copy.env_path())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777;
                assert_eq!(mode, 0o600);
            }
            assert!(folder.path().join("apollo.json").is_file(), "source intact");

            // The second copy gets a numbered id.
            let mut two = state.clone();
            two.instances.push(copy);
            assert_eq!(duplicate_instance(&two, "t").unwrap().id, "t-copy-2");
            assert!(duplicate_instance(&two, "nope").is_err());
        });
    }

    #[test]
    fn duplicate_everywhere_instance_stays_everywhere() {
        let home = tempfile::tempdir().unwrap();
        temp_env::with_var("HOME", Some(home.path()), || {
            let config = home.path().join(".apollo").join("instances").join("e");
            std::fs::create_dir_all(&config).unwrap();
            std::fs::write(config.join("apollo.json"), "{}").unwrap();
            let mut state = DesktopState::default();
            state.upsert(Instance {
                id: "e".into(),
                name: "e".into(),
                everywhere: true,
                workspace: home.path().to_path_buf(),
                config_dir: config.clone(),
                provider: "chatgpt".into(),
                model: "gpt-5.5".into(),
                permission_profile: "auto".into(),
                color: None,
                pinned: false,
            });
            let copy = duplicate_instance(&state, "e").unwrap();
            assert!(copy.everywhere);
            assert_eq!(copy.workspace, home.path());
            assert_eq!(
                copy.config_dir,
                home.path().join(".apollo").join("instances").join("e-copy")
            );
        });
    }
}
