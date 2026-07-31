use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use crate::memory::embeddings::EmbeddingProvider;
use crate::memory::MemoryBackend;

use super::{Tool, ToolResult, ToolSpec};

pub struct EmbeddingStatusTool {
    provider: Arc<dyn EmbeddingProvider>,
}

impl EmbeddingStatusTool {
    pub fn new(provider: Arc<dyn EmbeddingProvider>) -> Self {
        Self { provider }
    }
}

#[async_trait]
impl Tool for EmbeddingStatusTool {
    fn name(&self) -> &str {
        "embedding_status"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "embedding_status".to_string(),
            description: "Show the configured embedding provider and vector dimensions."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {}
            }),
        }
    }

    async fn execute(&self, _arguments: &str) -> anyhow::Result<ToolResult> {
        Ok(ToolResult::success(format!(
            "Embedding provider: {}\nDimensions: {}",
            self.provider.name(),
            self.provider.dimensions()
        )))
    }
}

pub struct EmbeddingStoreTool {
    provider: Arc<dyn EmbeddingProvider>,
    memory: Arc<dyn MemoryBackend>,
}

impl EmbeddingStoreTool {
    pub fn new(provider: Arc<dyn EmbeddingProvider>, memory: Arc<dyn MemoryBackend>) -> Self {
        Self { provider, memory }
    }
}

#[derive(Deserialize)]
struct EmbeddingStoreArgs {
    namespace: String,
    key: String,
    text: String,
}

#[async_trait]
impl Tool for EmbeddingStoreTool {
    fn name(&self) -> &str {
        "embedding_store"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "embedding_store".to_string(),
            description:
                "Generate an embedding for text and store it in the active memory backend."
                    .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "namespace": { "type": "string" },
                    "key": { "type": "string" },
                    "text": { "type": "string" }
                },
                "required": ["namespace", "key", "text"]
            }),
        }
    }

    async fn execute(&self, arguments: &str) -> anyhow::Result<ToolResult> {
        let args: EmbeddingStoreArgs = serde_json::from_str(arguments)?;
        let vector = self.provider.embed_one(&args.text).await?;
        self.memory
            .store_embedding(
                &args.namespace,
                &args.key,
                &vector,
                &args.text,
                self.provider.name(),
            )
            .await?;
        Ok(ToolResult::success(format!(
            "Stored embedding for {}/{} with {} dimensions",
            args.namespace,
            args.key,
            vector.len()
        )))
    }
}

pub struct EmbeddingSearchTool {
    provider: Arc<dyn EmbeddingProvider>,
    memory: Arc<dyn MemoryBackend>,
}

impl EmbeddingSearchTool {
    pub fn new(provider: Arc<dyn EmbeddingProvider>, memory: Arc<dyn MemoryBackend>) -> Self {
        Self { provider, memory }
    }
}

#[derive(Deserialize)]
struct EmbeddingSearchArgs {
    namespace: String,
    query: String,
    limit: Option<usize>,
}

#[async_trait]
impl Tool for EmbeddingSearchTool {
    fn name(&self) -> &str {
        "embedding_search"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "embedding_search".to_string(),
            description: "Embed a query and run semantic search over stored vectors.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "namespace": { "type": "string" },
                    "query": { "type": "string" },
                    "limit": { "type": "integer" }
                },
                "required": ["namespace", "query"]
            }),
        }
    }

    async fn execute(&self, arguments: &str) -> anyhow::Result<ToolResult> {
        let args: EmbeddingSearchArgs = serde_json::from_str(arguments)?;
        let limit = args.limit.unwrap_or(5);
        let query_vector = self.provider.embed_one(&args.query).await?;
        let results = self
            .memory
            .search_embeddings(&args.namespace, &query_vector, limit)
            .await?;

        if results.is_empty() {
            return Ok(ToolResult::success("No embedding matches found."));
        }

        let output = results
            .iter()
            .map(|entry| format!("{}:{} — {}", entry.namespace, entry.key, entry.text))
            .collect::<Vec<_>>()
            .join("\n");
        Ok(ToolResult::success(output))
    }
}
