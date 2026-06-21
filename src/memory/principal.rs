//! Principal — one user, many channel chat_ids.

use std::collections::HashMap;
use std::sync::Arc;

use crate::memory::MemoryBackend;

pub const PRINCIPAL_NS: &str = "principal";
pub const ALIASES_KEY: &str = "chat_aliases";

pub fn default_aliases(principal_id: &str) -> HashMap<String, String> {
    let mut m = HashMap::new();
    m.insert("cli".to_string(), principal_id.to_string());
    m.insert("heartbeat".to_string(), principal_id.to_string());
    m
}

pub async fn load_chat_aliases(
    memory: &Arc<dyn MemoryBackend>,
    principal_id: &str,
) -> HashMap<String, String> {
    let mut aliases = default_aliases(principal_id);
    if let Ok(Some(entry)) = memory.recall(PRINCIPAL_NS, ALIASES_KEY).await {
        if let Ok(extra) = serde_json::from_str::<HashMap<String, String>>(&entry.value) {
            aliases.extend(extra);
        }
    }
    aliases
}

pub fn resolve_history_chat_ids(
    incoming_chat_id: &str,
    principal_id: &str,
    aliases: &HashMap<String, String>,
) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    let canonical = aliases
        .get(incoming_chat_id)
        .cloned()
        .unwrap_or_else(|| incoming_chat_id.to_string());

    ids.push(canonical.clone());
    ids.push(incoming_chat_id.to_string());

    for (alias, target) in aliases {
        if target == &canonical || target == principal_id {
            ids.push(alias.clone());
        }
    }
    ids.push(principal_id.to_string());
    ids.sort();
    ids.dedup();
    ids
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_ids() {
        let mut a = default_aliases("user-1");
        a.insert("12345".to_string(), "user-1".to_string());
        let ids = resolve_history_chat_ids("12345", "user-1", &a);
        assert!(ids.contains(&"12345".to_string()));
        assert!(ids.contains(&"user-1".to_string()));
    }
}
