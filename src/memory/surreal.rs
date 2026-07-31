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
    DEFINE INDEX IF NOT EXISTS embedding_lookup_idx ON embeddings FIELDS namespace, key UNIQUE;
    DEFINE INDEX IF NOT EXISTS embedding_namespace_idx ON embeddings FIELDS namespace;
    -- TODO: define an MTREE index on `vector` for ANN-accelerated KNN when
    -- the embedding dimension is fixed at deploy time. MTREE requires a
    -- fixed DIMENSION, so it cannot be used while the table stores vectors
    -- from multiple providers (e.g. OpenAI 1536-dim, Gemini 768-dim).
    -- Example: DEFINE INDEX embedding_vector_mtree ON embeddings FIELDS vector MTREE DIMENSION 1536 DISTANCE COSINE;

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
    ) -> Result<()> {
        let row = EmbeddingRow {
            namespace: namespace.to_string(),
            key: key.to_string(),
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
        // Use SurrealDB's built-in KNN vector search (<| |> operator).
        // This performs a server-side nearest-neighbour scan and avoids
        // loading every embedding into the client. When an MTREE index is
        // defined on the vector field the query planner uses it; otherwise
        // it falls back to a brute-force scan inside the database engine.
        //
        // If the KNN query fails (e.g. dimension mismatch, empty table) we
        // fall back to the in-process cosine similarity scan below.
        let knn_result = self
            .db()
            .await?
            .query(
                "SELECT * FROM embeddings
                 WHERE namespace = $namespace
                   AND vector <| $query_vector |>
                 LIMIT $limit",
            )
            .bind(("namespace", namespace.to_string()))
            .bind(("query_vector", query_vector.to_vec()))
            .bind(("limit", limit as i64))
            .await;

        if let Ok(mut result) = knn_result {
            let rows: std::result::Result<Vec<EmbeddingRow>, _> = result.take(0);
            if let Ok(rows) = rows {
                if !rows.is_empty() {
                    return Ok(rows
                        .into_iter()
                        .map(|row| EmbeddingEntry {
                            namespace: row.namespace,
                            key: row.key,
                            vector: row.vector,
                            text: row.text,
                            created_at: parse_timestamp(&row.created_at),
                        })
                        .collect());
                }
            }
        }

        // Fallback: load all embeddings for the namespace and do cosine
        // search in-process. This handles mixed-dimension vectors or any
        // case where the KNN operator is unavailable.
        let mut result = self
            .db()
            .await?
            .query("SELECT * FROM embeddings WHERE namespace = $namespace")
            .bind(("namespace", namespace.to_string()))
            .await?;
        let rows: Vec<EmbeddingRow> = result.take(0)?;

        let mut scored: Vec<(f32, EmbeddingRow)> = rows
            .into_iter()
            .map(|row| {
                let sim = cosine_similarity(query_vector, &row.vector);
                (sim, row)
            })
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

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }

    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let ma: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let mb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();

    if ma == 0.0 || mb == 0.0 {
        return 0.0;
    }

    dot / (ma * mb)
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
    async fn test_surreal_embeddings() {
        let dir = tempfile::tempdir().unwrap();
        let mem = SurrealMemory::new(dir.path()).await.unwrap();

        let vec1 = vec![1.0, 0.0, 0.0];
        let vec2 = vec![0.0, 1.0, 0.0];
        let vec3 = vec![0.9, 0.1, 0.0];

        mem.store_embedding("ns", "e1", &vec1, "first")
            .await
            .unwrap();
        mem.store_embedding("ns", "e2", &vec2, "second")
            .await
            .unwrap();
        mem.store_embedding("ns", "e3", &vec3, "third")
            .await
            .unwrap();

        let results = mem.search_embeddings("ns", &vec1, 2).await.unwrap();
        assert!(!results.is_empty());
        // The closest to [1,0,0] should be e1 or e3
        assert!(results[0].key == "e1" || results[0].key == "e3");
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
}
