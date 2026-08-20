//! SurrealDB-backed memory storage using the local RocksDB engine.

use anyhow::Result;
use async_trait::async_trait;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use surrealdb::engine::local::RocksDb;
use surrealdb::Surreal;
use tokio::sync::{OnceCell, RwLock};

use super::traits::*;

#[derive(Clone)]
pub struct SurrealMemory {
    path: PathBuf,
    cell: Arc<OnceCell<Surreal<surrealdb::engine::local::Db>>>,
    /// Namespace → cached vectors, the candidate set every similarity search
    /// scans. Shared across clones, because `SurrealMemory` is cloned freely
    /// and all clones address the same embedded database.
    vectors: Arc<RwLock<HashMap<String, Vec<CachedVector>>>>,
}

/// One namespace entry in the vector cache.
///
/// `dim` is stored alongside the vector so a namespace can hold vectors from
/// several embedding models at once, which is exactly what an MTREE index could
/// not do. Filtering happens per entry at scan time.
struct CachedVector {
    key: String,
    dim: i64,
    vector: Vec<f32>,
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
    /// Packed little-endian `f32` payload, base64-encoded — the canonical
    /// vector encoding.
    ///
    /// One SurrealDB `Value` per row instead of one per component. Reading a
    /// 1536-element `array` back out costs ~0.27ms per row in `Value`
    /// materialization alone, which dominated the old scan.
    ///
    /// It is a base64 `string` and not a native `bytes` field because neither
    /// reachable `Bytes` type round-trips: `surrealdb::Bytes` is a newtype with
    /// a derived `Serialize`, so it stores as an array of 6144 integers — the
    /// exact cost this encoding exists to avoid — and `surrealdb::sql::Bytes`,
    /// which does emit a native bytes value, is not exported by the 2.3 SDK.
    vector_b64: String,
    text: String,
    created_at: String,
    #[serde(default)]
    dim: Option<i64>,
    #[serde(default)]
    model: Option<String>,
}

/// Row shape used to populate the vector cache for a namespace.
///
/// `vector_b64` is the current encoding; `vector` is only present on rows
/// written before it existed. Selecting both keeps legacy rows scoreable
/// without a migration pass, and costs nothing for new rows, which do not carry
/// a `vector` field at all.
#[derive(Debug, Clone, Deserialize)]
struct EmbeddingCandidate {
    key: String,
    dim: i64,
    #[serde(default)]
    vector_b64: Option<String>,
    #[serde(default)]
    vector: Option<Vec<f32>>,
}

/// The fields a search returns that are not worth caching, fetched for the
/// top-`k` winners only.
#[derive(Debug, Clone, Deserialize)]
struct EmbeddingDetail {
    key: String,
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
            vectors: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    fn memory_id(namespace: &str, key: &str) -> String {
        format!("{namespace}::{key}")
    }

    /// Populate the vector cache for `namespace` if it is not already loaded.
    ///
    /// This is the only full read of the `embeddings` table, and it happens
    /// once per namespace per process. Two concurrent first searches may both
    /// run it; they produce the same contents, so the duplicate is wasteful but
    /// not wrong, and that is cheaper than serialising every search behind a
    /// write lock.
    async fn ensure_vectors_loaded(&self, namespace: &str) -> Result<()> {
        if self.vectors.read().await.contains_key(namespace) {
            return Ok(());
        }

        // Rows with no `dim` are skipped by the query, matching the behaviour
        // of the previous per-search filter: a vector whose dimension was never
        // recorded is not comparable to anything and must not be ranked.
        let mut result = self
            .db()
            .await?
            .query(
                "SELECT key, dim, vector_b64, vector FROM embeddings \
                 WHERE namespace = $namespace AND dim != NONE",
            )
            .bind(("namespace", namespace.to_string()))
            .await?;
        let rows: Vec<EmbeddingCandidate> = result.take(0)?;

        let mut raw: Vec<u8> = Vec::new();
        let mut entries: Vec<CachedVector> = Vec::with_capacity(rows.len());
        for row in rows {
            let mut vector = Vec::new();
            match (&row.vector_b64, row.vector) {
                (Some(encoded), _) => unpack_vector_into(encoded, &mut raw, &mut vector),
                (None, Some(v)) => vector = v,
                // Neither encoding present: unscoreable, so it never enters the
                // candidate set.
                (None, None) => continue,
            }
            entries.push(CachedVector {
                key: row.key,
                dim: row.dim,
                vector,
            });
        }

        self.vectors
            .write()
            .await
            .insert(namespace.to_string(), entries);
        Ok(())
    }

    /// Write-through: keep a loaded namespace in step with a new or replaced
    /// vector, so a store followed by a search sees the store.
    ///
    /// A namespace that is not loaded is left alone — it will read the new row
    /// from the table when it is first loaded.
    async fn cache_vector(&self, namespace: &str, key: &str, vector: &[f32]) {
        let mut cache = self.vectors.write().await;
        let Some(entries) = cache.get_mut(namespace) else {
            return;
        };
        let entry = CachedVector {
            key: key.to_string(),
            dim: vector.len() as i64,
            vector: vector.to_vec(),
        };
        match entries.iter_mut().find(|e| e.key == key) {
            Some(existing) => *existing = entry,
            None => entries.push(entry),
        }
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
    -- `vector` is the legacy array encoding. New writes emit `blob` instead, so
    -- the field is optional and only present on rows written before the change.
    DEFINE FIELD IF NOT EXISTS vector ON embeddings TYPE option<array>;
    DEFINE FIELD IF NOT EXISTS vector_b64 ON embeddings TYPE option<string>;
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
            vector_b64: pack_vector(vector),
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
        self.cache_vector(namespace, key, vector).await;
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

        // Exact brute-force cosine scan, but over an in-process cache rather
        // than over SurrealDB rows.
        //
        // Reading candidates out of SurrealDB is the bottleneck, and it is not
        // only the vector payload: a `SELECT namespace, key` that projects no
        // vector at all still costs ~0.07ms per row, so the query layer alone
        // puts a ~700ms floor under a 10k-row search. No row encoding gets
        // below that, which is why the vectors are held in memory instead. The
        // packed encoding still pays for itself — it halves both the cost of
        // the one-time load and the on-disk size.
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
        self.ensure_vectors_loaded(namespace).await?;

        // `cosine_similarity` returns None for mismatched lengths rather than
        // scoring them, so an entry that slipped past the `dim` filter is
        // dropped instead of ranked.
        let ranked: Vec<(String, Vec<f32>)> = {
            let cache = self.vectors.read().await;
            let Some(entries) = cache.get(namespace) else {
                return Ok(Vec::new());
            };
            let query_magnitude = magnitude(query_vector);
            let mut scored: Vec<(f32, usize)> = Vec::with_capacity(entries.len());
            for (idx, entry) in entries.iter().enumerate() {
                if entry.dim != dim {
                    continue;
                }
                let sim =
                    cosine_similarity_with_magnitude(query_vector, query_magnitude, &entry.vector);
                if let Some(sim) = sim {
                    scored.push((sim, idx));
                }
            }
            scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
            scored.truncate(limit);
            scored
                .into_iter()
                .map(|(_, idx)| (entries[idx].key.clone(), entries[idx].vector.clone()))
                .collect()
        };

        if ranked.is_empty() {
            return Ok(Vec::new());
        }

        // `text` and `created_at` are deliberately not cached. They are read
        // back for the handful of winners only, which keeps the cache
        // proportional to the vectors alone and keeps the returned text from
        // ever going stale against the table.
        // Looked up by namespace and key rather than by record id: rows created
        // outside `store_embedding` do not necessarily follow the `ns::key` id
        // convention, and dropping them here would silently lose a hit that the
        // scan had already ranked.
        let keys: Vec<String> = ranked.iter().map(|(key, _)| key.clone()).collect();
        let mut result = self
            .db()
            .await?
            .query(
                "SELECT key, text, created_at FROM embeddings \
                 WHERE namespace = $namespace AND key IN $keys",
            )
            .bind(("namespace", namespace.to_string()))
            .bind(("keys", keys))
            .await?;
        let rows: Vec<EmbeddingDetail> = result.take(0)?;
        let details: std::collections::HashMap<String, EmbeddingDetail> =
            rows.into_iter().map(|r| (r.key.clone(), r)).collect();

        Ok(ranked
            .into_iter()
            .filter_map(|(key, vector)| {
                let detail = details.get(&key)?;
                Some(EmbeddingEntry {
                    namespace: namespace.to_string(),
                    key,
                    vector,
                    text: detail.text.clone(),
                    created_at: parse_timestamp(&detail.created_at),
                })
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

/// Pack an `f32` vector into base64-encoded little-endian bytes.
///
/// Endianness is fixed rather than native so a database file stays readable if
/// it is moved between a little- and a big-endian host.
fn pack_vector(vector: &[f32]) -> String {
    let mut bytes = Vec::with_capacity(vector.len() * 4);
    for x in vector {
        bytes.extend_from_slice(&x.to_le_bytes());
    }
    BASE64.encode(&bytes)
}

/// Decode a packed vector into `out`, replacing its contents.
///
/// Undecodable input yields an empty vector, and a trailing partial component
/// is ignored. Either way the resulting length does not match the query, so
/// `cosine_similarity` drops the row rather than scoring a truncated vector.
fn unpack_vector_into(encoded: &str, scratch: &mut Vec<u8>, out: &mut Vec<f32>) {
    out.clear();
    scratch.clear();
    if BASE64.decode_vec(encoded, scratch).is_err() {
        return;
    }
    out.reserve(scratch.len() / 4);
    for chunk in scratch.chunks_exact(4) {
        out.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }
}

/// Allocating form of `unpack_vector_into`, for callers with nothing to reuse.
#[cfg(test)]
fn unpack_vector(encoded: &str) -> Vec<f32> {
    let mut scratch = Vec::new();
    let mut out = Vec::new();
    unpack_vector_into(encoded, &mut scratch, &mut out);
    out
}

/// Cosine similarity, or `None` when the two vectors are not comparable.
///
/// Vectors of different lengths come from different embedding models and have
/// no meaningful similarity; returning `None` forces callers to drop the pair
/// rather than treat a fabricated score as a real one.
///
/// The scan calls `cosine_similarity_with_magnitude` directly; this wrapper is
/// the form the tests pin that behaviour against.
#[cfg(test)]
fn cosine_similarity(a: &[f32], b: &[f32]) -> Option<f32> {
    cosine_similarity_with_magnitude(a, magnitude(a), b)
}

/// Cosine similarity with the query's magnitude supplied by the caller.
///
/// The query is fixed for a whole scan, so its magnitude is a loop invariant
/// worth about a third of the arithmetic. Hoisting it is bit-identical: it is
/// the same value computed by the same operations in the same order, just once
/// instead of once per candidate.
fn cosine_similarity_with_magnitude(a: &[f32], ma: f32, b: &[f32]) -> Option<f32> {
    if a.len() != b.len() || a.is_empty() {
        return None;
    }

    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let mb: f32 = magnitude(b);

    if ma == 0.0 || mb == 0.0 {
        return Some(0.0);
    }

    Some(dot / (ma * mb))
}

fn magnitude(v: &[f32]) -> f32 {
    v.iter().map(|x| x * x).sum::<f32>().sqrt()
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

    fn mock_dim(dim: usize) -> Vec<f32> {
        let mut v = vec![0.0f32; dim];
        if dim > 0 {
            v[0] = 1.0;
        }
        v
    }

    async fn setup_memory() -> (tempfile::TempDir, SurrealMemory) {
        let dir = tempfile::tempdir().unwrap();
        let mem = SurrealMemory::new(dir.path()).await.unwrap();
        (dir, mem)
    }

    #[tokio::test]
    async fn test_surreal_store_and_recall() {
        let (_dir, mem) = setup_memory().await;

        mem.store("test", "greeting", "hello world", None)
            .await
            .unwrap();
        let val = mem.recall("test", "greeting").await.unwrap();
        assert!(val.is_some());
        assert_eq!(val.unwrap().value, "hello world");
    }

    #[tokio::test]
    async fn test_surreal_recall_missing() {
        let (_dir, mem) = setup_memory().await;

        let val = mem.recall("test", "missing-key").await.unwrap();
        assert!(val.is_none());
    }

    #[tokio::test]
    async fn test_surreal_search() {
        let (_dir, mem) = setup_memory().await;

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
        let (_dir, mem) = setup_memory().await;

        mem.store("ns", "del-key", "to delete", None).await.unwrap();
        assert!(mem.recall("ns", "del-key").await.unwrap().is_some());

        mem.forget("ns", "del-key").await.unwrap();
        assert!(mem.recall("ns", "del-key").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_surreal_conversation_history() {
        let (_dir, mem) = setup_memory().await;

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
        let (_dir, mem) = setup_memory().await;

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
        let (_dir, mem) = setup_memory().await;

        let vec1 = mock_dim(3);
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

    /// The scan reads a cache, so a write after the cache is warm has to be
    /// visible to the next search, and a rewrite of an existing key has to
    /// replace it rather than add a duplicate.
    #[tokio::test]
    async fn test_search_embeddings_sees_writes_after_cache_is_warm() {
        let (_dir, mem) = setup_memory().await;

        mem.store_embedding("ns", "a", &[1.0, 0.0, 0.0], "a", "m")
            .await
            .unwrap();
        // Warms the cache for "ns".
        assert_eq!(
            mem.search_embeddings("ns", &[1.0, 0.0, 0.0], 10)
                .await
                .unwrap()
                .len(),
            1
        );

        mem.store_embedding("ns", "b", &[0.0, 1.0, 0.0], "b", "m")
            .await
            .unwrap();
        let results = mem
            .search_embeddings("ns", &[0.0, 1.0, 0.0], 10)
            .await
            .unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].key, "b");

        // Rewriting "b" to point the other way must move it, not duplicate it.
        mem.store_embedding("ns", "b", &[1.0, 0.0, 0.0], "b2", "m")
            .await
            .unwrap();
        let results = mem
            .search_embeddings("ns", &[0.0, 1.0, 0.0], 10)
            .await
            .unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].key, "a");
        assert_eq!(results.iter().filter(|r| r.key == "b").count(), 1);
        assert_eq!(
            results.iter().find(|r| r.key == "b").unwrap().text,
            "b2",
            "text must come from the table, not a stale cache entry"
        );
    }

    #[test]
    fn test_pack_vector_round_trips() {
        let v = vec![1.0f32, -0.5, 0.0, 1e-8, f32::MAX, f32::MIN];
        assert_eq!(unpack_vector(&pack_vector(&v)), v);
        assert!(unpack_vector(&pack_vector(&[])).is_empty());
        // Undecodable input must not produce a partial vector that could be
        // scored against a query of the same length by accident.
        assert!(unpack_vector("not base64 !!!").is_empty());
    }

    /// Rows written before the packed encoding existed still carry `vector` as
    /// an array and no `vector_b64`. They must keep ranking normally rather
    /// than silently vanishing from every search.
    #[tokio::test]
    async fn test_search_embeddings_reads_legacy_array_rows() {
        let (_dir, mem) = setup_memory().await;
        let db = mem.db().await.unwrap();

        db.query(
            "CREATE embeddings:legacy SET namespace = 'ns', key = 'legacy',
             vector = [1.0, 0.0, 0.0], text = 'legacy row',
             created_at = '2024-01-01T00:00:00Z', dim = 3",
        )
        .await
        .unwrap();
        mem.store_embedding("ns", "modern", &[0.0, 1.0, 0.0], "modern row", "m")
            .await
            .unwrap();

        // Both encodings compete in the same ranking, and the query is aligned
        // with the legacy row, so the legacy row must win.
        let results = mem
            .search_embeddings("ns", &[1.0, 0.0, 0.0], 10)
            .await
            .unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].key, "legacy");
        assert_eq!(results[0].vector, vec![1.0, 0.0, 0.0]);
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
        let (_dir, mem) = setup_memory().await;

        // Simulates a provider switch: 3-dim rows from the old model coexist
        // with 8-dim rows from the new one.
        let old_dim = mock_dim(3);
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
        let (_dir, mem) = setup_memory().await;
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
        let (_dir, mem) = setup_memory().await;

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
        let (_dir, mem) = setup_memory().await;

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
        let (_dir, mem) = setup_memory().await;

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
            let (dir, mem) = setup_memory().await;
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
                "packed size={size:>6} dim={DIM} search_median={median:>8.2}ms \
                 search_p95={p95:>8.2}ms search_max={max:>8.2}ms insert_total={:>8.2}s \
                 insert_per_row={:>6.2}ms disk={:>7.1}MB",
                write_elapsed.as_secs_f64(),
                write_elapsed.as_secs_f64() * 1000.0 / size as f64,
                dir_size_mb(dir.path()),
            );
            drop(mem);
            drop(dir);

            // The same corpus in the previous `array` encoding, on the same
            // machine in the same run, so the comparison is not against numbers
            // recorded on another day.
            //
            // Both arms search the same in-memory cache, so their `median` is
            // expected to match — the encoding no longer affects a warm search.
            // What this arm measures is what the encoding still does affect:
            // insert cost, on-disk size, and the cold first query, which loads
            // the cache and shows up in `search_max`.
            let (dir, mem) = setup_memory().await;
            let db = mem.db().await.unwrap();
            let mut rng = Xorshift(0x2545_F491_4F6C_DD1D);

            let write_start = std::time::Instant::now();
            for i in 0..size {
                let v = rng.unit_vector(DIM);
                db.query(
                    "CREATE type::thing('embeddings', $id) SET namespace = 'bench', \
                     key = $id, vector = $v, text = 'text', \
                     created_at = '2024-01-01T00:00:00Z', dim = $dim, model = 'bench-model'",
                )
                .bind(("id", format!("k{i}")))
                .bind(("v", v))
                .bind(("dim", DIM as i64))
                .await
                .unwrap();
            }
            let write_elapsed = write_start.elapsed();

            let mut samples: Vec<u128> = Vec::with_capacity(QUERIES);
            for q in &queries {
                let start = std::time::Instant::now();
                let hits = mem.search_embeddings("bench", q, 10).await.unwrap();
                samples.push(start.elapsed().as_micros());
                assert_eq!(hits.len(), 10.min(size));
            }
            let (median, p95, max) = percentiles(&mut samples);
            println!(
                "array  size={size:>6} dim={DIM} search_median={median:>8.2}ms \
                 search_p95={p95:>8.2}ms search_max={max:>8.2}ms insert_total={:>8.2}s \
                 insert_per_row={:>6.2}ms disk={:>7.1}MB",
                write_elapsed.as_secs_f64(),
                write_elapsed.as_secs_f64() * 1000.0 / size as f64,
                dir_size_mb(dir.path()),
            );
        }
    }

    fn dir_size_mb(path: &Path) -> f64 {
        fn walk(path: &Path) -> u64 {
            let Ok(entries) = std::fs::read_dir(path) else {
                return 0;
            };
            entries
                .flatten()
                .map(|e| match e.metadata() {
                    Ok(m) if m.is_dir() => walk(&e.path()),
                    Ok(m) => m.len(),
                    Err(_) => 0,
                })
                .sum()
        }
        walk(path) as f64 / (1024.0 * 1024.0)
    }

    /// Feasibility spike for a per-dimension MTREE index: does SurrealDB
    /// accept a mixed-dimension table once an MTREE of a fixed DIMENSION
    /// exists on `vector`, and does KNN beat the scan?
    #[tokio::test]
    #[ignore = "benchmark: slow, writes a large corpus"]
    async fn bench_mtree_feasibility() {
        const DIM: usize = 1536;
        const SIZE: usize = 2_000;

        let (_dir, mem) = setup_memory().await;
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
