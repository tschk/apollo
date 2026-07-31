//! SurrealDB-backed memory storage using the local RocksDB engine.

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use surrealdb::engine::local::RocksDb;
use surrealdb::Surreal;
use tokio::sync::OnceCell;

use super::traits::*;

#[derive(Clone)]
pub struct SurrealMemory {
    path: PathBuf,
    cell: Arc<OnceCell<Surreal<surrealdb::engine::local::Db>>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MemoryRow {
    namespace: String,
    key: String,
    value: String,
    metadata: Option<serde_json::Value>,
    created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ConversationRow {
    chat_id: String,
    sender_id: String,
    role: String,
    content: String,
    seq: i64,
    created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StickerRow {
    sticker_id: String,
    file_id: String,
    description: String,
    analyzed_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct EmbeddingRow {
    namespace: String,
    key: String,
    vector: Vec<f32>,
    text: String,
    created_at: String,
    #[serde(default)]
    dim: Option<i64>,
    #[serde(default)]
    model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FileIndexRow {
    path: String,
    hash: String,
    last_indexed: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ChunkRow {
    file_path: String,
    start_line: u32,
    end_line: u32,
    content: String,
    embedding: Option<Vec<f32>>,
    created_at: String,
}

impl SurrealMemory {
    pub async fn db(&self) -> Result<Surreal<surrealdb::engine::local::Db>> {
        let db = self
            .cell
            .get_or_try_init(|| async {
                let db = Surreal::new::<RocksDb>(self.path.as_path()).await?;
                db.use_ns("claw").use_db("memory").await?;
                db.query(SCHEMA_SQL).await?;
                Ok::<_, anyhow::Error>(db)
            })
            .await?;
        Ok(db.clone())
    }

    pub async fn new<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                tokio::fs::create_dir_all(parent).await?;
            }
        }
        Ok(Self {
            path,
            cell: Arc::new(OnceCell::new()),
        })
    }

    fn memory_id(namespace: &str, key: &str) -> String {
        format!("{namespace}::{key}")
    }
}

const SCHEMA_SQL: &str = r#"
    DEFINE TABLE IF NOT EXISTS memories SCHEMALESS;
    DEFINE FIELD IF NOT EXISTS namespace ON memories TYPE string;
    DEFINE FIELD IF NOT EXISTS key ON memories TYPE string;
    DEFINE FIELD IF NOT EXISTS value ON memories TYPE string;
    DEFINE FIELD IF NOT EXISTS metadata ON memories TYPE option<object>;
    DEFINE FIELD IF NOT EXISTS created_at ON memories TYPE string;
    DEFINE INDEX IF NOT EXISTS memory_lookup_idx ON memories FIELDS namespace, key UNIQUE;
    DEFINE INDEX IF NOT EXISTS memory_namespace_idx ON memories FIELDS namespace;

    DEFINE TABLE IF NOT EXISTS conversations SCHEMALESS;
    DEFINE FIELD IF NOT EXISTS chat_id ON conversations TYPE string;
    DEFINE FIELD IF NOT EXISTS sender_id ON conversations TYPE string;
    DEFINE FIELD IF NOT EXISTS role ON conversations TYPE string;
    DEFINE FIELD IF NOT EXISTS content ON conversations TYPE string;
    DEFINE FIELD IF NOT EXISTS seq ON conversations TYPE int;
    DEFINE FIELD IF NOT EXISTS created_at ON conversations TYPE string;
    DEFINE INDEX IF NOT EXISTS conversation_chat_idx ON conversations FIELDS chat_id, seq;

    DEFINE TABLE IF NOT EXISTS sticker_cache SCHEMALESS;
    DEFINE FIELD IF NOT EXISTS sticker_id ON sticker_cache TYPE string;
    DEFINE FIELD IF NOT EXISTS file_id ON sticker_cache TYPE string;
    DEFINE FIELD IF NOT EXISTS description ON sticker_cache TYPE string;
    DEFINE FIELD IF NOT EXISTS analyzed_at ON sticker_cache TYPE string;
    DEFINE INDEX IF NOT EXISTS sticker_id_idx ON sticker_cache FIELDS sticker_id UNIQUE;

    DEFINE TABLE IF NOT EXISTS embeddings SCHEMALESS;
    DEFINE FIELD IF NOT EXISTS namespace ON embeddings TYPE string;
    DEFINE FIELD IF NOT EXISTS key ON embeddings TYPE string;
    DEFINE FIELD IF NOT EXISTS vector ON embeddings TYPE array;
    DEFINE FIELD IF NOT EXISTS text ON embeddings TYPE string;
    DEFINE FIELD IF NOT EXISTS created_at ON embeddings TYPE string;
    DEFINE FIELD IF NOT EXISTS dim ON embeddings TYPE option<int>;
    DEFINE FIELD IF NOT EXISTS model ON embeddings TYPE option<string>;
    DEFINE INDEX IF NOT EXISTS embedding_lookup_idx ON embeddings FIELDS namespace, key UNIQUE;
    DEFINE INDEX IF NOT EXISTS embedding_namespace_idx ON embeddings FIELDS namespace;
    DEFINE INDEX IF NOT EXISTS embedding_namespace_dim_idx ON embeddings FIELDS namespace, dim;
    -- No MTREE index on `vector`, and this is now settled by measurement, not
    -- by assumption. See `bench_mtree_feasibility` and
    -- `bench_search_embeddings_scaling` in this file; numbers from a release
    -- build on macOS arm64 at dim 1536.
    --
    -- `DEFINE INDEX ... MTREE DIMENSION 1536 DIST COSINE` is accepted, but:
    --   1. It locks the whole table to that one dimension. Storing a 3-dim
    --      vector afterwards fails with "Incorrect vector dimension (3).
    --      Expected a vector of 1536 dimension." So "one index per distinct
    --      dimension" is not possible on a shared table at all — it would
    --      need dimension-partitioned tables, not just extra indexes.
    --   2. It is ~25x more expensive to write: 29.76ms/row indexed versus
    --      ~1.2ms/row unindexed.
    --   3. It does not even pay off on reads. At 2000 rows,
    --      `vector <|10,COSINE|> $q` had a 2558ms median against 961ms for
    --      the brute-force scan — 2.7x slower.
    --
    -- The scan itself is not cheap (~0.27ms per candidate row, so ~30ms at
    -- 100 rows and ~2.5s at 10k), but the cost is materializing a
    -- 1536-element array as SurrealDB `Value`s per row, not the cosine
    -- arithmetic. Scoring in-engine with
    -- `vector::similarity::cosine(...) ORDER BY score LIMIT k` was measured
    -- too and is ~1.6x slower still, because it walks the same arrays.
    -- Anything that actually fixes this has to make a row cheaper to read
    -- (a compact encoding rather than an array of `Value`s), which MTREE
    -- does not do.

    DEFINE TABLE IF NOT EXISTS files SCHEMALESS;
    DEFINE FIELD IF NOT EXISTS path ON files TYPE string;
    DEFINE FIELD IF NOT EXISTS hash ON files TYPE string;
    DEFINE FIELD IF NOT EXISTS last_indexed ON files TYPE string;
    DEFINE INDEX IF NOT EXISTS file_path_idx ON files FIELDS path UNIQUE;

    DEFINE TABLE IF NOT EXISTS chunks SCHEMALESS;
    DEFINE FIELD IF NOT EXISTS file_path ON chunks TYPE string;
    DEFINE FIELD IF NOT EXISTS start_line ON chunks TYPE int;
    DEFINE FIELD IF NOT EXISTS end_line ON chunks TYPE int;
    DEFINE FIELD IF NOT EXISTS content ON chunks TYPE string;
    DEFINE FIELD IF NOT EXISTS embedding ON chunks TYPE option<array>;
    DEFINE FIELD IF NOT EXISTS created_at ON chunks TYPE string;
    DEFINE INDEX IF NOT EXISTS chunk_file_idx ON chunks FIELDS file_path;

    DEFINE TABLE IF NOT EXISTS cron_jobs SCHEMALESS;
    DEFINE FIELD IF NOT EXISTS name ON cron_jobs TYPE string;
    DEFINE FIELD IF NOT EXISTS schedule ON cron_jobs TYPE string;
    DEFINE FIELD IF NOT EXISTS task ON cron_jobs TYPE string;
    DEFINE FIELD IF NOT EXISTS channel ON cron_jobs TYPE string;
    DEFINE FIELD IF NOT EXISTS model ON cron_jobs TYPE string;
    DEFINE FIELD IF NOT EXISTS enabled ON cron_jobs TYPE bool;
    DEFINE FIELD IF NOT EXISTS last_run ON cron_jobs TYPE option<string>;
    DEFINE FIELD IF NOT EXISTS next_run ON cron_jobs TYPE option<string>;
    DEFINE INDEX IF NOT EXISTS cron_name_idx ON cron_jobs FIELDS name UNIQUE;

    DEFINE TABLE IF NOT EXISTS memory_nodes SCHEMALESS;
    DEFINE FIELD IF NOT EXISTS id ON memory_nodes TYPE string;
    DEFINE FIELD IF NOT EXISTS kind ON memory_nodes TYPE string;
    DEFINE FIELD IF NOT EXISTS text ON memory_nodes TYPE string;
    DEFINE FIELD IF NOT EXISTS confidence ON memory_nodes TYPE float;
    DEFINE FIELD IF NOT EXISTS status ON memory_nodes TYPE string;
    DEFINE FIELD IF NOT EXISTS created_at ON memory_nodes TYPE string;
    DEFINE INDEX IF NOT EXISTS memory_node_id_idx ON memory_nodes FIELDS id UNIQUE;

    DEFINE TABLE IF NOT EXISTS memory_edges SCHEMALESS;
    DEFINE FIELD IF NOT EXISTS from_id ON memory_edges TYPE string;
    DEFINE FIELD IF NOT EXISTS to_id ON memory_edges TYPE string;
    DEFINE FIELD IF NOT EXISTS rel ON memory_edges TYPE string;
    DEFINE FIELD IF NOT EXISTS created_at ON memory_edges TYPE string;

    DEFINE ANALYZER IF NOT EXISTS memory_analyzer TOKENIZERS blank, class FILTERS lowercase, snowball(english);
    DEFINE INDEX IF NOT EXISTS memory_fts_idx ON memories FIELDS value
        SEARCH ANALYZER memory_analyzer BM25;
"#;

fn parse_timestamp(value: &str) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .unwrap_or_else(|_| chrono::Utc::now())
}

#[async_trait]
impl MemoryBackend for SurrealMemory {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    async fn store(
        &self,
        namespace: &str,
        key: &str,
        value: &str,
        metadata: Option<serde_json::Value>,
    ) -> Result<()> {
        let created_at = chrono::Utc::now().to_rfc3339();
        let row = MemoryRow {
            namespace: namespace.to_string(),
            key: key.to_string(),
            value: value.to_string(),
            metadata,
            created_at,
        };
        let _: Option<MemoryRow> = self
            .db()
            .await?
            .upsert(("memories", Self::memory_id(namespace, key)))
            .content(row)
            .await?;
        Ok(())
    }

    async fn recall(&self, namespace: &str, key: &str) -> Result<Option<MemoryEntry>> {
        let row: Option<MemoryRow> = self
            .db()
            .await?
            .select(("memories", Self::memory_id(namespace, key)))
            .await?;
        Ok(row.map(|entry| MemoryEntry {
            key: entry.key,
            value: entry.value,
            metadata: entry.metadata,
            created_at: parse_timestamp(&entry.created_at),
        }))
    }

    async fn search(&self, namespace: &str, query: &str, limit: usize) -> Result<Vec<MemoryEntry>> {
        // Try full-text search with BM25 ranking first
        let mut result = self
            .db()
            .await?
            .query(
                "SELECT *, search::score(1) AS score
                 FROM memories
                 WHERE namespace = $namespace
                   AND value @1@ $query
                 ORDER BY score DESC
                 LIMIT $limit",
            )
            .bind(("namespace", namespace.to_string()))
            .bind(("query", query.to_string()))
            .bind(("limit", limit as i64))
            .await?;
        let rows: Vec<MemoryRow> = result.take(0)?;

        if !rows.is_empty() {
            return Ok(rows
                .into_iter()
                .map(|entry| MemoryEntry {
                    key: entry.key,
                    value: entry.value,
                    metadata: entry.metadata,
                    created_at: parse_timestamp(&entry.created_at),
                })
                .collect());
        }

        // Fallback to CONTAINS for partial matches
        let query_lower = query.to_lowercase();
        let mut result = self.db().await?
            .query(
                "SELECT * FROM memories
                 WHERE namespace = $namespace
                   AND (string::lowercase(key) CONTAINS $query OR string::lowercase(value) CONTAINS $query)
                 ORDER BY created_at DESC
                 LIMIT $limit"
            )
            .bind(("namespace", namespace.to_string()))
            .bind(("query", query_lower))
            .bind(("limit", limit as i64))
            .await?;
        let rows: Vec<MemoryRow> = result.take(0)?;
        Ok(rows
            .into_iter()
            .map(|entry| MemoryEntry {
                key: entry.key,
                value: entry.value,
                metadata: entry.metadata,
                created_at: parse_timestamp(&entry.created_at),
            })
            .collect())
    }

    async fn forget(&self, namespace: &str, key: &str) -> Result<()> {
        let _: Option<MemoryRow> = self
            .db()
            .await?
            .delete(("memories", Self::memory_id(namespace, key)))
            .await?;
        Ok(())
    }

    async fn list(&self, namespace: &str) -> Result<Vec<MemoryEntry>> {
        let mut result = self.db().await?
            .query("SELECT key, value, metadata, created_at FROM memories WHERE namespace = $namespace ORDER BY created_at DESC")
            .bind(("namespace", namespace.to_string()))
            .await?;
        let rows: Vec<MemoryRow> = result.take(0)?;
        Ok(rows
            .into_iter()
            .map(|entry| MemoryEntry {
                key: entry.key,
                value: entry.value,
                metadata: entry.metadata,
                created_at: parse_timestamp(&entry.created_at),
            })
            .collect())
    }

    async fn store_conversation(
        &self,
        chat_id: &str,
        sender_id: &str,
        role: &str,
        content: &str,
    ) -> Result<()> {
        let now = chrono::Utc::now();
        let row = ConversationRow {
            chat_id: chat_id.to_string(),
            sender_id: sender_id.to_string(),
            role: role.to_string(),
            content: content.to_string(),
            seq: now.timestamp_millis(),
            created_at: now.to_rfc3339(),
        };
        let _: Option<ConversationRow> = self
            .db()
            .await?
            .create("conversations")
            .content(row)
            .await?;
        Ok(())
    }

    async fn store_conversation_batch(&self, entries: &[(&str, &str, &str, &str)]) -> Result<()> {
        for (offset, (chat_id, sender_id, role, content)) in entries.iter().enumerate() {
            let now = chrono::Utc::now();
            let row = ConversationRow {
                chat_id: (*chat_id).to_string(),
                sender_id: (*sender_id).to_string(),
                role: (*role).to_string(),
                content: (*content).to_string(),
                seq: now.timestamp_millis() + offset as i64,
                created_at: now.to_rfc3339(),
            };
            let _: Option<ConversationRow> = self
                .db()
                .await?
                .create("conversations")
                .content(row)
                .await?;
        }
        Ok(())
    }

    async fn get_conversation_history(
        &self,
        chat_id: &str,
        limit: usize,
    ) -> Result<Vec<(String, String)>> {
        let mut result = self
            .db()
            .await?
            .query(
                "SELECT * FROM conversations
                 WHERE chat_id = $chat_id
                 ORDER BY seq DESC
                 LIMIT $limit",
            )
            .bind(("chat_id", chat_id.to_string()))
            .bind(("limit", limit as i64))
            .await?;
        let mut rows: Vec<ConversationRow> = result.take(0)?;
        rows.reverse();
        Ok(rows
            .into_iter()
            .map(|row| (row.role, row.content))
            .collect())
    }

    async fn clear_conversation(&self, chat_id: &str) -> Result<()> {
        self.db()
            .await?
            .query("DELETE FROM conversations WHERE chat_id = $chat_id")
            .bind(("chat_id", chat_id.to_string()))
            .await?
            .check()?;
        Ok(())
    }

    async fn search_conversations(
        &self,
        query: &str,
        limit: usize,
        chat_id: Option<&str>,
    ) -> Result<Vec<ConversationSearchHit>> {
        let query = query.to_lowercase();
        let mut result = if let Some(chat_id) = chat_id {
            self.db()
                .await?
                .query(
                    "SELECT * FROM conversations
                     WHERE chat_id = $chat_id
                       AND string::lowercase(content) CONTAINS $query
                     ORDER BY seq DESC
                     LIMIT $limit",
                )
                .bind(("chat_id", chat_id.to_string()))
                .bind(("query", query))
                .bind(("limit", limit as i64))
                .await?
        } else {
            self.db()
                .await?
                .query(
                    "SELECT * FROM conversations
                     WHERE string::lowercase(content) CONTAINS $query
                     ORDER BY seq DESC
                     LIMIT $limit",
                )
                .bind(("query", query))
                .bind(("limit", limit as i64))
                .await?
        };
        let rows: Vec<ConversationRow> = result.take(0)?;
        Ok(rows
            .into_iter()
            .map(|row| ConversationSearchHit {
                chat_id: row.chat_id,
                role: row.role,
                content: row.content,
                created_at: parse_timestamp(&row.created_at),
            })
            .collect())
    }

    async fn get_sticker_cache(&self, sticker_id: &str) -> Result<Option<String>> {
        let row: Option<StickerRow> = self
            .db()
            .await?
            .select(("sticker_cache", sticker_id))
            .await?;
        Ok(row.map(|entry| entry.description))
    }

    async fn store_sticker_cache(
        &self,
        sticker_id: &str,
        file_id: &str,
        description: &str,
    ) -> Result<()> {
        let row = StickerRow {
            sticker_id: sticker_id.to_string(),
            file_id: file_id.to_string(),
            description: description.to_string(),
            analyzed_at: chrono::Utc::now().to_rfc3339(),
        };
        let _: Option<StickerRow> = self
            .db()
            .await?
            .upsert(("sticker_cache", sticker_id))
            .content(row)
            .await?;
        Ok(())
    }

    // ── Embeddings ──

    async fn store_embedding(
        &self,
        namespace: &str,
        key: &str,
        vector: &[f32],
        text: &str,
        model: &str,
    ) -> Result<()> {
        let row = EmbeddingRow {
            namespace: namespace.to_string(),
            key: key.to_string(),
            dim: Some(vector.len() as i64),
            model: Some(model.to_string()),
            vector: vector.to_vec(),
            text: text.to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        let id = Self::memory_id(namespace, key);
        let _: Option<EmbeddingRow> = self
            .db()
            .await?
            .upsert(("embeddings", &id))
            .content(row)
            .await?;
        Ok(())
    }

    async fn search_embeddings(
        &self,
        namespace: &str,
        query_vector: &[f32],
        limit: usize,
    ) -> Result<Vec<EmbeddingEntry>> {
        if query_vector.is_empty() {
            return Ok(Vec::new());
        }
        let dim = query_vector.len() as i64;

        // Brute-force cosine scan over the namespace's embeddings of the
        // matching dimension.
        //
        // SurrealDB's KNN operator is not used here. `vector <|K,COSINE|> $v`
        // only produces matches when an MTREE index is defined on the field,
        // and there deliberately is none (see SCHEMA_SQL) — the earlier form
        // `vector <| $v |>` did not even parse, so the KNN path silently
        // errored and every search already fell through to this scan.
        //
        // The `dim` filter is what keeps the comparison meaningful: vectors
        // produced by a different embedding model have a different length and
        // are not comparable, so they must never enter the candidate set.
        // Rows written before `dim` was recorded have no dimension and are
        // therefore excluded; they are picked up again on their next write.
        let mut result = self
            .db()
            .await?
            .query("SELECT * FROM embeddings WHERE namespace = $namespace AND dim = $dim")
            .bind(("namespace", namespace.to_string()))
            .bind(("dim", dim))
            .await?;
        let rows: Vec<EmbeddingRow> = result.take(0)?;

        // `cosine_similarity` returns None for mismatched lengths rather than
        // scoring them, so a stale row that slipped past the `dim` filter is
        // dropped instead of ranked.
        let mut scored: Vec<(f32, EmbeddingRow)> = rows
            .into_iter()
            .filter_map(|row| cosine_similarity(query_vector, &row.vector).map(|sim| (sim, row)))
            .collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit);

        Ok(scored
            .into_iter()
            .map(|(_, row)| EmbeddingEntry {
                namespace: row.namespace,
                key: row.key,
                vector: row.vector,
                text: row.text,
                created_at: parse_timestamp(&row.created_at),
            })
            .collect())
    }

    // ── File indexing ──

    async fn store_file_index(&self, path: &str, hash: &str) -> Result<()> {
        let row = FileIndexRow {
            path: path.to_string(),
            hash: hash.to_string(),
            last_indexed: chrono::Utc::now().to_rfc3339(),
        };
        // Use path hash as record ID to avoid special chars
        let id = format!("{:x}", md5_hash(path));
        let _: Option<FileIndexRow> = self.db().await?.upsert(("files", &id)).content(row).await?;
        Ok(())
    }

    async fn get_file_index(&self, path: &str) -> Result<Option<FileIndex>> {
        let id = format!("{:x}", md5_hash(path));
        let row: Option<FileIndexRow> = self.db().await?.select(("files", &id)).await?;
        Ok(row.map(|r| FileIndex {
            path: r.path,
            hash: r.hash,
            last_indexed: parse_timestamp(&r.last_indexed),
        }))
    }

    // ── Code chunks ──

    async fn store_chunk(
        &self,
        file_path: &str,
        start_line: u32,
        end_line: u32,
        content: &str,
        embedding: Option<&[f32]>,
    ) -> Result<()> {
        let row = ChunkRow {
            file_path: file_path.to_string(),
            start_line,
            end_line,
            content: content.to_string(),
            embedding: embedding.map(|e| e.to_vec()),
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        let _: Option<ChunkRow> = self.db().await?.create("chunks").content(row).await?;
        Ok(())
    }

    async fn get_chunks_for_file(&self, file_path: &str) -> Result<Vec<Chunk>> {
        let mut result = self
            .db()
            .await?
            .query("SELECT * FROM chunks WHERE file_path = $file_path ORDER BY start_line ASC")
            .bind(("file_path", file_path.to_string()))
            .await?;
        let rows: Vec<ChunkRow> = result.take(0)?;
        Ok(rows
            .into_iter()
            .map(|r| Chunk {
                file_path: r.file_path,
                start_line: r.start_line,
                end_line: r.end_line,
                content: r.content,
                embedding: r.embedding,
                created_at: parse_timestamp(&r.created_at),
            })
            .collect())
    }

    async fn delete_chunks_for_file(&self, file_path: &str) -> Result<()> {
        self.db()
            .await?
            .query("DELETE FROM chunks WHERE file_path = $file_path")
            .bind(("file_path", file_path.to_string()))
            .await?;
        Ok(())
    }
}

/// Cosine similarity, or `None` when the two vectors are not comparable.
///
/// Vectors of different lengths come from different embedding models and have
/// no meaningful similarity; returning `None` forces callers to drop the pair
/// rather than treat a fabricated score as a real one.
fn cosine_similarity(a: &[f32], b: &[f32]) -> Option<f32> {
    if a.len() != b.len() || a.is_empty() {
        return None;
    }

    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let ma: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let mb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();

    if ma == 0.0 || mb == 0.0 {
        return Some(0.0);
    }

    Some(dot / (ma * mb))
}

/// Simple hash for file path → record ID
fn md5_hash(input: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    input.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_md5_hash_deterministic() {
        let h1 = md5_hash("test-path");
        let h2 = md5_hash("test-path");
        assert_eq!(h1, h2);
    }

    #[test]
    fn test_md5_hash_different_inputs() {
        let h1 = md5_hash("path-a");
        let h2 = md5_hash("path-b");
        assert_ne!(h1, h2);
    }

    #[tokio::test]
    async fn test_surreal_store_and_recall() {
        let dir = tempfile::tempdir().unwrap();
        let mem = SurrealMemory::new(dir.path()).await.unwrap();

        mem.store("test", "greeting", "hello world", None)
            .await
            .unwrap();
        let val = mem.recall("test", "greeting").await.unwrap();
        assert!(val.is_some());
        assert_eq!(val.unwrap().value, "hello world");
    }

    #[tokio::test]
    async fn test_surreal_recall_missing() {
        let dir = tempfile::tempdir().unwrap();
        let mem = SurrealMemory::new(dir.path()).await.unwrap();

        let val = mem.recall("test", "missing-key").await.unwrap();
        assert!(val.is_none());
    }

    #[tokio::test]
    async fn test_surreal_search() {
        let dir = tempfile::tempdir().unwrap();
        let mem = SurrealMemory::new(dir.path()).await.unwrap();

        mem.store("ns", "k1", "the quick brown fox", None)
            .await
            .unwrap();
        mem.store("ns", "k2", "lazy dog sleeps", None)
            .await
            .unwrap();
        mem.store("ns", "k3", "fox runs fast", None).await.unwrap();

        let results: Vec<MemoryEntry> = mem.search("ns", "fox", 10).await.unwrap();
        assert!(!results.is_empty());
        assert!(results.iter().any(|e| e.value.contains("fox")));
    }

    #[tokio::test]
    async fn test_surreal_delete() {
        let dir = tempfile::tempdir().unwrap();
        let mem = SurrealMemory::new(dir.path()).await.unwrap();

        mem.store("ns", "del-key", "to delete", None).await.unwrap();
        assert!(mem.recall("ns", "del-key").await.unwrap().is_some());

        mem.forget("ns", "del-key").await.unwrap();
        assert!(mem.recall("ns", "del-key").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_surreal_conversation_history() {
        let dir = tempfile::tempdir().unwrap();
        let mem = SurrealMemory::new(dir.path()).await.unwrap();

        mem.store_conversation("chat-1", "user-1", "user", "Hello")
            .await
            .unwrap();
        // Small delay to ensure different seq timestamps
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        mem.store_conversation("chat-1", "assistant", "assistant", "Hi there")
            .await
            .unwrap();

        let history: Vec<(String, String)> =
            mem.get_conversation_history("chat-1", 10).await.unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].1, "Hello");
        assert_eq!(history[1].1, "Hi there");
    }

    #[tokio::test]
    async fn test_surreal_clear_conversation_is_scoped_to_one_chat() {
        let dir = tempfile::tempdir().unwrap();
        let mem = SurrealMemory::new(dir.path()).await.unwrap();

        mem.store_conversation("chat-1", "user-1", "user", "keep me out")
            .await
            .unwrap();
        mem.store_conversation("chat-2", "user-1", "user", "keep me")
            .await
            .unwrap();

        mem.clear_conversation("chat-1").await.unwrap();

        assert!(mem
            .get_conversation_history("chat-1", 10)
            .await
            .unwrap()
            .is_empty());
        let survivors = mem.get_conversation_history("chat-2", 10).await.unwrap();
        assert_eq!(survivors.len(), 1);
        assert_eq!(survivors[0].1, "keep me");

        // Clearing an empty chat is not an error.
        mem.clear_conversation("chat-1").await.unwrap();
    }

    #[tokio::test]
    async fn test_surreal_embeddings() {
        let dir = tempfile::tempdir().unwrap();
        let mem = SurrealMemory::new(dir.path()).await.unwrap();

        let vec1 = vec![1.0, 0.0, 0.0];
        let vec2 = vec![0.0, 1.0, 0.0];
        let vec3 = vec![0.9, 0.1, 0.0];

        mem.store_embedding("ns", "e1", &vec1, "first", "test-model")
            .await
            .unwrap();
        mem.store_embedding("ns", "e2", &vec2, "second", "test-model")
            .await
            .unwrap();
        mem.store_embedding("ns", "e3", &vec3, "third", "test-model")
            .await
            .unwrap();

        let results = mem.search_embeddings("ns", &vec1, 2).await.unwrap();
        assert!(!results.is_empty());
        // The closest to [1,0,0] should be e1 or e3
        assert!(results[0].key == "e1" || results[0].key == "e3");
    }

    #[test]
    fn test_cosine_similarity_rejects_mismatched_lengths() {
        assert_eq!(cosine_similarity(&[1.0, 0.0], &[1.0, 0.0, 0.0]), None);
        assert_eq!(cosine_similarity(&[], &[]), None);
        assert_eq!(cosine_similarity(&[1.0, 0.0], &[1.0, 0.0]), Some(1.0));
        assert_eq!(cosine_similarity(&[0.0, 0.0], &[1.0, 0.0]), Some(0.0));
    }

    #[tokio::test]
    async fn test_search_embeddings_excludes_other_dimensions() {
        let dir = tempfile::tempdir().unwrap();
        let mem = SurrealMemory::new(dir.path()).await.unwrap();

        // Simulates a provider switch: 3-dim rows from the old model coexist
        // with 8-dim rows from the new one.
        let old_dim = vec![1.0f32, 0.0, 0.0];
        let new_dim = vec![0.25f32; 8];
        mem.store_embedding("ns", "old", &old_dim, "old model", "old-model")
            .await
            .unwrap();
        mem.store_embedding("ns", "new", &new_dim, "new model", "new-model")
            .await
            .unwrap();

        let results = mem.search_embeddings("ns", &new_dim, 10).await.unwrap();
        assert!(
            results.iter().all(|e| e.key != "old"),
            "a vector of a different dimension must never be returned"
        );
        assert!(results.iter().any(|e| e.key == "new"));

        // And symmetrically for the old dimension.
        let results = mem.search_embeddings("ns", &old_dim, 10).await.unwrap();
        assert!(results.iter().all(|e| e.key != "new"));
        assert!(results.iter().any(|e| e.key == "old"));
    }

    #[tokio::test]
    async fn test_search_embeddings_skips_rows_without_dimension() {
        let dir = tempfile::tempdir().unwrap();
        let mem = SurrealMemory::new(dir.path()).await.unwrap();
        let db = mem.db().await.unwrap();

        // A legacy row written before `dim`/`model` were recorded.
        db.query(
            "CREATE embeddings:legacy SET namespace = 'ns', key = 'legacy',
             vector = [1.0, 0.0, 0.0], text = 'legacy', created_at = '2024-01-01T00:00:00Z'",
        )
        .await
        .unwrap();

        let results = mem
            .search_embeddings("ns", &[1.0, 0.0, 0.0], 10)
            .await
            .unwrap();
        assert!(
            results.is_empty(),
            "rows with no recorded dimension must not be returned"
        );
    }

    #[tokio::test]
    async fn test_surreal_file_index() {
        let dir = tempfile::tempdir().unwrap();
        let mem = SurrealMemory::new(dir.path()).await.unwrap();

        mem.store_file_index("/src/main.rs", "abc123")
            .await
            .unwrap();
        let idx = mem.get_file_index("/src/main.rs").await.unwrap();
        assert!(idx.is_some());
        assert_eq!(idx.unwrap().hash, "abc123");

        let missing = mem.get_file_index("/src/nonexistent.rs").await.unwrap();
        assert!(missing.is_none());
    }

    #[tokio::test]
    async fn test_surreal_chunks() {
        let dir = tempfile::tempdir().unwrap();
        let mem = SurrealMemory::new(dir.path()).await.unwrap();

        mem.store_chunk("/src/lib.rs", 1, 10, "fn main() {}", None)
            .await
            .unwrap();
        mem.store_chunk("/src/lib.rs", 11, 20, "fn helper() {}", None)
            .await
            .unwrap();

        let chunks = mem.get_chunks_for_file("/src/lib.rs").await.unwrap();
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].start_line, 1);
        assert_eq!(chunks[1].start_line, 11);

        mem.delete_chunks_for_file("/src/lib.rs").await.unwrap();
        let empty = mem.get_chunks_for_file("/src/lib.rs").await.unwrap();
        assert!(empty.is_empty());
    }

    #[tokio::test]
    async fn test_surreal_sticker_cache() {
        let dir = tempfile::tempdir().unwrap();
        let mem = SurrealMemory::new(dir.path()).await.unwrap();

        mem.store_sticker_cache("stk-1", "file-1", "A happy cat")
            .await
            .unwrap();
        let desc: Option<String> = mem.get_sticker_cache("stk-1").await.unwrap();
        assert_eq!(desc, Some("A happy cat".to_string()));

        let missing: Option<String> = mem.get_sticker_cache("stk-999").await.unwrap();
        assert!(missing.is_none());
    }

    // Reproducible xorshift PRNG so the corpus is realistic (non-zero, spread
    // over the unit sphere) without pulling in a `rand` dev-dependency.
    struct Xorshift(u64);

    impl Xorshift {
        fn next_f32(&mut self) -> f32 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            ((x >> 40) as f32 / 16_777_216.0) * 2.0 - 1.0
        }

        fn unit_vector(&mut self, dim: usize) -> Vec<f32> {
            let mut v: Vec<f32> = (0..dim).map(|_| self.next_f32()).collect();
            let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
            if norm > 0.0 {
                for x in v.iter_mut() {
                    *x /= norm;
                }
            }
            v
        }
    }

    /// Latency of `search_embeddings` against the brute-force cosine scan at
    /// growing corpus sizes. Ignored by default: it writes hundreds of MB and
    /// takes minutes.
    ///
    /// Run with `cargo test --release -p apollo-agent bench_search_embeddings
    /// -- --ignored --nocapture`. Debug numbers are not meaningful.
    #[tokio::test]
    #[ignore = "benchmark: slow, writes a large corpus"]
    async fn bench_search_embeddings_scaling() {
        const DIM: usize = 1536;
        const QUERIES: usize = 100;
        // A 10k-row corpus at 1536 dims costs ~900MB on disk, so 50k needs
        // ~4.5GB free. Override with APOLLO_BENCH_SIZES=100,1000 on a small
        // disk.
        let sizes: Vec<usize> = match std::env::var("APOLLO_BENCH_SIZES") {
            Ok(s) => s.split(',').filter_map(|p| p.trim().parse().ok()).collect(),
            Err(_) => vec![100, 1_000, 10_000, 50_000],
        };

        for size in sizes {
            let dir = tempfile::tempdir().unwrap();
            let mem = SurrealMemory::new(dir.path()).await.unwrap();
            let mut rng = Xorshift(0x2545_F491_4F6C_DD1D);

            let write_start = std::time::Instant::now();
            for i in 0..size {
                let v = rng.unit_vector(DIM);
                mem.store_embedding("bench", &format!("k{i}"), &v, "text", "bench-model")
                    .await
                    .unwrap();
            }
            let write_elapsed = write_start.elapsed();

            let queries: Vec<Vec<f32>> = (0..QUERIES).map(|_| rng.unit_vector(DIM)).collect();
            let mut samples: Vec<u128> = Vec::with_capacity(QUERIES);
            for q in &queries {
                let start = std::time::Instant::now();
                let hits = mem.search_embeddings("bench", q, 10).await.unwrap();
                samples.push(start.elapsed().as_micros());
                assert_eq!(hits.len(), 10.min(size));
            }
            let (median, p95, max) = percentiles(&mut samples);

            println!(
                "size={size:>6} dim={DIM} search_median={median:>8.2}ms search_p95={p95:>8.2}ms \
                 search_max={max:>8.2}ms insert_total={:>8.2}s insert_per_row={:>6.2}ms",
                write_elapsed.as_secs_f64(),
                write_elapsed.as_secs_f64() * 1000.0 / size as f64,
            );

            // Same ranking, but scored inside SurrealDB so only the top `k`
            // rows cross the boundary instead of every candidate vector.
            let db = mem.db().await.unwrap();
            let mut samples: Vec<u128> = Vec::with_capacity(QUERIES);
            for q in &queries {
                let start = std::time::Instant::now();
                let mut result = db
                    .query(
                        "SELECT namespace, key, text, created_at, \
                         vector::similarity::cosine(vector, $q) AS score \
                         FROM embeddings WHERE namespace = $namespace AND dim = $dim \
                         ORDER BY score DESC LIMIT 10",
                    )
                    .bind(("namespace", "bench".to_string()))
                    .bind(("dim", DIM as i64))
                    .bind(("q", q.clone()))
                    .await
                    .unwrap();
                let rows: Vec<serde_json::Value> = result.take(0).unwrap();
                samples.push(start.elapsed().as_micros());
                assert_eq!(rows.len(), 10.min(size));
            }
            let (median, p95, max) = percentiles(&mut samples);
            println!(
                "size={size:>6} dim={DIM}  indb_median={median:>8.2}ms  indb_p95={p95:>8.2}ms  \
                 indb_max={max:>8.2}ms"
            );
        }
    }

    /// Feasibility spike for a per-dimension MTREE index: does SurrealDB
    /// accept a mixed-dimension table once an MTREE of a fixed DIMENSION
    /// exists on `vector`, and does KNN beat the scan?
    #[tokio::test]
    #[ignore = "benchmark: slow, writes a large corpus"]
    async fn bench_mtree_feasibility() {
        const DIM: usize = 1536;
        const SIZE: usize = 2_000;

        let dir = tempfile::tempdir().unwrap();
        let mem = SurrealMemory::new(dir.path()).await.unwrap();
        let mut rng = Xorshift(0x2545_F491_4F6C_DD1D);
        let db = mem.db().await.unwrap();

        let define = db
            .query(format!(
                "DEFINE INDEX IF NOT EXISTS embedding_vector_mtree_{DIM} ON embeddings \
                 FIELDS vector MTREE DIMENSION {DIM} DIST COSINE"
            ))
            .await;
        println!("define_mtree_ok={}", define.is_ok());
        if let Err(e) = &define {
            println!("define_mtree_err={e}");
            return;
        }

        let start = std::time::Instant::now();
        for i in 0..SIZE {
            let v = rng.unit_vector(DIM);
            mem.store_embedding("bench", &format!("k{i}"), &v, "text", "bench-model")
                .await
                .unwrap();
        }
        println!(
            "mtree_insert_total={:.2}s per_row={:.2}ms",
            start.elapsed().as_secs_f64(),
            start.elapsed().as_secs_f64() * 1000.0 / SIZE as f64
        );

        // Does a vector of another dimension survive in the same table?
        let mismatched = mem
            .store_embedding("bench", "other-dim", &[1.0, 0.0, 0.0], "t", "small-model")
            .await;
        println!("mixed_dim_insert_ok={}", mismatched.is_ok());
        if let Err(e) = &mismatched {
            println!("mixed_dim_insert_err={e}");
        }

        let mut knn: Vec<u128> = Vec::new();
        let mut scan: Vec<u128> = Vec::new();
        for _ in 0..20 {
            let q = rng.unit_vector(DIM);

            let start = std::time::Instant::now();
            let mut result = db
                .query(
                    "SELECT namespace, key, text, created_at FROM embeddings \
                     WHERE vector <|10,COSINE|> $q",
                )
                .bind(("q", q.clone()))
                .await
                .unwrap();
            let rows: Vec<serde_json::Value> = result.take(0).unwrap();
            knn.push(start.elapsed().as_micros());
            assert!(!rows.is_empty(), "KNN returned nothing — index not used");

            let start = std::time::Instant::now();
            mem.search_embeddings("bench", &q, 10).await.unwrap();
            scan.push(start.elapsed().as_micros());
        }
        let (km, kp, kx) = percentiles(&mut knn);
        let (sm, sp, sx) = percentiles(&mut scan);
        println!("size={SIZE} knn_median={km:.2}ms knn_p95={kp:.2}ms knn_max={kx:.2}ms");
        println!("size={SIZE} scan_median={sm:.2}ms scan_p95={sp:.2}ms scan_max={sx:.2}ms");
    }

    fn percentiles(samples: &mut [u128]) -> (f64, f64, f64) {
        samples.sort_unstable();
        (
            samples[samples.len() / 2] as f64 / 1000.0,
            samples[(samples.len() * 95) / 100] as f64 / 1000.0,
            *samples.last().unwrap() as f64 / 1000.0,
        )
    }
}
