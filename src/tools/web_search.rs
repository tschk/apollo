//! Web search tool.
//!
//! Tries backends in order of result quality, using the first one configured:
//!
//! 1. **SearXNG** (`SEARXNG_URL`) — a self-hosted metasearch instance. Full web
//!    results, no third-party key, nothing leaves the machine except the query.
//! 2. **DuckDuckGo** — keyless, always available. Reads the lite result page
//!    for a ranked list, and falls back to the Instant Answer API when that
//!    page is unavailable.
//! 3. **Perplexity** (`PERPLEXITY_API_KEY`) — paid, kept for existing setups.
//!
//! The lite endpoint answers 202 with a challenge stub to clients that do not
//! look like browsers, so the request carries browser headers. That is a
//! best-effort path by nature: when it is refused the Instant Answer API still
//! answers "what is X", but only a SearXNG instance gives durable result lists.

use async_trait::async_trait;
use serde::Deserialize;

use super::traits::*;
use crate::text::truncate_chars;

/// Cap on characters returned to the model from any one backend.
const MAX_RESULT_CHARS: usize = 4000;

/// DuckDuckGo's plainest result page — table markup, no scripting.
const DDG_LITE_URL: &str = "https://lite.duckduckgo.com/lite/";

/// Markers DuckDuckGo puts on an anti-bot challenge page instead of results.
const DDG_BLOCK_MARKERS: &[&str] = &[
    "anomaly-modal",
    "challenge-platform",
    "DDG.anomalyDetection",
];

const DDG_USER_AGENTS: &[&str] = &[
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/122.0.0.0 Safari/537.36",
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/121.0.0.0 Safari/537.36",
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/123.0.0.0 Safari/537.36",
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:123.0) Gecko/20100101 Firefox/123.0",
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.2 Safari/605.1.15",
];

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
        Ok(crate::http::standard())
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

    /// Ranked results when DuckDuckGo serves them, instant answers otherwise.
    async fn search_duckduckgo(query: &str) -> anyhow::Result<String> {
        match Self::scrape_duckduckgo(query).await {
            Ok(text) => Ok(text),
            Err(e) => {
                tracing::debug!("duckduckgo result page unavailable ({e}), using instant answers");
                Self::duckduckgo_instant(query).await
            }
        }
    }

    async fn scrape_duckduckgo(query: &str) -> anyhow::Result<String> {
        let resp = Self::client()?
            .post(DDG_LITE_URL)
            .header(reqwest::header::USER_AGENT, pick_user_agent())
            .header(
                reqwest::header::ACCEPT,
                "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
            )
            .header(reqwest::header::ACCEPT_LANGUAGE, "en-US,en;q=0.9")
            .header(reqwest::header::REFERER, "https://lite.duckduckgo.com/")
            .form(&[("q", query), ("kl", "us-en"), ("safe", "1")])
            .send()
            .await?;

        // 202 is how the endpoint refuses a client it does not consider a browser.
        if resp.status().as_u16() == 202 {
            anyhow::bail!("soft-blocked");
        }
        if !resp.status().is_success() {
            anyhow::bail!("duckduckgo returned {}", resp.status());
        }

        let body = resp.text().await?;
        if looks_blocked(&body) {
            anyhow::bail!("challenge page");
        }

        let hits = parse_lite_results(&body);
        if hits.is_empty() {
            anyhow::bail!("no results parsed");
        }
        Ok(format_results(hits.into_iter().take(8)))
    }

    async fn duckduckgo_instant(query: &str) -> anyhow::Result<String> {
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

/// Rotates the user agent so a long-running agent looks less uniform.
fn pick_user_agent() -> &'static str {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as usize)
        .unwrap_or(0);
    DDG_USER_AGENTS[nanos % DDG_USER_AGENTS.len()]
}

fn looks_blocked(body: &str) -> bool {
    DDG_BLOCK_MARKERS.iter().any(|m| body.contains(m))
}

/// Read one attribute out of an opening tag, tolerating either quote style.
fn attr(tag: &str, name: &str) -> Option<String> {
    let at = tag.find(&format!("{name}="))?;
    let rest = &tag[at + name.len() + 1..];
    let quote = rest.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let rest = &rest[quote.len_utf8()..];
    let end = rest.find(quote)?;
    Some(rest[..end].to_string())
}

/// Strip tags, decode the entities DuckDuckGo emits, and collapse whitespace.
fn clean_text(fragment: &str) -> String {
    let mut out = String::with_capacity(fragment.len());
    let mut in_tag = false;
    for ch in fragment.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    // `&amp;` decodes last so an escaped entity is not decoded twice.
    let decoded = out
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&#39;", "'")
        .replace("&amp;", "&");
    decoded.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Pull results out of the lite page, which lists each hit as an anchor of
/// class `result-link` followed by an optional `result-snippet` cell.
fn parse_lite_results(html: &str) -> Vec<SearchHit> {
    let mut hits = Vec::new();
    let mut rest = html;

    while let Some(marker) = rest.find("result-link") {
        let (before, after) = rest.split_at(marker);
        let Some(gt) = after.find('>') else { break };
        let body = &after[gt + 1..];
        let Some(end) = body.find("</a>") else { break };
        let tail = &body[end..];

        let url = before
            .rfind("<a ")
            .and_then(|start| attr(&before[start..], "href"))
            .unwrap_or_default();
        let title = clean_text(&body[..end]);

        // A snippet belongs to this hit only if it precedes the next one.
        let next = tail.find("result-link").unwrap_or(tail.len());
        let snippet = tail[..next]
            .find("result-snippet")
            .and_then(|at| {
                let segment = &tail[at..next];
                let open = segment.find('>')?;
                let close = segment[open..].find("</td>")?;
                Some(clean_text(&segment[open + 1..open + close]))
            })
            .unwrap_or_default();

        if !url.is_empty() && !title.is_empty() {
            hits.push(SearchHit {
                title,
                url,
                snippet,
            });
        }
        rest = tail;
    }

    hits
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

    const LITE_PAGE: &str = r#"
      <table border="0">
        <tr><td valign="top">1.&nbsp;</td>
            <td><a rel="nofollow" href="https://tokio.rs/" class='result-link'>Tokio - An asynchronous <b>Rust</b> runtime</a></td></tr>
        <tr><td>&nbsp;</td>
            <td class='result-snippet'><b>Tokio</b> is a library for fast &amp; reliable apps.</td></tr>
        <tr><td valign="top">2.&nbsp;</td>
            <td><a rel="nofollow" href="https://docs.rs/tokio" class='result-link'>tokio - Rust</a></td></tr>
      </table>"#;

    #[test]
    fn parses_lite_result_page() {
        let hits = parse_lite_results(LITE_PAGE);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].url, "https://tokio.rs/");
        assert_eq!(hits[0].title, "Tokio - An asynchronous Rust runtime");
        assert_eq!(
            hits[0].snippet,
            "Tokio is a library for fast & reliable apps."
        );
        assert_eq!(hits[1].url, "https://docs.rs/tokio");
    }

    #[test]
    fn does_not_borrow_a_later_hits_snippet() {
        // The second hit has no snippet of its own and must not inherit one.
        let hits = parse_lite_results(LITE_PAGE);
        assert!(hits[1].snippet.is_empty());
    }

    #[test]
    fn parsing_a_page_without_results_yields_nothing() {
        assert!(parse_lite_results("<html><body>nothing here</body></html>").is_empty());
    }

    #[test]
    fn detects_challenge_pages() {
        assert!(looks_blocked("<div id=\"anomaly-modal\">"));
        assert!(looks_blocked("window.DDG.anomalyDetection = {}"));
        assert!(!looks_blocked(LITE_PAGE));
    }

    #[test]
    fn reads_attributes_in_either_quote_style() {
        assert_eq!(attr("<a href=\"x\">", "href").as_deref(), Some("x"));
        assert_eq!(attr("<a href='y'>", "href").as_deref(), Some("y"));
        assert_eq!(attr("<a rel=nofollow>", "href"), None);
    }

    #[test]
    fn user_agent_comes_from_the_pool() {
        assert!(DDG_USER_AGENTS.contains(&pick_user_agent()));
    }

    /// Canary for the scraped path: DuckDuckGo can change its markup or start
    /// refusing this client at any time. Ignored by default — it needs network.
    /// Run with: `cargo test --lib duckduckgo_still_serves -- --ignored`
    #[tokio::test]
    #[ignore]
    async fn duckduckgo_still_serves_a_result_page() {
        let out = WebSearchTool::scrape_duckduckgo("rust tokio runtime")
            .await
            .expect("lite endpoint refused or markup changed");
        assert!(out.contains("http"), "no links in: {out}");
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
