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

pub struct EmbeddingsClient {
    api_key: String,
}

impl EmbeddingsClient {
    pub fn new(api_key: String) -> Self {
        Self { api_key }
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

        let client = reqwest::Client::new();
        let resp = client
            .post(url)
            .header("x-goog-api-key", &self.api_key)
            .json(&request)
            .send()
            .await?;

        let result: serde_json::Value = resp.json().await?;

        if let Some(embedding) = result
            .get("embedding")
            .and_then(|e| e.get("values"))
            .and_then(|v| v.as_array())
        {
            let vector: Vec<f32> = embedding
                .iter()
                .filter_map(|v| v.as_f64().map(|f| f as f32))
                .collect();
            Ok(vector)
        } else {
            Err(anyhow::anyhow!("Failed to extract embedding from response"))
        }
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

/// Cosine similarity between two vectors
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }

    let dot_product: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let magnitude_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let magnitude_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();

    if magnitude_a == 0.0 || magnitude_b == 0.0 {
        return 0.0;
    }

    dot_product / (magnitude_a * magnitude_b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cosine_similarity() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];
        assert_eq!(cosine_similarity(&a, &b), 1.0);

        let a = vec![1.0, 0.0];
        let b = vec![0.0, 1.0];
        assert_eq!(cosine_similarity(&a, &b), 0.0);
    }

    #[test]
    fn test_cosine_similarity_edge_cases() {
        // Empty vectors
        let a: Vec<f32> = vec![];
        let b: Vec<f32> = vec![];
        assert_eq!(cosine_similarity(&a, &b), 0.0);

        // Mismatched lengths
        let a = vec![1.0];
        let b = vec![1.0, 0.0];
        assert_eq!(cosine_similarity(&a, &b), 0.0);

        // Zero magnitude vectors
        let a = vec![0.0, 0.0];
        let b = vec![0.0, 0.0];
        assert_eq!(cosine_similarity(&a, &b), 0.0);

        // One zero magnitude vector
        let a = vec![1.0, 1.0];
        let b = vec![0.0, 0.0];
        assert_eq!(cosine_similarity(&a, &b), 0.0);
    }
}
