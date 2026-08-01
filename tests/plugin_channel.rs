//! A plugin can add a channel or a tool without touching core.
//!
//! The value being protected here is that registration reaches the thing that
//! consumes it: `register_channel` must reach the registry `--channel`
//! consults, and `register_tool` must reach the agent's tool list. Both were
//! once accepted, logged, and dropped — "the plugin API accepted it" is not
//! evidence that anything downstream can use it.

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

// ───────────────────────── plugin tools ─────────────────────────

struct Barometer;

#[async_trait]
impl apollo::tools::Tool for Barometer {
    fn name(&self) -> &str {
        "barometer"
    }
    fn spec(&self) -> apollo::tools::ToolSpec {
        apollo::tools::ToolSpec {
            name: "barometer".to_string(),
            description: "reads the pressure".to_string(),
            parameters: serde_json::json!({"type": "object", "properties": {}}),
        }
    }
    async fn execute(&self, _arguments: &str) -> anyhow::Result<apollo::tools::ToolResult> {
        Ok(apollo::tools::ToolResult::success("1013 hPa"))
    }
}

struct ToolPlugin;

#[async_trait]
impl Plugin for ToolPlugin {
    fn name(&self) -> &str {
        "tool-plugin"
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
        ctx.register_tool(Arc::new(Barometer));
    }
}

#[tokio::test]
async fn plugin_registered_tool_is_kept_not_dropped() {
    let mut registry = PluginRegistry::new();
    assert!(registry.tools().is_empty());

    registry.register(Arc::new(ToolPlugin)).await;

    let names: Vec<&str> = registry.tools().iter().map(|t| t.name()).collect();
    assert_eq!(
        names,
        vec!["barometer"],
        "register_tool did not survive PluginRegistry::register"
    );

    // And it is the real tool, not a placeholder.
    let result = registry.tools()[0].execute("{}").await.unwrap();
    assert_eq!(result.output, "1013 hPa");
    assert!(!result.is_error);
}

#[tokio::test]
async fn host_plugin_discovery_is_retained() {
    let dir = tempfile::tempdir().unwrap();
    let plugin = dir.path().join("plugins/demo");
    std::fs::create_dir_all(&plugin).unwrap();
    std::fs::write(
        plugin.join("SKILL.md"),
        "---\nname: demo\ndescription: d\n---\n",
    )
    .unwrap();

    let mut registry = PluginRegistry::new();
    registry.ingest_host_plugins(dir.path(), &[]);

    assert_eq!(
        registry.host_plugins().len(),
        1,
        "discovered host plugins were logged and thrown away"
    );
    assert_eq!(registry.host_plugins()[0].name.as_deref(), Some("demo"));
}

/// The assertion that actually matters: a plugin tool has to arrive in the
/// agent's own tool list, not merely survive inside the plugin registry.
#[tokio::test]
async fn plugin_tool_reaches_the_agent_tool_list() {
    use apollo::agent::AgentRunner;
    use apollo::memory::surreal::SurrealMemory;
    use apollo::providers::traits::{ChatRequest, ChatResponse, Provider, ProviderCapabilities};

    struct SilentProvider;

    #[async_trait]
    impl Provider for SilentProvider {
        fn name(&self) -> &str {
            "silent"
        }
        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities::default()
        }
        async fn chat(&self, _req: &ChatRequest<'_>) -> anyhow::Result<ChatResponse> {
            anyhow::bail!("not called")
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let memory = SurrealMemory::new(dir.path()).await.unwrap();

    let runner = AgentRunner::new(
        Arc::new(SilentProvider) as Arc<dyn Provider>,
        Vec::new(),
        Arc::new(memory),
        "system",
        "test-model",
    );
    assert!(runner.tools.read().await.is_empty());

    let mut registry = PluginRegistry::new();
    registry.register(Arc::new(ToolPlugin)).await;
    let runner = runner.with_plugin_registry(registry).await;

    let names: Vec<String> = runner
        .tools
        .read()
        .await
        .iter()
        .map(|t| t.name().to_string())
        .collect();
    assert!(
        names.contains(&"barometer".to_string()),
        "plugin tool never reached the agent; agent has {names:?}"
    );
}
