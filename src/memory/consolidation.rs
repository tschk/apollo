//! Daily memory consolidation — Instinct-style background producer.
//!
//! Writes the brief keys that `context_inject` already reads, expands open
//! loops into dream graph nodes, appends a dated timeline digest, and when
//! zkr is enabled stores a cited daily review of recent turns.

use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration as StdDuration;

use chrono::{Duration, Timelike, Utc};

use super::brief::{parse_bullet_list, BRIEF_NS, OPEN_LOOPS_KEY, TIME_CONTEXTS_KEY};
use super::session_note::daily_note_path;
use super::surreal::SurrealMemory;
#[cfg(feature = "zkr-memory")]
use super::zkr::ZkrStore;
use super::MemoryBackend;

const MAX_HISTORY: usize = 40;
const MAX_DIGEST_CHARS: usize = 4_000;
const MAX_BRIEF_LINES: usize = 12;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ConsolidationReport {
    pub day: String,
    pub time_contexts: usize,
    pub open_loops: usize,
    pub dreams: usize,
    pub skipped: bool,
}

pub struct ConsolidationInput<'a> {
    pub workspace: &'a Path,
    pub memory: &'a Arc<dyn MemoryBackend>,
    pub chat_id: &'a str,
    pub force: bool,
}

pub async fn consolidate_daily(
    input: ConsolidationInput<'_>,
) -> anyhow::Result<ConsolidationReport> {
    let today = Utc::now().date_naive();
    let day = today.format("%Y-%m-%d").to_string();
    let mut report = ConsolidationReport {
        day: day.clone(),
        ..ConsolidationReport::default()
    };

    if !input.force && already_ran_today(input.memory, &day).await {
        report.skipped = true;
        return Ok(report);
    }

    let history = input
        .memory
        .get_conversation_history(input.chat_id, MAX_HISTORY)
        .await
        .unwrap_or_default();
    let note_path = daily_note_path(input.workspace, today);
    let notes = tokio::fs::read_to_string(&note_path)
        .await
        .unwrap_or_default();

    let (time_contexts, open_loops) = derive_brief(&history, &notes, &day);
    write_brief(input.memory, &time_contexts, &open_loops).await?;
    report.time_contexts = time_contexts.len();
    report.open_loops = open_loops.len();

    if let Some(surreal) = input.memory.as_any().downcast_ref::<SurrealMemory>() {
        if let Ok(n) = surreal.graph_dream_from_loops(&open_loops).await {
            report.dreams = n;
        }
    }

    append_timeline_digest(input.workspace, &day, &time_contexts, &open_loops, &history)?;
    input
        .memory
        .store(BRIEF_NS, "last_consolidated_day", &day, None)
        .await?;
    Ok(report)
}

#[cfg(feature = "zkr-memory")]
pub async fn review_recent_turns(
    store: &ZkrStore,
    day: &str,
    query: &str,
    summary: &str,
) -> anyhow::Result<bool> {
    let pack = store.search(query.to_string(), 8).await?;
    let evidence_ids = pack
        .items
        .iter()
        .flat_map(|item| item.evidence_ids.iter().cloned())
        .collect::<Vec<_>>();
    if evidence_ids.is_empty() {
        return Ok(false);
    }
    store
        .store_review(day.to_string(), summary.to_string(), evidence_ids)
        .await?;
    Ok(true)
}

async fn already_ran_today(memory: &Arc<dyn MemoryBackend>, day: &str) -> bool {
    matches!(
        memory.recall(BRIEF_NS, "last_consolidated_day").await,
        Ok(Some(entry)) if entry.value.trim() == day
    )
}

fn derive_brief(
    history: &[(String, String)],
    notes: &str,
    day: &str,
) -> (Vec<String>, Vec<String>) {
    let mut time_contexts = vec![format!("{day}: daily consolidation")];
    let mut open_loops = Vec::new();

    for line in parse_bullet_list(notes) {
        if looks_open(&line) {
            push_unique(&mut open_loops, line);
        } else {
            push_unique(&mut time_contexts, line);
        }
    }

    for (role, content) in history.iter().rev() {
        let snippet = first_sentence(content);
        if snippet.is_empty() {
            continue;
        }
        if looks_open(&snippet) {
            push_unique(&mut open_loops, format!("{role}: {snippet}"));
        } else if *role == "user" {
            push_unique(&mut time_contexts, snippet);
        }
        if time_contexts.len() >= MAX_BRIEF_LINES && open_loops.len() >= MAX_BRIEF_LINES {
            break;
        }
    }

    time_contexts.truncate(MAX_BRIEF_LINES);
    open_loops.truncate(MAX_BRIEF_LINES);
    (time_contexts, open_loops)
}

fn looks_open(text: &str) -> bool {
    let lower = text.to_lowercase();
    [
        "todo",
        "still",
        "pending",
        "follow up",
        "need to",
        "waiting",
        "open loop",
        "later",
        "unfinished",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn first_sentence(text: &str) -> String {
    let trimmed = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    match crate::text::truncate_chars_counted(trimmed, 160) {
        Some((head, _)) => head,
        None => trimmed.to_string(),
    }
}

fn push_unique(target: &mut Vec<String>, value: String) {
    if value.is_empty() {
        return;
    }
    if !target.iter().any(|existing| existing == &value) {
        target.push(value);
    }
}

async fn write_brief(
    memory: &Arc<dyn MemoryBackend>,
    time_contexts: &[String],
    open_loops: &[String],
) -> anyhow::Result<()> {
    let stamp = Utc::now().to_rfc3339();
    let contexts = format_brief_file(&stamp, time_contexts);
    let loops = format_brief_file(&stamp, open_loops);
    memory
        .store(BRIEF_NS, TIME_CONTEXTS_KEY, &contexts, None)
        .await?;
    memory.store(BRIEF_NS, OPEN_LOOPS_KEY, &loops, None).await?;
    Ok(())
}

fn format_brief_file(stamp: &str, lines: &[String]) -> String {
    let mut out = format!("# updated {stamp}\n");
    for line in lines {
        out.push_str("- ");
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn append_timeline_digest(
    workspace: &Path,
    day: &str,
    time_contexts: &[String],
    open_loops: &[String],
    history: &[(String, String)],
) -> std::io::Result<()> {
    let dir = workspace.join("memory");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{day}.md"));
    let mut body = format!("\n## Consolidation {day}\n");
    if !time_contexts.is_empty() {
        body.push_str("\n### Time contexts\n");
        for line in time_contexts {
            body.push_str("- ");
            body.push_str(line);
            body.push('\n');
        }
    }
    if !open_loops.is_empty() {
        body.push_str("\n### Open loops\n");
        for line in open_loops {
            body.push_str("- ");
            body.push_str(line);
            body.push('\n');
        }
    }
    if !history.is_empty() {
        body.push_str("\n### Recent turns\n");
        let mut used = 0usize;
        for (role, content) in history.iter().rev().take(8) {
            let snippet = first_sentence(content);
            if snippet.is_empty() {
                continue;
            }
            let line = format!("- {role}: {snippet}\n");
            used = used.saturating_add(line.len());
            if used > MAX_DIGEST_CHARS {
                break;
            }
            body.push_str(&line);
        }
    }
    use std::io::Write;
    if path.exists() {
        let mut file = std::fs::OpenOptions::new().append(true).open(&path)?;
        file.write_all(body.as_bytes())?;
    } else {
        std::fs::write(path, format!("# Session notes {day}\n{body}"))?;
    }
    Ok(())
}

pub fn yesterday_query() -> String {
    let day = (Utc::now() - Duration::days(1))
        .date_naive()
        .format("%Y-%m-%d");
    format!("conversation {day}")
}

pub struct ConsolidationTicker {
    pub workspace: PathBuf,
    pub memory: Arc<dyn MemoryBackend>,
    pub chat_id: String,
    pub interval_secs: u64,
    pub quiet_start_hour: u32,
    pub quiet_end_hour: u32,
    #[cfg(feature = "zkr-memory")]
    pub zkr: Option<Arc<ZkrStore>>,
}

pub fn start_daily_consolidation(config: ConsolidationTicker) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(StdDuration::from_secs(config.interval_secs));
        interval.tick().await;
        loop {
            interval.tick().await;
            let hour = chrono::Local::now().hour();
            if hour >= config.quiet_start_hour || hour < config.quiet_end_hour {
                continue;
            }
            match consolidate_daily(ConsolidationInput {
                workspace: &config.workspace,
                memory: &config.memory,
                chat_id: &config.chat_id,
                force: false,
            })
            .await
            {
                Ok(report) if report.skipped => {
                    tracing::debug!("memory consolidation already ran for {}", report.day);
                }
                Ok(report) => {
                    tracing::info!(
                        day = %report.day,
                        contexts = report.time_contexts,
                        loops = report.open_loops,
                        dreams = report.dreams,
                        "daily memory consolidation complete"
                    );
                    #[cfg(feature = "zkr-memory")]
                    if let Some(store) = &config.zkr {
                        let summary = format!(
                            "Daily consolidation {} — {} time contexts, {} open loops",
                            report.day, report.time_contexts, report.open_loops
                        );
                        match review_recent_turns(store, &report.day, &yesterday_query(), &summary)
                            .await
                        {
                            Ok(true) => {
                                tracing::info!("zkr daily review stored for {}", report.day)
                            }
                            Ok(false) => tracing::debug!("zkr daily review skipped: no evidence"),
                            Err(error) => tracing::warn!("zkr daily review failed: {error}"),
                        }
                    }
                }
                Err(error) => tracing::warn!("memory consolidation failed: {error}"),
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_brief_splits_open_loops() {
        let history = vec![
            ("user".into(), "Ship the memory loop today".into()),
            ("assistant".into(), "Still waiting on the vendor".into()),
        ];
        let (contexts, loops) = derive_brief(&history, "- follow up on graph\n", "2026-09-21");
        assert!(contexts.iter().any(|c| c.contains("Ship the memory")));
        assert!(loops.iter().any(|c| c.contains("waiting on the vendor")));
        assert!(loops.iter().any(|c| c.contains("follow up on graph")));
    }

    #[tokio::test]
    async fn consolidate_daily_writes_brief_keys() {
        let dir = tempfile::tempdir().unwrap();
        let memory: Arc<dyn MemoryBackend> = Arc::new(
            crate::memory::surreal::SurrealMemory::new(dir.path())
                .await
                .unwrap(),
        );
        memory
            .store_conversation("cli", "user", "user", "Ship the memory loop today")
            .await
            .unwrap();
        memory
            .store_conversation(
                "cli",
                "assistant",
                "assistant",
                "Still waiting on the vendor",
            )
            .await
            .unwrap();

        let report = consolidate_daily(ConsolidationInput {
            workspace: dir.path(),
            memory: &memory,
            chat_id: "cli",
            force: true,
        })
        .await
        .unwrap();
        assert!(!report.skipped);
        assert!(report.open_loops > 0);

        let loops = memory
            .recall(BRIEF_NS, OPEN_LOOPS_KEY)
            .await
            .unwrap()
            .unwrap();
        assert!(loops.value.contains("waiting on the vendor"));
        assert!(loops.value.contains("# updated"));

        let again = consolidate_daily(ConsolidationInput {
            workspace: dir.path(),
            memory: &memory,
            chat_id: "cli",
            force: false,
        })
        .await
        .unwrap();
        assert!(again.skipped);
    }
}
