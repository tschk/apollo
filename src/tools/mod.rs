//! Tool abstraction — agent capabilities matching OpenClaw's tool set.
//!
//! Core tools (OpenClaw parity):
//!   group:runtime  — exec (shell commands)
//!   group:fs       — Read, Write, Edit
//!   group:web      — web_search, web_fetch
//!   group:memory   — memory_search, memory_get
//!   group:sessions — session_status, list_models
//!   group:messaging — message (Telegram send/edit/delete)

pub mod brief;
pub mod browser;
pub mod claude_usage;
pub mod coding_swarm;
pub mod config_tool;
pub mod cron_tool;
pub mod doctor;
pub mod dynamic;
pub mod edit;
pub mod embeddings;
pub mod file_ops;
pub mod guardrails;
pub mod mcp;
pub mod message;
pub mod mode_switch;
pub mod network;
#[cfg(feature = "peekaboo")]
pub mod peekaboo;
#[cfg(feature = "rs-gbrain")]
pub mod rs_gbrain;
pub mod sandbox;
pub mod session;
pub mod shell;
pub mod skill_manager;
pub mod sleep_tool;
pub mod todo_write;
pub mod tool_search;
pub mod toolsets;
pub mod traits;
pub mod vibemania;
pub mod web_fetch;
pub mod web_search;
pub mod worktree;

pub use brief::BriefTool;
pub use coding_swarm::CodingSwarmTool;
pub use config_tool::ConfigTool;
pub use cron_tool::CronTool;
#[cfg(feature = "peekaboo")]
pub use peekaboo::PeekabooTool;
#[cfg(feature = "rs-gbrain")]
pub use rs_gbrain::{BrainGetTool, BrainPutTool, BrainQueryTool, BrainSearchTool};
pub use sleep_tool::SleepTool;
pub use todo_write::TodoWriteTool;
pub use tool_search::ToolSearchTool;
pub use traits::{Tool, ToolResult, ToolSpec};
pub use vibemania::VibemaniaTool;
pub use worktree::WorktreeTool;
