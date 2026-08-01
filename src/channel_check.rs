//! `apollo channel-check` — drive one channel against its **real** service.
//!
//! `tests/channel_conformance.rs` proves a channel is correct against a mock
//! built from the published API shapes. That closes "compiles" versus "works",
//! and nothing more: a wrong field name, a deprecated endpoint or a missing
//! scope all pass a mock happily and fail against the real service. Every
//! channel except Telegram and CLI shipped verified only that far.
//!
//! This closes the rest of the gap, but it needs credentials and a human to
//! echo a message, so it is a command rather than a test.

use std::time::Duration;

use crate::channels::{Channel, ChannelSettings, MediaKind, OutgoingMedia, OutgoingMessage};

/// One step's outcome, printed as it happens so a hang is attributable.
pub struct StepReport {
    pub name: &'static str,
    pub ok: bool,
    pub detail: String,
}

impl StepReport {
    fn pass(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            ok: true,
            detail: detail.into(),
        }
    }
    fn fail(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            ok: false,
            detail: detail.into(),
        }
    }

    fn print(&self) {
        let mark = if self.ok { "ok" } else { "FAIL" };
        println!("  [{mark}] {} — {}", self.name, self.detail);
    }
}

/// What the check needs beyond the channel itself.
pub struct CheckOptions {
    pub chat_id: Option<String>,
    pub wait: Duration,
    pub media: bool,
}

/// Run the check. Returns the steps, all of which must pass for the channel to
/// be considered verified against the real service.
pub async fn run(
    channel: &mut dyn Channel,
    settings: &ChannelSettings,
    opts: &CheckOptions,
) -> Vec<StepReport> {
    let mut steps = Vec::new();
    let name = channel.name().to_string();

    // A nonce makes the echo unambiguous: a channel that replays history or
    // loops its own messages back would otherwise look like a pass.
    let nonce = format!("apollo-check-{}", uuid::Uuid::new_v4().simple());

    let rx = match channel.start().await {
        Ok(rx) => {
            steps.push(StepReport::pass("start", "receiver opened"));
            Some(rx)
        }
        Err(e) => {
            steps.push(StepReport::fail("start", e.to_string()));
            None
        }
    };
    steps.last().unwrap().print();

    let chat_id = opts
        .chat_id
        .clone()
        .or_else(|| settings.get("chat_id"))
        .or_else(|| settings.get("channel_id"))
        .unwrap_or_default();

    // send
    let sent = channel
        .send(OutgoingMessage {
            chat_id: chat_id.clone(),
            text: format!("{name} check — please reply with: {nonce}"),
            reply_to: None,
        })
        .await;
    steps.push(match &sent {
        Ok(id) => StepReport::pass(
            "send",
            match id {
                Some(id) => format!("delivered, id {id}"),
                None => "delivered".to_string(),
            },
        ),
        Err(e) => StepReport::fail("send", e.to_string()),
    });
    steps.last().unwrap().print();

    // media, only if the channel claims to support it
    if opts.media {
        if channel.supports_media() {
            let path = std::env::temp_dir().join("apollo-channel-check.txt");
            let step = match tokio::fs::write(&path, b"apollo channel-check attachment\n").await {
                Err(e) => StepReport::fail("media", format!("could not write temp file: {e}")),
                Ok(()) => {
                    let media = OutgoingMedia::file(&chat_id, MediaKind::Document, &path)
                        .with_caption("apollo channel-check attachment");
                    match channel.send_media(media).await {
                        Ok(_) => StepReport::pass("media", "attachment uploaded"),
                        Err(e) => StepReport::fail("media", e.to_string()),
                    }
                }
            };
            let _ = tokio::fs::remove_file(&path).await;
            steps.push(step);
        } else {
            steps.push(StepReport::pass(
                "media",
                "channel reports no media support — skipped",
            ));
        }
        steps.last().unwrap().print();
    }

    // receive — the half a mock can never really prove
    if let Some(mut rx) = rx {
        println!(
            "  .... waiting {}s for you to reply with the nonce",
            opts.wait.as_secs()
        );
        let deadline = tokio::time::Instant::now() + opts.wait;
        let step = loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break StepReport::fail(
                    "receive",
                    format!("no message containing {nonce} within the timeout"),
                );
            }
            match tokio::time::timeout(remaining, rx.recv()).await {
                Ok(Some(msg)) if msg.text.contains(&nonce) => {
                    break StepReport::pass(
                        "receive",
                        format!("echo from {} in chat {}", msg.sender_id, msg.chat_id),
                    );
                }
                // Other traffic is fine; keep waiting for ours.
                Ok(Some(_)) => continue,
                Ok(None) => {
                    break StepReport::fail("receive", "receiver closed before the echo arrived")
                }
                Err(_) => {
                    break StepReport::fail(
                        "receive",
                        format!("no message containing {nonce} within the timeout"),
                    )
                }
            }
        };
        steps.push(step);
        steps.last().unwrap().print();
    }

    let _ = channel.stop().await;
    steps
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::IncomingMessage;
    use async_trait::async_trait;
    use tokio::sync::mpsc;

    /// A channel that echoes whatever it is sent, which is what a cooperative
    /// human does during a real check.
    struct EchoChannel {
        tx: Option<mpsc::Sender<IncomingMessage>>,
    }

    #[async_trait]
    impl Channel for EchoChannel {
        fn name(&self) -> &str {
            "echo"
        }
        async fn start(&mut self) -> anyhow::Result<mpsc::Receiver<IncomingMessage>> {
            let (tx, rx) = mpsc::channel(4);
            self.tx = Some(tx);
            Ok(rx)
        }
        async fn send(&self, message: OutgoingMessage) -> anyhow::Result<Option<String>> {
            if let Some(tx) = &self.tx {
                let _ = tx
                    .send(IncomingMessage {
                        id: "1".into(),
                        sender_id: "u".into(),
                        sender_name: None,
                        chat_id: message.chat_id.clone(),
                        text: message.text.clone(),
                        is_group: false,
                        reply_to: None,
                        timestamp: chrono::Utc::now(),
                    })
                    .await;
            }
            Ok(Some("m1".into()))
        }
        async fn stop(&mut self) -> anyhow::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn a_working_channel_passes_every_step() {
        let mut channel = EchoChannel { tx: None };
        let steps = run(
            &mut channel,
            &ChannelSettings::default(),
            &CheckOptions {
                chat_id: Some("c1".into()),
                wait: Duration::from_secs(5),
                media: false,
            },
        )
        .await;

        assert!(
            steps.iter().all(|s| s.ok),
            "{:?}",
            steps
                .iter()
                .map(|s| (s.name, s.ok, &s.detail))
                .collect::<Vec<_>>()
        );
        assert!(steps.iter().any(|s| s.name == "receive"));
    }

    /// A channel that sends nothing back must fail the receive step rather
    /// than reporting a clean run — that is the whole point of the command.
    struct SilentChannel;

    #[async_trait]
    impl Channel for SilentChannel {
        fn name(&self) -> &str {
            "silent"
        }
        async fn start(&mut self) -> anyhow::Result<mpsc::Receiver<IncomingMessage>> {
            let (_tx, rx) = mpsc::channel(1);
            Ok(rx)
        }
        async fn send(&self, _message: OutgoingMessage) -> anyhow::Result<Option<String>> {
            Ok(None)
        }
        async fn stop(&mut self) -> anyhow::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn a_deaf_channel_fails_the_receive_step() {
        let mut channel = SilentChannel;
        let steps = run(
            &mut channel,
            &ChannelSettings::default(),
            &CheckOptions {
                chat_id: Some("c1".into()),
                wait: Duration::from_millis(200),
                media: false,
            },
        )
        .await;

        let receive = steps.iter().find(|s| s.name == "receive").unwrap();
        assert!(!receive.ok, "a deaf channel must not pass");
        assert!(steps.iter().find(|s| s.name == "send").unwrap().ok);
    }
}
