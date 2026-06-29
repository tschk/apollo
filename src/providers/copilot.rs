//! GitHub Copilot provider — token exchange + OpenAI-compatible API
//! Reverse-engineered from OpenClaw's github-copilot-token module

use async_trait::async_trait;
use serde_json::Value;

use super::traits::*;
use crate::text::truncate_chars;
use crate::tools::ToolSpec;

const COPILOT_TOKEN_URL: &str = "https://api.github.com/copilot_internal/v2/token";
const DEFAULT_COPILOT_API_BASE: &str = "https://api.individual.githubcopilot.com";

pub struct CopilotProvider {
    github_token: String,
    api_token: Option<String>,
    base_url: String,
}

impl CopilotProvider {
    pub fn new(github_token: impl Into<String>) -> Self {
        Self {
            github_token: github_token.into(),
            api_token: None,
            base_url: DEFAULT_COPILOT_API_BASE.to_string(),
        }
    }

    /// Load from OpenClaw's cached token file
    pub fn from_openclaw() -> anyhow::Result<Self> {
        let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("No home dir"))?;
        let token_path = home.join(".openclaw/credentials/github-copilot.token.json");

        if !token_path.exists() {
            return Err(anyhow::anyhow!("No Copilot token at {:?}", token_path));
        }

        let content = std::fs::read_to_string(&token_path)?;
        let data: Value = serde_json::from_str(&content)?;

        let token = data["token"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("No token field"))?;

        // Derive API base URL from token's proxy-ep field
        let base_url = derive_base_url(token);

        Ok(Self {
            github_token: String::new(),
            api_token: Some(token.to_string()),
            base_url,
        })
    }

    async fn fetch_new_token(&self) -> anyhow::Result<String> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()?;

        let resp = client
            .get(COPILOT_TOKEN_URL)
            .header("Accept", "application/json")
            .header("Authorization", format!("Bearer {}", &self.github_token))
            .send()
            .await?;

        if !resp.status().is_success() {
            anyhow::bail!("Copilot token exchange failed: {}", resp.status());
        }

        let data: Value = resp.json().await?;
        let token = data["token"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("No token in response"))?
            .to_string();

        Ok(token)
    }

    /// Exchange GitHub token for Copilot API token
    async fn ensure_token(&mut self) -> anyhow::Result<String> {
        if let Some(ref token) = self.api_token {
            return Ok(token.clone());
        }

        let token = self.fetch_new_token().await?;

        self.base_url = derive_base_url(&token);
        self.api_token = Some(token.clone());

        Ok(token)
    }
}

/// Derive API base URL from Copilot token's proxy-ep field
fn derive_base_url(token: &str) -> String {
    if let Some(caps) = token.split(';').find(|s| s.trim().starts_with("proxy-ep=")) {
        let proxy_ep = caps.trim().trim_start_matches("proxy-ep=").trim();
        if !proxy_ep.is_empty() {
            let host = proxy_ep
                .trim_start_matches("https://")
                .trim_start_matches("http://")
                .replace("proxy.", "api.");
            return format!("https://{}", host);
        }
    }
    DEFAULT_COPILOT_API_BASE.to_string()
}

#[async_trait]
impl Provider for CopilotProvider {
    fn name(&self) -> &str {
        "github-copilot"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            native_tools: true,
            streaming: true,
            vision: true,
            max_context: 128_000,
        }
    }

    async fn chat(&self, request: &ChatRequest<'_>) -> anyhow::Result<ChatResponse> {
        let token = self
            .api_token
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("No Copilot token available"))?;

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()?;

        let messages: Vec<Value> = request
            .messages
            .iter()
            .map(|m| serde_json::json!({ "role": &m.role, "content": &m.content }))
            .collect();

        let mut body = serde_json::json!({
            "model": request.model,
            "messages": messages,
            "max_tokens": request.max_tokens.unwrap_or(4096),
            "temperature": request.temperature,
        });

        if let Some(tools) = request.tools {
            if !tools.is_empty() {
                let tools_payload: Vec<Value> = tools
                    .iter()
                    .map(|t| {
                        serde_json::json!({
                            "type": "function",
                            "function": {
                                "name": t.name,
                                "description": t.description,
                                "parameters": t.parameters,
                            }
                        })
                    })
                    .collect();
                body["tools"] = Value::Array(tools_payload);
            }
        }

        let resp = client
            .post(format!("{}/chat/completions", self.base_url))
            .header("Authorization", format!("Bearer {}", token))
            .header("Content-Type", "application/json")
            .header("Copilot-Integration-Id", "aclaw")
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!(
                "Copilot API error {}: {}",
                status,
                truncate_chars(&text, 300)
            );
        }

        let data: Value = resp.json().await?;

        let choice = &data["choices"][0];
        let message = &choice["message"];

        let text = message["content"].as_str().map(|s| s.to_string());

        let mut tool_calls = Vec::new();
        if let Some(calls) = message["tool_calls"].as_array() {
            for call in calls {
                tool_calls.push(ToolCall {
                    id: call["id"].as_str().unwrap_or("").to_string(),
                    name: call["function"]["name"].as_str().unwrap_or("").to_string(),
                    arguments: call["function"]["arguments"]
                        .as_str()
                        .unwrap_or("{}")
                        .to_string(),
                });
            }
        }

        let usage = data["usage"].as_object().map(|u| Usage {
            input_tokens: u.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
            output_tokens: u
                .get("completion_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32,
        });

        Ok(ChatResponse {
            text,
            tool_calls,
            usage,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_derive_base_url() {
        let token = "tid=abc;proxy-ep=proxy.individual.githubcopilot.com;exp=123";
        assert_eq!(
            derive_base_url(token),
            "https://api.individual.githubcopilot.com"
        );
    }

    #[test]
    fn test_derive_base_url_default() {
        assert_eq!(derive_base_url("no-proxy-ep"), DEFAULT_COPILOT_API_BASE);
    }
}
