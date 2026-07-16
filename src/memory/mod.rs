//! Memory abstraction — persistent agent state.
//! Inspired by ZeroClaw's pluggable memory + NanoClaw's per-group isolation.

pub mod brief;
pub mod context_inject;
pub mod embeddings;
pub mod graph;
pub mod principal;
pub mod recall;
pub mod search;
pub mod session_note;
pub mod surreal;
pub mod traits;

pub use traits::MemoryBackend;
