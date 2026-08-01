//! Channel abstraction — communication interfaces
//! Enable only what you need via Cargo features.

pub mod formatting;
pub mod http_inject;
pub mod media;
pub mod registry;
pub mod traits;
#[cfg(any(
    feature = "channel-googlechat",
    feature = "channel-msteams",
    feature = "channel-whatsapp"
))]
pub mod webhook;

// Core channels (always available or feature-gated)
#[cfg(feature = "channel-cli")]
pub mod cli;
#[cfg(feature = "channel-discord")]
pub mod discord;
#[cfg(feature = "channel-discord")]
pub mod discord_gateway;
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

pub use registry::{ChannelBuilder, ChannelRegistry, ChannelSettings};
pub use traits::{
    Channel, Delivery, DraftChannel, IncomingMessage, MediaKind, MediaSource, OutgoingMedia,
    OutgoingMessage,
};
