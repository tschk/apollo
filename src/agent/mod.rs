//! Agent — the autonomous agent loop.
//! Receives messages, uses tools, responds via channels.
//! Inspired by HiClaw's Manager/Worker pattern.
//!
//! The loop itself is owned by the rx4 (rotary) harness via
//! `RotaryAgentBridge`; apollo owns everything around it.

pub mod build_runner;
pub mod compaction;
pub mod guardrail_store;
pub mod hooks;
pub mod loop_runner;
pub mod mode;
pub mod rotary_bridge;
pub mod stream;
pub mod streaming;

pub use guardrail_store::{ChatGuardrailHook, GuardrailStore};
pub use build_runner::{
    BuildResult, BuildRunner, BuildRunnerConfig, CompileError, DiagnosticSeverity,
};
pub use loop_runner::AgentRunner;
pub use mode::{agent_mode_from_permission_profile, AgentMode, NullChannel};
pub use rotary_bridge::{
    build_rx4_skill_engine, chat_message_to_rx4, register_apollo_tools, rx4_message_to_chat,
    RotaryAgentBridge, RotaryBridgeConfig, RotaryProviderAdapter, ToolHookContext,
};
pub use streaming::{stream_channel, StreamChunk, StreamReceiver, StreamSender};
