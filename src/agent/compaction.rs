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

use crate::providers::{ChatMessage, ChatRequest, Provider};
use crate::text::truncate_chars_counted;

const KEEP_RECENT: usize = 6;

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

pub struct DefaultCompactor {
    provider: Option<Arc<dyn Provider>>,
    model: String,
}

impl DefaultCompactor {
    pub fn new() -> Self {
        Self {
            provider: None,
            model: String::new(),
        }
    }

    pub fn with_provider(provider: Arc<dyn Provider>, model: impl Into<String>) -> Self {
        Self {
            provider: Some(provider),
            model: model.into(),
        }
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
        if messages.len() <= KEEP_RECENT + 2 {
            return CompressResult {
                did_compact: false,
                messages: messages.to_vec(),
            };
        }

        let system_msgs: Vec<&ChatMessage> =
            messages.iter().filter(|m| m.role == "system").collect();
        let non_system: Vec<&ChatMessage> =
            messages.iter().filter(|m| m.role != "system").collect();

        if non_system.len() <= KEEP_RECENT {
            return CompressResult {
                did_compact: false,
                messages: messages.to_vec(),
            };
        }

        let (old_msgs, recent_msgs) = non_system.split_at(non_system.len() - KEEP_RECENT);
        let original_task = task.unwrap_or("unknown");
        let summary_input = transcript_for_summary(old_msgs);
        let summary = self
            .summarize(original_task, &summary_input, old_msgs.len())
            .await;

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

impl DefaultCompactor {
    async fn summarize(&self, task: &str, transcript: &str, old_count: usize) -> String {
        let prompt = compact_prompt(task, transcript);
        if let Some(provider) = &self.provider {
            if !self.model.is_empty() {
                let messages = [ChatMessage::user(prompt.clone())];
                let request = ChatRequest {
                    messages: &messages,
                    tools: None,
                    model: &self.model,
                    temperature: 0.2,
                    max_tokens: Some(800),
                };
                match provider.chat(&request).await {
                    Ok(response) => {
                        let text = response.text_or_empty().trim();
                        if !text.is_empty() {
                            return text.to_string();
                        }
                    }
                    Err(error) => tracing::warn!("compaction summarizer failed: {error}"),
                }
            }
        }
        fallback_summary(old_count, transcript)
    }
}

fn transcript_for_summary(old_msgs: &[&ChatMessage]) -> String {
    let mut summary_input = String::new();
    for m in old_msgs {
        let role_label = match m.role.as_str() {
            "user" => "User",
            "assistant" | "assistant_tool_use" => "Assistant",
            "tool_result" => "Tool Result",
            _ => &m.role,
        };
        let content = match truncate_chars_counted(&m.content, 500) {
            Some((head, _)) => format!("{head}..."),
            None => m.content.clone(),
        };
        summary_input.push_str(&format!("[{role_label}]: {content}\n"));
    }
    summary_input
}

fn compact_prompt(task: &str, transcript: &str) -> String {
    format!(
        "Summarize this conversation as a compaction recap. Original task: \"{task}\"\n\n\
         Preserve: conversation anchors, exact identifiers (ids, paths, names), \
         completed work, and open loops still pending. Be concise.\n\n\
         Conversation:\n{transcript}"
    )
}

fn fallback_summary(old_count: usize, transcript: &str) -> String {
    let mut anchors = Vec::new();
    for line in transcript.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        anchors.push(trimmed.to_string());
        if anchors.len() == 6 {
            break;
        }
    }
    if anchors.is_empty() {
        format!("[Compacted — {old_count} earlier messages. Prior context summarized.]")
    } else {
        format!(
            "[Compacted — {old_count} earlier messages]\n{}",
            anchors.join("\n")
        )
    }
}

// ── Convenience factory ───────────────────────────────────────────────────

pub fn default_compactor() -> Arc<dyn Compactor> {
    Arc::new(DefaultCompactor::new())
}

pub fn llm_compactor(provider: Arc<dyn Provider>, model: impl Into<String>) -> Arc<dyn Compactor> {
    Arc::new(DefaultCompactor::with_provider(provider, model))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::traits::ProviderCapabilities;
    use crate::providers::ChatResponse;

    struct ScriptedProvider {
        text: String,
    }

    #[async_trait]
    impl Provider for ScriptedProvider {
        fn name(&self) -> &str {
            "scripted"
        }

        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities::default()
        }

        async fn chat(&self, request: &ChatRequest<'_>) -> anyhow::Result<ChatResponse> {
            assert!(request
                .messages
                .iter()
                .any(|m| m.content.contains("open loops")));
            Ok(ChatResponse {
                text: Some(self.text.clone()),
                tool_calls: vec![],
                usage: None,
            })
        }
    }

    fn long_conversation() -> Vec<ChatMessage> {
        let mut messages = vec![ChatMessage::system("sys")];
        for i in 0..12 {
            messages.push(ChatMessage::user(format!("user turn {i} ticket T-{i}")));
            messages.push(ChatMessage::assistant(format!("ack {i}")));
        }
        messages
    }

    #[tokio::test]
    async fn fallback_keeps_anchors_without_provider() {
        let result = DefaultCompactor::new()
            .compress(&long_conversation(), Some("ship memory"))
            .await;
        assert!(result.did_compact);
        let recap = &result.messages[1].content;
        assert!(recap.contains("Compacted"));
        assert!(recap.contains("ticket T-0"));
        assert!(!recap.contains("Prior context summarized."));
    }

    #[tokio::test]
    async fn llm_summary_is_used_when_provider_returns_text() {
        let provider = Arc::new(ScriptedProvider {
            text: "Open loop: wait on vendor. Identifier: T-9.".into(),
        });
        let result = DefaultCompactor::with_provider(provider, "fast")
            .compress(&long_conversation(), Some("ship memory"))
            .await;
        assert!(result.did_compact);
        assert!(result.messages[1]
            .content
            .contains("Open loop: wait on vendor"));
    }
}
