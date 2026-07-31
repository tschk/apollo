use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[async_trait]
pub trait EmbeddingProvider: Send + Sync {
    fn name(&self) -> &str;
    fn dimensions(&self) -> usize;
    async fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>>;

    async fn embed_one(&self, text: &str) -> Result<Vec<f32>> {
        let mut results = self.embed(&[text]).await?;
        results.pop().context("No embedding returned")
    }
}

// Noop provider for keyword-only fallback
pub struct NoopEmbedding;

#[async_trait]
impl EmbeddingProvider for NoopEmbedding {
    fn name(&self) -> &str {
        "noop"
    }

    fn dimensions(&self) -> usize {
        0
    }

    async fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        Ok(vec![vec![]; texts.len()])
    }
}

// OpenAI provider
pub struct OpenAiEmbedding {
    client: reqwest::Client,
    api_key: Option<String>,
    base_url: String,
    model: String,
    dimensions: usize,
}

pub struct GeminiEmbedding {
    client: reqwest::Client,
    api_key: String,
    model: String,
    dimensions: usize,
}

impl GeminiEmbedding {
    pub fn new(api_key: String, model: Option<String>) -> Self {
        let model = model.unwrap_or_else(|| "text-embedding-004".to_string());
        Self {
            client: crate::http::shared(),
            api_key,
            model,
            dimensions: 768,
        }
    }
}

impl OpenAiEmbedding {
    pub fn new(api_key: Option<String>, model: Option<String>, base_url: Option<String>) -> Self {
        let model = model.unwrap_or_else(|| "text-embedding-3-small".to_string());
        let dimensions = if model.contains("text-embedding-3-small") {
            1536
        } else if model.contains("text-embedding-3-large") {
            3072
        } else {
            1536 // default
        };

        Self {
            client: crate::http::shared(),
            api_key,
            base_url: base_url.unwrap_or_else(|| "https://api.openai.com".to_string()),
            model,
            dimensions,
        }
    }
}

#[derive(Serialize)]
struct OpenAiEmbeddingRequest {
    input: Vec<String>,
    model: String,
}

#[derive(Deserialize)]
struct OpenAiEmbeddingResponse {
    data: Vec<OpenAiEmbeddingData>,
}

#[derive(Deserialize)]
struct OpenAiEmbeddingData {
    embedding: Vec<f32>,
}

#[async_trait]
impl EmbeddingProvider for OpenAiEmbedding {
    fn name(&self) -> &str {
        &self.model
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }

    async fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        let url = format!("{}/v1/embeddings", self.base_url);

        let request = OpenAiEmbeddingRequest {
            input: texts.iter().map(|s| s.to_string()).collect(),
            model: self.model.clone(),
        };

        let mut request_builder = self
            .client
            .post(&url)
            .header("Content-Type", "application/json");
        if let Some(api_key) = &self.api_key {
            request_builder =
                request_builder.header("Authorization", format!("Bearer {}", api_key));
        }
        let response = request_builder
            .json(&request)
            .send()
            .await
            .context("Failed to send embedding request")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("OpenAI API error {}: {}", status, body);
        }

        let response: OpenAiEmbeddingResponse = response
            .json()
            .await
            .context("Failed to parse embedding response")?;

        Ok(response.data.into_iter().map(|d| d.embedding).collect())
    }
}

#[derive(Serialize)]
struct GeminiPart {
    text: String,
}

#[derive(Serialize)]
struct GeminiContent {
    parts: Vec<GeminiPart>,
}

#[derive(Serialize)]
struct GeminiEmbeddingRequest {
    model: String,
    content: GeminiContent,
}

#[derive(Deserialize)]
struct GeminiEmbeddingResponse {
    embedding: Option<GeminiEmbeddingValues>,
}

#[derive(Deserialize)]
struct GeminiEmbeddingValues {
    values: Vec<f32>,
}

#[async_trait]
impl EmbeddingProvider for GeminiEmbedding {
    fn name(&self) -> &str {
        &self.model
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }

    async fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        let mut results = Vec::with_capacity(texts.len());
        for text in texts {
            let url = format!(
                "https://generativelanguage.googleapis.com/v1beta/models/{}:embedContent",
                self.model
            );
            let request = GeminiEmbeddingRequest {
                model: format!("models/{}", self.model),
                content: GeminiContent {
                    parts: vec![GeminiPart {
                        text: (*text).to_string(),
                    }],
                },
            };

            let response = self
                .client
                .post(&url)
                .header("x-goog-api-key", &self.api_key)
                .json(&request)
                .send()
                .await
                .context("Failed to send Gemini embedding request")?;

            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                anyhow::bail!("Gemini API error {}: {}", status, body);
            }

            let response: GeminiEmbeddingResponse = response
                .json()
                .await
                .context("Failed to parse Gemini embedding response")?;

            let values = response
                .embedding
                .map(|embedding| embedding.values)
                .context("Gemini response did not include embedding values")?;
            results.push(values);
        }
        Ok(results)
    }
}

// Factory function
pub fn create_embedding_provider(
    provider_type: &str,
    api_key: Option<String>,
    model: Option<String>,
    base_url: Option<String>,
) -> Result<Arc<dyn EmbeddingProvider>> {
    match provider_type.to_lowercase().as_str() {
        "noop" | "none" | "keyword" => Ok(Arc::new(NoopEmbedding)),
        "openai" => {
            let api_key = api_key.context("OpenAI API key required")?;
            Ok(Arc::new(OpenAiEmbedding::new(
                Some(api_key),
                model,
                base_url,
            )))
        }
        "openai_compat" => Ok(Arc::new(OpenAiEmbedding::new(api_key, model, base_url))),
        "ollama" | "local" => {
            let model = model.or_else(|| Some("nomic-embed-text".to_string()));
            let base_url = base_url.or_else(|| Some("http://localhost:11434".to_string()));
            Ok(Arc::new(OpenAiEmbedding::new(None, model, base_url)))
        }
        "gemini" => {
            let api_key = api_key.context("Gemini API key required")?;
            Ok(Arc::new(GeminiEmbedding::new(api_key, model)))
        }
        _ => anyhow::bail!("Unknown embedding provider: {}", provider_type),
    }
}
