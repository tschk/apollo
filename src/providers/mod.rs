//! LLM Provider abstraction — swap backends without changing agent logic.
//! Enable only what you need via Cargo features.

pub mod defaults;
pub mod retry;
pub mod traits;

// Always available
pub mod oauth;

#[cfg(feature = "provider-anthropic")]
pub mod anthropic;
#[cfg(feature = "provider-copilot")]
pub mod copilot;
// OpenAI-compat covers: openai, openrouter, groq, together, mistral, deepseek,
// fireworks, perplexity, xai, moonshot, venice, huggingface, siliconflow,
// cerebras, minimax, vercel, cloudflare, and any custom endpoint
#[cfg(feature = "provider-ollama")]
pub mod ollama;
pub mod openai_compat;

#[cfg(feature = "rs-ai")]
pub mod rs_ai;
// Shared login store, common to apollo and telekinesis.
#[cfg(feature = "rs-ai")]
pub mod shared_credentials;

pub use traits::{ChatMessage, ChatRequest, ChatResponse, Provider, ToolCall};
