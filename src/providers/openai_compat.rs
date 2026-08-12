//! OpenAI-compatible provider — works with OpenAI, OpenRouter, Groq, Together, etc.

use async_trait::async_trait;
use serde_json::Value;

use super::retry::send_with_retry;
use super::traits::*;
use crate::text::truncate_chars;
use crate::tools::ToolSpec;

pub struct OpenAiCompatProvider {
    api_key: String,
    base_url: String,
    provider_name: String,
}

impl OpenAiCompatProvider {
    pub fn new(
        api_key: impl Into<String>,
        base_url: impl Into<String>,
        name: impl Into<String>,
    ) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: base_url.into(),
            provider_name: name.into(),
        }
    }

    /// OpenAI
    pub fn openai(api_key: impl Into<String>) -> Self {
        Self::new(api_key, "https://api.openai.com/v1", "openai")
    }

    /// OpenRouter
    pub fn openrouter(api_key: impl Into<String>) -> Self {
        Self::new(api_key, "https://openrouter.ai/api/v1", "openrouter")
    }

    /// Groq
    pub fn groq(api_key: impl Into<String>) -> Self {
        Self::new(api_key, "https://api.groq.com/openai/v1", "groq")
    }

    /// Together AI
    pub fn together(api_key: impl Into<String>) -> Self {
        Self::new(api_key, "https://api.together.xyz/v1", "together")
    }

    /// Mistral AI
    pub fn mistral(api_key: impl Into<String>) -> Self {
        Self::new(api_key, "https://api.mistral.ai/v1", "mistral")
    }

    /// DeepSeek
    pub fn deepseek(api_key: impl Into<String>) -> Self {
        Self::new(api_key, "https://api.deepseek.com/v1", "deepseek")
    }

    /// Fireworks AI
    pub fn fireworks(api_key: impl Into<String>) -> Self {
        Self::new(
            api_key,
            "https://api.fireworks.ai/inference/v1",
            "fireworks",
        )
    }

    /// Perplexity AI
    pub fn perplexity(api_key: impl Into<String>) -> Self {
        Self::new(api_key, "https://api.perplexity.ai", "perplexity")
    }

    /// xAI (Grok)
    pub fn xai(api_key: impl Into<String>) -> Self {
        Self::new(api_key, "https://api.x.ai/v1", "xai")
    }

    /// Moonshot / Kimi
    pub fn moonshot(api_key: impl Into<String>) -> Self {
        Self::new(api_key, "https://api.moonshot.ai/v1", "moonshot")
    }

    /// Venice AI
    pub fn venice(api_key: impl Into<String>) -> Self {
        Self::new(api_key, "https://api.venice.ai/api/v1", "venice")
    }

    /// HuggingFace Inference
    pub fn huggingface(api_key: impl Into<String>) -> Self {
        Self::new(
            api_key,
            "https://api-inference.huggingface.co/v1",
            "huggingface",
        )
    }

    /// SiliconFlow
    pub fn siliconflow(api_key: impl Into<String>) -> Self {
        Self::new(api_key, "https://api.siliconflow.cn/v1", "siliconflow")
    }

    /// Cerebras
    pub fn cerebras(api_key: impl Into<String>) -> Self {
        Self::new(api_key, "https://api.cerebras.ai/v1", "cerebras")
    }

    /// MiniMax (Anthropic-compatible)
    pub fn minimax(api_key: impl Into<String>) -> Self {
        Self::new(api_key, "https://api.minimax.io/v1", "minimax")
    }

    /// Vercel AI Gateway
    pub fn vercel(api_key: impl Into<String>) -> Self {
        Self::new(api_key, "https://gateway.vercel.ai/v1", "vercel")
    }

    /// Cloudflare Workers AI
    pub fn cloudflare(api_key: impl Into<String>, account_id: &str) -> Self {
        Self::new(
            api_key,
            format!(
                "https://api.cloudflare.com/client/v4/accounts/{}/ai/v1",
                account_id
            ),
            "cloudflare",
        )
    }

    /// Fetch the provider's current `/models` snapshot.
    ///
    /// OpenRouter returns a superset of the OpenAI model object. The parser
    /// keeps its context limits, modalities, supported parameters, and
    /// pricing while still accepting sparse OpenAI-compatible responses.
    pub async fn list_remote_models(&self) -> anyhow::Result<Vec<ModelInfo>> {
        let response = send_with_retry(
            crate::http::shared()
                .get(format!("{}/models", self.base_url.trim_end_matches('/')))
                .header("Authorization", format!("Bearer {}", self.api_key))
                .header("Accept", "application/json"),
            self.name(),
        )
        .await?;
        let status = response.status();
        let body = response.text().await?;
        if !status.is_success() {
            anyhow::bail!(
                "{} models API error {}: {}",
                self.provider_name,
                status,
                truncate_chars(&body, 200)
            );
        }
        let value: Value = serde_json::from_str(&body)?;
        let data = value
            .get("data")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow::anyhow!("{} models response has no data array", self.name()))?;
        Ok(data
            .iter()
            .filter_map(|model| parse_model_info(model, &self.provider_name))
            .collect())
    }

    fn build_tools_payload(&self, tools: &[ToolSpec]) -> Vec<Value> {
        tools
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
            .collect()
    }
}

#[async_trait]
impl Provider for OpenAiCompatProvider {
    fn name(&self) -> &str {
        &self.provider_name
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            native_tools: true,
            streaming: true,
            vision: true,
            max_context: 128_000,
            native_web_search: false,
        }
    }

    async fn list_models(&self) -> anyhow::Result<Vec<ModelInfo>> {
        self.list_remote_models().await
    }

    async fn chat(&self, request: &ChatRequest<'_>) -> anyhow::Result<ChatResponse> {
        let client = crate::http::shared();

        let messages: Vec<Value> = request
            .messages
            .iter()
            .map(|m| serde_json::json!({ "role": &m.role, "content": &m.content }))
            .collect();

        let mut body = serde_json::json!({
            "model": request.model,
            "messages": messages,
            "temperature": request.temperature,
        });

        if let Some(max) = request.max_tokens {
            body["max_tokens"] = Value::Number(max.into());
        }

        if let Some(tools) = request.tools {
            if !tools.is_empty() {
                body["tools"] = Value::Array(self.build_tools_payload(tools));
            }
        }

        let resp = send_with_retry(
            client
                .post(format!("{}/chat/completions", self.base_url))
                .header("Authorization", format!("Bearer {}", self.api_key))
                .header("Content-Type", "application/json")
                .json(&body),
            self.name(),
        )
        .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!(
                "{} API error {}: {}",
                self.provider_name,
                status,
                truncate_chars(&text, 200)
            );
        }

        let data: Value = resp.json().await?;
        let choice = &data["choices"][0];

        let text = choice["message"]["content"].as_str().map(String::from);

        let tool_calls = choice["message"]["tool_calls"]
            .as_array()
            .map(|calls| {
                calls
                    .iter()
                    .map(|tc| ToolCall {
                        id: tc["id"].as_str().unwrap_or("").to_string(),
                        name: tc["function"]["name"].as_str().unwrap_or("").to_string(),
                        arguments: tc["function"]["arguments"]
                            .as_str()
                            .unwrap_or("{}")
                            .to_string(),
                    })
                    .collect()
            })
            .unwrap_or_default();

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

fn parse_model_info(value: &Value, provider: &str) -> Option<ModelInfo> {
    let id = value.get("id")?.as_str()?.to_string();
    let mut info = ModelInfo {
        id: id.clone(),
        provider: provider.to_string(),
        display_name: value
            .get("name")
            .or_else(|| value.get("display_name"))
            .and_then(Value::as_str)
            .unwrap_or(&id)
            .to_string(),
        description: value
            .get("description")
            .and_then(Value::as_str)
            .map(str::to_string),
        capabilities: ["text_input", "text_output", "streaming"]
            .into_iter()
            .map(str::to_string)
            .collect(),
        input_modalities: Vec::new(),
        output_modalities: Vec::new(),
        supported_parameters: std::collections::BTreeSet::new(),
        context_window: None,
        max_output_tokens: None,
        pricing: parse_pricing(value.get("pricing")),
    };

    if let Some(architecture) = value.get("architecture") {
        info.input_modalities = string_array(architecture.get("input_modalities"));
        info.output_modalities = string_array(architecture.get("output_modalities"));
        if !info.input_modalities.is_empty() {
            info.capabilities.remove("text_input");
            if info.input_modalities.iter().any(|item| item == "text") {
                info.capabilities.insert("text_input".into());
            }
            if info.input_modalities.iter().any(|item| item == "image") {
                info.capabilities.insert("image_input".into());
            }
            if info.input_modalities.iter().any(|item| item == "audio") {
                info.capabilities.insert("audio_input".into());
            }
        }
        if !info.output_modalities.is_empty() {
            info.capabilities.remove("text_output");
            if info.output_modalities.iter().any(|item| item == "text") {
                info.capabilities.insert("text_output".into());
            } else {
                info.capabilities.remove("streaming");
            }
            if info.output_modalities.iter().any(|item| item == "image") {
                info.capabilities.insert("image_output".into());
                info.capabilities.insert("image_generation".into());
            }
            if info.output_modalities.iter().any(|item| item == "audio") {
                info.capabilities.insert("audio_output".into());
            }
        }
    }

    if let Some(parameters) = value.get("supported_parameters").and_then(Value::as_array) {
        for parameter in parameters.iter().filter_map(Value::as_str) {
            info.supported_parameters.insert(parameter.to_string());
            match parameter {
                "tools" | "tool_choice" => {
                    info.capabilities.insert("tool_calling".into());
                }
                "structured_outputs" | "response_format" => {
                    info.capabilities.insert("structured_output".into());
                }
                "reasoning" | "include_reasoning" => {
                    info.capabilities.insert("extended_thinking".into());
                }
                _ => {}
            }
        }
    }

    let top_provider = value.get("top_provider");
    info.context_window = top_provider
        .and_then(|item| item.get("context_length"))
        .and_then(value_u64)
        .or_else(|| value.get("context_length").and_then(value_u64));
    info.max_output_tokens = top_provider
        .and_then(|item| item.get("max_completion_tokens"))
        .and_then(value_u64)
        .or_else(|| value.get("max_output_tokens").and_then(value_u64));
    Some(info)
}

fn string_array(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn value_u64(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str()?.parse::<u64>().ok())
}

fn number(value: Option<&Value>) -> Option<f64> {
    value.and_then(|item| item.as_f64().or_else(|| item.as_str()?.parse().ok()))
}

fn parse_pricing(value: Option<&Value>) -> Option<ModelPricing> {
    let object = value?.as_object()?;
    Some(ModelPricing {
        input_per_token: number(object.get("prompt")),
        output_per_token: number(object.get("completion")),
        request: number(object.get("request")),
        image_input: number(object.get("image")),
        reasoning: number(object.get("internal_reasoning")),
        cache_read: number(object.get("input_cache_read")),
        cache_write: number(object.get("input_cache_write")),
    })
}

#[cfg(test)]
mod tests {
    use super::parse_model_info;

    #[test]
    fn parses_openrouter_limits_and_capabilities() {
        let value = serde_json::json!({
            "id": "openai/gpt-4o-mini",
            "name": "GPT-4o Mini",
            "architecture": {"input_modalities": ["text", "image"], "output_modalities": ["text"]},
            "context_length": 128000,
            "top_provider": {"context_length": 128000, "max_completion_tokens": 16384},
            "pricing": {"prompt": "0.00000015", "completion": "0.0000006"},
            "supported_parameters": ["tools", "structured_outputs"]
        });
        let model = parse_model_info(&value, "openrouter").unwrap();
        assert_eq!(model.context_window, Some(128_000));
        assert_eq!(model.max_output_tokens, Some(16_384));
        assert!(model.capabilities.contains("image_input"));
        assert!(model.capabilities.contains("tool_calling"));
        assert_eq!(model.pricing.unwrap().input_per_token, Some(0.00000015));
    }
}
