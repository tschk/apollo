//! Cost tracking — token counting and billing for LLM calls
//! Phase 4 feature: Production billing support

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;

/// Cost per 1M tokens (input/output separate)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCost {
    pub model: String,
    pub input_cost_per_1m: f64,
    pub output_cost_per_1m: f64,
}

/// Token usage for a call
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: usize,
    pub output_tokens: usize,
    pub total_tokens: usize,
}

impl TokenUsage {
    pub fn calculate_cost(&self, cost: &ModelCost) -> f64 {
        let input_cost = (self.input_tokens as f64 / 1_000_000.0) * cost.input_cost_per_1m;
        let output_cost = (self.output_tokens as f64 / 1_000_000.0) * cost.output_cost_per_1m;
        input_cost + output_cost
    }
}

/// Cost record for a single LLM call
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostRecord {
    pub id: String,
    pub model: String,
    pub input_tokens: usize,
    pub output_tokens: usize,
    pub cost_usd: f64,
    /// False means usage was recorded but no configured price was available.
    #[serde(default = "default_pricing_known")]
    pub pricing_known: bool,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

fn default_pricing_known() -> bool {
    true
}

/// Estimated input shape for one provider request. These counts are based on
/// characters because providers do not expose tokenizers uniformly; they are
/// intended for comparing Apollo configurations, not billing.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContextSnapshot {
    pub system_chars: usize,
    pub history_chars: usize,
    pub tool_chars: usize,
    pub estimated_input_tokens: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContextSummary {
    pub request_count: usize,
    pub system_chars: usize,
    pub history_chars: usize,
    pub tool_chars: usize,
    pub estimated_input_tokens: usize,
}

/// Claude API rate limit status (from response headers)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitStatus {
    pub requests_limit: Option<usize>,
    pub requests_remaining: Option<usize>,
    pub input_tokens_limit: Option<usize>,
    pub input_tokens_remaining: Option<usize>,
    pub output_tokens_limit: Option<usize>,
    pub output_tokens_remaining: Option<usize>,
    pub tokens_reset: Option<String>,
}

/// Cost tracker (in-memory + persistent accounting hooks)
pub struct CostTracker {
    costs: Arc<RwLock<Vec<CostRecord>>>,
    contexts: Arc<RwLock<ContextSummary>>,
    models: Arc<RwLock<Vec<ModelCost>>>,
    rate_limit_status: Arc<RwLock<Option<RateLimitStatus>>>,
}

impl CostTracker {
    pub fn new() -> Self {
        let models = vec![
            ModelCost {
                model: "claude-opus-4-6".to_string(),
                input_cost_per_1m: 15.0,
                output_cost_per_1m: 75.0,
            },
            ModelCost {
                model: "claude-3-5-sonnet-20241022".to_string(),
                input_cost_per_1m: 3.0,
                output_cost_per_1m: 15.0,
            },
            ModelCost {
                model: "gpt-4-turbo".to_string(),
                input_cost_per_1m: 10.0,
                output_cost_per_1m: 30.0,
            },
            ModelCost {
                model: "gpt-4".to_string(),
                input_cost_per_1m: 30.0,
                output_cost_per_1m: 60.0,
            },
            ModelCost {
                model: "gpt-3.5-turbo".to_string(),
                input_cost_per_1m: 0.5,
                output_cost_per_1m: 1.5,
            },
            ModelCost {
                model: "gemini-2.0-flash".to_string(),
                input_cost_per_1m: 0.075,
                output_cost_per_1m: 0.3,
            },
        ];

        Self {
            costs: Arc::new(RwLock::new(Vec::new())),
            contexts: Arc::new(RwLock::new(ContextSummary::default())),
            models: Arc::new(RwLock::new(models)),
            rate_limit_status: Arc::new(RwLock::new(None)),
        }
    }

    /// Record a cost from an LLM call
    pub async fn record(&self, model: &str, usage: TokenUsage) -> anyhow::Result<()> {
        let models = self.models.read().await;
        let model_cost = models.iter().find(|m| m.model == model).cloned();

        let (cost_usd, pricing_known) = match model_cost {
            Some(model_cost) => (usage.calculate_cost(&model_cost), true),
            None => {
                tracing::warn!(model, "recording usage without a configured model price");
                (0.0, false)
            }
        };

        let record = CostRecord {
            id: uuid::Uuid::new_v4().to_string(),
            model: model.to_string(),
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cost_usd,
            pricing_known,
            timestamp: chrono::Utc::now(),
        };

        self.costs.write().await.push(record);
        Ok(())
    }

    /// Record the estimated shape of a provider request for harness telemetry.
    pub async fn record_context(&self, snapshot: ContextSnapshot) {
        let mut summary = self.contexts.write().await;
        summary.request_count += 1;
        summary.system_chars += snapshot.system_chars;
        summary.history_chars += snapshot.history_chars;
        summary.tool_chars += snapshot.tool_chars;
        summary.estimated_input_tokens += snapshot.estimated_input_tokens;
    }

    /// Aggregate prompt-shape telemetry since this tracker was created.
    pub async fn context_summary(&self) -> ContextSummary {
        self.contexts.read().await.clone()
    }

    /// Get cost summary
    pub async fn summary(&self) -> CostSummary {
        let costs = self.costs.read().await;

        // Folded from a positive zero rather than summed: `<f64 as Sum>` starts
        // at -0.0, so an empty tracker would otherwise report "-0.0" spent.
        let total_cost: f64 = costs.iter().map(|c| c.cost_usd).fold(0.0, |acc, c| acc + c);
        let total_tokens: usize = costs.iter().map(|c| c.input_tokens + c.output_tokens).sum();

        let mut by_model: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
        let mut unpriced_models = std::collections::BTreeSet::new();
        for cost in costs.iter() {
            *by_model.entry(cost.model.clone()).or_insert(0.0) += cost.cost_usd;
            if !cost.pricing_known {
                unpriced_models.insert(cost.model.clone());
            }
        }

        let context = self.context_summary().await;
        CostSummary {
            total_cost,
            total_tokens,
            by_model,
            call_count: costs.len(),
            unpriced_call_count: costs.iter().filter(|cost| !cost.pricing_known).count(),
            unpriced_models: unpriced_models.into_iter().collect(),
            pricing_complete: costs.iter().all(|cost| cost.pricing_known),
            context,
        }
    }

    /// Get cost history (with date filtering)
    pub async fn history(&self, days: usize) -> Vec<CostRecord> {
        let costs = self.costs.read().await;
        let cutoff = chrono::Utc::now() - chrono::Duration::days(days as i64);

        costs
            .iter()
            .filter(|c| c.timestamp > cutoff)
            .cloned()
            .collect()
    }

    /// Update rate limit status from Anthropic API response headers
    pub async fn update_rate_limits(&self, headers: &reqwest::header::HeaderMap) {
        let parse_usize = |key| {
            headers
                .get(key)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse().ok())
        };

        let parse_string = |key| {
            headers
                .get(key)
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string())
        };

        let status = RateLimitStatus {
            requests_limit: parse_usize("anthropic-ratelimit-requests-limit"),
            requests_remaining: parse_usize("anthropic-ratelimit-requests-remaining"),
            input_tokens_limit: parse_usize("anthropic-ratelimit-input-tokens-limit"),
            input_tokens_remaining: parse_usize("anthropic-ratelimit-input-tokens-remaining"),
            output_tokens_limit: parse_usize("anthropic-ratelimit-output-tokens-limit"),
            output_tokens_remaining: parse_usize("anthropic-ratelimit-output-tokens-remaining"),
            tokens_reset: parse_string("anthropic-ratelimit-tokens-reset"),
        };

        *self.rate_limit_status.write().await = Some(status);
    }

    /// Get current rate limit status
    pub async fn get_rate_limits(&self) -> Option<RateLimitStatus> {
        self.rate_limit_status.read().await.clone()
    }
}

impl Default for CostTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CostSummary {
    pub total_cost: f64,
    pub total_tokens: usize,
    pub by_model: std::collections::HashMap<String, f64>,
    pub call_count: usize,
    /// Calls whose token usage was recorded without a known price.
    #[serde(default)]
    pub unpriced_call_count: usize,
    #[serde(default)]
    pub unpriced_models: Vec<String>,
    /// False means `total_cost` excludes one or more calls with unknown
    /// pricing; it must not be presented as a complete bill.
    #[serde(default = "default_pricing_complete")]
    pub pricing_complete: bool,
    pub context: ContextSummary,
}

fn default_pricing_complete() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_token_cost_calculation() {
        let cost = ModelCost {
            model: "test".to_string(),
            input_cost_per_1m: 1.0,
            output_cost_per_1m: 2.0,
        };

        let usage = TokenUsage {
            input_tokens: 1_000_000,
            output_tokens: 1_000_000,
            total_tokens: 2_000_000,
        };

        let calculated = usage.calculate_cost(&cost);
        assert_eq!(calculated, 3.0); // 1.0 + 2.0
    }

    #[tokio::test]
    async fn test_cost_tracking() {
        let tracker = CostTracker::new();

        tracker
            .record(
                "claude-opus-4-6",
                TokenUsage {
                    input_tokens: 100,
                    output_tokens: 50,
                    total_tokens: 150,
                },
            )
            .await
            .unwrap();

        let summary = tracker.summary().await;
        assert_eq!(summary.call_count, 1);
        assert!(summary.total_cost > 0.0);
    }

    #[tokio::test]
    async fn unknown_model_usage_is_reported_as_unpriced() {
        let tracker = CostTracker::new();
        tracker
            .record(
                "future-model",
                TokenUsage {
                    input_tokens: 100,
                    output_tokens: 50,
                    total_tokens: 150,
                },
            )
            .await
            .unwrap();

        let summary = tracker.summary().await;
        assert_eq!(summary.unpriced_call_count, 1);
        assert_eq!(summary.unpriced_models, vec!["future-model"]);
        assert!(!summary.pricing_complete);
        assert_eq!(summary.total_cost, 0.0);
    }

    #[tokio::test]
    async fn an_empty_tracker_reports_a_positive_zero_cost() {
        let summary = CostTracker::new().summary().await;
        assert_eq!(summary.call_count, 0);
        assert_eq!(summary.total_cost, 0.0);
        assert!(!summary.total_cost.is_sign_negative());
        assert_eq!(format!("{:.1}", summary.total_cost), "0.0");
    }

    #[tokio::test]
    async fn test_update_rate_limits() {
        let tracker = CostTracker::new();
        let mut headers = reqwest::header::HeaderMap::new();

        headers.insert(
            "anthropic-ratelimit-requests-limit",
            "1000".parse().unwrap(),
        );
        headers.insert(
            "anthropic-ratelimit-requests-remaining",
            "999".parse().unwrap(),
        );
        headers.insert(
            "anthropic-ratelimit-input-tokens-limit",
            "400000".parse().unwrap(),
        );
        headers.insert(
            "anthropic-ratelimit-input-tokens-remaining",
            "399000".parse().unwrap(),
        );
        headers.insert(
            "anthropic-ratelimit-output-tokens-limit",
            "100000".parse().unwrap(),
        );
        headers.insert(
            "anthropic-ratelimit-output-tokens-remaining",
            "99000".parse().unwrap(),
        );
        headers.insert(
            "anthropic-ratelimit-tokens-reset",
            "2023-11-20T12:00:00Z".parse().unwrap(),
        );

        tracker.update_rate_limits(&headers).await;

        let status = tracker.get_rate_limits().await.unwrap();

        assert_eq!(status.requests_limit, Some(1000));
        assert_eq!(status.requests_remaining, Some(999));
        assert_eq!(status.input_tokens_limit, Some(400000));
        assert_eq!(status.input_tokens_remaining, Some(399000));
        assert_eq!(status.output_tokens_limit, Some(100000));
        assert_eq!(status.output_tokens_remaining, Some(99000));
        assert_eq!(
            status.tokens_reset,
            Some("2023-11-20T12:00:00Z".to_string())
        );
    }

    #[tokio::test]
    async fn context_telemetry_aggregates_request_shape() {
        let tracker = CostTracker::new();
        tracker
            .record_context(ContextSnapshot {
                system_chars: 40,
                history_chars: 80,
                tool_chars: 20,
                estimated_input_tokens: 35,
            })
            .await;
        let summary = tracker.context_summary().await;
        assert_eq!(summary.request_count, 1);
        assert_eq!(summary.estimated_input_tokens, 35);
        assert_eq!(summary.system_chars, 40);

        tracker
            .record_context(ContextSnapshot {
                system_chars: 2,
                history_chars: 3,
                tool_chars: 4,
                estimated_input_tokens: 5,
            })
            .await;
        let summary = tracker.context_summary().await;
        assert_eq!(summary.request_count, 2);
        assert_eq!(summary.system_chars, 42);
        assert_eq!(summary.estimated_input_tokens, 40);
    }

    #[tokio::test]
    async fn test_record_unknown_model_adds_unpriced_record() {
        let tracker = CostTracker::new();
        let usage = TokenUsage {
            input_tokens: 10,
            output_tokens: 20,
            total_tokens: 30,
        };

        tracker.record("unknown-model-123", usage).await.unwrap();

        let history = tracker.history(1).await;
        assert_eq!(history.len(), 1);

        let record = &history[0];
        assert_eq!(record.model, "unknown-model-123");
        assert_eq!(record.input_tokens, 10);
        assert_eq!(record.output_tokens, 20);
        assert_eq!(record.cost_usd, 0.0);
        assert_eq!(record.pricing_known, false);
    }

    #[tokio::test]
    async fn test_record_known_model_adds_priced_record() {
        let tracker = CostTracker::new();
        let usage = TokenUsage {
            input_tokens: 1_000_000,
            output_tokens: 1_000_000,
            total_tokens: 2_000_000,
        };

        // Based on CostTracker::new() defaults:
        // gpt-4-turbo: input = 10.0, output = 30.0
        tracker.record("gpt-4-turbo", usage).await.unwrap();

        let history = tracker.history(1).await;
        assert_eq!(history.len(), 1);

        let record = &history[0];
        assert_eq!(record.model, "gpt-4-turbo");
        assert_eq!(record.input_tokens, 1_000_000);
        assert_eq!(record.output_tokens, 1_000_000);
        assert_eq!(record.cost_usd, 40.0); // 10.0 + 30.0
        assert_eq!(record.pricing_known, true);
    }
}
