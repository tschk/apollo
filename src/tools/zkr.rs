use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;

use super::{Tool, ToolResult, ToolSpec};
use crate::memory::zkr::{claim_kind, source_kind, ZkrStore};

pub struct ZkrTool {
    store: Arc<ZkrStore>,
}

impl ZkrTool {
    pub fn new(store: Arc<ZkrStore>) -> Self {
        Self { store }
    }
}

#[derive(Deserialize)]
struct ZkrArgs {
    action: String,
    text: Option<String>,
    source_kind: Option<String>,
    ingestion_key: Option<String>,
    subject: Option<String>,
    predicate: Option<String>,
    value: Option<String>,
    claim_kind: Option<String>,
    valid_from: Option<i64>,
    query: Option<String>,
    limit: Option<u32>,
    kind: Option<String>,
    id: Option<String>,
    claim_id: Option<String>,
    source_id: Option<String>,
    valid_at: Option<i64>,
}

#[async_trait]
impl Tool for ZkrTool {
    fn name(&self) -> &str {
        "zkr_memory"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name().to_string(),
            description: "Store, search, inspect, correct, and delete evidence-backed temporal memory with citations.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["remember", "search", "get", "correct", "delete", "profiles"] },
                    "text": { "type": "string" },
                    "source_kind": { "type": "string", "enum": ["conversation", "screen", "audio", "document", "integration", "user_correction"] },
                    "ingestion_key": { "type": "string" },
                    "subject": { "type": "string" },
                    "predicate": { "type": "string" },
                    "value": { "type": "string" },
                    "claim_kind": { "type": "string", "enum": ["fact", "profile_fact", "preference", "task", "skill", "recommendation"] },
                    "valid_from": { "type": "integer" },
                    "query": { "type": "string" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 100 },
                    "kind": { "type": "string", "enum": ["source", "evidence", "claim"] },
                    "id": { "type": "string" },
                    "claim_id": { "type": "string" },
                    "source_id": { "type": "string" },
                    "valid_at": { "type": "integer" }
                },
                "required": ["action"]
            }),
        }
    }

    async fn execute(&self, arguments: &str) -> anyhow::Result<ToolResult> {
        let args: ZkrArgs = serde_json::from_str(arguments)?;
        let result = match args.action.as_str() {
            "remember" => {
                let text = args
                    .text
                    .ok_or_else(|| anyhow::anyhow!("text is required"))?;
                let claim = match (args.subject, args.predicate, args.value, args.claim_kind) {
                    (Some(subject), Some(predicate), Some(value), Some(kind)) => {
                        Some(zkr::ClaimInput {
                            subject,
                            predicate,
                            value,
                            kind: claim_kind(&kind)?,
                            valid_from: args
                                .valid_from
                                .unwrap_or_else(|| chrono::Utc::now().timestamp()),
                        })
                    }
                    (None, None, None, None) => None,
                    _ => anyhow::bail!(
                        "subject, predicate, value, and claim_kind must be provided together"
                    ),
                };
                let remembered = self
                    .store
                    .remember(
                        text,
                        source_kind(args.source_kind.as_deref().unwrap_or("integration"))?,
                        args.ingestion_key,
                        claim,
                        chrono::Utc::now().timestamp(),
                    )
                    .await?;
                serde_json::to_string_pretty(&remembered)?
            }
            "search" => serde_json::to_string_pretty(
                &self
                    .store
                    .search(
                        args.query
                            .ok_or_else(|| anyhow::anyhow!("query is required"))?,
                        args.limit.unwrap_or(10),
                    )
                    .await?,
            )?,
            "get" => serde_json::to_string_pretty(
                &self
                    .store
                    .get(
                        args.kind
                            .as_deref()
                            .ok_or_else(|| anyhow::anyhow!("kind is required"))?,
                        args.id.ok_or_else(|| anyhow::anyhow!("id is required"))?,
                    )
                    .await?,
            )?,
            "correct" => serde_json::to_string_pretty(
                &self
                    .store
                    .correct(
                        args.claim_id
                            .ok_or_else(|| anyhow::anyhow!("claim_id is required"))?,
                        args.text
                            .ok_or_else(|| anyhow::anyhow!("text is required"))?,
                        args.value
                            .ok_or_else(|| anyhow::anyhow!("value is required"))?,
                        args.valid_at
                            .ok_or_else(|| anyhow::anyhow!("valid_at is required"))?,
                    )
                    .await?,
            )?,
            "delete" => serde_json::to_string_pretty(
                &self
                    .store
                    .delete(
                        args.source_id
                            .ok_or_else(|| anyhow::anyhow!("source_id is required"))?,
                    )
                    .await?,
            )?,
            "profiles" => {
                serde_json::to_string_pretty(&self.store.profiles(args.limit.unwrap_or(20)).await?)?
            }
            other => anyhow::bail!("unknown zkr action: {other}"),
        };
        Ok(ToolResult::success(result))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn remember_and_search_actions_return_citations() {
        let dir = tempfile::tempdir().unwrap();
        let store =
            Arc::new(ZkrStore::open(&dir.path().join("memory.db"), "test", "person").unwrap());
        let tool = ZkrTool::new(store);
        let remembered = tool
            .execute(r#"{"action":"remember","text":"prefers short answers","source_kind":"conversation"}"#)
            .await
            .unwrap();
        assert!(!remembered.is_error);
        let searched = tool
            .execute(r#"{"action":"search","query":"short answers"}"#)
            .await
            .unwrap();
        assert!(searched.output.contains("evidence_ids"));
    }
}
