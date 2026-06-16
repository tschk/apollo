use async_trait::async_trait;
use tokio::sync::mpsc;

use super::traits::{Channel, IncomingMessage, OutgoingMessage};

pub struct HttpInjectChannel;

impl Default for HttpInjectChannel {
    fn default() -> Self {
        Self::new()
    }
}

impl HttpInjectChannel {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Channel for HttpInjectChannel {
    fn name(&self) -> &str {
        "http"
    }

    async fn start(&mut self) -> anyhow::Result<mpsc::Receiver<IncomingMessage>> {
        let (_tx, rx) = mpsc::channel(1);
        Ok(rx)
    }

    async fn send(&self, message: OutgoingMessage) -> anyhow::Result<Option<String>> {
        tracing::debug!(target: "unthinkclaw::http", "reply: {}", message.text);
        Ok(None)
    }

    async fn stop(&mut self) -> anyhow::Result<()> {
        Ok(())
    }
}
