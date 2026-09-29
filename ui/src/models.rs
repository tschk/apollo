//! The model list for a provider, found automatically — the telekinesis
//! approach:
//!
//! 1. **live** — the provider's own `GET {base_url}/models` with the key or
//!    login the user just gave (Anthropic: `x-api-key`; OAuth plans: the
//!    `rs_ai_oauth` model listing). When a provider answers, its answer is
//!    the list.
//! 2. **models.dev** — the public catalog at `https://models.dev/api.json`,
//!    cached for a day.
//! 3. **built-in** — the static ids in `rs_ai_providers::catalog`.
//!
//! Lists that came from the network are cached at
//! `~/.apollo/models/<provider>.json` so the picker is instant next time.
//! Everything here blocks; the UI calls it on a worker thread. Keys are only
//! ever put in request headers — never logged, cached or returned in errors.

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

const MODELS_DEV_URL: &str = "https://models.dev/api.json";
const MODELS_DEV_TTL: u64 = 24 * 60 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Source {
    Live,
    ModelsDev,
    Catalog,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::Live => "live /models",
            Source::ModelsDev => "models.dev",
            Source::Catalog => "built-in catalog",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelList {
    pub provider: String,
    pub source: Source,
    pub fetched_at: u64,
    pub models: Vec<String>,
}

/// How to authenticate a live listing. Owned so it can move to a thread.
#[derive(Clone)]
pub enum Auth {
    None,
    Bearer(String),
    AnthropicKey(String),
    ClaudeLogin(String),
    OAuthPlan(rs_ai_oauth::OAuthProvider, String),
}

impl std::fmt::Debug for Auth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Auth::None => "Auth::None",
            Auth::Bearer(_) => "Auth::Bearer(***)",
            Auth::AnthropicKey(_) => "Auth::AnthropicKey(***)",
            Auth::ClaudeLogin(_) => "Auth::ClaudeLogin(***)",
            Auth::OAuthPlan(..) => "Auth::OAuthPlan(***)",
        })
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn cache_dir() -> Option<PathBuf> {
    crate::setup::home_dir().map(|h| h.join(".apollo").join("models"))
}

fn cache_file(provider: &str) -> Option<PathBuf> {
    let safe: String = provider
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    cache_dir().map(|d| d.join(format!("{safe}.json")))
}

pub fn cached(provider: &str) -> Option<ModelList> {
    let text = std::fs::read_to_string(cache_file(provider)?).ok()?;
    serde_json::from_str(&text).ok()
}

fn store(list: &ModelList) {
    let Some(path) = cache_file(&list.provider) else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_string_pretty(list) {
        let _ = std::fs::write(path, json);
    }
}

fn client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(8))
        .user_agent("apollo-ui")
        .build()
        .map_err(|e| format!("http client: {e}"))
}

/// GET `{base_url}/models`.
pub fn fetch_live(base_url: &str, auth: &Auth) -> Result<Vec<String>, String> {
    if let Auth::OAuthPlan(provider, token) = auth {
        let models = rs_ai_oauth::fetch_models(*provider, token)
            .map_err(|e| plain_live_error(&scrub(&e.to_string(), token)))?;
        return Ok(dedupe(models.into_iter().map(|m| m.id)));
    }
    let url = format!("{}/models", base_url.trim().trim_end_matches('/'));
    let mut req = client()?.get(&url).header("Accept", "application/json");
    let secret = match auth {
        Auth::None | Auth::OAuthPlan(..) => None,
        Auth::Bearer(k) => {
            req = req.bearer_auth(k);
            Some(k)
        }
        Auth::AnthropicKey(k) => {
            req = req
                .header("x-api-key", k)
                .header("anthropic-version", "2023-06-01");
            Some(k)
        }
        Auth::ClaudeLogin(t) => {
            req = req
                .bearer_auth(t)
                .header("anthropic-version", "2023-06-01")
                .header("anthropic-beta", "oauth-2025-04-20");
            Some(t)
        }
    };
    let hide = |e: String| match secret {
        Some(s) => scrub(&e, s),
        None => e,
    };
    let resp = req
        .send()
        .map_err(|e| hide(format!("could not reach {url}: {e}")))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(plain_live_error(&status.as_u16().to_string()));
    }
    let value: serde_json::Value = resp
        .json()
        .map_err(|e| hide(format!("/models was not JSON: {e}")))?;
    let models = parse_listing(&value);
    if models.is_empty() {
        return Err("/models answered with no models".into());
    }
    Ok(models)
}

/// Never let a key end up in an error shown on screen.
/// A sentence for the screen. Never the provider's raw body.
fn plain_live_error(err: &str) -> String {
    let lower = err.to_ascii_lowercase();
    if lower.contains("401")
        || lower.contains("403")
        || lower.contains("forbidden")
        || lower.contains("scope")
    {
        "the account can't list models".into()
    } else if lower.contains("404") {
        "this provider has no model listing".into()
    } else {
        "the provider didn't answer".into()
    }
}

fn scrub(text: &str, secret: &str) -> String {
    if secret.len() >= 6 {
        text.replace(secret, "***")
    } else {
        text.to_string()
    }
}

fn dedupe(ids: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for id in ids {
        let id = id.trim().to_string();
        if !id.is_empty() && !out.contains(&id) {
            out.push(id);
        }
    }
    out
}

/// `{"data":[{"id":..}]}` (OpenAI, Anthropic) or `{"models":[{"name"|"id":..}]}`
/// (Ollama, Gemini).
pub fn parse_listing(v: &serde_json::Value) -> Vec<String> {
    let pick = |arr: &Vec<serde_json::Value>, keys: &[&str]| -> Vec<String> {
        arr.iter()
            .filter_map(|m| {
                keys.iter()
                    .find_map(|k| m.get(*k).and_then(|x| x.as_str()))
                    .or_else(|| m.as_str())
                    .map(|s| s.strip_prefix("models/").unwrap_or(s).to_string())
            })
            .collect()
    };
    if let Some(arr) = v.get("data").and_then(|d| d.as_array()) {
        return dedupe(pick(arr, &["id", "name"]));
    }
    if let Some(arr) = v.get("models").and_then(|d| d.as_array()) {
        return dedupe(pick(arr, &["id", "name", "model"]));
    }
    Vec::new()
}

/// The models.dev catalog, from the day-old cache or the network.
fn models_dev_catalog() -> Option<serde_json::Value> {
    let path = cache_dir().map(|d| d.join("models.dev.json"));
    let cached: Option<serde_json::Value> = path
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str(&t).ok());
    if let Some(c) = &cached {
        let at = c["fetched_at"].as_u64().unwrap_or(0);
        if now().saturating_sub(at) < MODELS_DEV_TTL {
            return Some(c["catalog"].clone());
        }
    }
    let fresh = client()
        .ok()
        .and_then(|c| c.get(MODELS_DEV_URL).send().ok())
        .filter(|r| r.status().is_success())
        .and_then(|r| r.json::<serde_json::Value>().ok());
    match fresh {
        Some(catalog) => {
            if let Some(p) = &path {
                if let Some(dir) = p.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                let wrapped = serde_json::json!({ "fetched_at": now(), "catalog": catalog });
                let _ = std::fs::write(p, wrapped.to_string());
            }
            Some(catalog)
        }
        // Offline: a stale catalog beats none.
        None => cached.map(|c| c["catalog"].clone()),
    }
}

/// Higher is a better default: a current general model outranks mini, nano,
/// image and preview variants, then a higher version outranks an older one.
fn model_rank(id: &str) -> (i32, i32, i32, i32) {
    let lower = id.to_ascii_lowercase();
    let general = if [
        "mini",
        "nano",
        "lite",
        "preview",
        "image",
        "tts",
        "live",
        "luna",
        "terra",
        "sol",
        "spark",
        "codex",
        "embed",
        "audio",
        "realtime",
        "transcribe",
        "search",
    ]
    .iter()
    .any(|w| lower.contains(w))
    {
        0
    } else {
        1
    };
    let mut nums = Vec::new();
    let mut cur = String::new();
    for c in lower.chars() {
        if c.is_ascii_digit() {
            cur.push(c);
        } else if !cur.is_empty() {
            nums.push(cur.parse::<i32>().unwrap_or(0));
            cur.clear();
        }
    }
    if !cur.is_empty() {
        nums.push(cur.parse::<i32>().unwrap_or(0));
    }
    (
        general,
        *nums.first().unwrap_or(&0),
        nums.get(1).copied().unwrap_or(0),
        nums.get(2).copied().unwrap_or(0),
    )
}

pub fn models_dev_from_json(
    v: &serde_json::Value,
    provider: &str,
    aliases: &[&str],
) -> Vec<String> {
    let entry = std::iter::once(provider)
        .chain(aliases.iter().copied())
        .find_map(|k| {
            v.get(k)
                .and_then(|p| p.get("models"))
                .and_then(|m| m.as_object())
        });
    let Some(models) = entry else {
        return Vec::new();
    };
    let mut ids: Vec<String> = models.keys().cloned().collect();
    // Newest general model first. The catalog's own default is often a
    // generation behind (it still says gpt-5.5), so it does not win ties.
    ids.sort_by(|a, b| model_rank(b).cmp(&model_rank(a)).then(a.cmp(b)));
    let _ = provider;
    ids
}

pub fn models_dev(provider: &str) -> Option<Vec<String>> {
    let aliases = rs_ai_providers::catalog::by_id(provider)
        .map(|s| s.aliases)
        .unwrap_or(&[]);
    let ids = models_dev_from_json(&models_dev_catalog()?, provider, aliases);
    (!ids.is_empty()).then_some(ids)
}

pub fn catalog_models(provider: &str) -> Vec<String> {
    match rs_ai_providers::catalog::by_id(provider) {
        Some(spec) => dedupe(
            std::iter::once(spec.default_model)
                .chain(spec.models.iter().copied())
                .map(str::to_string),
        ),
        None => Vec::new(),
    }
}

fn with_extra(mut models: Vec<String>, extra: &[&str]) -> Vec<String> {
    for id in extra.iter().rev() {
        let id = id.trim();
        if id.is_empty() {
            continue;
        }
        if let Some(i) = models.iter().position(|m| m == id) {
            models.remove(i);
        }
        models.insert(0, id.to_string());
    }
    models
}

/// Live listing if possible, else models.dev, else the built-in catalog.
/// Returns the list and, when a live attempt failed, why.
pub fn resolve(
    provider: &str,
    live: Option<(&str, Auth)>,
    extra: &[&str],
) -> (ModelList, Option<String>) {
    let mut live_error = None;
    if let Some((base_url, auth)) = live {
        match fetch_live(base_url, &auth) {
            Ok(models) => {
                let list = ModelList {
                    provider: provider.into(),
                    source: Source::Live,
                    fetched_at: now(),
                    models,
                };
                store(&list);
                let models = with_extra(list.models.clone(), extra);
                return (ModelList { models, ..list }, None);
            }
            Err(e) => live_error = Some(e),
        }
    }
    if let Some(models) = models_dev(provider) {
        let list = ModelList {
            provider: provider.into(),
            source: Source::ModelsDev,
            fetched_at: now(),
            models,
        };
        store(&list);
        let models = with_extra(list.models.clone(), extra);
        return (ModelList { models, ..list }, live_error);
    }
    let list = ModelList {
        provider: provider.into(),
        source: Source::Catalog,
        fetched_at: now(),
        models: with_extra(catalog_models(provider), extra),
    };
    (list, live_error)
}

/// Case-insensitive substring filter. An empty query returns the full list.
pub fn filter_models(models: &[String], query: &str) -> Vec<String> {
    let query = query.trim().to_ascii_lowercase();
    if query.is_empty() {
        return models.to_vec();
    }
    models
        .iter()
        .filter(|model| model.to_ascii_lowercase().contains(&query))
        .cloned()
        .collect()
}

/// Effort tokens models.dev lists for this model, in catalog order.
/// `None` means the model has no reasoning dial, so the UI hides it.
pub fn effort_levels(model_id: &str) -> Option<Vec<String>> {
    // Cache only. Rendering must not wait on the network; a model refresh
    // fills this file, and the next frame grows or hides the dial.
    let path = cache_dir()?.join("models.dev.json");
    let text = std::fs::read_to_string(path).ok()?;
    let wrapped: serde_json::Value = serde_json::from_str(&text).ok()?;
    let catalog = wrapped.get("catalog").cloned().unwrap_or(wrapped);
    effort_levels_from_catalog(&catalog, model_id)
}

pub fn effort_levels_from_catalog(
    catalog: &serde_json::Value,
    model_id: &str,
) -> Option<Vec<String>> {
    let want = model_id.trim();
    if want.is_empty() {
        return None;
    }
    let providers = catalog.as_object()?;
    for provider in providers.values() {
        let Some(models) = provider.get("models").and_then(|m| m.as_object()) else {
            continue;
        };
        let Some(model) = models.get(want).or_else(|| {
            models
                .values()
                .find(|entry| entry.get("id").and_then(|v| v.as_str()) == Some(want))
        }) else {
            continue;
        };
        if let Some(levels) = effort_values(model) {
            return Some(levels);
        }
    }
    None
}

fn effort_values(model: &serde_json::Value) -> Option<Vec<String>> {
    let options = model.get("reasoning_options")?.as_array()?;
    for option in options {
        if option.get("type").and_then(|t| t.as_str()) != Some("effort") {
            continue;
        }
        let values = option
            .get("values")?
            .as_array()?
            .iter()
            .filter_map(|v| v.as_str())
            .filter_map(effort_token)
            .map(str::to_string)
            .collect::<Vec<_>>();
        if !values.is_empty() {
            return Some(values);
        }
    }
    None
}

pub fn effort_token(value: &str) -> Option<&'static str> {
    match value.trim() {
        "none" => Some("none"),
        "minimal" => Some("minimal"),
        "low" => Some("low"),
        "medium" => Some("medium"),
        "high" => Some("high"),
        "xhigh" => Some("xhigh"),
        "max" => Some("max"),
        _ => None,
    }
}

/// Keep `current` when the model still offers it. Otherwise the middle option,
/// or nothing when the model has no dial.
pub fn snap_effort(model_id: &str, current: &str) -> String {
    match effort_levels(model_id) {
        Some(levels) if levels.iter().any(|level| level == current) => current.to_string(),
        Some(levels) => levels
            .iter()
            .find(|level| *level == "medium" || *level == "high")
            .cloned()
            .or_else(|| levels.first().cloned())
            .unwrap_or_default(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_openai_anthropic_and_ollama_shapes() {
        let openai = json!({"object":"list","data":[{"id":"gpt-5"},{"id":"gpt-5-mini"}]});
        assert_eq!(parse_listing(&openai), vec!["gpt-5", "gpt-5-mini"]);
        let anthropic =
            json!({"data":[{"type":"model","id":"claude-sonnet-4-6"}],"has_more":false});
        assert_eq!(parse_listing(&anthropic), vec!["claude-sonnet-4-6"]);
        let ollama = json!({"models":[{"name":"llama3"},{"name":"models/gemini-3"}]});
        assert_eq!(parse_listing(&ollama), vec!["llama3", "gemini-3"]);
        assert!(parse_listing(&json!({"error":"nope"})).is_empty());
        let dup = json!({"data":[{"id":"a"},{"id":" a "},{"id":""},{"id":"b"}]});
        assert_eq!(parse_listing(&dup), vec!["a", "b"]);
    }

    #[test]
    fn filter_is_a_case_insensitive_substring() {
        let models = vec!["gpt-5.6".into(), "gpt-6-astra".into(), "gpt-5.5".into()];
        assert_eq!(
            filter_models(&models, "6-A"),
            vec!["gpt-6-astra".to_string()]
        );
        assert_eq!(filter_models(&models, "gpt-6-luna"), Vec::<String>::new());
        assert_eq!(filter_models(&models, "  ").len(), 3);
    }

    #[test]
    fn effort_levels_follow_the_model() {
        let catalog = json!({
            "openai": {"models": {
                "gpt-5.5": {"reasoning_options": [{"type": "effort", "values": ["low", "medium", "high", "xhigh"]}]},
                "gpt-image": {"reasoning_options": []}
            }}
        });
        assert_eq!(
            effort_levels_from_catalog(&catalog, "gpt-5.5"),
            Some(vec![
                "low".into(),
                "medium".into(),
                "high".into(),
                "xhigh".into()
            ])
        );
        assert!(effort_levels_from_catalog(&catalog, "gpt-image").is_none());
        assert!(effort_levels_from_catalog(&catalog, "missing").is_none());
    }

    #[test]
    fn models_dev_uses_aliases() {
        let v = json!({"zhipu": {"models": {"glm-5": {}, "glm-4.7": {}}}});
        assert_eq!(
            models_dev_from_json(&v, "nobody", &["zhipu"]),
            vec!["glm-5", "glm-4.7"]
        );
        let openai = serde_json::json!({"openai": {"models": {"gpt-5.5": {}, "gpt-5.6": {}, "gpt-5.4-mini": {}}}});
        let ranked = models_dev_from_json(&openai, "openai", &[]);
        assert_eq!(ranked[0], "gpt-5.6");
        assert!(models_dev_from_json(&v, "nobody", &[]).is_empty());
    }

    #[test]
    fn catalog_models_start_with_default() {
        let spec = rs_ai_providers::catalog::by_id("zai-coding-plan").unwrap();
        let models = catalog_models("zai-coding-plan");
        assert_eq!(models[0], spec.default_model);
        assert!(catalog_models("no-such-provider").is_empty());
    }

    #[test]
    fn cache_round_trip_and_offline_resolve() {
        let home = tempfile::tempdir().unwrap();
        temp_env::with_var("HOME", Some(home.path()), || {
            let list = ModelList {
                provider: "x/y".into(),
                source: Source::Live,
                fetched_at: 1,
                models: vec!["m".into()],
            };
            store(&list);
            assert_eq!(cached("x/y"), Some(list));

            // A fresh models.dev cache means no network is touched.
            let dir = cache_dir().unwrap();
            let catalog = json!({"zai-coding-plan": {"models": {"glm-5.1": {}, "glm-4.7": {}}}});
            std::fs::write(
                dir.join("models.dev.json"),
                json!({"fetched_at": now(), "catalog": catalog}).to_string(),
            )
            .unwrap();
            let (got, err) = resolve("zai-coding-plan", None, &["my-model"]);
            assert_eq!(got.source, Source::ModelsDev);
            assert_eq!(got.models[0], "my-model");
            assert!(got.models.contains(&"glm-5.1".to_string()));
            assert!(err.is_none());
            assert_eq!(cached("zai-coding-plan").unwrap().source, Source::ModelsDev);

            let (got, _) = resolve("groq", None, &[]);
            assert_eq!(got.source, Source::Catalog);
            assert!(!got.models.is_empty());
        });
    }

    #[test]
    fn live_errors_stay_short() {
        let raw = "Network error: models request returned status 403 Forbidden: Missing scopes: api.model.read";
        assert_eq!(plain_live_error(raw), "the account can't list models");
        assert!(!plain_live_error(raw).contains("scope"));
    }

    #[test]
    fn debug_never_shows_the_key() {
        let a = Auth::Bearer("sk-secret-value".into());
        assert!(!format!("{a:?}").contains("secret"));
        assert_eq!(
            scrub("bad sk-secret-value here", "sk-secret-value"),
            "bad *** here"
        );
    }
}
