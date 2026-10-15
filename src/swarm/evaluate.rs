//! Evaluate loop — generator-evaluator feedback cycle.
//!
//! Pattern:
//! 1. Generator agent produces output
//! 2. Evaluator agent reviews and scores it
//! 3. If score < threshold, generator revises
//! 4. Max 5 revision rounds
//! 5. Returns best result

use anyhow::{bail, Result};
use std::sync::Arc;

use crate::providers::{ChatMessage, ChatRequest, Provider};

/// Configuration for an evaluate loop
#[derive(Debug, Clone)]
pub struct EvaluateConfig {
    /// Prompt for the generator
    pub generator_prompt: String,
    /// Prompt for the evaluator (receives generator output)
    pub evaluator_prompt: String,
    /// Quality threshold (0.0 - 1.0) — below this triggers revision
    pub quality_threshold: f64,
    /// Maximum revision rounds
    pub max_rounds: usize,
    /// Model for generator
    pub generator_model: String,
    /// Model for evaluator
    pub evaluator_model: String,
    /// Temperature for generator
    pub generator_temperature: f64,
    /// Temperature for evaluator (usually lower)
    pub evaluator_temperature: f64,
}

impl Default for EvaluateConfig {
    fn default() -> Self {
        Self {
            generator_prompt: String::new(),
            evaluator_prompt: String::new(),
            quality_threshold: 0.7,
            max_rounds: 5,
            generator_model: "claude-sonnet-4-5".to_string(),
            evaluator_model: "claude-sonnet-4-5".to_string(),
            generator_temperature: 0.7,
            evaluator_temperature: 0.3,
        }
    }
}

/// Result of an evaluation round
#[derive(Debug, Clone)]
pub struct EvalRound {
    pub round: usize,
    pub output: String,
    pub score: f64,
    pub feedback: String,
    pub passed: bool,
}

/// Result of the full evaluate loop
#[derive(Debug)]
pub struct EvalResult {
    pub final_output: String,
    pub final_score: f64,
    pub rounds: Vec<EvalRound>,
    pub passed: bool,
}

/// Run a generator-evaluator feedback loop
pub async fn evaluate_loop(
    provider: Arc<dyn Provider>,
    config: &EvaluateConfig,
) -> Result<EvalResult> {
    if config.generator_prompt.is_empty() {
        bail!("Generator prompt is required");
    }
    if config.evaluator_prompt.is_empty() {
        bail!("Evaluator prompt is required");
    }
    if config.max_rounds == 0 || config.max_rounds > 10 {
        bail!("Max rounds must be 1-10");
    }

    let mut rounds = Vec::new();
    let mut conversation: Vec<ChatMessage> = vec![ChatMessage {
        role: "user".to_string(),
        content: config.generator_prompt.clone(),
        tool_use_id: None,
    }];

    for round in 1..=config.max_rounds {
        // Step 1: Generate
        let generator_model = config.generator_model.clone();
        let gen_request = ChatRequest {
            messages: &conversation,
            tools: None,
            model: &generator_model,
            temperature: config.generator_temperature,
            max_tokens: Some(4096),
            reasoning_effort: None,
        };

        let gen_response = provider.chat(&gen_request).await?;
        let output = gen_response.text.unwrap_or_default();

        // Add generator output to conversation
        conversation.push(ChatMessage {
            role: "assistant".to_string(),
            content: output.clone(),
            tool_use_id: None,
        });

        // Step 2: Evaluate
        let eval_prompt = format!(
            "{}\n\n---\n\nContent to evaluate:\n{}\n\n---\n\nRespond with JSON: {{\"score\": 0.0-1.0, \"feedback\": \"...\", \"passed\": true/false}}",
            config.evaluator_prompt, output
        );
        let eval_messages = [ChatMessage {
            role: "user".to_string(),
            content: eval_prompt,
            tool_use_id: None,
        }];
        let evaluator_model = config.evaluator_model.clone();

        let eval_request = ChatRequest {
            messages: &eval_messages,
            tools: None,
            model: &evaluator_model,
            temperature: config.evaluator_temperature,
            max_tokens: Some(1024),
            reasoning_effort: None,
        };

        let eval_response = provider.chat(&eval_request).await?;
        let eval_text = eval_response.text.unwrap_or_default();

        // Parse evaluation response
        let (score, feedback, passed) = parse_eval_response(&eval_text, config.quality_threshold);

        let eval_round = EvalRound {
            round,
            output: output.clone(),
            score,
            feedback: feedback.clone(),
            passed,
        };
        rounds.push(eval_round);

        if passed {
            return Ok(EvalResult {
                final_output: output,
                final_score: score,
                rounds,
                passed: true,
            });
        }

        // Step 3: Prepare revision prompt
        if round < config.max_rounds {
            conversation.push(ChatMessage {
                role: "user".to_string(),
                content: format!(
                    "The evaluator scored this {:.0}% and provided this feedback:\n\n{}\n\nPlease revise your output to address this feedback.",
                    score * 100.0, feedback
                ),
                tool_use_id: None,
            });
        }
    }

    // Return best result even if it didn't pass
    let best = rounds
        .iter()
        .max_by(|a, b| {
            a.score
                .partial_cmp(&b.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .unwrap();

    Ok(EvalResult {
        final_output: best.output.clone(),
        final_score: best.score,
        rounds,
        passed: false,
    })
}

/// Parse evaluator response, extracting score and feedback
fn parse_eval_response(text: &str, threshold: f64) -> (f64, String, bool) {
    // Try JSON parsing first
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(text) {
        let score = v["score"].as_f64().unwrap_or(0.5);
        let feedback = v["feedback"].as_str().unwrap_or("No feedback").to_string();
        let passed = v["passed"].as_bool().unwrap_or(score >= threshold);
        return (score, feedback, passed);
    }

    // Try to find JSON in the text
    if let Some(start) = text.find('{') {
        if let Some(end) = text.rfind('}') {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text[start..=end]) {
                let score = v["score"].as_f64().unwrap_or(0.5);
                let feedback = v["feedback"].as_str().unwrap_or("No feedback").to_string();
                let passed = v["passed"].as_bool().unwrap_or(score >= threshold);
                return (score, feedback, passed);
            }
        }
    }

    // Fallback: assume moderate quality
    (0.5, text.to_string(), false)
}
