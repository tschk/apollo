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

/// How many times a paused turn is resumed before returning what there is.
const MAX_PAUSE_RESUMES: u32 = 5;

/// Fold one response's token counts into the running total for the turn.
fn add_usage(total: &mut Option<Usage>, data: &Value) {
    let Some(u) = data["usage"].as_object() else {
        return;
    };
    let field = |k: &str| u.get(k).and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let entry = total.get_or_insert_with(Usage::default);
    entry.input_tokens += field("input_tokens");
    entry.output_tokens += field("output_tokens");
}

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

        let mut text_parts = Vec::new();
        let mut tool_calls = Vec::new();
        let mut usage: Option<Usage> = None;

        // A turn that exhausts the server tool loop comes back as `pause_turn`
        // with a partial answer. Resuming means replaying the paused assistant
        // turn verbatim and asking again — the server picks up where it left
        // off, so no extra user message is added.
        for attempt in 0..=MAX_PAUSE_RESUMES {
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
                        // Server tools resolve before the response is returned,
                        // so these are a record of what ran, not work for
                        // apollo. Only a failure needs surfacing — the model
                        // folds successful results into its own text.
                        Some("web_search_tool_result") => {
                            if let Some(code) = web_search_error(block) {
                                tracing::warn!("anthropic web search failed: {code}");
                            }
                        }
                        _ => {}
                    }
                }
            }

            // Each attempt reports only its own tokens, so the caller sees the
            // whole turn rather than the last leg of it.
            add_usage(&mut usage, &data);

            if data["stop_reason"].as_str() != Some("pause_turn") {
                break;
            }
            // A pending client tool call outranks resuming: apollo has to run it
            // before the turn can go anywhere.
            if !tool_calls.is_empty() {
                break;
            }
            if attempt == MAX_PAUSE_RESUMES {
                tracing::warn!(
                    "anthropic still paused after {MAX_PAUSE_RESUMES} resumes; \
                     returning a partial response"
                );
                break;
            }

            let Some(content) = data.get("content").cloned() else {
                tracing::warn!("anthropic paused the turn without content; cannot resume");
                break;
            };
            // The paused turn has to go back exactly as it arrived — the server
            // reads its own tool blocks out of it to resume. Failing to append
            // would re-send the same request and be billed for the same pause,
            // so stop instead of retrying something that cannot change.
            let Some(messages) = body["messages"].as_array_mut() else {
                tracing::warn!("anthropic request had no message list; cannot resume");
                break;
            };
            messages.push(serde_json::json!({
                "role": "assistant",
                "content": content,
            }));
            tracing::debug!("resuming a paused anthropic turn (attempt {})", attempt + 1);
        }

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
    use std::sync::atomic::{AtomicUsize, Ordering};

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

    /// Serve canned responses that pause the turn `pauses` times before
    /// finishing. Returns the base URL and the request counter.
    async fn paused_server(pauses: usize) -> (String, std::sync::Arc<AtomicUsize>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen = std::sync::Arc::new(AtomicUsize::new(0));
        let counter = std::sync::Arc::clone(&seen);

        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let n = counter.fetch_add(1, Ordering::SeqCst);
                let mut buf = vec![0u8; 65536];
                let _ = socket.read(&mut buf).await;

                let payload = if n < pauses {
                    serde_json::json!({
                        "content": [
                            {"type": "server_tool_use", "id": "s", "name": "web_search",
                             "input": {"query": "q"}},
                            {"type": "text", "text": format!("part{n} ")},
                        ],
                        "usage": {"input_tokens": 100, "output_tokens": 10},
                        "stop_reason": "pause_turn",
                    })
                } else {
                    serde_json::json!({
                        "content": [{"type": "text", "text": "final."}],
                        "usage": {"input_tokens": 100, "output_tokens": 10},
                        "stop_reason": "end_turn",
                    })
                }
                .to_string();

                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
                     content-length: {}\r\nconnection: close\r\n\r\n{}",
                    payload.len(),
                    payload
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.flush().await;
            }
        });

        (format!("http://{addr}"), seen)
    }

    async fn chat_against(base: &str) -> ChatResponse {
        let p = AnthropicProvider::new("sk-ant-test").with_base_url(base);
        let msgs = [ChatMessage::user("hi")];
        let req = ChatRequest {
            messages: &msgs,
            tools: None,
            model: "claude-opus-5",
            temperature: 0.0,
            max_tokens: Some(64),
        };
        p.chat(&req).await.expect("mock call")
    }

    #[tokio::test]
    async fn a_paused_turn_resumes_and_keeps_every_leg() {
        let (base, seen) = paused_server(2).await;
        let r = chat_against(&base).await;
        // Text from all three legs, in order — a resume that dropped the
        // earlier legs would still look like a valid answer.
        assert_eq!(r.text.as_deref(), Some("part0 part1 final."));
        assert_eq!(seen.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn usage_covers_the_whole_turn_not_just_the_last_leg() {
        let (base, _) = paused_server(2).await;
        let usage = chat_against(&base).await.usage.expect("usage");
        assert_eq!(usage.input_tokens, 300);
        assert_eq!(usage.output_tokens, 30);
    }

    #[tokio::test]
    async fn a_turn_that_never_unpauses_gives_up() {
        // Server pauses forever; the loop must stop rather than spin.
        let (base, seen) = paused_server(usize::MAX).await;
        let r = chat_against(&base).await;
        assert_eq!(
            seen.load(Ordering::SeqCst) as u32,
            MAX_PAUSE_RESUMES + 1,
            "should stop after the resume cap"
        );
        // Partial text still comes back rather than an error or nothing.
        assert!(r.text.unwrap_or_default().starts_with("part0 "));
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
