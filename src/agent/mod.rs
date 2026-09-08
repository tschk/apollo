//! Agent — the autonomous agent loop.
//! Receives messages, uses tools, responds via channels.
//! Inspired by HiClaw's Manager/Worker pattern.
//!
//! The loop itself is owned by the rx4 (rotary) harness via
//! `RotaryAgentBridge`; apollo owns everything around it.

pub mod build_runner;
pub mod compaction;
pub mod hooks;
pub mod loop_runner;
pub mod mode;
pub mod rotary_bridge;
pub mod stream;
pub mod streaming;

pub use build_runner::{
    BuildResult, BuildRunner, BuildRunnerConfig, CompileError, DiagnosticSeverity,
};
pub use loop_runner::AgentRunner;
pub use mode::{agent_mode_from_permission_profile, AgentMode, NullChannel};
pub use rotary_bridge::{
    apply_recorded_steps, build_rx4_skill_engine, chat_message_to_rx4, record_failure_notice,
    record_recovery_action, record_recovery_kind, record_rx4_event, record_rx4_event_value,
    record_spill_notice, record_tool_spill, register_apollo_tools, runtime_pty_worker,
    rx4_message_to_chat, RotaryAgentBridge, RotaryBridgeConfig, RotaryProviderAdapter,
    Rx4TrajectoryRecorder, ToolHookContext,
};
pub use streaming::{stream_channel, StreamChunk, StreamReceiver, StreamSender};
