use std::sync::Arc;

use serde::Serialize;

use crate::channels::Channel;
use crate::providers::Provider;
use crate::tools::Tool;

pub const CAPABILITY_MANIFEST_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CapabilityManifest {
    pub version: u32,
    pub provider: ProviderCapability,
    pub channel: ChannelCapability,
    pub tools: Vec<ToolCapability>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProviderCapability {
    pub name: String,
    pub native_tools: bool,
    pub streaming: bool,
    pub vision: bool,
    pub max_context: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChannelCapability {
    pub name: String,
    pub draft_updates: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ToolCapability {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

impl CapabilityManifest {
    pub fn discover(
        provider: &dyn Provider,
        channel: &dyn Channel,
        tools: &[Arc<dyn Tool>],
    ) -> Self {
        let provider_capabilities = provider.capabilities();
        let mut tool_capabilities = tools
            .iter()
            .map(|tool| {
                let spec = tool.spec();
                ToolCapability {
                    name: spec.name,
                    description: spec.description,
                    parameters: spec.parameters,
                }
            })
            .collect::<Vec<_>>();
        tool_capabilities.sort_unstable_by(|left, right| left.name.cmp(&right.name));

        Self {
            version: CAPABILITY_MANIFEST_VERSION,
            provider: ProviderCapability {
                name: provider.name().to_string(),
                native_tools: provider_capabilities.native_tools,
                streaming: provider_capabilities.streaming,
                vision: provider_capabilities.vision,
                max_context: provider_capabilities.max_context,
            },
            channel: ChannelCapability {
                name: channel.name().to_string(),
                draft_updates: channel.supports_draft_updates(),
            },
            tools: tool_capabilities,
        }
    }
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use tokio::sync::mpsc;

    use super::*;
    use crate::channels::{IncomingMessage, OutgoingMessage};
    use crate::providers::traits::ProviderCapabilities;
    use crate::providers::{ChatRequest, ChatResponse};
    use crate::tools::{ToolResult, ToolSpec};

    struct TestProvider;

    #[async_trait]
    impl Provider for TestProvider {
        fn name(&self) -> &str {
            "test-provider"
        }

        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                native_tools: true,
                streaming: false,
                vision: true,
                max_context: 128_000,
            }
        }

        async fn chat(&self, _request: &ChatRequest<'_>) -> anyhow::Result<ChatResponse> {
            Ok(ChatResponse::default())
        }
    }

    struct TestChannel;

    #[async_trait]
    impl Channel for TestChannel {
        fn name(&self) -> &str {
            "test-channel"
        }

        async fn start(&mut self) -> anyhow::Result<mpsc::Receiver<IncomingMessage>> {
            let (_sender, receiver) = mpsc::channel(1);
            Ok(receiver)
        }

        async fn send(&self, _message: OutgoingMessage) -> anyhow::Result<Option<String>> {
            Ok(None)
        }

        fn supports_draft_updates(&self) -> bool {
            true
        }

        async fn stop(&mut self) -> anyhow::Result<()> {
            Ok(())
        }
    }

    struct TestTool(&'static str);

    #[async_trait]
    impl Tool for TestTool {
        fn name(&self) -> &str {
            self.0
        }

        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: self.0.to_string(),
                description: format!("{} description", self.0),
                parameters: serde_json::json!({"type": "object"}),
            }
        }

        async fn execute(&self, _arguments: &str) -> anyhow::Result<ToolResult> {
            Ok(ToolResult::success(""))
        }
    }

    #[test]
    fn discovery_reports_active_runtime_capabilities_in_stable_order() {
        let tools: Vec<Arc<dyn Tool>> =
            vec![Arc::new(TestTool("zeta")), Arc::new(TestTool("alpha"))];

        let manifest = CapabilityManifest::discover(&TestProvider, &TestChannel, &tools);

        assert_eq!(manifest.version, CAPABILITY_MANIFEST_VERSION);
        assert_eq!(manifest.provider.name, "test-provider");
        assert!(manifest.provider.native_tools);
        assert!(manifest.provider.vision);
        assert_eq!(manifest.provider.max_context, 128_000);
        assert_eq!(manifest.channel.name, "test-channel");
        assert!(manifest.channel.draft_updates);
        assert_eq!(
            manifest
                .tools
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            ["alpha", "zeta"]
        );
    }

    #[test]
    fn discovery_serializes_as_a_versioned_contract() {
        let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(TestTool("alpha"))];
        let manifest = CapabilityManifest::discover(&TestProvider, &TestChannel, &tools);

        let value = serde_json::to_value(manifest).unwrap();

        assert_eq!(value["version"], 1);
        assert_eq!(value["provider"]["name"], "test-provider");
        assert_eq!(value["channel"]["name"], "test-channel");
        assert_eq!(value["tools"][0]["name"], "alpha");
    }
}
