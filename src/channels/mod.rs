//! Channel abstraction — communication interfaces
//! Enable only what you need via Cargo features.

pub mod formatting;
pub mod http_inject;
pub mod traits;

// Core channels (always available or feature-gated)
#[cfg(feature = "channel-cli")]
pub mod cli;
#[cfg(feature = "channel-discord")]
pub mod discord;
#[cfg(feature = "channel-googlechat")]
pub mod googlechat;
#[cfg(feature = "channel-irc")]
pub mod irc;
#[cfg(feature = "channel-matrix")]
pub mod matrix;
#[cfg(feature = "channel-msteams")]
pub mod msteams;
#[cfg(feature = "channel-signal")]
pub mod signal;
#[cfg(feature = "channel-slack")]
pub mod slack;
#[cfg(feature = "channel-telegram")]
pub mod telegram;
#[cfg(feature = "channel-whatsapp")]
pub mod whatsapp;

pub use traits::{Channel, Delivery, DraftChannel, IncomingMessage, OutgoingMessage};
