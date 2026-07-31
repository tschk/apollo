//! Web fetch tool — download and extract readable content from URLs.

use async_trait::async_trait;
use serde::Deserialize;

use super::network::validate_public_http_url;
use super::traits::*;
use crate::text::truncate_chars_counted;

pub struct WebFetchTool {
    client: reqwest::Client,
}

impl WebFetchTool {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .user_agent("apollo/0.1")
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("Failed to create HTTP client"),
        }
    }
}

impl Default for WebFetchTool {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Deserialize)]
struct FetchArgs {
    url: String,
    #[serde(default = "default_max_chars")]
    max_chars: usize,
}

fn default_max_chars() -> usize {
    50_000
}

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &str {
        "web_fetch"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "web_fetch".to_string(),
            description: "Fetch and extract readable content from a URL (HTML → text). Use for reading web pages.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "HTTP or HTTPS URL to fetch"
                    },
                    "max_chars": {
                        "type": "integer",
                        "description": "Maximum characters to return (default 50000)"
                    }
                },
                "required": ["url"]
            }),
        }
    }

    async fn execute(&self, arguments: &str) -> anyhow::Result<ToolResult> {
        let args: FetchArgs = serde_json::from_str(arguments)?;

        let _ = validate_public_http_url(&args.url, &[]).await?;

        let resp = match self.client.get(&args.url).send().await {
            Ok(r) => r,
            Err(e) => return Ok(ToolResult::error(format!("Fetch error: {}", e))),
        };

        if !resp.status().is_success() {
            return Ok(ToolResult::error(format!("HTTP {}", resp.status())));
        }

        const MAX_BYTES: usize = 1_048_576;
        let bytes = match resp.bytes().await {
            Ok(bytes) => bytes,
            Err(e) => return Ok(ToolResult::error(format!("Fetch error: {}", e))),
        };
        if bytes.len() > MAX_BYTES {
            return Ok(ToolResult::error(format!(
                "Response too large: {} bytes (max {})",
                bytes.len(),
                MAX_BYTES
            )));
        }

        let text = String::from_utf8_lossy(&bytes);

        // Simple HTML stripping (remove tags, decode entities)
        let cleaned = strip_html(&text);

        let truncated = match truncate_chars_counted(&cleaned, args.max_chars) {
            Some((head, dropped)) => {
                format!("{head}...\n[truncated: {dropped} chars dropped]")
            }
            None => cleaned,
        };

        Ok(ToolResult::success(truncated))
    }
}

/// Basic HTML tag stripping
fn strip_html(html: &str) -> String {
    let mut result = String::with_capacity(html.len());
    let mut in_tag = false;
    let mut in_script = false;
    let mut in_style = false;

    let lower = html.to_lowercase();
    let chars: Vec<char> = html.chars().collect();
    let lower_chars: Vec<char> = lower.chars().collect();

    let mut i = 0;
    while i < chars.len() {
        // Detect script/style blocks
        if i + 7 < lower_chars.len() {
            let slice: String = lower_chars[i..i + 7].iter().collect();
            if slice == "<script" {
                in_script = true;
            }
            if slice == "<style "
                || (i + 6 < lower_chars.len()
                    && lower_chars[i..i + 6].iter().collect::<String>() == "<style")
            {
                in_style = true;
            }
        }
        if i + 8 < lower_chars.len() {
            let slice: String = lower_chars[i..i + 9.min(lower_chars.len())]
                .iter()
                .collect();
            if slice.starts_with("</script") {
                in_script = false;
            }
        }
        if i + 7 < lower_chars.len() {
            let slice: String = lower_chars[i..i + 8.min(lower_chars.len())]
                .iter()
                .collect();
            if slice.starts_with("</style") {
                in_style = false;
            }
        }

        if chars[i] == '<' {
            in_tag = true;
            // Add newline for block elements
            if i + 3 < chars.len() {
                let tag: String = lower_chars[i + 1..i + 3.min(lower_chars.len())]
                    .iter()
                    .collect();
                if tag.starts_with('p')
                    || tag.starts_with('h')
                    || tag.starts_with("br")
                    || tag.starts_with("di")
                    || tag.starts_with("li")
                {
                    result.push('\n');
                }
            }
        } else if chars[i] == '>' {
            in_tag = false;
        } else if !in_tag && !in_script && !in_style {
            result.push(chars[i]);
        }
        i += 1;
    }

    // Decode common entities
    result = result
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&nbsp;", " ")
        .replace("&#39;", "'");

    // Collapse multiple newlines
    while result.contains("\n\n\n") {
        result = result.replace("\n\n\n", "\n\n");
    }

    result.trim().to_string()
}
