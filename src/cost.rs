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
    pub timestamp: chrono::DateTime<chrono::Utc>,
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
            models: Arc::new(RwLock::new(models)),
            rate_limit_status: Arc::new(RwLock::new(None)),
        }
    }

    /// Record a cost from an LLM call
    pub async fn record(&self, model: &str, usage: TokenUsage) -> anyhow::Result<()> {
        let models = self.models.read().await;
        let model_cost = models
            .iter()
            .find(|m| m.model == model)
            .cloned()
            .unwrap_or_else(|| ModelCost {
                model: model.to_string(),
                input_cost_per_1m: 0.0,
                output_cost_per_1m: 0.0,
            });

        let cost_usd = usage.calculate_cost(&model_cost);

        let record = CostRecord {
            id: uuid::Uuid::new_v4().to_string(),
            model: model.to_string(),
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cost_usd,
            timestamp: chrono::Utc::now(),
        };

        self.costs.write().await.push(record);
        Ok(())
    }

    /// Get cost summary
    pub async fn summary(&self) -> CostSummary {
        let costs = self.costs.read().await;

        let total_cost: f64 = costs.iter().map(|c| c.cost_usd).sum();
        let total_tokens: usize = costs.iter().map(|c| c.input_tokens + c.output_tokens).sum();

        let mut by_model: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
        for cost in costs.iter() {
            *by_model.entry(cost.model.clone()).or_insert(0.0) += cost.cost_usd;
        }

        CostSummary {
            total_cost,
            total_tokens,
            by_model,
            call_count: costs.len(),
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
}
