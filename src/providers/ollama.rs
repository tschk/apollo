//! Ollama provider — local model support via Ollama API.

use async_trait::async_trait;
use serde_json::Value;

use super::retry::send_with_retry;
use super::traits::*;
use crate::text::truncate_chars;

pub struct OllamaProvider {
    base_url: String,
}

impl OllamaProvider {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
        }
    }

    fn build_request_body(&self, request: &ChatRequest<'_>) -> Value {
        let messages: Vec<Value> = request
            .messages
            .iter()
            .map(|m| serde_json::json!({ "role": &m.role, "content": &m.content }))
            .collect();

        serde_json::json!({
            "model": request.model,
            "messages": messages,
            "stream": false,
            "options": {
                "temperature": request.temperature,
            }
        })
    }

    fn parse_response(&self, data: Value) -> ChatResponse {
        let text = data["message"]["content"].as_str().map(String::from);

        ChatResponse {
            text,
            tool_calls: vec![],
            usage: None,
        }
    }
}

impl Default for OllamaProvider {
    fn default() -> Self {
        Self::new("http://localhost:11434")
    }
}

#[async_trait]
impl Provider for OllamaProvider {
    fn name(&self) -> &str {
        "ollama"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            native_tools: false, // Most Ollama models don't support native tools
            streaming: true,
            vision: false,
            max_context: 32_000,
            native_web_search: false,
        }
    }

    async fn chat(&self, request: &ChatRequest<'_>) -> anyhow::Result<ChatResponse> {
        let client = crate::http::shared();
        let body = self.build_request_body(request);

        let resp = send_with_retry(
            client
                .post(format!("{}/api/chat", self.base_url))
                .json(&body),
            self.name(),
        )
        .await?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("Ollama error: {}", truncate_chars(&text, 200));
        }

        let data: Value = resp.json().await?;
        Ok(self.parse_response(data))
    }
}
