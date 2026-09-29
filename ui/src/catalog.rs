//! The API-key providers the onboarding offers, built from
//! `rs_ai_providers::catalog` — the same catalog telekinesis uses — rather
//! than a hand-kept list.
//!
//! apollo constructs a handful of providers natively and reads their key
//! from the provider's own variable (`bootstrap::catalog_env_key`). Every
//! other OpenAI-compatible catalog entry is run through apollo's generic
//! OpenAI-compatible client: `provider.name` is the catalog id,
//! `provider.base_url` the catalog URL, and the key goes to `.env` as
//! [`CUSTOM_KEY_VAR`], which apollo reads for any configured provider.

use std::sync::LazyLock;

use rs_ai_providers::catalog::{self, ProviderApi, ProviderSpec};

use crate::setup::{Auth, ProviderInfo, CUSTOM_KEY_VAR};

/// Catalog id → (apollo provider name, key variable apollo reads for it).
/// Only providers `bootstrap::build_provider` constructs by name.
fn native(id: &str) -> Option<(&'static str, &'static str)> {
    Some(match id {
        "openai" => ("openai", "OPENAI_API_KEY"),
        "anthropic" => ("anthropic", "ANTHROPIC_API_KEY"),
        "google" => ("gemini", "GEMINI_API_KEY"),
        "openrouter" => ("openrouter", "OPENROUTER_API_KEY"),
        "xai" => ("xai", "XAI_API_KEY"),
        "deepseek" => ("deepseek", "DEEPSEEK_API_KEY"),
        "groq" => ("groq", "GROQ_API_KEY"),
        "togetherai" => ("together", "TOGETHER_API_KEY"),
        "mistral" => ("mistral", "MISTRAL_API_KEY"),
        "fireworks-ai" => ("fireworks", "FIREWORKS_API_KEY"),
        "perplexity" => ("perplexity", "PERPLEXITY_API_KEY"),
        "moonshotai" => ("moonshot", "MOONSHOT_API_KEY"),
        "venice" => ("venice", "VENICE_API_KEY"),
        "huggingface" => ("huggingface", "HF_TOKEN"),
        "siliconflow" => ("siliconflow", "SILICONFLOW_API_KEY"),
        "cerebras" => ("cerebras", "CEREBRAS_API_KEY"),
        "minimax" => ("minimax", "MINIMAX_API_KEY"),
        _ => return None,
    })
}

/// Shown first, in this order. Everything else follows alphabetically.
const FEATURED: &[&str] = &[
    "openrouter",
    "openai",
    "anthropic",
    "google",
    "xai",
    "deepseek",
    "zai-coding-plan",
    "zai",
    "kimi-for-coding",
    "moonshotai",
    "alibaba-coding-plan",
    "alibaba",
    "minimax",
    "xiaomi",
    "opencode-go",
    "groq",
    "mistral",
    "togetherai",
    "fireworks-ai",
    "cerebras",
    "deepinfra",
    "nvidia",
    "huggingface",
    "perplexity",
    "siliconflow",
    "venice",
    "ollama-cloud",
];

/// Entries handled elsewhere or not usable with a single key.
const SKIP: &[&str] = &[
    "github-copilot", // sign-in card
    "ollama",         // Ollama row (local, no key)
    "lmstudio",       // local; use the custom endpoint
    "cohere",         // v1 API is not OpenAI-compatible
];

/// Better first picks than the catalog's alphabetical defaults.
fn preferred_model(id: &str) -> Option<&'static str> {
    Some(match id {
        "openrouter" => "z-ai/glm-5.2",
        "openai" => "gpt-5.4",
        "anthropic" => "claude-sonnet-4-6",
        "google" => "gemini-3.1-pro-preview",
        "xai" => "grok-build-0.1",
        "deepseek" => "deepseek-v4-pro",
        "moonshotai" => "kimi-k3",
        "zai" | "zai-coding-plan" => "glm-5.1",
        "groq" => "openai/gpt-oss-120b",
        _ => return None,
    })
}

fn blurb(spec: &ProviderSpec) -> &'static str {
    match spec.id {
        "openrouter" => "one key, many models",
        "anthropic" => "claude api key (console.anthropic.com)",
        "zai-coding-plan" => "glm coding plan",
        "kimi-for-coding" => "kimi coding plan",
        "alibaba-coding-plan" => "qwen coding plan",
        "alibaba" => "qwen · dashscope",
        "minimax" => "minimax models",
        "opencode-go" => "opencode zen",
        _ => "api key",
    }
}

fn info(spec: &'static ProviderSpec) -> ProviderInfo {
    let (id, var, base_url) = match native(spec.id) {
        Some((name, var)) => (name, var, None),
        None => (spec.id, CUSTOM_KEY_VAR, Some(spec.base_url)),
    };
    ProviderInfo {
        id,
        label: spec.name,
        auth: Auth::ApiKey(var),
        default_model: preferred_model(spec.id).unwrap_or(spec.default_model),
        blurb: blurb(spec),
        catalog: spec.id,
        base_url,
    }
}

fn usable(spec: &ProviderSpec) -> bool {
    !SKIP.contains(&spec.id)
        && !spec.base_url.is_empty()
        && !spec.base_url.contains('{')
        && !spec.env_vars.is_empty()
        && matches!(
            spec.api,
            ProviderApi::OpenAiCompatible | ProviderApi::Anthropic
        )
        && (spec.api != ProviderApi::Anthropic || spec.id == "anthropic")
}

/// Every provider behind the dropdown: featured ones, the rest of the
/// catalog A–Z, then Ollama and the custom endpoint.
pub static PROVIDERS: LazyLock<Vec<ProviderInfo>> = LazyLock::new(|| {
    let mut out: Vec<ProviderInfo> = FEATURED
        .iter()
        .filter_map(|id| catalog::by_id(id))
        .filter(|s| usable(s))
        .map(info)
        .collect();
    let mut rest: Vec<&'static ProviderSpec> = catalog::API_KEY_PROVIDERS
        .iter()
        .filter(|s| usable(s) && !FEATURED.contains(&s.id))
        .collect();
    rest.sort_by_key(|s| s.name.to_ascii_lowercase());
    let mut seen: Vec<&str> = out.iter().map(|p| p.catalog).collect();
    for spec in rest {
        if !seen.contains(&spec.id) {
            seen.push(spec.id);
            out.push(info(spec));
        }
    }
    out.push(ProviderInfo {
        id: "ollama",
        label: "Ollama",
        auth: Auth::Local,
        default_model: "llama3.2",
        blurb: "local models, no key",
        catalog: "ollama",
        base_url: None,
    });
    out.push(ProviderInfo {
        id: "custom",
        label: "Custom endpoint",
        auth: Auth::Custom,
        default_model: "",
        blurb: "name, base url, api key",
        catalog: "",
        base_url: None,
    });
    out
});

#[cfg(test)]
mod tests {
    use super::*;

    fn find(catalog_id: &str) -> Option<&'static ProviderInfo> {
        PROVIDERS.iter().find(|p| p.catalog == catalog_id)
    }

    #[test]
    fn featured_come_first_and_custom_last() {
        assert_eq!(PROVIDERS[0].catalog, "openrouter");
        assert_eq!(PROVIDERS.last().unwrap().id, "custom");
        assert!(PROVIDERS.len() > 40, "whole catalog is offered");
    }

    #[test]
    fn native_providers_use_their_own_variable() {
        let google = find("google").unwrap();
        assert_eq!(google.id, "gemini");
        assert_eq!(google.env_var(), Some("GEMINI_API_KEY"));
        assert_eq!(google.base_url, None);
        let claude = find("anthropic").unwrap();
        assert_eq!(claude.id, "anthropic");
        assert_eq!(claude.env_var(), Some("ANTHROPIC_API_KEY"));
    }

    #[test]
    fn generic_providers_carry_base_url_and_shared_key() {
        for id in ["zai-coding-plan", "kimi-for-coding", "alibaba", "xiaomi"] {
            let p = find(id).unwrap_or_else(|| panic!("{id} offered"));
            assert_eq!(p.id, id);
            assert_eq!(p.env_var(), Some(CUSTOM_KEY_VAR));
            assert!(p.base_url.unwrap().starts_with("https://"));
        }
    }

    #[test]
    fn oauth_and_multi_field_entries_are_skipped() {
        assert!(find("github-copilot").is_none());
        assert!(find("amazon-bedrock").is_none());
        assert!(find("cloudflare-workers-ai").is_none());
        let ids: Vec<_> = PROVIDERS.iter().map(|p| p.id).collect();
        let mut dedup = ids.clone();
        dedup.sort();
        dedup.dedup();
        assert_eq!(ids.len(), dedup.len(), "ids are unique");
    }
}
