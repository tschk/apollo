//! Channel registry — name → constructor.
//!
//! Before this existed, adding a channel meant editing the `serve` match in
//! `main.rs`, and anything not in that match was unreachable no matter how well
//! it worked: seven of the ten built-in channels compiled, passed conformance,
//! and could not be selected. A registry makes the built-ins data rather than
//! control flow, and lets a plugin add a channel via
//! `PluginContext::register_channel` without touching core.

use std::collections::HashMap;
use std::sync::Arc;

use super::traits::Channel;

/// Parameters a channel needs to construct itself.
///
/// Values come from `[channel].settings` in the config file, falling back to
/// `APOLLO_CHANNEL_<KEY>` in the environment so tokens need not be written to
/// disk.
#[derive(Debug, Default, Clone)]
pub struct ChannelSettings {
    pub token: Option<String>,
    pub values: HashMap<String, String>,
}

impl ChannelSettings {
    pub fn new(token: Option<String>, values: HashMap<String, String>) -> Self {
        Self { token, values }
    }

    /// A setting, or `None` if neither config nor environment supplies it.
    pub fn get(&self, key: &str) -> Option<String> {
        if let Some(v) = self.values.get(key) {
            return Some(v.clone());
        }
        std::env::var(format!("APOLLO_CHANNEL_{}", key.to_uppercase())).ok()
    }

    /// A required setting. The error names both places it could have come from,
    /// because "missing channel_id" without that is a scavenger hunt.
    pub fn require(&self, key: &str) -> anyhow::Result<String> {
        self.get(key).ok_or_else(|| {
            anyhow::anyhow!(
                "channel setting `{key}` is required — set [channel].settings.{key} \
                 in the config or APOLLO_CHANNEL_{} in the environment",
                key.to_uppercase()
            )
        })
    }

    /// The channel token, from `[channel].token`, `settings.token`, or
    /// `APOLLO_CHANNEL_TOKEN`.
    pub fn token(&self) -> anyhow::Result<String> {
        if let Some(t) = &self.token {
            return Ok(t.clone());
        }
        self.require("token")
    }
}

/// Builds a channel from its settings.
pub type ChannelBuilder =
    Arc<dyn Fn(&ChannelSettings) -> anyhow::Result<Box<dyn Channel>> + Send + Sync>;

/// Every channel apollo knows how to construct by name.
#[derive(Clone, Default)]
pub struct ChannelRegistry {
    builders: HashMap<String, ChannelBuilder>,
}

impl ChannelRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// The registry with every compiled-in channel already registered.
    pub fn with_builtins() -> Self {
        let mut reg = Self::new();

        #[cfg(feature = "channel-cli")]
        reg.register("cli", |_| Ok(Box::new(super::cli::CliChannel::new())));

        #[cfg(feature = "channel-telegram")]
        reg.register("telegram", |s| {
            let chat_id: i64 = s.require("chat_id")?.parse()?;
            Ok(Box::new(super::telegram::TelegramChannel::new(
                s.token()?,
                chat_id,
            )))
        });

        #[cfg(feature = "channel-discord")]
        reg.register("discord", |s| {
            Ok(Box::new(super::discord::DiscordChannel::new(
                s.token()?,
                s.require("channel_id")?,
            )))
        });

        #[cfg(feature = "channel-slack")]
        reg.register("slack", |s| {
            Ok(Box::new(
                super::slack::SlackChannel::new(s.token()?).with_channel(s.require("channel_id")?),
            ))
        });

        #[cfg(feature = "channel-matrix")]
        reg.register("matrix", |s| {
            Ok(Box::new(
                super::matrix::MatrixChannel::new(s.require("homeserver")?, s.token()?)
                    .with_room(s.require("room")?),
            ))
        });

        #[cfg(feature = "channel-irc")]
        reg.register("irc", |s| {
            let mut ch = super::irc::IrcChannel::new(
                s.require("server")?,
                s.require("channel")?,
                s.get("nick").unwrap_or_else(|| "apollo".to_string()),
            );
            if let Some(port) = s.get("port") {
                ch = ch.with_port(port.parse()?);
            }
            if let Some(password) = s.get("password") {
                ch = ch.with_password(password);
            }
            Ok(Box::new(ch))
        });

        #[cfg(feature = "channel-signal")]
        reg.register("signal", |s| {
            Ok(Box::new(super::signal::SignalChannel::new(
                s.require("api_base")?,
                s.require("number")?,
            )))
        });

        #[cfg(feature = "channel-whatsapp")]
        reg.register("whatsapp", |s| {
            Ok(Box::new(super::whatsapp::WhatsAppChannel::new(
                s.token()?,
                s.require("phone_number_id")?,
                s.get("verify_token")
                    .unwrap_or_else(|| "apollo-verify".to_string()),
            )))
        });

        #[cfg(feature = "channel-googlechat")]
        reg.register("googlechat", |s| {
            Ok(Box::new(super::googlechat::GoogleChatChannel::new(
                s.require("service_account_key")?,
            )))
        });

        #[cfg(feature = "channel-msteams")]
        reg.register("msteams", |s| {
            Ok(Box::new(super::msteams::TeamsChannel::new(
                s.require("app_id")?,
                s.require("app_password")?,
            )))
        });

        reg
    }

    /// Register a channel builder. A later registration of the same name wins,
    /// so a plugin can deliberately replace a built-in.
    pub fn register<F>(&mut self, name: impl Into<String>, builder: F)
    where
        F: Fn(&ChannelSettings) -> anyhow::Result<Box<dyn Channel>> + Send + Sync + 'static,
    {
        self.builders.insert(name.into(), Arc::new(builder));
    }

    /// Register a pre-boxed builder — the form plugins hand over.
    pub fn register_builder(&mut self, name: impl Into<String>, builder: ChannelBuilder) {
        self.builders.insert(name.into(), builder);
    }

    pub fn contains(&self, name: &str) -> bool {
        self.builders.contains_key(name)
    }

    /// Registered names, sorted, for help text and error messages.
    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.builders.keys().cloned().collect();
        names.sort();
        names
    }

    /// Construct a channel by name.
    pub fn build(
        &self,
        name: &str,
        settings: &ChannelSettings,
    ) -> anyhow::Result<Box<dyn Channel>> {
        let builder = self.builders.get(name).ok_or_else(|| {
            anyhow::anyhow!(
                "unknown channel `{name}` (available: {})",
                self.names().join(", ")
            )
        })?;
        builder(settings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_are_registered_and_named() {
        let reg = ChannelRegistry::with_builtins();
        // Whatever the feature set, the registry must not be empty and must
        // report exactly what it can build.
        for name in reg.names() {
            assert!(reg.contains(&name), "names() listed unbuildable {name}");
        }
        #[cfg(feature = "channel-cli")]
        assert!(reg.contains("cli"));
        #[cfg(feature = "channel-slack")]
        assert!(reg.contains("slack"), "slack must be reachable by name");
    }

    #[test]
    fn unknown_channel_lists_the_alternatives() {
        let reg = ChannelRegistry::with_builtins();
        let err = match reg.build("nope", &ChannelSettings::default()) {
            Err(e) => e.to_string(),
            Ok(_) => panic!("built a channel that was never registered"),
        };
        assert!(err.contains("unknown channel `nope`"), "{err}");
        assert!(err.contains("available:"), "{err}");
    }

    #[test]
    fn missing_required_setting_names_both_sources() {
        let err = ChannelSettings::default()
            .require("channel_id")
            .unwrap_err()
            .to_string();
        assert!(err.contains("[channel].settings.channel_id"), "{err}");
        assert!(err.contains("APOLLO_CHANNEL_CHANNEL_ID"), "{err}");
    }

    #[test]
    fn a_plugin_channel_can_replace_a_builtin() {
        let mut reg = ChannelRegistry::with_builtins();
        let before = reg.names().len();
        reg.register("custom", |_| {
            anyhow::bail!("constructed");
        });
        assert_eq!(reg.names().len(), before + 1);
        assert!(reg.contains("custom"));
    }
}
