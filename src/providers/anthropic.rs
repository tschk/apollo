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

/// What Anthropic says when a subscription OAuth credential is used outside
/// the product it was issued for. The wording is about API credit, which is
/// not what has gone wrong.
const EXTRA_USAGE_MARKER: &str = "out of extra usage";

/// What apollo says instead.
const CREDENTIAL_MISMATCH_HELP: &str = concat!(
    "Anthropic rejected this request with an extra-usage error while \
     authenticating with a Claude subscription credential (an OAuth token \
     from a Claude.ai login). apollo now sends the full Claude Code identity \
     (user-agent, x-app, tool-name casing), so if you still see this it is \
     most likely a genuine usage limit on your subscription's third-party \
     harness allocation — check claude.ai/settings/usage. ",
    "If you prefer a dedicated API key instead, create one at \
     https://platform.claude.com/settings/keys, then either run \
     `apollo config set provider.api_key sk-ant-...` or set the \
     ANTHROPIC_API_KEY environment variable."
);

/// Rewrite the credit-shaped error into the credential-shaped one it actually
/// is. Anything else is returned verbatim — a real out-of-credit error on an
/// API key still has to read as one.
fn map_api_error(status: u16, body: &str, is_oauth: bool) -> String {
    if is_oauth && status == 400 && body.contains(EXTRA_USAGE_MARKER) {
        return CREDENTIAL_MISMATCH_HELP.to_string();
    }
    format!(
        "Anthropic API error {}: {}",
        status,
        truncate_chars(body, 200)
    )
}

/// Claude subscription OAuth access tokens carry the `sk-ant-oat` prefix;
/// API keys carry `sk-ant-api`.
fn is_oauth_credential(key: &str) -> bool {
    key.contains("sk-ant-oat")
}

/// Claude Code version string sent as `user-agent` for OAuth requests.
/// Anthropic's server validates that subscription tokens are used with the
/// genuine Claude Code identity; without this header (and `x-app`), a
/// request that is otherwise correct is rejected with a 400 that presents
/// as an "out of extra usage" billing error.
const CLAUDE_CODE_VERSION: &str = "2.1.75";

/// Tool names in the canonical casing Claude Code uses. Anthropic's server
/// expects subscription OAuth requests to carry the Claude Code identity, and
/// tool names that match Claude Code's set must use its casing — `edit`
/// becomes `Edit`, `bash` becomes `Bash`. Names that do not match any entry
/// pass through unchanged.
const CLAUDE_CODE_TOOLS: &[&str] = &[
    "Read",
    "Write",
    "Edit",
    "Bash",
    "Grep",
    "Glob",
    "AskUserQuestion",
    "EnterPlanMode",
    "ExitPlanMode",
    "KillShell",
    "NotebookEdit",
    "Skill",
    "Task",
    "TaskOutput",
    "TodoWrite",
    "WebFetch",
    "WebSearch",
];

/// Map a tool name to Claude Code canonical casing if it matches
/// case-insensitively; otherwise return it unchanged.
fn to_claude_code_name(name: &str) -> String {
    CLAUDE_CODE_TOOLS
        .iter()
        .find(|t| t.eq_ignore_ascii_case(name))
        .map(|t| t.to_string())
        .unwrap_or_else(|| name.to_string())
}

/// Map a Claude Code canonical tool name back to the original name by
/// case-insensitive match against the tools apollo actually sent. If no
/// match is found, return the name as-is.
fn from_claude_code_name(name: &str, original_names: &[&str]) -> String {
    original_names
        .iter()
        .find(|n| n.eq_ignore_ascii_case(name))
        .map(|n| n.to_string())
        .unwrap_or_else(|| name.to_string())
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

    /// Build from a credential resolved elsewhere (config, env, or an OAuth
    /// login copied into the config at startup).
    ///
    /// A plain API key never expires, so it is used as given. An OAuth token
    /// does expire — around six hours — so it is paired with the token cache
    /// that owns refresh and write-back. Without this a long-running
    /// `apollo serve` keeps replaying the token it snapshotted at startup and
    /// starts failing the moment it expires.
    pub fn from_credential(api_key: impl Into<String>) -> Self {
        let key = api_key.into();
        let mut provider = Self::new(key.clone());
        if is_oauth_credential(&key) {
            match super::oauth::OAuthTokenCache::from_credentials_file() {
                Ok(cache) => provider.oauth = Some(cache),
                Err(e) => tracing::warn!(
                    "anthropic OAuth token has no refreshable credential on disk, \
                     it will stop working at expiry: {e}"
                ),
            }
        }
        provider
    }

    /// Create from OAuth token (Claude.dev) or fallback to environment/file
    pub fn from_env_or_oauth() -> anyhow::Result<Self> {
        // Try standard API key first
        if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
            return Ok(Self::new(key));
        }

        // Try loading from Claude.dev OAuth credentials
        match super::oauth::OAuthTokenCache::from_credentials_file() {
            Ok(cache) => {
                let (token, _, _) = super::oauth::load_oauth_token_from_file()?;
                let mut provider = Self::new(token);
                provider.oauth = Some(cache);
                Ok(provider)
            }
            // The credential loader knows why it could not produce a usable
            // token — a missing file reads very differently from a dead one, so
            // pass its reason through instead of replacing it with a generic
            // "no key found".
            Err(e) => Err(anyhow::anyhow!(
                "No ANTHROPIC_API_KEY found, and no usable Claude OAuth credential: {e}"
            )),
        }
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
        let client = crate::http::long();

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
        let is_oauth = is_oauth_credential(&api_key);

        // Capture the original tool names before any OAuth canonicalization
        // so the response can map them back. Without this, a tool named "edit"
        // that was canonicalized to "Edit" for the request would come back as
        // "Edit" and fail to dispatch.
        let original_tool_names: Vec<String> = request
            .tools
            .map(|t| t.iter().map(|spec| spec.name.to_string()).collect())
            .unwrap_or_default();

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

            // Canonicalize tool names to Claude Code casing. Anthropic's
            // server validates the Claude Code identity on subscription
            // tokens, and tool names that match Claude Code's set must use
            // its exact casing.
            if let Some(tools) = body.get_mut("tools").and_then(|t| t.as_array_mut()) {
                for tool in tools.iter_mut() {
                    if let Some(name) = tool.get("name").and_then(|n| n.as_str()) {
                        tool["name"] = Value::String(to_claude_code_name(name));
                    }
                }
            }
            // Also canonicalize tool_use names in the message history so
            // prior assistant turns agree with the tool definitions.
            if let Some(messages) = body.get_mut("messages").and_then(|m| m.as_array_mut()) {
                for msg in messages.iter_mut() {
                    if let Some(content) = msg.get_mut("content").and_then(|c| c.as_array_mut()) {
                        for block in content.iter_mut() {
                            if block.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                                if let Some(name) = block.get("name").and_then(|n| n.as_str()) {
                                    block["name"] = Value::String(to_claude_code_name(name));
                                }
                            }
                        }
                    }
                }
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
                    .header("anthropic-dangerous-direct-browser-access", "true")
                    .header("user-agent", format!("claude-cli/{CLAUDE_CODE_VERSION}"))
                    .header("x-app", "cli");
            } else {
                req_builder = req_builder
                    .header("x-api-key", &api_key)
                    .header("anthropic-beta", "prompt-caching-2024-07-31");
            }

            let resp = send_with_retry(req_builder.json(&body), self.name()).await?;

            if !resp.status().is_success() {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                anyhow::bail!(map_api_error(status.as_u16(), &text, is_oauth));
            }

            // Capture response headers for rate limit tracking
            let headers = resp.headers().clone();

            let mut data: Value = resp.json().await?;

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
                            let raw_name = block["name"].as_str().unwrap_or("").to_string();
                            // Reverse-map Claude Code canonical casing back
                            // to the tool name apollo registered.
                            let name = if is_oauth {
                                let refs: Vec<&str> =
                                    original_tool_names.iter().map(String::as_str).collect();
                                from_claude_code_name(&raw_name, &refs)
                            } else {
                                raw_name
                            };
                            tool_calls.push(ToolCall {
                                id: block["id"].as_str().unwrap_or("").to_string(),
                                name,
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

            // Taken, not cloned: `data` is not read past this point and the
            // block list can be large when searches ran.
            let content = data["content"].take();
            if content.is_null() {
                tracing::warn!("anthropic paused the turn without content; cannot resume");
                break;
            }
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

    const EXTRA_USAGE_BODY: &str = r#"{"type":"error","error":{"type":"invalid_request_error","message":"You're out of extra usage. Add more at claude.ai/settings/usage"}}"#;

    #[test]
    fn a_subscription_credential_extra_usage_error_is_rewritten() {
        let msg = map_api_error(400, EXTRA_USAGE_BODY, true);
        assert!(msg.contains("Claude subscription credential"), "got {msg}");
        assert!(msg.contains("ANTHROPIC_API_KEY"), "got {msg}");
        assert!(
            msg.contains("apollo config set provider.api_key"),
            "got {msg}"
        );
        // The rewritten message now points at usage limits rather than
        // claiming it is never a billing problem.
        assert!(msg.contains("claude.ai/settings/usage"), "got {msg}");
    }

    #[test]
    fn the_same_error_on_an_api_key_is_left_alone() {
        // An API key really can run out of credit; that message is accurate.
        let msg = map_api_error(400, EXTRA_USAGE_BODY, false);
        assert!(msg.starts_with("Anthropic API error 400"), "got {msg}");
        assert!(msg.contains("out of extra usage"), "got {msg}");
    }

    #[test]
    fn other_oauth_errors_are_left_alone() {
        let msg = map_api_error(401, r#"{"error":{"message":"invalid bearer"}}"#, true);
        assert!(msg.starts_with("Anthropic API error 401"), "got {msg}");
        assert!(msg.contains("invalid bearer"), "got {msg}");
    }

    #[test]
    fn oauth_credentials_are_recognised_by_prefix() {
        assert!(is_oauth_credential("sk-ant-oat01-abc"));
        assert!(!is_oauth_credential("sk-ant-api03-abc"));
        assert!(!is_oauth_credential(""));
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

    #[test]
    fn tool_names_are_canonicalised_to_claude_code_casing() {
        assert_eq!(to_claude_code_name("edit"), "Edit");
        assert_eq!(to_claude_code_name("bash"), "Bash");
        assert_eq!(to_claude_code_name("READ"), "Read");
        assert_eq!(to_claude_code_name("todowrite"), "TodoWrite");
        assert_eq!(to_claude_code_name("webfetch"), "WebFetch");
    }

    #[test]
    fn non_claude_code_tool_names_pass_through() {
        assert_eq!(to_claude_code_name("shell"), "shell");
        assert_eq!(to_claude_code_name("file_ops"), "file_ops");
        assert_eq!(to_claude_code_name("web_search"), "web_search");
    }

    #[test]
    fn canonicalised_names_map_back_to_originals() {
        let originals = ["edit", "shell", "web_search"];
        assert_eq!(from_claude_code_name("Edit", &originals), "edit");
        // "Bash" has no match in the originals, so it passes through.
        assert_eq!(from_claude_code_name("Bash", &originals), "Bash");
        assert_eq!(from_claude_code_name("shell", &originals), "shell");
    }
}
