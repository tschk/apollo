//! Agent — the autonomous agent loop.
//! Receives messages, uses tools, responds via channels.
//! Inspired by HiClaw's Manager/Worker pattern.
//!
//! The core agent loop can run via either:
//! - `AgentRunner` — apollo's built-in loop (default, legacy)
//! - `RotaryAgentBridge` — delegates to rx4 (rotary) agent harness engine

pub mod compaction;
pub mod hooks;
pub mod loop_runner;
pub mod mode;
pub mod rotary_bridge;
pub mod stream;
pub mod streaming;

pub use loop_runner::AgentRunner;
pub use mode::{agent_mode_from_permission_profile, AgentMode, NullChannel, PendingPlan};
pub use rotary_bridge::{
    build_rx4_skill_engine, chat_message_to_rx4, chat_messages_to_rx4, discover_skills_via_rx4,
    match_skill_via_rx4, register_apollo_tools, rx4_message_to_chat, tool_specs_to_rx4_json,
    RotaryAgentBridge, RotaryBridgeConfig, RotaryMemoryBridge, RotaryProviderAdapter,
    ToolHookContext,
};
pub use streaming::{stream_channel, StreamChunk, StreamReceiver, StreamSender};
