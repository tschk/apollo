//! Anthropic (Claude) provider implementation.
//! Supports both API keys and OAuth tokens (Claude.dev)

use async_trait::async_trait;
use serde_json::Value;

use super::retry::send_with_retry;
use super::traits::*;
use crate::cost::{CostTracker, TokenUsage};
use crate::text::truncate_chars;
use crate::tools::ToolSpec;

/// Server tool with dynamic filtering, on models that accept it.
const WEB_SEARCH_FILTERED: &str = "web_search_20260209";
/// The original server tool, accepted by every model that has web search.
const WEB_SEARCH_BASIC: &str = "web_search_20250305";

/// Cap on server-side searches per turn. Also keeps the turn clear of the
/// server tool loop's own iteration limit, which would end it in `pause_turn`.
const WEB_SEARCH_MAX_USES: u32 = 5;

/// Model families that accept the filtered web search tool. Anything else gets
/// the basic tool, which is the safe floor.
const FILTERED_SEARCH_MODELS: &[&str] = &[
    "claude-opus-5",
    "claude-opus-4-8",
    "claude-opus-4-7",
    "claude-opus-4-6",
    "claude-sonnet-5",
    "claude-sonnet-4-6",
    "claude-fable-5",
    "claude-mythos-5",
];

/// Error code from a `web_search_tool_result` block, if it failed.
///
/// A search that worked carries a list of results in `content`; one that failed
/// carries a single object instead, and the request still returns HTTP 200. The
/// shape is the only signal.
fn web_search_error(block: &Value) -> Option<String> {
    let content = block.get("content")?;
    if content.is_array() {
        return None;
    }
    Some(
        content
            .get("error_code")
            .and_then(|c| c.as_str())
            .unwrap_or("unknown error")
            .to_string(),
    )
}

/// Pick the newest web search tool the model accepts.
fn web_search_tool_type(model: &str) -> &'static str {
    if FILTERED_SEARCH_MODELS.iter().any(|m| model.starts_with(m)) {
        WEB_SEARCH_FILTERED
    } else {
        WEB_SEARCH_BASIC
    }
}

pub struct AnthropicProvider {
    api_key: String,
    /// Present when the credential is an OAuth token loaded from the Claude
    /// credentials file. The cache refreshes on expiry and persists the new
    /// token, so a long-running process survives past the first hour.
    oauth: Option<super::oauth::OAuthTokenCache>,
    base_url: String,
    cost_tracker: Option<std::sync::Arc<CostTracker>>,
    /// Declare Anthropic's server-side web search alongside apollo's own tools.
    native_web_search: bool,
}

impl AnthropicProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            oauth: None,
            base_url: "https://api.anthropic.com/v1".to_string(),
            cost_tracker: None,
            native_web_search: false,
        }
    }

    /// Create from OAuth token (Claude.dev) or fallback to environment/file
    pub fn from_env_or_oauth() -> anyhow::Result<Self> {
        // Try standard API key first
        if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
            return Ok(Self::new(key));
        }

        // Try loading from Claude.dev OAuth credentials
        if let Ok(cache) = super::oauth::OAuthTokenCache::from_credentials_file() {
            let (token, _, _) = super::oauth::load_oauth_token_from_file()?;
            let mut provider = Self::new(token);
            provider.oauth = Some(cache);
            return Ok(provider);
        }

        Err(anyhow::anyhow!(
            "No ANTHROPIC_API_KEY found. Set env var or install Claude for Desktop with OAuth token."
        ))
    }

    /// Resolve the credential for a request, refreshing an expired OAuth token
    /// first. Falls back to the token captured at construction when there is no
    /// OAuth cache (plain API key) or the refresh fails.
    async fn resolve_key(&self) -> String {
        match &self.oauth {
            Some(cache) => match cache.get_token().await {
                Ok(token) => token,
                Err(e) => {
                    tracing::warn!("OAuth token refresh failed, using cached token: {}", e);
                    self.api_key.clone()
                }
            },
            None => self.api_key.clone(),
        }
    }

    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    pub fn with_cost_tracker(mut self, tracker: std::sync::Arc<CostTracker>) -> Self {
        self.cost_tracker = Some(tracker);
        self
    }

    pub fn with_native_web_search(mut self, enabled: bool) -> Self {
        self.native_web_search = enabled;
        self
    }

    /// Convert internal ChatMessage list to Anthropic API format.
    /// Handles: system (filtered), user, assistant, tool_result → user with content blocks
    fn build_anthropic_messages(&self, messages: &[ChatMessage]) -> Vec<Value> {
        let mut result: Vec<Value> = Vec::new();

        for msg in messages {
            match msg.role.as_str() {
                "system" => continue, // handled separately
                "user" => {
                    result.push(serde_json::json!({
                        "role": "user",
                        "content": &msg.content,
                    }));
                }
                "assistant" => {
                    result.push(serde_json::json!({
                        "role": "assistant",
                        "content": &msg.content,
                    }));
                }
                "assistant_tool_use" => {
                    // Assistant message that requested tool use — reconstruct content blocks
                    // The content field has the text, tool_use_id has serialized tool calls
                    if let Some(tool_json) = &msg.tool_use_id {
                        if let Ok(blocks) = serde_json::from_str::<Vec<Value>>(tool_json) {
                            result.push(serde_json::json!({
                                "role": "assistant",
                                "content": blocks,
                            }));
                        }
                    }
                }
                "tool_result" => {
                    // Anthropic wants tool results as role "user" with tool_result content blocks
                    if let Some(tool_use_id) = &msg.tool_use_id {
                        result.push(serde_json::json!({
                            "role": "user",
                            "content": [{
                                "type": "tool_result",
                                "tool_use_id": tool_use_id,
                                "content": &msg.content,
                            }],
                        }));
                    }
                }
                other => {
                    // Fallback
                    result.push(serde_json::json!({
                        "role": other,
                        "content": &msg.content,
                    }));
                }
            }
        }

        result
    }

    fn build_tools_payload(&self, tools: &[ToolSpec]) -> Vec<Value> {
        tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.parameters,
                })
            })
            .collect()
    }

    /// Extract usage from Anthropic API response and record cost
    async fn record_usage(&self, data: &Value, model: &str) {
        if let Some(tracker) = &self.cost_tracker {
            if let Some(usage_obj) = data.get("usage").and_then(|v| v.as_object()) {
                let input_tokens = usage_obj
                    .get("input_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as usize;
                let output_tokens = usage_obj
                    .get("output_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as usize;

                let usage = TokenUsage {
                    input_tokens,
                    output_tokens,
                    total_tokens: input_tokens + output_tokens,
                };

                if let Err(e) = tracker.record(model, usage).await {
                    tracing::warn!("Failed to record cost: {}", e);
                }
            }
        }
    }
}

#[async_trait]
impl Provider for AnthropicProvider {
    fn name(&self) -> &str {
        "anthropic"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            native_tools: true,
            streaming: true,
            vision: true,
            max_context: 200_000,
            native_web_search: self.native_web_search,
        }
    }

    async fn chat(&self, request: &ChatRequest<'_>) -> anyhow::Result<ChatResponse> {
        // Create client with 120s socket timeout (LLM calls can be slow)
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()?;

        // Split system message from conversation (combine multiple system msgs)
        // Use prompt caching for system prompts (cache_control breakpoint)
        let system: Option<Value> = {
            let sys_parts: Vec<&str> = request
                .messages
                .iter()
                .filter(|m| m.role == "system")
                .map(|m| m.content.as_str())
                .collect();
            if sys_parts.is_empty() {
                None
            } else {
                // Wrap system prompt with cache_control for prompt caching
                Some(serde_json::json!([{
                    "type": "text",
                    "text": sys_parts.join("\n\n---\n\n"),
                    "cache_control": {"type": "ephemeral"}
                }]))
            }
        };

        // Build Anthropic-format messages
        let messages: Vec<Value> = self.build_anthropic_messages(request.messages);

        let mut body = serde_json::json!({
            "model": request.model,
            "messages": messages,
            "max_tokens": request.max_tokens.unwrap_or(8192),
            "temperature": request.temperature,
        });

        if let Some(sys) = system {
            body["system"] = sys;
        }

        let mut tool_payload = request
            .tools
            .filter(|t| !t.is_empty())
            .map(|t| self.build_tools_payload(t))
            .unwrap_or_default();

        // Anthropic runs this one itself; the results come back in the same
        // response rather than as a tool call apollo has to execute.
        if self.native_web_search {
            tool_payload.push(serde_json::json!({
                "type": web_search_tool_type(request.model),
                "name": "web_search",
                "max_uses": WEB_SEARCH_MAX_USES,
            }));
        }

        if !tool_payload.is_empty() {
            body["tools"] = Value::Array(tool_payload);
        }

        // Detect OAuth tokens (sk-ant-oat) vs API keys (sk-ant-api)
        let api_key = self.resolve_key().await;
        let is_oauth = api_key.contains("sk-ant-oat");

        // OAuth tokens require the system prompt to start with the Claude Code identity prefix
        if is_oauth {
            let prefix = serde_json::json!({
                "type": "text",
                "text": "You are Claude Code, Anthropic's official CLI for Claude.",
                "cache_control": {"type": "ephemeral"}
            });
            match body.get("system") {
                Some(Value::Array(blocks)) => {
                    let mut new_blocks = vec![prefix];
                    new_blocks.extend(blocks.iter().cloned());
                    body["system"] = Value::Array(new_blocks);
                }
                Some(Value::String(s)) => {
                    body["system"] = serde_json::json!([
                        prefix,
                        {"type": "text", "text": s, "cache_control": {"type": "ephemeral"}}
                    ]);
                }
                None => {
                    body["system"] = serde_json::json!([prefix]);
                }
                _ => {}
            }
        }

        let mut req_builder = client
            .post(format!("{}/messages", self.base_url))
            .header("content-type", "application/json")
            .header("anthropic-version", "2023-06-01");

        if is_oauth {
            req_builder = req_builder
                .header("Authorization", format!("Bearer {api_key}"))
                .header(
                    "anthropic-beta",
                    "claude-code-20250219,oauth-2025-04-20,interleaved-thinking-2025-05-14",
                )
                .header("anthropic-dangerous-direct-browser-access", "true");
        } else {
            req_builder = req_builder
                .header("x-api-key", &api_key)
                .header("anthropic-beta", "prompt-caching-2024-07-31");
        }

        let resp = send_with_retry(req_builder.json(&body), self.name()).await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!(
                "Anthropic API error {}: {}",
                status,
                truncate_chars(&text, 200)
            );
        }

        // Capture response headers for rate limit tracking
        let headers = resp.headers().clone();

        let data: Value = resp.json().await?;

        // Record usage for cost tracking
        self.record_usage(&data, request.model).await;

        // Update rate limits from response headers
        if let Some(tracker) = &self.cost_tracker {
            tracker.update_rate_limits(&headers).await;
        }

        let mut text_parts = Vec::new();
        let mut tool_calls = Vec::new();

        if let Some(content) = data["content"].as_array() {
            for block in content {
                match block["type"].as_str() {
                    Some("text") => {
                        if let Some(t) = block["text"].as_str() {
                            text_parts.push(t.to_string());
                        }
                    }
                    Some("tool_use") => {
                        tool_calls.push(ToolCall {
                            id: block["id"].as_str().unwrap_or("").to_string(),
                            name: block["name"].as_str().unwrap_or("").to_string(),
                            arguments: block["input"].to_string(),
                        });
                    }
                    // Server tools resolve before the response is returned, so
                    // these are a record of what ran, not work for apollo. Only
                    // a failure needs surfacing — the model folds successful
                    // results into its own text.
                    Some("web_search_tool_result") => {
                        if let Some(code) = web_search_error(block) {
                            tracing::warn!("anthropic web search failed: {code}");
                        }
                    }
                    _ => {}
                }
            }
        }

        // The server tool loop stopped at its own iteration limit rather than
        // finishing. `max_uses` above is set to keep turns clear of this, so it
        // should not happen — say so if it does rather than return a truncated
        // answer as if it were complete.
        if data["stop_reason"].as_str() == Some("pause_turn") {
            tracing::warn!(
                "anthropic paused the turn at the server tool limit; the response may be incomplete"
            );
        }

        let usage = data["usage"].as_object().map(|u| Usage {
            input_tokens: u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
            output_tokens: u.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
        });

        Ok(ChatResponse {
            text: if text_parts.is_empty() {
                None
            } else {
                Some(text_parts.join(""))
            },
            tool_calls,
            usage,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filtered_search_only_for_models_that_accept_it() {
        assert_eq!(web_search_tool_type("claude-opus-5"), WEB_SEARCH_FILTERED);
        assert_eq!(
            web_search_tool_type("claude-sonnet-4-6"),
            WEB_SEARCH_FILTERED
        );
        // apollo's own onboarding default predates the filtered tool.
        assert_eq!(web_search_tool_type("claude-sonnet-4-5"), WEB_SEARCH_BASIC);
        assert_eq!(web_search_tool_type("claude-haiku-4-5"), WEB_SEARCH_BASIC);
        assert_eq!(web_search_tool_type(""), WEB_SEARCH_BASIC);
    }

    #[test]
    fn a_result_list_is_not_an_error() {
        let block = serde_json::json!({
            "type": "web_search_tool_result",
            "content": [{"type": "web_search_result", "url": "https://example.com"}],
        });
        assert_eq!(web_search_error(&block), None);
    }

    #[test]
    fn a_result_object_is_an_error() {
        let block = serde_json::json!({
            "type": "web_search_tool_result",
            "content": {"type": "web_search_tool_result_error", "error_code": "max_uses_exceeded"},
        });
        assert_eq!(
            web_search_error(&block).as_deref(),
            Some("max_uses_exceeded")
        );
    }

    #[test]
    fn an_error_without_a_code_still_reports() {
        let block = serde_json::json!({"content": {}});
        assert_eq!(web_search_error(&block).as_deref(), Some("unknown error"));
    }

    #[test]
    fn native_search_is_off_unless_asked_for() {
        assert!(!AnthropicProvider::new("k").capabilities().native_web_search);
        assert!(
            AnthropicProvider::new("k")
                .with_native_web_search(true)
                .capabilities()
                .native_web_search
        );
    }
}
