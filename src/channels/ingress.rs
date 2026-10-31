//! Inbound allowlists for every channel.
//!
//! `[channel].allowed_chat_ids` and `allowed_sender_ids` used to be honoured
//! only by Telegram, so the same config on Discord, Slack or a webhook channel
//! silently admitted anyone who could post there — straight into an agent with
//! shell access. `FilteredChannel` applies them to any `Channel` at the one
//! place all inbound messages pass: the receiver `start()` returns.

use async_trait::async_trait;
use tokio::sync::mpsc;

use super::traits::{Channel, DraftChannel, IncomingMessage, OutgoingMedia, OutgoingMessage};

#[derive(Clone, Debug, Default)]
pub struct IngressFilter {
    pub allowed_chat_ids: Vec<String>,
    pub allowed_sender_ids: Vec<String>,
}

impl IngressFilter {
    pub fn new(allowed_chat_ids: Vec<String>, allowed_sender_ids: Vec<String>) -> Self {
        if allowed_chat_ids
            .iter()
            .chain(&allowed_sender_ids)
            .any(|x| x.trim().is_empty())
        {
            tracing::warn!("channel allowlist contains a blank entry; it matches nothing");
        }
        Self {
            allowed_chat_ids,
            allowed_sender_ids,
        }
    }

    pub fn is_open(&self) -> bool {
        self.allowed_chat_ids.is_empty() && self.allowed_sender_ids.is_empty()
    }

    /// An empty list admits everything on that axis. A non-empty list must
    /// match — including an empty sender id, which fails closed rather than
    /// bypassing the sender list. Blank entries in a list never match: `[""]`
    /// is a configured list that admits nobody, not an open one.
    pub fn allows(&self, chat_id: &str, sender_id: &str) -> bool {
        let listed = |list: &[String], id: &str| {
            list.is_empty() || list.iter().any(|x| !x.trim().is_empty() && x == id)
        };
        listed(&self.allowed_chat_ids, chat_id) && listed(&self.allowed_sender_ids, sender_id)
    }
}

/// Wraps a channel so only messages its `IngressFilter` admits are delivered.
pub struct FilteredChannel {
    inner: Box<dyn Channel>,
    filter: IngressFilter,
}

impl FilteredChannel {
    pub fn new(inner: Box<dyn Channel>, filter: IngressFilter) -> Self {
        Self { inner, filter }
    }
}

#[async_trait]
impl Channel for FilteredChannel {
    fn name(&self) -> &str {
        self.inner.name()
    }

    async fn start(&mut self) -> anyhow::Result<mpsc::Receiver<IncomingMessage>> {
        let mut upstream = self.inner.start().await?;
        if self.filter.is_open() {
            tracing::warn!(
                "{}: no allowed_chat_ids/allowed_sender_ids configured; accepting messages from anyone",
                self.inner.name()
            );
            return Ok(upstream);
        }
        let (tx, rx) = mpsc::channel(32);
        let filter = self.filter.clone();
        let name = self.inner.name().to_string();
        tokio::spawn(async move {
            while let Some(msg) = upstream.recv().await {
                if !filter.allows(&msg.chat_id, &msg.sender_id) {
                    tracing::warn!(
                        "{name} ingress denied chat={} sender={}",
                        msg.chat_id,
                        msg.sender_id
                    );
                    continue;
                }
                if tx.send(msg).await.is_err() {
                    break;
                }
            }
        });
        Ok(rx)
    }

    async fn send(&self, message: OutgoingMessage) -> anyhow::Result<Option<String>> {
        self.inner.send(message).await
    }

    async fn send_typing(&self, chat_id: &str) -> anyhow::Result<()> {
        self.inner.send_typing(chat_id).await
    }

    async fn send_media(&self, media: OutgoingMedia) -> anyhow::Result<Option<String>> {
        self.inner.send_media(media).await
    }

    fn supports_media(&self) -> bool {
        self.inner.supports_media()
    }

    async fn edit(&self, chat_id: &str, message_id: &str, new_text: &str) -> anyhow::Result<()> {
        self.inner.edit(chat_id, message_id, new_text).await
    }

    fn as_draft(&self) -> Option<&dyn DraftChannel> {
        self.inner.as_draft()
    }

    async fn stop(&mut self) -> anyhow::Result<()> {
        self.inner.stop().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scripted(Vec<(&'static str, &'static str)>);

    #[async_trait]
    impl Channel for Scripted {
        fn name(&self) -> &str {
            "scripted"
        }
        async fn start(&mut self) -> anyhow::Result<mpsc::Receiver<IncomingMessage>> {
            let (tx, rx) = mpsc::channel(8);
            for (chat, sender) in self.0.drain(..) {
                tx.send(IncomingMessage {
                    id: format!("{chat}/{sender}"),
                    sender_id: sender.into(),
                    sender_name: None,
                    chat_id: chat.into(),
                    text: "hi".into(),
                    is_group: false,
                    reply_to: None,
                    timestamp: chrono::Utc::now(),
                })
                .await?;
            }
            Ok(rx)
        }
        async fn send(&self, _: OutgoingMessage) -> anyhow::Result<Option<String>> {
            Ok(None)
        }
        async fn stop(&mut self) -> anyhow::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn only_listed_chats_and_senders_get_through() {
        let inner = Scripted(vec![
            ("c1", "alice"),   // allowed
            ("c1", "mallory"), // wrong sender
            ("c2", "alice"),   // wrong chat
            ("c1", ""),        // missing sender must not bypass the list
            ("c1", "bob"),     // allowed
        ]);
        let mut ch = FilteredChannel::new(
            Box::new(inner),
            IngressFilter::new(vec!["c1".into()], vec!["alice".into(), "bob".into()]),
        );
        let mut rx = ch.start().await.unwrap();
        let mut got = Vec::new();
        while let Some(m) = rx.recv().await {
            got.push(m.id);
        }
        assert_eq!(got, vec!["c1/alice", "c1/bob"]);
    }

    #[test]
    fn a_blank_allowlist_entry_admits_nobody() {
        let f = IngressFilter::new(vec![], vec!["".into(), " ".into()]);
        assert!(!f.allows("c1", ""));
        assert!(!f.allows("c1", " "));
        assert!(!f.allows("c1", "alice"));
    }
}
