//! unthinkclaw — Lightweight agent runtime
//! Successor to OpenClaw. Best-of-breed from ZeroClaw, NanoClaw, HiClaw.
//!
//! Core traits (all swappable):
//! - `Provider` — LLM backend (Anthropic, OpenAI, Gemini, Ollama, OpenRouter, Groq)
//! - `Channel` — Communication (CLI, Telegram, Discord, Matrix, WebSocket)
//! - `Tool` — Agent capability (Shell, File I/O, Vibemania, custom)
//! - `MemoryBackend` — Persistent SurrealDB state, vector embeddings, file-based tooling
//! - `RuntimeAdapter` — Execution (Native, Docker, WASM planned)
//!
//! Embeddings — Vector search for semantic memory
//! Swarms — Manager/Worker pattern for parallel execution
//! Plugins — JSON-RPC 2.0 extensibility
//! Cost — Token counting and billing (Phase 4)
//! Scheduler — Cron-based task automation (Phase 4)

pub mod agent;
pub mod agent_http;
pub mod bootstrap;
pub mod channels;
pub mod claw_adapter;
pub mod context;
pub mod config;
pub mod cost;
pub mod cron_scheduler;
pub mod diagnostics;
pub mod embeddings;
pub mod heartbeat;
pub mod mcp;
pub mod mcp_server;
pub mod memory;
pub mod plugin;
pub mod policy;
pub mod prompt;
pub mod providers;
pub mod runtime;
pub mod scheduler;
pub mod self_update;
pub mod skills;
pub mod streaming_parser;
#[cfg(feature = "swarm")]
pub mod swarm;
pub mod telegram_runtime;
pub mod text;
pub mod tools;
pub mod trajectory;
pub mod autonomous;

pub use agent::{AgentMode, AgentRunner};
pub use channels::Channel;
pub use cost::CostTracker;
pub use scheduler::Scheduler;
#[cfg(feature = "swarm")]
pub use swarm::SwarmCoordinator;
#[cfg(feature = "swarm")]
pub use swarm::{ConcurrencyScheduler, DelegationManager, HandoffManager, TeamManager};
