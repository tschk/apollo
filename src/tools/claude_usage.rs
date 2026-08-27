//! Claude usage tracking tool — check rate limits and remaining quota

use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

use super::traits::{Tool, ToolResult, ToolSpec};
use crate::cost::CostTracker;

pub struct ClaudeUsageTool {
    cost_tracker: Arc<CostTracker>,
}

impl ClaudeUsageTool {
    pub fn new(cost_tracker: Arc<CostTracker>) -> Self {
        Self { cost_tracker }
    }
}

#[async_trait]
impl Tool for ClaudeUsageTool {
    fn name(&self) -> &str {
        "claude_usage"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "claude_usage".to_string(),
            description: "Check Claude API usage, rate limits, and remaining quota. Shows requests/tokens remaining and cost summary.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["limits", "cost", "both"],
                        "description": "What to check: 'limits' (rate limits), 'cost' (spending), or 'both'"
                    }
                },
                "required": ["action"]
            }),
        }
    }

    async fn execute(&self, arguments: &str) -> anyhow::Result<ToolResult> {
        let args: serde_json::Value = serde_json::from_str(arguments).unwrap_or_default();
        let action = args["action"].as_str().unwrap_or("both");

        let mut output = Vec::new();

        // Rate limits
        if action == "limits" || action == "both" {
            if let Some(limits) = self.cost_tracker.get_rate_limits().await {
                output.push("⚡ Claude API Rate Limits\n".to_string());

                if let (Some(rem), Some(lim)) = (limits.requests_remaining, limits.requests_limit) {
                    let pct = (rem as f64 / lim as f64 * 100.0) as usize;
                    output.push(format!("Requests: {}/{} ({}% left)", rem, lim, pct));
                }

                if let (Some(rem), Some(lim)) =
                    (limits.input_tokens_remaining, limits.input_tokens_limit)
                {
                    let pct = (rem as f64 / lim as f64 * 100.0) as usize;
                    output.push(format!(
                        "Input tokens: {}/{} ({}% left)",
                        format_tokens(rem),
                        format_tokens(lim),
                        pct
                    ));
                }

                if let (Some(rem), Some(lim)) =
                    (limits.output_tokens_remaining, limits.output_tokens_limit)
                {
                    let pct = (rem as f64 / lim as f64 * 100.0) as usize;
                    output.push(format!(
                        "Output tokens: {}/{} ({}% left)",
                        format_tokens(rem),
                        format_tokens(lim),
                        pct
                    ));
                }

                if let Some(reset) = limits.tokens_reset {
                    output.push(format!("Resets: {}", reset));
                }

                output.push(String::new());
            } else {
                output.push(
                    "⚠️ No rate limit data yet (need at least one API call first)".to_string(),
                );
                output.push(String::new());
            }
        }

        // Cost summary
        if action == "cost" || action == "both" {
            let summary = self.cost_tracker.summary().await;

            output.push("💰 Cost Summary\n".to_string());
            output.push(format!("Total spent: ${:.4}", summary.total_cost));
            output.push(format!(
                "Total tokens: {}",
                format_tokens(summary.total_tokens)
            ));
            output.push(format!("API calls: {}", summary.call_count));
            if !summary.pricing_complete {
                output.push(format!(
                    "⚠️ Cost is incomplete: {} call(s) have no configured model price.",
                    summary.unpriced_call_count
                ));
            }

            if !summary.by_model.is_empty() {
                output.push(String::new());
                output.push("By model:".to_string());
                let mut models: Vec<_> = summary.by_model.iter().collect();
                models.sort_by(|a, b| b.1.partial_cmp(a.1).unwrap_or(std::cmp::Ordering::Equal));

                for (model, cost) in models {
                    output.push(format!("• {}: ${:.4}", model, cost));
                }
            }
        }

        Ok(ToolResult::success(output.join("\n")))
    }
}

fn format_tokens(n: usize) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}
