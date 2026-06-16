//! Agent — the autonomous agent loop.
//! Receives messages, uses tools, responds via channels.
//! Inspired by HiClaw's Manager/Worker pattern.

pub mod compaction;
pub mod hooks;
pub mod loop_runner;
pub mod mode;
pub mod stream;
pub mod streaming;

pub use loop_runner::AgentRunner;
pub use mode::{agent_mode_from_permission_profile, AgentMode, NullChannel, PendingPlan};
pub use streaming::{stream_channel, StreamChunk, StreamReceiver, StreamSender};
