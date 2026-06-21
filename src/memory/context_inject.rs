//! Assemble Vellum-style context blocks before the user turn.

use std::path::Path;
use std::sync::Arc;

use super::brief::{format_brief_xml, load_brief};
use super::graph::GraphNode;
use super::principal::{load_chat_aliases, resolve_history_chat_ids};
use super::recall::build_supporting_recall;
use super::surreal::SurrealMemory;
use crate::memory::MemoryBackend;

pub struct InjectConfig<'a> {
    pub workspace: &'a Path,
    pub principal_id: Option<&'a str>,
    pub graph_recall_limit: usize,
}

pub async fn merged_history(
    memory: &Arc<dyn MemoryBackend>,
    chat_id: &str,
    principal_id: Option<&str>,
    per_chat_limit: usize,
) -> anyhow::Result<Vec<(String, String)>> {
    let Some(pid) = principal_id.filter(|s| !s.is_empty()) else {
        return memory
            .get_conversation_history(chat_id, per_chat_limit)
            .await;
    };

    let aliases = load_chat_aliases(memory, pid).await;
    let chat_ids = resolve_history_chat_ids(chat_id, pid, &aliases);
    let mut merged: Vec<(String, String)> = Vec::new();

    for cid in chat_ids {
        let rows = memory
            .get_conversation_history(&cid, per_chat_limit)
            .await?;
        for row in rows {
            if !merged.contains(&row) {
                merged.push(row);
            }
        }
    }

    if merged.len() > per_chat_limit {
        merged = merged.split_off(merged.len().saturating_sub(per_chat_limit));
    }
    Ok(merged)
}

pub async fn personal_context_blocks(
    memory: &Arc<dyn MemoryBackend>,
    cfg: InjectConfig<'_>,
    user_text: &str,
) -> Vec<String> {
    let mut blocks = Vec::new();

    let (tc, ol) = load_brief(memory).await;
    if let Some(b) = format_brief_xml(&tc, &ol) {
        blocks.push(b);
    }

    if let Some(r) = build_supporting_recall(cfg.workspace, memory, user_text, 3).await {
        blocks.push(r);
    }

    if let Some(surreal) = memory.as_any().downcast_ref::<SurrealMemory>() {
        if let Ok(seed) = surreal
            .graph_match_text(user_text, cfg.graph_recall_limit)
            .await
        {
            let mut nodes: Vec<GraphNode> = seed;
            if let Some(first) = nodes.first() {
                if let Ok(neighbors) = surreal.graph_neighbors(&first.id, 3).await {
                    for n in neighbors {
                        if !nodes.iter().any(|x| x.id == n.id) {
                            nodes.push(n);
                        }
                    }
                }
            }
            if let Some(g) = surreal.format_graph_context(&nodes) {
                blocks.push(g);
            }
        }
    }

    blocks
}
