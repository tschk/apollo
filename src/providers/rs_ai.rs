//! rs_ai-backed provider adapter for first-class ChatGPT, Gemini, xAI, Claude,
//! Cloudflare and generic OpenAI-compatible endpoints.

use async_trait::async_trait;
use rs_ai_core::{
    GenerateOptions, GenerateResult, Message, Prompt, ToolCallRequest, ToolDefinition,
};

use crate::providers::traits::{
    ChatRequest, ChatResponse, Provider, ProviderCapabilities, ToolCall, Usage,
};

/// Provider implementation backed by the `rs_ai` / `rs_ai_core` SDK.
pub struct RsAiProvider {
    provider_name: String,
    model_id: String,
    api_key: String,
    base_url: Option<String>,
    account_id: Option<String>,
}

impl RsAiProvider {
    pub fn new(
        provider_name: &str,
        model_id: &str,
        api_key: &str,
        base_url: Option<String>,
        account_id: Option<String>,
    ) -> Self {
        Self {
            provider_name: provider_name.to_string(),
            model_id: model_id.to_string(),
            api_key: api_key.to_string(),
            base_url,
            account_id,
        }
    }

    fn effective_model_id(&self) -> &str {
        if !self.model_id.is_empty() {
            return &self.model_id;
        }
        match self.provider_name.as_str() {
            "chatgpt" | "openai" => "gpt-4o",
            "gemini" => "gemini-2.5-flash",
            "xai" | "grok" => "grok-4.20-reasoning",
            "claude" => "claude-sonnet-4-6",
            "cloudflare" => "@cf/meta/llama-3.1-8b-instruct",
            _ => "",
        }
    }

    fn build_model(&self) -> anyhow::Result<Box<dyn rs_ai_core::LanguageModel>> {
        use rs_ai_providers::{
            ChatGptProvider, ClaudeProvider, CloudflareProvider, GeminiProvider,
            OpenAiCompatibleConfig, OpenAiCompatibleProvider, XaiProvider,
        };

        let model: Box<dyn rs_ai_core::LanguageModel> = match self.provider_name.as_str() {
            "claude" => {
                Box::new(ClaudeProvider::new(&self.api_key).model(self.effective_model_id()))
            }
            "chatgpt" | "openai" => {
                Box::new(ChatGptProvider::new(&self.api_key).model(self.effective_model_id()))
            }
            "gemini" => {
                Box::new(GeminiProvider::new(&self.api_key).model(self.effective_model_id()))
            }
            "xai" | "grok" => {
                Box::new(XaiProvider::new(&self.api_key).model(self.effective_model_id()))
            }
            "cloudflare" => {
                let account_id = self
                    .account_id
                    .as_deref()
                    .or(self.base_url.as_deref())
                    .unwrap_or("")
                    .to_string();
                Box::new(
                    CloudflareProvider::new(account_id, &self.api_key)
                        .model(self.effective_model_id()),
                )
            }
            other => {
                let base_url = self
                    .base_url
                    .as_deref()
                    .unwrap_or("https://api.openai.com/v1");
                let config = OpenAiCompatibleConfig::new(base_url, &self.api_key);
                let provider = OpenAiCompatibleProvider::new(config, other, other);
                provider.language_model(self.effective_model_id())
            }
        };
        Ok(model)
    }
}

#[async_trait]
impl Provider for RsAiProvider {
    fn name(&self) -> &str {
        &self.provider_name
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            native_tools: true,
            streaming: true,
            vision: matches!(
                self.provider_name.as_str(),
                "claude" | "chatgpt" | "openai" | "gemini" | "xai" | "grok"
            ),
            max_context: 200_000,
        }
    }

    async fn chat(&self, request: &ChatRequest<'_>) -> anyhow::Result<ChatResponse> {
        let model = self.build_model()?;

        let messages: Vec<Message> = request
            .messages
            .iter()
            .map(|m| match m.role.as_str() {
                "system" => Message::system(&m.content),
                "assistant" => Message::assistant(&m.content),
                "tool_result" => {
                    Message::tool_result(m.tool_use_id.as_deref().unwrap_or(""), &m.content)
                }
                _ => Message::user(&m.content),
            })
            .collect();

        let prompt = Prompt::Messages(messages);

        let mut options = GenerateOptions::default().with_temperature(request.temperature);
        if let Some(max_tokens) = request.max_tokens {
            options = options.with_max_tokens(max_tokens);
        }

        let tools: Vec<ToolDefinition> = request
            .tools
            .unwrap_or(&[])
            .iter()
            .map(|t| ToolDefinition {
                name: t.name.clone(),
                description: t.description.clone(),
                parameters: t.parameters.clone(),
                examples: None,
            })
            .collect();
        if !tools.is_empty() {
            options = options
                .with_tools(tools)
                .with_tool_choice(rs_ai_core::ToolChoice::Auto);
        }

        let result = model
            .generate(prompt, options)
            .await
            .map_err(|e| anyhow::anyhow!("rs_ai provider error: {e}"))?;

        Ok(map_generate_result(result)?)
    }
}

fn map_generate_result(result: GenerateResult) -> anyhow::Result<ChatResponse> {
    let tool_calls = result
        .tool_calls
        .iter()
        .map(|tc: &ToolCallRequest| -> anyhow::Result<ToolCall> {
            Ok(ToolCall {
                id: tc.id.clone(),
                name: tc.name.clone(),
                arguments: serde_json::to_string(&tc.arguments)?,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    let usage = Usage {
        input_tokens: result.usage.prompt_tokens.unwrap_or(0) as u32,
        output_tokens: result.usage.completion_tokens.unwrap_or(0) as u32,
    };

    Ok(ChatResponse {
        text: result.text,
        tool_calls,
        usage: Some(usage),
    })
}
