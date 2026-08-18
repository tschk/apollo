//! Pluggable context compaction for long conversations.
//!
//! Two implementations, and the difference matters:
//!
//! - [`DefaultCompactor`] drops the old turns and leaves a marker in their
//!   place. It needs nothing and knows nothing.
//! - [`LlmCompactor`] asks a model to summarize them first, so what the marker
//!   replaces is preserved rather than discarded.
//!
//! `DefaultCompactor` was documented as "summarization-based using a fast
//! model" while it built a summarization prompt, dropped it on the floor, and
//! emitted the truncation marker anyway. It is honest truncation now, and the
//! summarizing one is the one that talks to a provider.
//!
//! The rx4 engine runs its own auto-compaction (`agent.auto_compact_after`),
//! so neither of these is on apollo's own turn path. They are the extension
//! point for an embedder that wants compaction under its own control.

use std::sync::Arc;

use async_trait::async_trait;

use crate::providers::{ChatMessage, ChatRequest, Provider};
use crate::text::truncate_chars_counted;

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

/// Drops old turns, keeping the system messages and the recent tail.
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
        let _ = task;

        CompressResult {
            did_compact: old_msgs.len() > 2,
            messages: assemble(
                &system_msgs,
                &format!("[Compacted — {} earlier messages dropped]", old_msgs.len()),
                recent_msgs,
            ),
        }
    }
}

// ── LLM compactor ─────────────────────────────────────────────────────────

/// Summarizes the dropped turns with a model before discarding them.
///
/// A failed or empty summary falls back to the same marker
/// [`DefaultCompactor`] would leave: compaction exists to keep a conversation
/// inside its context window, so it must not fail the turn when the
/// summarizer is unavailable.
pub struct LlmCompactor {
    provider: Arc<dyn Provider>,
    model: String,
    /// Turns kept verbatim at the end of the conversation.
    keep_recent: usize,
    /// Chars of each old message the summarizer is shown.
    per_message_chars: usize,
}

impl LlmCompactor {
    pub fn new(provider: Arc<dyn Provider>, model: impl Into<String>) -> Self {
        Self {
            provider,
            model: model.into(),
            keep_recent: 6,
            per_message_chars: 500,
        }
    }

    pub fn keeping_recent(mut self, keep_recent: usize) -> Self {
        self.keep_recent = keep_recent;
        self
    }

    fn prompt(&self, old_msgs: &[&ChatMessage], task: Option<&str>) -> String {
        let mut transcript = String::new();
        for m in old_msgs {
            let role_label = match m.role.as_str() {
                "user" => "User",
                "assistant" | "assistant_tool_use" => "Assistant",
                "tool_result" => "Tool Result",
                _ => &m.role,
            };
            let content = match truncate_chars_counted(&m.content, self.per_message_chars) {
                Some((head, _)) => format!("{head}..."),
                None => m.content.clone(),
            };
            transcript.push_str(&format!("[{role_label}]: {content}\n"));
        }

        format!(
            "Summarize this conversation concisely. Original task: \"{}\"\n\n\
             Focus on: what was accomplished, key results, what's still pending.\n\n\
             Conversation:\n{transcript}",
            task.unwrap_or("unknown")
        )
    }

    async fn summarize(&self, old_msgs: &[&ChatMessage], task: Option<&str>) -> Option<String> {
        let messages = [ChatMessage::user(self.prompt(old_msgs, task))];
        let request = ChatRequest {
            messages: &messages,
            tools: None,
            model: &self.model,
            temperature: 0.3,
            max_tokens: Some(1024),
        };
        match self.provider.chat(&request).await {
            Ok(response) if !response.text_or_empty().trim().is_empty() => {
                Some(response.text_or_empty().trim().to_string())
            }
            Ok(_) => {
                tracing::warn!("compaction summary was empty; dropping the messages instead");
                None
            }
            Err(e) => {
                tracing::warn!("compaction summary failed ({e}); dropping the messages instead");
                None
            }
        }
    }
}

#[async_trait]
impl Compactor for LlmCompactor {
    fn name(&self) -> &str {
        "llm_compactor"
    }

    fn should_compress(&self, info: &ContextInfo) -> bool {
        DefaultCompactor.should_compress(info)
    }

    async fn compress(&self, messages: &[ChatMessage], task: Option<&str>) -> CompressResult {
        if messages.len() <= self.keep_recent + 2 {
            return CompressResult {
                did_compact: false,
                messages: messages.to_vec(),
            };
        }

        let system_msgs: Vec<&ChatMessage> =
            messages.iter().filter(|m| m.role == "system").collect();
        let non_system: Vec<&ChatMessage> =
            messages.iter().filter(|m| m.role != "system").collect();

        if non_system.len() <= self.keep_recent {
            return CompressResult {
                did_compact: false,
                messages: messages.to_vec(),
            };
        }

        let (old_msgs, recent_msgs) = non_system.split_at(non_system.len() - self.keep_recent);
        let body = match self.summarize(old_msgs, task).await {
            Some(summary) => format!(
                "[Compacted — {} earlier messages summarized]\n\n{summary}",
                old_msgs.len()
            ),
            None => format!("[Compacted — {} earlier messages dropped]", old_msgs.len()),
        };

        CompressResult {
            did_compact: old_msgs.len() > 2,
            messages: assemble(&system_msgs, &body, recent_msgs),
        }
    }
}

/// System messages, then the compaction marker as a user/assistant exchange,
/// then the recent tail verbatim.
fn assemble(
    system_msgs: &[&ChatMessage],
    body: &str,
    recent_msgs: &[&ChatMessage],
) -> Vec<ChatMessage> {
    let mut compacted: Vec<ChatMessage> = system_msgs.iter().map(|m| (*m).clone()).collect();
    compacted.push(ChatMessage {
        role: "user".into(),
        content: body.to_string(),
        tool_use_id: None,
    });
    compacted.push(ChatMessage {
        role: "assistant".into(),
        content: "Understood, continuing from summary.".into(),
        tool_use_id: None,
    });
    compacted.extend(recent_msgs.iter().map(|m| (*m).clone()));
    compacted
}

// ── Convenience factory ───────────────────────────────────────────────────

pub fn default_compactor() -> Arc<dyn Compactor> {
    Arc::new(DefaultCompactor::new())
}

/// A compactor that summarizes with `model` before dropping old turns.
pub fn llm_compactor(provider: Arc<dyn Provider>, model: impl Into<String>) -> Arc<dyn Compactor> {
    Arc::new(LlmCompactor::new(provider, model))
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::providers::traits::ProviderCapabilities;
    use crate::providers::ChatResponse;

    /// Records what it was asked to summarize and answers with a fixed reply,
    /// or fails when told to.
    struct FakeProvider {
        reply: Option<String>,
        seen: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl Provider for FakeProvider {
        fn name(&self) -> &str {
            "fake"
        }

        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities::default()
        }

        async fn chat(&self, request: &ChatRequest<'_>) -> anyhow::Result<ChatResponse> {
            self.seen
                .lock()
                .unwrap()
                .push(request.messages[0].content.clone());
            match &self.reply {
                Some(reply) => Ok(ChatResponse {
                    text: Some(reply.clone()),
                    ..Default::default()
                }),
                None => anyhow::bail!("summarizer unavailable"),
            }
        }
    }

    fn conversation(n: usize) -> Vec<ChatMessage> {
        let mut messages = vec![ChatMessage::system("you are a bot")];
        for i in 0..n {
            messages.push(ChatMessage::user(format!("question {i}")));
            messages.push(ChatMessage::assistant(format!("answer {i}")));
        }
        messages
    }

    #[tokio::test]
    async fn the_llm_compactor_puts_the_summary_where_the_messages_were() {
        let provider = Arc::new(FakeProvider {
            reply: Some("Booked the table, deposit still unpaid.".into()),
            seen: Mutex::new(Vec::new()),
        });
        let compactor = LlmCompactor::new(Arc::clone(&provider) as Arc<dyn Provider>, "fast-model");

        let messages = conversation(10);
        let result = compactor.compress(&messages, Some("take a booking")).await;

        assert!(result.did_compact);
        assert_eq!(result.messages[0].role, "system", "system prompt is kept");
        assert!(
            result.messages[1].content.contains("Booked the table"),
            "{:?}",
            result.messages[1].content
        );
        assert!(
            result.messages.last().unwrap().content.contains("answer 9"),
            "the recent tail is kept verbatim"
        );
        assert!(result.messages.len() < messages.len());

        let seen = provider.seen.lock().unwrap();
        assert_eq!(seen.len(), 1, "one summarization call per compaction");
        assert!(seen[0].contains("take a booking"), "{}", seen[0]);
        assert!(
            seen[0].contains("question 0"),
            "the old turns are summarized"
        );
        assert!(
            !seen[0].contains("answer 9"),
            "the kept tail is not re-summarized"
        );
    }

    #[tokio::test]
    async fn a_failing_summarizer_still_compacts() {
        let provider = Arc::new(FakeProvider {
            reply: None,
            seen: Mutex::new(Vec::new()),
        });
        let compactor = LlmCompactor::new(provider as Arc<dyn Provider>, "fast-model");

        let messages = conversation(10);
        let result = compactor.compress(&messages, None).await;

        assert!(
            result.did_compact,
            "compaction must not depend on the model"
        );
        assert!(result.messages[1].content.contains("dropped"));
        assert!(result.messages.len() < messages.len());
    }

    #[tokio::test]
    async fn a_short_conversation_is_left_alone_by_both() {
        let provider = Arc::new(FakeProvider {
            reply: Some("summary".into()),
            seen: Mutex::new(Vec::new()),
        });
        let messages = conversation(2);

        let llm = LlmCompactor::new(Arc::clone(&provider) as Arc<dyn Provider>, "m")
            .compress(&messages, None)
            .await;
        assert!(!llm.did_compact);
        assert_eq!(llm.messages.len(), messages.len());
        assert!(
            provider.seen.lock().unwrap().is_empty(),
            "nothing to summarize means no model call"
        );

        let plain = DefaultCompactor::new().compress(&messages, None).await;
        assert!(!plain.did_compact);
        assert_eq!(plain.messages.len(), messages.len());
    }

    #[tokio::test]
    async fn the_default_compactor_says_what_it_actually_did() {
        let result = DefaultCompactor::new()
            .compress(&conversation(10), Some("task"))
            .await;
        assert!(result.did_compact);
        assert!(
            result.messages[1].content.contains("dropped"),
            "the marker must not claim a summary that was never made: {:?}",
            result.messages[1].content
        );
    }

    #[test]
    fn compaction_triggers_at_three_quarters_of_the_window() {
        let info = |total_chars| ContextInfo {
            message_count: 20,
            total_chars,
            max_chars: 1000,
            compactions_done: 0,
        };
        assert!(!DefaultCompactor.should_compress(&info(700)));
        assert!(DefaultCompactor.should_compress(&info(800)));
    }
}
