//! rs_ai-backed provider adapter for first-class ChatGPT, Claude, Gemini,
//! xAI, Cloudflare and generic OpenAI-compatible endpoints.

use async_trait::async_trait;
use rs_ai_core::{
    GenerateOptions, GenerateResult, Message, Prompt, ToolCallRequest, ToolDefinition,
};
use rs_ai_oauth::{fetch_models_async, ModelInfo as OAuthModelInfo, OAuthProvider};

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
            "anthropic" | "claude" => "claude-sonnet-4-6",
            "gemini" => "gemini-2.5-flash",
            "xai" | "grok" => "grok-4.20-reasoning",
            "cloudflare" => "@cf/meta/llama-3.1-8b-instruct",
            _ => "",
        }
    }

    /// Build the language model for a request.
    ///
    /// The key is a parameter rather than `self.api_key` so an OAuth access
    /// token refreshed mid-flight in `chat` can be used without rebuilding
    /// the provider.
    fn build_model(&self, api_key: &str) -> anyhow::Result<Box<dyn rs_ai_core::LanguageModel>> {
        use rs_ai_providers::{
            ChatGptProvider, ClaudeProvider, CloudflareProvider, GeminiProvider,
            OpenAiCompatibleConfig, OpenAiCompatibleProvider, XaiProvider,
        };

        let model: Box<dyn rs_ai_core::LanguageModel> = match self.provider_name.as_str() {
            "chatgpt" | "openai" => {
                Box::new(ChatGptProvider::new(api_key).model(self.effective_model_id()))
            }
            "anthropic" | "claude" => {
                let provider = ClaudeProvider::new(api_key);
                let provider = match &self.base_url {
                    Some(url) => provider.with_base_url(url.clone()),
                    None => provider,
                };
                Box::new(provider.model(self.effective_model_id()))
            }
            "gemini" => Box::new(GeminiProvider::new(api_key).model(self.effective_model_id())),
            "xai" | "grok" => Box::new(XaiProvider::new(api_key).model(self.effective_model_id())),
            "cloudflare" => {
                let account_id = self
                    .account_id
                    .as_deref()
                    .or(self.base_url.as_deref())
                    .unwrap_or("")
                    .to_string();
                Box::new(
                    CloudflareProvider::new(account_id, api_key).model(self.effective_model_id()),
                )
            }
            other => {
                let base_url = self
                    .base_url
                    .as_deref()
                    .unwrap_or("https://api.openai.com/v1");
                let config = OpenAiCompatibleConfig::new(base_url, api_key);
                let provider = OpenAiCompatibleProvider::new(config, other, other);
                provider.language_model(self.effective_model_id())
            }
        };
        Ok(model)
    }

    /// Resolve the key for a Claude request, refreshing subscription OAuth
    /// tokens when they have expired.
    ///
    /// Only applies when the configured key is itself a Claude OAuth token
    /// (`sk-ant-oat…`) — an API key never expires and is used as-is. The
    /// shared store holds the canonical token pair: when it is expired but
    /// refreshable it is refreshed, saved back, and the fresh access token is
    /// used for this request. Nothing here ever logs a token.
    async fn claude_oauth_key(&self) -> String {
        if !matches!(self.provider_name.as_str(), "anthropic" | "claude")
            || !rs_ai_oauth::claude_code::is_anthropic_oauth_token(&self.api_key)
        {
            return self.api_key.clone();
        }
        let Some(tokens) = rs_ai_oauth::credentials::load(&OAuthProvider::Claude) else {
            return self.api_key.clone();
        };
        if !rs_ai_oauth::credentials::is_expired(&tokens) {
            return tokens.access_token;
        }
        if tokens.refresh_token.is_none() {
            return self.api_key.clone();
        }
        match rs_ai_oauth::refresh_oauth_token(OAuthProvider::Claude, &tokens).await {
            Ok(new) => {
                if let Err(error) = rs_ai_oauth::credentials::save(&OAuthProvider::Claude, &new) {
                    tracing::warn!("failed to persist refreshed claude token: {error}");
                }
                new.access_token
            }
            // Not worth failing the request over: fall through on the key we
            // have and let the API be the judge.
            Err(error) => {
                tracing::warn!("claude token refresh failed: {error}");
                self.api_key.clone()
            }
        }
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
                "chatgpt" | "openai" | "anthropic" | "claude" | "gemini" | "xai" | "grok"
            ),
            max_context: 200_000,
            native_web_search: false,
        }
    }

    async fn list_models(&self) -> anyhow::Result<Vec<crate::providers::ModelInfo>> {
        let Some(provider) = OAuthProvider::parse(&self.provider_name) else {
            return Ok(Vec::new());
        };
        let models = fetch_models_async(provider, &self.api_key)
            .await
            .map_err(|error| anyhow::anyhow!("model discovery failed: {error}"))?;
        Ok(models.into_iter().map(map_model_info).collect())
    }

    async fn chat(&self, request: &ChatRequest<'_>) -> anyhow::Result<ChatResponse> {
        let api_key = self.claude_oauth_key().await;
        let model = self.build_model(&api_key)?;

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

fn map_model_info(model: OAuthModelInfo) -> crate::providers::ModelInfo {
    crate::providers::ModelInfo {
        id: model.id,
        provider: model.provider,
        display_name: model.display_name,
        description: model.description,
        capabilities: model.capabilities,
        input_modalities: model.input_modalities,
        output_modalities: model.output_modalities,
        supported_parameters: model.supported_parameters,
        context_window: model.limits.context_window,
        max_output_tokens: model.limits.max_output_tokens,
        pricing: model.pricing.map(|pricing| crate::providers::ModelPricing {
            input_per_token: pricing.input_per_token,
            output_per_token: pricing.output_per_token,
            request: pricing.request,
            image_input: pricing.image_input,
            reasoning: pricing.reasoning,
            cache_read: pricing.cache_read,
            cache_write: pricing.cache_write,
        }),
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
