//! A plain-text model reply must reach the caller.
//!
//! Channels without draft support (HTTP, MCP, CLI, swarm) get the assistant
//! reply as the return value of `handle_message`. Regression guard for the
//! turn returning an empty string while the text was silently dropped.

use std::sync::Arc;

use apollo::agent::mode::NullChannel;
use apollo::agent::AgentRunner;
use apollo::channels::{
    Channel, DraftChannel as DraftChannelTrait, IncomingMessage, OutgoingMessage,
};
use apollo::memory::surreal::SurrealMemory;
use apollo::providers::traits::{ChatRequest, ChatResponse, Provider, ProviderCapabilities};
use async_trait::async_trait;

const REPLY: &str = "Ok! How can I assist you today?";

struct TextOnlyProvider;

#[async_trait]
impl Provider for TextOnlyProvider {
    fn name(&self) -> &str {
        "text-only"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            native_tools: false,
            streaming: false,
            vision: false,
            max_context: 32_000,
            native_web_search: false,
        }
    }

    async fn chat(&self, _request: &ChatRequest<'_>) -> anyhow::Result<ChatResponse> {
        Ok(ChatResponse {
            text: Some(REPLY.to_string()),
            tool_calls: vec![],
            usage: None,
        })
    }
}

struct DraftChannel;

#[async_trait]
impl Channel for DraftChannel {
    fn name(&self) -> &str {
        "draft"
    }

    async fn start(&mut self) -> anyhow::Result<tokio::sync::mpsc::Receiver<IncomingMessage>> {
        let (_tx, rx) = tokio::sync::mpsc::channel(1);
        Ok(rx)
    }

    async fn send(&self, _message: OutgoingMessage) -> anyhow::Result<Option<String>> {
        Ok(None)
    }

    fn as_draft(&self) -> Option<&dyn DraftChannelTrait> {
        Some(self)
    }

    async fn stop(&mut self) -> anyhow::Result<()> {
        Ok(())
    }
}

#[async_trait]
impl DraftChannelTrait for DraftChannel {
    async fn send_draft(&self, _chat_id: &str, _text: &str) -> anyhow::Result<Option<String>> {
        Ok(Some("draft-1".to_string()))
    }

    async fn update_draft_progress(
        &self,
        _chat_id: &str,
        _message_id: &str,
        _text: &str,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    async fn finalize_draft(
        &self,
        _chat_id: &str,
        _message_id: &str,
        _text: &str,
    ) -> anyhow::Result<()> {
        Ok(())
    }
}

async fn runner() -> AgentRunner {
    let dir = tempfile::tempdir().unwrap();
    let memory = SurrealMemory::new(dir.path()).await.unwrap();
    std::mem::forget(dir);
    AgentRunner::new(
        Arc::new(TextOnlyProvider),
        vec![],
        Arc::new(memory),
        "test",
        "test-model",
    )
}

fn message(chat_id: &str) -> IncomingMessage {
    IncomingMessage {
        id: "m1".to_string(),
        sender_id: "test".to_string(),
        sender_name: None,
        chat_id: chat_id.to_string(),
        text: "say ok".to_string(),
        is_group: false,
        reply_to: None,
        timestamp: chrono::Utc::now(),
    }
}

#[tokio::test]
async fn plain_text_reply_is_returned_to_non_draft_channels() {
    let runner = runner().await;
    let response = runner
        .handle_message(&message("plain"), &NullChannel::new("test"))
        .await
        .unwrap();
    assert_eq!(response, REPLY);
}

#[tokio::test]
async fn draft_channels_get_an_empty_return_to_avoid_double_send() {
    let runner = runner().await;
    let response = runner
        .handle_message(&message("draft"), &DraftChannel)
        .await
        .unwrap();
    assert!(response.is_empty(), "draft channel got {response:?}");
}
