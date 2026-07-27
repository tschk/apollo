//! Web search tool.
//!
//! Tries backends in order of result quality, using the first one configured:
//!
//! 1. **SearXNG** (`SEARXNG_URL`) — a self-hosted metasearch instance. Full web
//!    results, no third-party key, nothing leaves the machine except the query.
//! 2. **DuckDuckGo Instant Answer** — official, keyless, always available. It
//!    returns topic abstracts rather than a ranked result list, so it answers
//!    "what is X" well and "find pages about X" poorly.
//! 3. **Perplexity** (`PERPLEXITY_API_KEY`) — paid, kept for existing setups.
//!
//! DuckDuckGo's `html.` and `lite.` HTML endpoints are deliberately not used:
//! they answer 202 with no results to non-browser clients.

use async_trait::async_trait;
use serde::Deserialize;

use super::traits::*;
use crate::text::truncate_chars;

/// Cap on characters returned to the model from any one backend.
const MAX_RESULT_CHARS: usize = 4000;

/// Search backends, most preferred first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Backend {
    /// Self-hosted SearXNG instance at this base URL.
    Searxng(String),
    /// DuckDuckGo Instant Answer API — no configuration required.
    DuckDuckGo,
    /// Perplexity chat completions with the given API key.
    Perplexity(String),
}

pub struct WebSearchTool {
    backend: Backend,
}

impl WebSearchTool {
    pub fn new() -> Self {
        Self {
            backend: Self::detect_backend(),
        }
    }

    /// Pick a backend from the environment. DuckDuckGo is the floor, so this
    /// always yields something usable.
    fn detect_backend() -> Backend {
        if let Ok(url) = std::env::var("SEARXNG_URL") {
            let url = url.trim().trim_end_matches('/');
            if !url.is_empty() {
                return Backend::Searxng(url.to_string());
            }
        }
        match std::env::var("PERPLEXITY_API_KEY") {
            Ok(key) if !key.trim().is_empty() => Backend::Perplexity(key),
            _ => Backend::DuckDuckGo,
        }
    }

    /// Override the backend, for tests and explicit configuration.
    pub fn with_backend(mut self, backend: Backend) -> Self {
        self.backend = backend;
        self
    }

    pub fn with_api_key(self, key: String) -> Self {
        self.with_backend(Backend::Perplexity(key))
    }

    pub fn backend(&self) -> &Backend {
        &self.backend
    }

    fn client() -> anyhow::Result<reqwest::Client> {
        Ok(reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()?)
    }

    async fn search_searxng(base_url: &str, query: &str) -> anyhow::Result<String> {
        let resp = Self::client()?
            .get(format!("{base_url}/search"))
            .query(&[("q", query), ("format", "json")])
            .send()
            .await?;

        if !resp.status().is_success() {
            anyhow::bail!("searxng returned {}", resp.status());
        }

        let body: serde_json::Value = resp.json().await?;
        let results = body
            .get("results")
            .and_then(|r| r.as_array())
            .ok_or_else(|| anyhow::anyhow!("searxng response had no results array"))?;

        Ok(format_results(results.iter().take(8).map(|r| SearchHit {
            title: field(r, "title"),
            url: field(r, "url"),
            snippet: field(r, "content"),
        })))
    }

    async fn search_duckduckgo(query: &str) -> anyhow::Result<String> {
        let resp = Self::client()?
            .get("https://api.duckduckgo.com/")
            .query(&[
                ("q", query),
                ("format", "json"),
                ("no_html", "1"),
                ("no_redirect", "1"),
            ])
            .send()
            .await?;

        if !resp.status().is_success() {
            anyhow::bail!("duckduckgo returned {}", resp.status());
        }

        // The endpoint answers with a text/javascript content type, so decode
        // the body ourselves rather than relying on `.json()`.
        let body: serde_json::Value = serde_json::from_str(&resp.text().await?)?;
        Ok(format_duckduckgo(&body))
    }

    async fn search_perplexity(api_key: &str, query: &str) -> anyhow::Result<String> {
        let resp = Self::client()?
            .post("https://api.perplexity.ai/chat/completions")
            .header("Authorization", format!("Bearer {api_key}"))
            .header("Content-Type", "application/json")
            .json(&serde_json::json!({
                "model": "sonar",
                "messages": [{"role": "user", "content": query}],
            }))
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("perplexity {}: {}", status, truncate_chars(&text, 200));
        }

        let data: serde_json::Value = resp.json().await?;
        Ok(data["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or("No results found")
            .to_string())
    }
}

struct SearchHit {
    title: String,
    url: String,
    snippet: String,
}

fn field(value: &serde_json::Value, key: &str) -> String {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string()
}

fn format_results(hits: impl Iterator<Item = SearchHit>) -> String {
    let mut out = String::new();
    for (i, hit) in hits.enumerate() {
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(&format!("{}. {}\n   {}", i + 1, hit.title, hit.url));
        if !hit.snippet.trim().is_empty() {
            out.push_str(&format!("\n   {}", hit.snippet.trim()));
        }
    }
    if out.is_empty() {
        "No results found".to_string()
    } else {
        truncate_chars(&out, MAX_RESULT_CHARS)
    }
}

/// Render an Instant Answer payload: the abstract when there is one, then the
/// related topics, which is all this endpoint offers in place of a result list.
fn format_duckduckgo(body: &serde_json::Value) -> String {
    let mut out = String::new();

    let abstract_text = field(body, "AbstractText");
    if !abstract_text.trim().is_empty() {
        out.push_str(abstract_text.trim());
        let source = field(body, "AbstractURL");
        if !source.is_empty() {
            out.push_str(&format!("\n\nSource: {source}"));
        }
    }

    let answer = field(body, "Answer");
    if !answer.trim().is_empty() {
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(answer.trim());
    }

    // RelatedTopics nests one level for grouped results, so flatten it.
    let mut topics: Vec<SearchHit> = Vec::new();
    if let Some(list) = body.get("RelatedTopics").and_then(|t| t.as_array()) {
        for entry in list {
            match entry.get("Topics").and_then(|t| t.as_array()) {
                Some(nested) => topics.extend(nested.iter().filter_map(related_topic)),
                None => topics.extend(related_topic(entry)),
            }
        }
    }

    if !topics.is_empty() {
        if !out.is_empty() {
            out.push_str("\n\nRelated:\n");
        }
        out.push_str(&format_results(topics.into_iter().take(8)));
    }

    if out.trim().is_empty() {
        "No results found".to_string()
    } else {
        truncate_chars(&out, MAX_RESULT_CHARS)
    }
}

fn related_topic(entry: &serde_json::Value) -> Option<SearchHit> {
    let text = field(entry, "Text");
    if text.trim().is_empty() {
        return None;
    }
    // Instant Answer topics have no separate title; the first clause reads as one.
    let title = text.split(" - ").next().unwrap_or(&text).to_string();
    Some(SearchHit {
        title,
        url: field(entry, "FirstURL"),
        snippet: String::new(),
    })
}

impl Default for WebSearchTool {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Deserialize)]
struct SearchArgs {
    query: String,
}

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &str {
        "web_search"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "web_search".to_string(),
            description: "Search the web for information. Returns relevant results with snippets."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Search query"
                    }
                },
                "required": ["query"]
            }),
        }
    }

    async fn execute(&self, arguments: &str) -> anyhow::Result<ToolResult> {
        let args: SearchArgs = serde_json::from_str(arguments)?;
        if args.query.trim().is_empty() {
            return Ok(ToolResult::error("query must not be empty"));
        }

        let outcome = match &self.backend {
            Backend::Searxng(url) => Self::search_searxng(url, &args.query).await,
            Backend::DuckDuckGo => Self::search_duckduckgo(&args.query).await,
            Backend::Perplexity(key) => Self::search_perplexity(key, &args.query).await,
        };

        match outcome {
            Ok(text) => Ok(ToolResult::success(text)),
            // A self-hosted instance that is down should not take search with
            // it — fall back to the keyless backend before giving up.
            Err(e) if matches!(self.backend, Backend::Searxng(_)) => {
                tracing::warn!("searxng search failed, falling back to duckduckgo: {e}");
                match Self::search_duckduckgo(&args.query).await {
                    Ok(text) => Ok(ToolResult::success(text)),
                    Err(e) => Ok(ToolResult::error(format!("search failed: {e}"))),
                }
            }
            Err(e) => Ok(ToolResult::error(format!("search failed: {e}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duckduckgo_is_the_keyless_default() {
        // Nothing configured should still leave search usable.
        assert_eq!(
            WebSearchTool::new().backend(),
            &Backend::DuckDuckGo,
            "expected the keyless backend when no env vars are set"
        );
    }

    #[test]
    fn formats_an_abstract_with_its_source() {
        let body = serde_json::json!({
            "AbstractText": "Rust is a general-purpose programming language.",
            "AbstractURL": "https://en.wikipedia.org/wiki/Rust",
            "RelatedTopics": [],
        });
        let out = format_duckduckgo(&body);
        assert!(out.contains("general-purpose programming language"));
        assert!(out.contains("Source: https://en.wikipedia.org/wiki/Rust"));
    }

    #[test]
    fn flattens_grouped_related_topics() {
        let body = serde_json::json!({
            "AbstractText": "",
            "RelatedTopics": [
                {"Text": "Top level - a description", "FirstURL": "https://example.com/a"},
                {"Topics": [
                    {"Text": "Nested one - detail", "FirstURL": "https://example.com/b"},
                    {"Text": "Nested two - detail", "FirstURL": "https://example.com/c"},
                ]},
            ],
        });
        let out = format_duckduckgo(&body);
        assert!(out.contains("Top level"));
        assert!(out.contains("https://example.com/b"));
        assert!(out.contains("https://example.com/c"));
    }

    #[test]
    fn empty_payload_reports_no_results() {
        let body = serde_json::json!({"AbstractText": "", "RelatedTopics": []});
        assert_eq!(format_duckduckgo(&body), "No results found");
    }

    #[test]
    fn skips_related_topics_without_text() {
        let body = serde_json::json!({
            "AbstractText": "",
            "RelatedTopics": [
                {"FirstURL": "https://example.com/no-text"},
                {"Text": "Has text", "FirstURL": "https://example.com/ok"},
            ],
        });
        let out = format_duckduckgo(&body);
        assert!(out.contains("Has text"));
        assert!(!out.contains("no-text"));
    }

    #[test]
    fn formats_searxng_style_hits() {
        let hits = vec![
            SearchHit {
                title: "First".into(),
                url: "https://example.com/1".into(),
                snippet: "  a snippet  ".into(),
            },
            SearchHit {
                title: "Second".into(),
                url: "https://example.com/2".into(),
                snippet: String::new(),
            },
        ];
        let out = format_results(hits.into_iter());
        assert!(out.starts_with("1. First"));
        assert!(out.contains("   a snippet"));
        assert!(out.contains("2. Second"));
    }
}
