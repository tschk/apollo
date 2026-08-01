//! Default model per provider.
//!
//! A provider configured with no model has to land somewhere. Picking the
//! wrong entry is a billing decision, not a cosmetic one, so these track the
//! defaults published by NousResearch/hermes-agent — the first entry of each
//! provider's curated list, which is what its
//! `get_default_model_for_provider()` returns.
//!
//! Only providers apollo can actually construct appear here. An unknown
//! provider yields `None` and the caller keeps whatever the config already
//! had, rather than being sent to a model that may not exist on it.

/// Provider name (and common aliases) to the model it defaults to.
const DEFAULTS: &[(&str, &str)] = &[
    ("chatgpt", "gpt-5.5"),
    ("openai", "gpt-5.4"),
    ("copilot", "gpt-5.4"),
    ("github-copilot", "gpt-5.4"),
    ("xai", "grok-build-0.1"),
    ("grok", "grok-build-0.1"),
    ("gemini", "gemini-3.1-pro-preview"),
    ("deepseek", "deepseek-v4-pro"),
    ("moonshot", "kimi-k3"),
    ("kimi", "kimi-k3"),
    ("minimax", "MiniMax-M3"),
    ("huggingface", "moonshotai/Kimi-K2.5"),
    // Metered aggregator: hermes routes this one through its catalog-labeled
    // default rather than the head of the list, because that list is ordered
    // most-capable-first and defaulting to the head would silently pick the
    // most expensive model.
    ("openrouter", "z-ai/glm-5.2"),
];

/// The model a provider falls back to when none was configured.
pub fn default_model_for_provider(provider: &str) -> Option<&'static str> {
    let key = provider.trim();
    DEFAULTS
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(key))
        .map(|(_, model)| *model)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oauth_providers_get_their_own_defaults() {
        assert_eq!(default_model_for_provider("chatgpt"), Some("gpt-5.5"));
        assert_eq!(default_model_for_provider("xai"), Some("grok-build-0.1"));
    }

    #[test]
    fn aliases_resolve_to_the_same_model() {
        for (a, b) in [("grok", "xai"), ("kimi", "moonshot")] {
            assert_eq!(
                default_model_for_provider(a),
                default_model_for_provider(b),
                "{a} and {b} should agree"
            );
        }
    }

    #[test]
    fn lookup_ignores_case_and_padding() {
        assert_eq!(default_model_for_provider("  ChatGpt "), Some("gpt-5.5"));
    }

    #[test]
    fn an_unknown_provider_picks_nothing() {
        assert_eq!(default_model_for_provider("not-a-provider"), None);
        assert_eq!(default_model_for_provider(""), None);
    }
}
