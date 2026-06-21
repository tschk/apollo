//! In-process rs_gbrain tools (replaces Bun sidecar when enabled).

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;
use tokio::sync::Mutex;

use crate::tools::{Tool, ToolResult, ToolSpec};
use rs_gbrain::{gather_context, BrainEngine};

fn shared_engine() -> Arc<Mutex<BrainEngine>> {
    static ENGINE: std::sync::OnceLock<Arc<Mutex<BrainEngine>>> = std::sync::OnceLock::new();
    ENGINE
        .get_or_init(|| {
            Arc::new(Mutex::new(
                BrainEngine::open_default().expect("rs_gbrain open"),
            ))
        })
        .clone()
}

pub struct BrainSearchTool;

#[async_trait]
impl Tool for BrainSearchTool {
    fn name(&self) -> &str {
        "brain_search"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "brain_search".into(),
            description: "FTS search local rs_gbrain SQLite brain".into(),
            parameters: json!({
                "type": "object",
                "properties": { "query": { "type": "string" } },
                "required": ["query"]
            }),
        }
    }

    async fn execute(&self, arguments: &str) -> anyhow::Result<ToolResult> {
        let args: serde_json::Value = serde_json::from_str(arguments).unwrap_or(json!({}));
        let q = args["query"].as_str().unwrap_or("").trim();
        if q.is_empty() {
            return Ok(ToolResult::error("query required"));
        }
        let engine = shared_engine();
        let guard = engine.lock().await;
        match guard.search(q, 10) {
            Ok(h) => Ok(ToolResult::success(serde_json::to_string(&h)?)),
            Err(e) => Ok(ToolResult::error(format!("{e:#}"))),
        }
    }
}

pub struct BrainQueryTool;

#[async_trait]
impl Tool for BrainQueryTool {
    fn name(&self) -> &str {
        "brain_query"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "brain_query".into(),
            description: "Answer from rs_gbrain with citations and gaps".into(),
            parameters: json!({
                "type": "object",
                "properties": { "question": { "type": "string" } },
                "required": ["question"]
            }),
        }
    }

    async fn execute(&self, arguments: &str) -> anyhow::Result<ToolResult> {
        let args: serde_json::Value = serde_json::from_str(arguments).unwrap_or(json!({}));
        let q = args["question"].as_str().unwrap_or("").trim();
        if q.is_empty() {
            return Ok(ToolResult::error("question required"));
        }
        let engine = shared_engine();
        let guard = engine.lock().await;
        match gather_context(&guard, q, 8) {
            Ok(ans) => Ok(ToolResult::success(serde_json::to_string(&ans)?)),
            Err(e) => Ok(ToolResult::error(format!("{e:#}"))),
        }
    }
}

pub struct BrainPutTool;

#[async_trait]
impl Tool for BrainPutTool {
    fn name(&self) -> &str {
        "brain_put"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "brain_put".into(),
            description: "Upsert a page in rs_gbrain".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "slug": { "type": "string" },
                    "title": { "type": "string" },
                    "body": { "type": "string" },
                    "page_type": { "type": "string" }
                },
                "required": ["slug", "body"]
            }),
        }
    }

    async fn execute(&self, arguments: &str) -> anyhow::Result<ToolResult> {
        let args: serde_json::Value = serde_json::from_str(arguments).unwrap_or(json!({}));
        let slug = args["slug"].as_str().unwrap_or("").trim();
        let body = args["body"].as_str().unwrap_or("");
        if slug.is_empty() {
            return Ok(ToolResult::error("slug required"));
        }
        let title = args["title"]
            .as_str()
            .unwrap_or(slug.rsplit('/').next().unwrap_or(slug));
        let page_type = args["page_type"].as_str().unwrap_or("note");
        let engine = shared_engine();
        let guard = engine.lock().await;
        guard.put_page(slug, title, page_type, body, "agent")?;
        Ok(ToolResult::success(format!("put {slug}")))
    }
}

pub struct BrainGetTool;

#[async_trait]
impl Tool for BrainGetTool {
    fn name(&self) -> &str {
        "brain_get"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "brain_get".into(),
            description: "Get rs_gbrain page by slug".into(),
            parameters: json!({
                "type": "object",
                "properties": { "slug": { "type": "string" } },
                "required": ["slug"]
            }),
        }
    }

    async fn execute(&self, arguments: &str) -> anyhow::Result<ToolResult> {
        let args: serde_json::Value = serde_json::from_str(arguments).unwrap_or(json!({}));
        let slug = args["slug"].as_str().unwrap_or("");
        let engine = shared_engine();
        let guard = engine.lock().await;
        match guard.get_page(slug)? {
            Some(p) => Ok(ToolResult::success(format!("{}\n---\n{}", p.title, p.body))),
            None => Ok(ToolResult::error("not found")),
        }
    }
}
