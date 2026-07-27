use std::path::Path;
use std::sync::Arc;

use parking_lot::Mutex;
use zkr::{
    ClaimId, ClaimInput, ClaimKind, CorrectInput, DeleteInput, EmbeddingTarget, GetInput, MemoryDb,
    PersonId, ProfilesInput, RememberInput, Remembered, RetrievalItem, RetrievalPack, SearchInput,
    SourceId, SourceKind, TenantId,
};

pub struct ZkrStore {
    db: Arc<Mutex<MemoryDb>>,
    tenant_id: TenantId,
    person_id: PersonId,
}

impl ZkrStore {
    pub fn open(
        path: &Path,
        tenant_id: impl Into<String>,
        person_id: impl Into<String>,
    ) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Ok(Self {
            db: Arc::new(Mutex::new(MemoryDb::open(path)?)),
            tenant_id: TenantId::new(tenant_id.into())?,
            person_id: PersonId::new(person_id.into())?,
        })
    }

    pub async fn remember(
        &self,
        text: String,
        kind: SourceKind,
        ingestion_key: Option<String>,
        claim: Option<ClaimInput>,
        captured_at: i64,
    ) -> anyhow::Result<Remembered> {
        let db = Arc::clone(&self.db);
        let tenant_id = self.tenant_id.clone();
        let person_id = self.person_id.clone();
        Ok(tokio::task::spawn_blocking(move || {
            db.lock().remember(RememberInput {
                tenant_id,
                person_id,
                ingestion_key,
                kind,
                text,
                captured_at,
                recorded_at: chrono::Utc::now().timestamp(),
                claim,
                feature_flag: None,
            })
        })
        .await??)
    }

    pub async fn search(&self, query: String, limit: u32) -> anyhow::Result<RetrievalPack> {
        let db = Arc::clone(&self.db);
        let tenant_id = self.tenant_id.clone();
        let person_id = self.person_id.clone();
        Ok(tokio::task::spawn_blocking(move || {
            db.lock().search(SearchInput {
                tenant_id,
                person_id,
                query,
                limit,
                query_embedding: None,
                as_of: None,
                enabled_features: Vec::new(),
            })
        })
        .await??)
    }

    pub async fn get(&self, kind: &str, id: String) -> anyhow::Result<RetrievalItem> {
        let target = match kind {
            "source" => EmbeddingTarget::Source(SourceId::new(id)?),
            "evidence" => EmbeddingTarget::Evidence(zkr::EvidenceId::new(id)?),
            "claim" => EmbeddingTarget::Claim(ClaimId::new(id)?),
            _ => anyhow::bail!("kind must be source, evidence, or claim"),
        };
        let db = Arc::clone(&self.db);
        let tenant_id = self.tenant_id.clone();
        let person_id = self.person_id.clone();
        Ok(tokio::task::spawn_blocking(move || {
            db.lock().get(GetInput {
                tenant_id,
                person_id,
                target,
            })
        })
        .await??)
    }

    pub async fn correct(
        &self,
        claim_id: String,
        text: String,
        value: String,
        valid_at: i64,
    ) -> anyhow::Result<zkr::Corrected> {
        let claim_id = ClaimId::new(claim_id).map_err(|e| anyhow::anyhow!(e))?;
        let db = Arc::clone(&self.db);
        let tenant_id = self.tenant_id.clone();
        let person_id = self.person_id.clone();
        Ok(tokio::task::spawn_blocking(move || {
            db.lock().correct(CorrectInput {
                tenant_id,
                person_id,
                claim_id,
                text,
                value,
                valid_at,
                recorded_at: chrono::Utc::now().timestamp(),
            })
        })
        .await??)
    }

    pub async fn delete(&self, source_id: String) -> anyhow::Result<zkr::Deleted> {
        let source_id = SourceId::new(source_id).map_err(|e| anyhow::anyhow!(e))?;
        let db = Arc::clone(&self.db);
        let tenant_id = self.tenant_id.clone();
        let person_id = self.person_id.clone();
        Ok(tokio::task::spawn_blocking(move || {
            db.lock().delete_source(DeleteInput {
                tenant_id,
                person_id,
                source_id,
                deleted_at: chrono::Utc::now().timestamp(),
            })
        })
        .await??)
    }

    pub async fn profiles(&self, limit: u32) -> anyhow::Result<Vec<zkr::ProfileEntry>> {
        let db = Arc::clone(&self.db);
        let tenant_id = self.tenant_id.clone();
        let person_id = self.person_id.clone();
        Ok(tokio::task::spawn_blocking(move || {
            db.lock().profiles(ProfilesInput {
                tenant_id,
                person_id,
                limit,
            })
        })
        .await??)
    }

    pub async fn capture_turn(
        &self,
        chat_id: &str,
        message_id: &str,
        user: &str,
        assistant: &str,
        captured_at: i64,
    ) -> anyhow::Result<()> {
        self.remember(
            user.to_string(),
            SourceKind::Conversation,
            Some(format!("{chat_id}:{message_id}:user")),
            None,
            captured_at,
        )
        .await?;
        self.remember(
            assistant.to_string(),
            SourceKind::Conversation,
            Some(format!("{chat_id}:{message_id}:assistant")),
            None,
            captured_at,
        )
        .await?;
        Ok(())
    }

    pub async fn context(&self, query: &str, limit: u32) -> anyhow::Result<Option<String>> {
        let pack = self.search(query.to_string(), limit).await?;
        if pack.items.is_empty() {
            return Ok(None);
        }
        let mut output = String::from("<zkr_memory>\n");
        for item in pack.items {
            let memory = serde_json::to_string(&item.memory)?;
            let evidence = item
                .evidence_ids
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(",");
            output.push_str(&format!(
                "- [{memory}] {} (evidence: {evidence}, relevance: {})\n",
                item.excerpt, item.relevance_basis_points
            ));
        }
        output.push_str("</zkr_memory>");
        Ok(Some(output))
    }

    pub async fn record_reflection(
        &self,
        context: &str,
        action: &str,
        outcome: &str,
        lesson: &str,
    ) -> anyhow::Result<Remembered> {
        let text =
            format!("Context: {context}\nAction: {action}\nOutcome: {outcome}\nLesson: {lesson}");
        let claim = ClaimInput {
            subject: context.to_string(),
            predicate: "improved".to_string(),
            value: lesson.to_string(),
            kind: ClaimKind::Skill,
            valid_from: chrono::Utc::now().timestamp(),
            tier: zkr::MemoryTier::LongTerm,
            processing_state: zkr::MemoryProcessingState::Processed,
        };
        self.remember(
            text,
            SourceKind::Integration,
            Some(format!("self_improve:{}", nanos())),
            Some(claim),
            chrono::Utc::now().timestamp(),
        )
        .await
    }

    pub async fn augment_prompt(&self, query: &str, base: &str) -> anyhow::Result<String> {
        let mut lessons = Vec::new();
        let mut seen = std::collections::HashSet::new();

        for q in ["self improve lessons", query] {
            let pack = self.search(q.to_string(), 5).await?;
            for item in pack.items {
                let text = item.excerpt.trim().to_string();
                if seen.insert(text.clone()) {
                    lessons.push(text);
                }
            }
        }

        if lessons.is_empty() {
            return Ok(base.to_string());
        }

        let mut augmented = base.to_string();
        augmented.push_str("\n\n<lessons_learned>\n");
        for (i, lesson) in lessons.iter().take(5).enumerate() {
            augmented.push_str(&format!("{}. {lesson}\n", i + 1));
        }
        augmented.push_str("</lessons_learned>");
        Ok(augmented)
    }
}

fn nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

pub fn source_kind(value: &str) -> anyhow::Result<SourceKind> {
    match value {
        "conversation" => Ok(SourceKind::Conversation),
        "screen" => Ok(SourceKind::Screen),
        "audio" => Ok(SourceKind::Audio),
        "document" => Ok(SourceKind::Document),
        "integration" => Ok(SourceKind::Integration),
        "user_correction" => Ok(SourceKind::UserCorrection),
        _ => anyhow::bail!("invalid source kind"),
    }
}

pub fn claim_kind(value: &str) -> anyhow::Result<ClaimKind> {
    match value {
        "fact" => Ok(ClaimKind::Fact),
        "profile_fact" => Ok(ClaimKind::ProfileFact),
        "preference" => Ok(ClaimKind::Preference),
        "task" => Ok(ClaimKind::Task),
        "skill" => Ok(ClaimKind::Skill),
        "recommendation" => Ok(ClaimKind::Recommendation),
        _ => anyhow::bail!("invalid claim kind"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn remembers_and_retrieves_cited_source() {
        let dir = tempfile::tempdir().unwrap();
        let store = ZkrStore::open(&dir.path().join("memory.db"), "test", "person").unwrap();
        let remembered = store
            .remember(
                "I prefer concise plans".to_string(),
                SourceKind::Conversation,
                Some("turn-1".to_string()),
                None,
                100,
            )
            .await
            .unwrap();
        let pack = store.search("concise plans".to_string(), 5).await.unwrap();
        assert_eq!(pack.items.len(), 1);
        assert_eq!(pack.items[0].evidence_ids, vec![remembered.evidence_id]);
    }

    #[tokio::test]
    async fn ingestion_key_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let store = ZkrStore::open(&dir.path().join("memory.db"), "test", "person").unwrap();
        let first = store
            .remember(
                "same turn".to_string(),
                SourceKind::Conversation,
                Some("turn-1".to_string()),
                None,
                100,
            )
            .await
            .unwrap();
        let second = store
            .remember(
                "same turn".to_string(),
                SourceKind::Conversation,
                Some("turn-1".to_string()),
                None,
                100,
            )
            .await
            .unwrap();
        assert_eq!(first.source_id, second.source_id);
    }
}
