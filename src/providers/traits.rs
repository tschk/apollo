//! Core Provider trait — defines the interface for LLM backends.
//! Inspired by ZeroClaw's trait system with simplifications.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::tools::ToolSpec;

/// A single message in a conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    /// For tool_result messages: the tool_use_id this is responding to
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: "system".into(),
            content: content.into(),
            tool_use_id: None,
        }
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".into(),
            content: content.into(),
            tool_use_id: None,
        }
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: "assistant".into(),
            content: content.into(),
            tool_use_id: None,
        }
    }
    pub fn tool_result(id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: "tool_result".into(),
            content: content.into(),
            tool_use_id: Some(id.into()),
        }
    }
    pub fn is_tool_result(&self) -> bool {
        self.role == "tool_result"
    }
}

/// A tool call requested by the LLM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

/// LLM response — text, tool calls, or both.
#[derive(Debug, Clone, Default)]
pub struct ChatResponse {
    pub text: Option<String>,
    pub tool_calls: Vec<ToolCall>,
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, Default)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
}

impl ChatResponse {
    pub fn has_tool_calls(&self) -> bool {
        !self.tool_calls.is_empty()
    }
    pub fn text_or_empty(&self) -> &str {
        self.text.as_deref().unwrap_or("")
    }
}

/// Chat request payload.
#[derive(Debug, Clone, Copy)]
pub struct ChatRequest<'a> {
    pub messages: &'a [ChatMessage],
    pub tools: Option<&'a [ToolSpec]>,
    pub model: &'a str,
    pub temperature: f64,
    pub max_tokens: Option<u32>,
}

/// Provider capabilities
#[derive(Debug, Clone, Default)]
pub struct ProviderCapabilities {
    /// Supports native tool calling (not prompt-injection)
    pub native_tools: bool,
    /// Supports streaming responses
    pub streaming: bool,
    /// Supports vision/image input
    pub vision: bool,
    /// Maximum context window
    pub max_context: u32,
    /// Runs web search on the provider's own infrastructure, so apollo's
    /// `web_search` tool must be left out of the tool set to avoid a name clash.
    pub native_web_search: bool,
}

/// The core Provider trait.
/// Implement this for each LLM backend (Anthropic, OpenAI, Gemini, Ollama, etc.)
#[async_trait]
pub trait Provider: Send + Sync {
    /// Provider name (e.g., "anthropic", "openai", "ollama")
    fn name(&self) -> &str;

    /// Query capabilities
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }

    /// Send a chat request and get a response.
    async fn chat(&self, request: &ChatRequest<'_>) -> anyhow::Result<ChatResponse>;

    /// Simple one-shot message (convenience wrapper)
    async fn simple_chat(&self, message: &str, model: &str) -> anyhow::Result<String> {
        let messages = [ChatMessage::user(message)];
        let request = ChatRequest {
            messages: &messages,
            tools: None,
            model,
            temperature: 0.7,
            max_tokens: None,
        };
        let response = self.chat(&request).await?;
        Ok(response.text.unwrap_or_default())
    }
}
