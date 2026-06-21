//! Anthropic (Claude) provider implementation.
//! Supports both API keys and OAuth tokens (Claude.dev)

use async_trait::async_trait;
use serde_json::Value;

use super::retry::send_with_retry;
use super::traits::*;
use crate::cost::{CostTracker, TokenUsage};
use crate::text::truncate_chars;
use crate::tools::ToolSpec;

pub struct AnthropicProvider {
    api_key: String,
    base_url: String,
    cost_tracker: Option<std::sync::Arc<CostTracker>>,
}

impl AnthropicProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: "https://api.anthropic.com/v1".to_string(),
            cost_tracker: None,
        }
    }

    /// Create from OAuth token (Claude.dev) or fallback to environment/file
    pub fn from_env_or_oauth() -> anyhow::Result<Self> {
        // Try standard API key first
        if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
            return Ok(Self::new(key));
        }

        // Try loading from Claude.dev OAuth credentials
        if let Ok((token, _, _)) = super::oauth::load_oauth_token_from_file() {
            return Ok(Self::new(token));
        }

        Err(anyhow::anyhow!(
            "No ANTHROPIC_API_KEY found. Set env var or install Claude for Desktop with OAuth token."
        ))
    }

    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    pub fn with_cost_tracker(mut self, tracker: std::sync::Arc<CostTracker>) -> Self {
        self.cost_tracker = Some(tracker);
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

        if let Some(tools) = request.tools {
            if !tools.is_empty() {
                body["tools"] = Value::Array(self.build_tools_payload(tools));
            }
        }

        // Detect OAuth tokens (sk-ant-oat) vs API keys (sk-ant-api)
        let is_oauth = self.api_key.contains("sk-ant-oat");

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
                .header("Authorization", format!("Bearer {}", &self.api_key))
                .header(
                    "anthropic-beta",
                    "claude-code-20250219,oauth-2025-04-20,interleaved-thinking-2025-05-14",
                )
                .header("anthropic-dangerous-direct-browser-access", "true");
        } else {
            req_builder = req_builder
                .header("x-api-key", &self.api_key)
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
                    _ => {}
                }
            }
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
