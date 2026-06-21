//! Pluggable context compaction for long conversations.
//!
//! Compactor trait: decides when to compact + performs the compaction.
//! DefaultCompactor: summarization-based using a fast model.
//!
//! Inspired by hermes-agent context_engine.py ABC pattern.
//!
//! pontytail: single compactor, no plugin discovery. Trait-based so plugins
//! can register via the PluginRegistry when multi-engine support is needed.

use std::sync::Arc;

use async_trait::async_trait;

use crate::providers::ChatMessage;

// ── Config ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ContextInfo {
    pub message_count: usize,
    pub total_chars: usize,
    pub max_chars: usize,
    pub compactions_done: usize,
}

#[derive(Debug, Clone)]
pub struct CompressResult {
    pub did_compact: bool,
    pub messages: Vec<ChatMessage>,
}

// ── Compactor trait ───────────────────────────────────────────────────────

#[async_trait]
pub trait Compactor: Send + Sync {
    fn name(&self) -> &str;
    fn should_compress(&self, info: &ContextInfo) -> bool;
    async fn compress(&self, messages: &[ChatMessage], task: Option<&str>) -> CompressResult;
}

// ── Default compactor ─────────────────────────────────────────────────────

pub struct DefaultCompactor;

impl DefaultCompactor {
    pub fn new() -> Self {
        Self
    }
}

impl Default for DefaultCompactor {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Compactor for DefaultCompactor {
    fn name(&self) -> &str {
        "default_compactor"
    }

    fn should_compress(&self, info: &ContextInfo) -> bool {
        // Compress when we exceed 75% of max context
        let threshold = (0.75 * info.max_chars as f64) as usize;
        info.total_chars > threshold
    }

    async fn compress(&self, messages: &[ChatMessage], task: Option<&str>) -> CompressResult {
        let keep_recent = 6;

        if messages.len() <= keep_recent + 2 {
            return CompressResult {
                did_compact: false,
                messages: messages.to_vec(),
            };
        }

        let system_msgs: Vec<&ChatMessage> =
            messages.iter().filter(|m| m.role == "system").collect();
        let non_system: Vec<&ChatMessage> =
            messages.iter().filter(|m| m.role != "system").collect();

        if non_system.len() <= keep_recent {
            return CompressResult {
                did_compact: false,
                messages: messages.to_vec(),
            };
        }

        let (old_msgs, recent_msgs) = non_system.split_at(non_system.len() - keep_recent);

        let mut summary_input = String::new();
        for m in old_msgs {
            let role_label = match m.role.as_str() {
                "user" => "User",
                "assistant" | "assistant_tool_use" => "Assistant",
                "tool_result" => "Tool Result",
                _ => &m.role,
            };
            let content = if m.content.len() > 500 {
                format!("{}...", &m.content[..500])
            } else {
                m.content.clone()
            };
            summary_input.push_str(&format!("[{}]: {}\n", role_label, content));
        }

        let original_task = task.unwrap_or("unknown");

        let compact_prompt = format!(
            "Summarize this conversation concisely. Original task: \"{}\"\n\n\
             Focus on: what was accomplished, key results, what's still pending.\n\n\
             Conversation:\n{}",
            original_task, summary_input
        );

        let _compact_msgs = [ChatMessage {
            role: "user".into(),
            content: compact_prompt,
            tool_use_id: None,
        }];

        // TODO: wire in provider for LLM-based summarization.
        // For now, fall back to simple truncation summary.
        let summary = format!(
            "[Compacted — {} earlier messages. Prior context summarized.]",
            old_msgs.len()
        );

        let mut compacted = Vec::new();
        for sm in &system_msgs {
            compacted.push((*sm).clone());
        }
        compacted.push(ChatMessage {
            role: "user".into(),
            content: format!(
                "[Compacted — {} earlier messages summarized]\n\n{}",
                old_msgs.len(),
                summary
            ),
            tool_use_id: None,
        });
        compacted.push(ChatMessage {
            role: "assistant".into(),
            content: "Understood, continuing from summary.".into(),
            tool_use_id: None,
        });
        for rm in recent_msgs {
            compacted.push((*rm).clone());
        }

        CompressResult {
            did_compact: old_msgs.len() > 2,
            messages: compacted,
        }
    }
}

// ── Convenience factory ───────────────────────────────────────────────────

pub fn default_compactor() -> Arc<dyn Compactor> {
    Arc::new(DefaultCompactor::new())
}
