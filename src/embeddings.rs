//! Vector embeddings for semantic memory search
//! Uses Gemini text-embedding-004 API (free tier)

use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Embedding {
    pub text: String,
    pub vector: Vec<f32>,
    pub metadata: serde_json::Value,
}

#[derive(Deserialize)]
struct GeminiResponse {
    embedding: GeminiEmbedding,
}

#[derive(Deserialize)]
struct GeminiEmbedding {
    values: Vec<f32>,
}

pub struct EmbeddingsClient {
    api_key: String,
    client: reqwest::Client,
}

impl EmbeddingsClient {
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            client: crate::http::shared(),
        }
    }

    /// Embed text using Gemini text-embedding-004
    pub async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let url = "https://generativelanguage.googleapis.com/v1beta/models/text-embedding-004:embedContent";

        let request = serde_json::json!({
            "model": "models/text-embedding-004",
            "content": {
                "parts": [{
                    "text": text
                }]
            }
        });

        let resp = self
            .client
            .post(url)
            .header("x-goog-api-key", &self.api_key)
            .json(&request)
            .send()
            .await?
            .error_for_status()?;

        let result: GeminiResponse = resp.json().await?;
        Ok(result.embedding.values)
    }

    /// Embed multiple texts in batch
    pub async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        let mut results = Vec::new();
        for text in texts {
            results.push(self.embed(text).await?);
        }
        Ok(results)
    }
}

/// Cosine similarity between two vectors, or `None` when they are not
/// comparable.
///
/// Vectors of different lengths come from different embedding models and have
/// no meaningful similarity, so no score is produced for them.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> Option<f32> {
    if a.len() != b.len() || a.is_empty() {
        return None;
    }

    let dot_product: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let magnitude_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let magnitude_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();

    if magnitude_a == 0.0 || magnitude_b == 0.0 {
        return Some(0.0);
    }

    Some(dot_product / (magnitude_a * magnitude_b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cosine_similarity() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];
        assert!((cosine_similarity(&a, &b).unwrap() - 1.0).abs() < 1e-6);

        let a = vec![1.0, 0.0];
        let b = vec![0.0, 1.0];
        assert!((cosine_similarity(&a, &b).unwrap() - 0.0).abs() < 1e-6);
    }

    #[test]
    fn test_cosine_similarity_edge_cases() {
        // Empty vectors
        let a: Vec<f32> = vec![];
        let b: Vec<f32> = vec![];
        assert_eq!(cosine_similarity(&a, &b), None);

        // Mismatched lengths
        let a = vec![1.0];
        let b = vec![1.0, 0.0];
        assert_eq!(cosine_similarity(&a, &b), None);

        // Zero vectors
        let a = vec![0.0, 0.0];
        let b = vec![0.0, 0.0];
        assert_eq!(cosine_similarity(&a, &b), Some(0.0));
    }
}
