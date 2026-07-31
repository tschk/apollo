//! BriefTool — AI-powered summarization using the fast model.
//!
//! Summarizes long text, files, or search results into a concise brief.
//! Uses the configured fast_model (Haiku) to keep cost low.

use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;

use super::traits::*;
use crate::providers::{ChatMessage, ChatRequest, Provider};
use crate::text::truncate_chars_counted;

pub struct BriefTool {
    provider: Arc<dyn Provider>,
    model: String,
}

impl BriefTool {
    pub fn new(provider: Arc<dyn Provider>, model: impl Into<String>) -> Self {
        Self {
            provider,
            model: model.into(),
        }
    }
}

#[derive(Deserialize)]
struct BriefArgs {
    /// Content to summarize
    text: String,
    /// Summary style: "bullets", "paragraph", "tldr" (default: bullets)
    #[serde(default = "default_style")]
    style: String,
    /// Max length hint in words (default: 150)
    #[serde(default = "default_max_words")]
    max_words: usize,
}

fn default_style() -> String {
    "bullets".to_string()
}
fn default_max_words() -> usize {
    150
}

#[async_trait]
impl Tool for BriefTool {
    fn name(&self) -> &str {
        "brief"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "brief".to_string(),
            description: "Summarize long text into a concise brief using AI. \
                Use to condense search results, file contents, or verbose output \
                before including it in a response."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "text": {
                        "type": "string",
                        "description": "Content to summarize"
                    },
                    "style": {
                        "type": "string",
                        "enum": ["bullets", "paragraph", "tldr"],
                        "description": "Output style (default: bullets)"
                    },
                    "max_words": {
                        "type": "integer",
                        "description": "Approximate word limit for the summary (default: 150)",
                        "minimum": 20,
                        "maximum": 500
                    }
                },
                "required": ["text"]
            }),
        }
    }

    async fn execute(&self, arguments: &str) -> anyhow::Result<ToolResult> {
        let args: BriefArgs = serde_json::from_str(arguments)?;

        if args.text.trim().is_empty() {
            return Ok(ToolResult::error("text is empty"));
        }

        let style_instruction = match args.style.as_str() {
            "paragraph" => "Write a concise paragraph summary.",
            "tldr" => "Write a one-sentence TL;DR.",
            _ => "Write a concise bullet-point summary (use - for each bullet).",
        };

        let prompt = format!(
            "{} Keep it under ~{} words. Do not add commentary.\n\n---\n{}",
            style_instruction,
            args.max_words,
            // Truncate input if very long
            match truncate_chars_counted(&args.text, 40_000) {
                Some((head, dropped)) => format!("{head}...[truncated {dropped} chars]"),
                None => args.text.clone(),
            }
        );

        let messages = [ChatMessage::user(&prompt)];
        let request = ChatRequest {
            messages: &messages,
            tools: None,
            model: &self.model,
            temperature: 0.3,
            max_tokens: Some(600),
        };

        match self.provider.chat(&request).await {
            Ok(resp) => {
                let summary = resp.text.unwrap_or_else(|| "(empty response)".to_string());
                Ok(ToolResult::success(summary))
            }
            Err(e) => Ok(ToolResult::error(format!("Brief failed: {}", e))),
        }
    }
}
