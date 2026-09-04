//! Tool abstraction — agent capabilities matching OpenClaw's tool set.
//!
//! Core tools (OpenClaw parity):
//!   group:runtime  — exec (shell commands)
//!   group:fs       — Read, Write, Edit
//!   group:web      — web_search, web_fetch
//!   group:memory   — memory_search, memory_get
//!   group:sessions — session_status
//!   group:messaging — message (Telegram send/edit/delete)

pub mod brief;
#[cfg(feature = "plugin-browser")]
pub mod browser;
pub mod build_runner;
pub mod child_proc;
#[cfg(feature = "swarm")]
pub mod coding_swarm;
pub mod config_tool;
pub mod confine;
pub mod cron_tool;
pub mod doctor;
pub mod dynamic;
pub mod edit;
pub mod embeddings;
pub mod file_ops;
pub mod guardrails;
#[cfg(feature = "plugin-advanced")]
pub mod mcp;
#[cfg(feature = "rs-ai")]
pub mod media;
#[cfg(feature = "channel-telegram")]
pub mod message;
pub mod mode_switch;
pub mod network;
#[cfg(feature = "computer-use-praefectus")]
pub mod praefectus;
pub mod pty_worker;
pub mod sandbox;
pub mod session;
pub mod shell;
#[cfg(feature = "plugin-skills")]
pub mod skill_manager;
#[cfg(feature = "plugin-skills")]
pub use skill_manager::SkillManagerTool;
pub mod sleep_tool;
pub mod telekinesis;
pub mod todo_write;
pub mod tool_search;
pub mod toolsets;
pub mod traits;
#[cfg(feature = "plugin-advanced")]
pub mod vibemania;
#[cfg(feature = "plugin-web")]
pub mod web_fetch;
#[cfg(feature = "plugin-web")]
pub mod web_search;
#[cfg(feature = "plugin-advanced")]
pub mod worktree;
#[cfg(feature = "zkr-memory")]
pub mod zkr;

pub use brief::BriefTool;
pub use build_runner::BuildRunnerTool;
#[cfg(feature = "swarm")]
pub use coding_swarm::CodingSwarmTool;
pub use config_tool::ConfigTool;
pub use cron_tool::CronTool;
pub use sleep_tool::SleepTool;
pub use telekinesis::TelekinesisTool;
pub use todo_write::TodoWriteTool;
pub use tool_search::ToolSearchTool;
pub use traits::{Tool, ToolResult, ToolSpec};
#[cfg(feature = "plugin-advanced")]
pub use vibemania::VibemaniaTool;
#[cfg(feature = "plugin-advanced")]
pub use worktree::WorktreeTool;
