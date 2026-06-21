//! Recall gate — inject file/KV snippets when the user references past context.

use std::path::Path;
use std::sync::Arc;

use crate::memory::search::memory_search;
use crate::memory::MemoryBackend;

const RECALL_PATTERNS: &[&str] = &[
    "remember",
    "last time",
    "we discussed",
    "as before",
    "earlier",
    "you said",
    "what did we",
    "previously",
    "again",
];

pub fn should_recall_gate(text: &str) -> bool {
    let lower = text.to_lowercase();
    RECALL_PATTERNS.iter().any(|p| lower.contains(p))
}

pub async fn build_supporting_recall(
    workspace: &Path,
    memory: &Arc<dyn MemoryBackend>,
    user_text: &str,
    limit: usize,
) -> Option<String> {
    if !should_recall_gate(user_text) {
        return None;
    }

    let mut bullets: Vec<String> = Vec::new();

    for hit in memory_search(workspace, user_text, limit)
        .into_iter()
        .take(limit)
    {
        bullets.push(format!(
            "- [{}:{}] {}",
            hit.path, hit.line_number, hit.snippet
        ));
    }

    if bullets.len() < limit {
        if let Ok(kv) = memory.search("notes", user_text, limit).await {
            for entry in kv.into_iter().take(limit.saturating_sub(bullets.len())) {
                bullets.push(format!("- [kv:{}] {}", entry.key, entry.value));
            }
        }
    }

    if bullets.is_empty() {
        return None;
    }

    let body = bullets.join("\n");
    Some(format!("<supporting_recall>\n{body}\n</supporting_recall>"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_triggers() {
        assert!(should_recall_gate("Do you remember the deploy?"));
        assert!(!should_recall_gate("deploy now"));
    }
}
