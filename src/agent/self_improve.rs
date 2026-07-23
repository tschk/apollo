//! Self-improvement loop backed by `zkr` evidence memory and consumed by the
//! `rx4` (rotary) agent bridge.
//!
//! Reflections are recorded as `SourceKind::Integration` memories tagged as
//! `ClaimKind::Skill`. Before each agent run the bridge asks this module for
//! relevant lessons; the most relevant excerpts are injected into the system
//! prompt, so the agent learns from previous successes and failures.

use std::sync::Arc;

use chrono::Utc;
use zkr::{ClaimInput, ClaimKind, MemoryProcessingState, MemoryTier, SourceKind};

use crate::memory::zkr::ZkrStore;

pub struct SelfImprove {
    store: Option<Arc<ZkrStore>>,
}

impl SelfImprove {
    pub fn new(store: Option<Arc<ZkrStore>>) -> Self {
        Self { store }
    }

    /// Record a reflection from a completed action.
    pub async fn record(
        &self,
        context: &str,
        action: &str,
        outcome: &str,
        lesson: &str,
    ) -> anyhow::Result<()> {
        let Some(store) = &self.store else {
            return Ok(());
        };

        let text =
            format!("Context: {context}\nAction: {action}\nOutcome: {outcome}\nLesson: {lesson}");
        let captured_at = Utc::now().timestamp();
        let claim = ClaimInput {
            subject: context.to_string(),
            predicate: "improved".to_string(),
            value: lesson.to_string(),
            kind: ClaimKind::Skill,
            valid_from: captured_at,
            tier: MemoryTier::LongTerm,
            processing_state: MemoryProcessingState::Processed,
        };

        store
            .remember(
                text,
                SourceKind::Integration,
                Some("self_improve".to_string()),
                Some(claim),
                captured_at,
            )
            .await?;
        Ok(())
    }

    /// Retrieve relevant lessons and augment the base system prompt.
    pub async fn augment_prompt(&self, prompt: &str, base_prompt: &str) -> anyhow::Result<String> {
        let Some(store) = &self.store else {
            return Ok(base_prompt.to_string());
        };

        let mut excerpts: Vec<String> = Vec::new();
        let mut seen = std::collections::HashSet::new();

        for query in ["self improve lessons", prompt] {
            let pack = store.search(query.to_string(), 5).await?;
            for item in pack.items {
                let text = item.excerpt.trim().to_string();
                if !text.contains("Lesson:") {
                    continue;
                }
                if seen.insert(text.clone()) {
                    excerpts.push(text);
                }
            }
        }

        if excerpts.is_empty() {
            return Ok(base_prompt.to_string());
        }

        let mut augmented = base_prompt.to_string();
        augmented.push_str("\n\n<lessons_learned>\n");
        for (i, ex) in excerpts.iter().take(5).enumerate() {
            augmented.push_str(&format!("{}. {ex}\n", i + 1));
        }
        augmented.push_str("</lessons_learned>");
        Ok(augmented)
    }
}
