//! A plugin can add a channel without touching core.
//!
//! The value being protected here is that `register_channel` reaches the
//! registry `--channel` consults. Plugin-registered *tools* are still logged
//! and dropped in `PluginRegistry::register`, so "the plugin API accepted it"
//! is not evidence that anything downstream can use it.

use apollo::channels::{Channel, ChannelSettings, IncomingMessage, OutgoingMessage};
use apollo::plugin::{Plugin, PluginContext, PluginRegistry};
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::mpsc;

struct CarrierPigeon {
    roost: String,
}

#[async_trait]
impl Channel for CarrierPigeon {
    fn name(&self) -> &str {
        "pigeon"
    }

    async fn start(&mut self) -> anyhow::Result<mpsc::Receiver<IncomingMessage>> {
        let (_tx, rx) = mpsc::channel(1);
        Ok(rx)
    }

    async fn send(&self, _message: OutgoingMessage) -> anyhow::Result<Option<String>> {
        Ok(Some(self.roost.clone()))
    }

    async fn stop(&mut self) -> anyhow::Result<()> {
        Ok(())
    }
}

struct PigeonPlugin;

#[async_trait]
impl Plugin for PigeonPlugin {
    fn name(&self) -> &str {
        "pigeon-plugin"
    }
    fn version(&self) -> &str {
        "1.0.0"
    }
    fn methods(&self) -> Vec<apollo::plugin::MethodSpec> {
        Vec::new()
    }
    async fn call(
        &self,
        _method: &str,
        _params: serde_json::Value,
    ) -> Result<serde_json::Value, apollo::plugin::PluginError> {
        Err(apollo::plugin::PluginError::new(-1, "no methods"))
    }

    async fn on_register(&self, ctx: &mut PluginContext) {
        ctx.register_channel("pigeon", |settings| {
            Ok(Box::new(CarrierPigeon {
                roost: settings.require("roost")?,
            }))
        });
    }
}

#[tokio::test]
async fn plugin_registered_channel_is_selectable_by_name() {
    let mut registry = PluginRegistry::new();
    assert!(
        !registry.channels().contains("pigeon"),
        "pigeon must not exist before the plugin registers it"
    );

    registry.register(Arc::new(PigeonPlugin)).await;

    assert!(
        registry.channels().contains("pigeon"),
        "register_channel did not reach the channel registry"
    );
    assert!(
        registry.channels().names().iter().any(|n| n == "pigeon"),
        "plugin channel missing from the name list users see"
    );

    let settings = ChannelSettings::new(
        None,
        [("roost".to_string(), "belfry".to_string())]
            .into_iter()
            .collect(),
    );
    let channel = registry
        .channels()
        .build("pigeon", &settings)
        .expect("plugin channel should build");

    assert_eq!(channel.name(), "pigeon");
    let sent = channel
        .send(OutgoingMessage {
            chat_id: "sky".to_string(),
            text: "hello".to_string(),
            reply_to: None,
        })
        .await
        .unwrap();
    assert_eq!(
        sent.as_deref(),
        Some("belfry"),
        "settings did not reach the plugin's builder"
    );
}

#[tokio::test]
async fn plugin_channel_reports_missing_settings() {
    let mut registry = PluginRegistry::new();
    registry.register(Arc::new(PigeonPlugin)).await;

    // No `roost` anywhere — the builder's own require() must surface.
    let err = match registry
        .channels()
        .build("pigeon", &ChannelSettings::default())
    {
        Err(e) => e.to_string(),
        Ok(_) => panic!("built a pigeon with no roost"),
    };
    assert!(err.contains("roost"), "{err}");
}

#[tokio::test]
async fn builtin_channels_are_reachable_by_name() {
    // The registry exists because seven built-in channels used to compile,
    // pass conformance, and still be unselectable.
    let registry = PluginRegistry::new();
    let names = registry.channels().names();

    #[cfg(feature = "channel-slack")]
    assert!(names.contains(&"slack".to_string()), "{names:?}");
    #[cfg(feature = "channel-irc")]
    assert!(names.contains(&"irc".to_string()), "{names:?}");
    #[cfg(feature = "channel-matrix")]
    assert!(names.contains(&"matrix".to_string()), "{names:?}");
    #[cfg(feature = "channel-signal")]
    assert!(names.contains(&"signal".to_string()), "{names:?}");
    #[cfg(feature = "channel-whatsapp")]
    assert!(names.contains(&"whatsapp".to_string()), "{names:?}");
    #[cfg(feature = "channel-googlechat")]
    assert!(names.contains(&"googlechat".to_string()), "{names:?}");
    #[cfg(feature = "channel-msteams")]
    assert!(names.contains(&"msteams".to_string()), "{names:?}");
}
