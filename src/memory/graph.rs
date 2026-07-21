//! Idea graph — nodes, edges, low-confidence "dream" hypotheses (Surreal-native).

use anyhow::Result;
use chrono::Utc;
use serde::{Deserialize, Serialize};

use super::surreal::SurrealMemory;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphNode {
    pub id: String,
    pub kind: String,
    pub text: String,
    pub confidence: f32,
    pub status: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphEdge {
    pub from_id: String,
    pub to_id: String,
    pub rel: String,
    pub created_at: String,
}

fn slug_id(prefix: &str, text: &str) -> String {
    let mut s: String = text
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    while s.contains("--") {
        s = s.replace("--", "-");
    }
    s = s.trim_matches('-').chars().take(48).collect();
    format!("{prefix}-{s}")
}

impl SurrealMemory {
    pub async fn graph_upsert_node(
        &self,
        kind: &str,
        text: &str,
        confidence: f32,
        status: &str,
    ) -> Result<String> {
        let id = slug_id(kind, text);
        let row = GraphNode {
            id: id.clone(),
            kind: kind.to_string(),
            text: text.to_string(),
            confidence,
            status: status.to_string(),
            created_at: Utc::now().to_rfc3339(),
        };
        let _: Option<GraphNode> = self
            .db()
            .upsert(("memory_nodes", id.as_str()))
            .content(row)
            .await?;
        Ok(id)
    }

    pub async fn graph_link(&self, from_id: &str, to_id: &str, rel: &str) -> Result<()> {
        let edge_id = format!("{from_id}::{rel}::{to_id}");
        let row = GraphEdge {
            from_id: from_id.to_string(),
            to_id: to_id.to_string(),
            rel: rel.to_string(),
            created_at: Utc::now().to_rfc3339(),
        };
        let _: Option<GraphEdge> = self
            .db()
            .upsert(("memory_edges", edge_id.as_str()))
            .content(row)
            .await?;
        Ok(())
    }

    pub async fn graph_neighbors(&self, node_id: &str, limit: usize) -> Result<Vec<GraphNode>> {
        let mut res = self
            .db()
            .query("SELECT * FROM memory_edges WHERE from_id = $id OR to_id = $id LIMIT 50")
            .bind(("id", node_id.to_string()))
            .await?;
        let edges: Vec<GraphEdge> = res.take(0)?;
        let mut nodes = Vec::new();
        for e in edges {
            let other = if e.from_id == node_id {
                e.to_id.as_str()
            } else {
                e.from_id.as_str()
            };
            let n: Option<GraphNode> = self.db().select(("memory_nodes", other)).await?;
            if let Some(n) = n {
                nodes.push(n);
            }
            if nodes.len() >= limit {
                break;
            }
        }
        Ok(nodes)
    }

    pub async fn graph_match_text(&self, query: &str, limit: usize) -> Result<Vec<GraphNode>> {
        let q = query.to_lowercase();
        let words: Vec<&str> = q.split_whitespace().filter(|w| w.len() > 2).collect();
        if words.is_empty() {
            return Ok(Vec::new());
        }
        let mut res = self
            .db()
            .query("SELECT * FROM memory_nodes LIMIT 200")
            .await?;
        let all: Vec<GraphNode> = res.take(0)?;
        let mut scored: Vec<(f32, GraphNode)> = all
            .into_iter()
            .filter_map(|n| {
                let t = n.text.to_lowercase();
                let score = words.iter().filter(|w| t.contains(**w)).count() as f32;
                if score > 0.0 {
                    Some((score, n))
                } else {
                    None
                }
            })
            .collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        Ok(scored.into_iter().take(limit).map(|(_, n)| n).collect())
    }

    /// Offline-style dream stub: link open-loop text to a low-confidence idea node.
    pub async fn graph_dream_from_loops(&self, open_loops: &[String]) -> Result<usize> {
        let mut n = 0usize;
        for line in open_loops.iter().take(5) {
            let parent = self.graph_upsert_node("loop", line, 0.9, "active").await?;
            let dream_text = format!("What if we explored: {line}?");
            let child = self
                .graph_upsert_node("dream", &dream_text, 0.35, "dream")
                .await?;
            self.graph_link(&parent, &child, "hypothesized").await?;
            n += 1;
        }
        Ok(n)
    }

    pub fn format_graph_context(&self, nodes: &[GraphNode]) -> Option<String> {
        if nodes.is_empty() {
            return None;
        }
        let mut out = String::from("<idea_graph>\n");
        for n in nodes {
            out.push_str(&format!(
                "  - [{}:{}] {} (conf {:.2})\n",
                n.kind, n.status, n.text, n.confidence
            ));
        }
        out.push_str("</idea_graph>");
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_stable() {
        assert!(slug_id("idea", "Hello World").starts_with("idea-"));
    }
}
