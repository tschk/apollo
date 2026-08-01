//! Discord gateway websocket.
//!
//! REST polling can only ever see the one channel it is configured with, on a
//! two-second delay, and never sees DMs, reactions or edits. The gateway is
//! Discord's real ingress, so it is the default transport; polling stays
//! available behind `transport = "polling"` for a token whose privileged
//! intents have not been granted.
//!
//! Only what apollo needs is implemented: HELLO/heartbeat, IDENTIFY, and
//! MESSAGE_CREATE dispatch. There is no session RESUME — a dropped connection
//! reconnects and re-identifies, which loses messages sent during the gap.
//! That is a real limitation, not an oversight; RESUME needs session state and
//! a separate resume URL, and is worth adding when someone needs it.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message as WsMessage;

use super::traits::IncomingMessage;

/// GUILD_MESSAGES | DIRECT_MESSAGES | MESSAGE_CONTENT.
///
/// MESSAGE_CONTENT is privileged: it must be enabled on the bot's application
/// page or Discord closes the connection at IDENTIFY. Without it every
/// message arrives with empty content, which would look exactly like the
/// dropped-sender bug this replaced.
pub const DEFAULT_INTENTS: u64 = (1 << 9) | (1 << 12) | (1 << 15);

/// The public gateway, at the API version apollo speaks.
pub const DEFAULT_GATEWAY_URL: &str = "wss://gateway.discord.gg/?v=10&encoding=json";

/// Run the gateway until the receiver is dropped, reconnecting as needed.
pub fn spawn(token: String, gateway_url: String, tx: mpsc::Sender<IncomingMessage>) {
    tokio::spawn(async move {
        let mut backoff = Duration::from_secs(1);
        loop {
            match run_once(&token, &gateway_url, &tx).await {
                Ok(()) => {
                    // Clean close — reconnect promptly.
                    backoff = Duration::from_secs(1);
                }
                Err(e) => {
                    tracing::warn!("Discord gateway disconnected: {e}");
                }
            }
            if tx.is_closed() {
                return;
            }
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(Duration::from_secs(60));
        }
    });
}

async fn run_once(
    token: &str,
    gateway_url: &str,
    tx: &mpsc::Sender<IncomingMessage>,
) -> anyhow::Result<()> {
    let (stream, _) = tokio_tungstenite::connect_async(gateway_url).await?;
    let (sink, mut source) = stream.split();
    let sink = Arc::new(tokio::sync::Mutex::new(sink));
    let seq = Arc::new(AtomicI64::new(-1));
    let mut heartbeat: Option<tokio::task::JoinHandle<()>> = None;

    // Whatever ends this connection, the heartbeat task must not outlive it.
    let result = loop {
        let Some(frame) = source.next().await else {
            break Ok(());
        };
        let frame = frame?;
        let text = match frame {
            WsMessage::Text(t) => t.to_string(),
            WsMessage::Binary(_) => continue,
            WsMessage::Close(_) => break Ok(()),
            _ => continue,
        };
        let Ok(payload) = serde_json::from_str::<Value>(&text) else {
            continue;
        };

        if let Some(s) = payload["s"].as_i64() {
            seq.store(s, Ordering::SeqCst);
        }

        match payload["op"].as_u64() {
            // HELLO — start heartbeating, then identify.
            Some(10) => {
                let interval = payload["d"]["heartbeat_interval"].as_u64().unwrap_or(41250);
                heartbeat = Some(spawn_heartbeat(
                    Arc::clone(&sink),
                    Arc::clone(&seq),
                    interval,
                ));
                let identify = serde_json::json!({
                    "op": 2,
                    "d": {
                        "token": token,
                        "intents": DEFAULT_INTENTS,
                        "properties": {
                            "os": std::env::consts::OS,
                            "browser": "apollo",
                            "device": "apollo",
                        }
                    }
                });
                sink.lock()
                    .await
                    .send(WsMessage::text(identify.to_string()))
                    .await?;
            }
            // Server asked for a heartbeat right now.
            Some(1) => {
                send_heartbeat(&sink, &seq).await?;
            }
            // Reconnect / invalid session — drop and start over.
            Some(7) | Some(9) => break Ok(()),
            // Dispatch. Only MESSAGE_CREATE is acted on; the rest of the
            // event stream is deliberately ignored.
            Some(0) if payload["t"].as_str() == Some("MESSAGE_CREATE") => {
                if let Some(incoming) = parse_message_create(&payload["d"]) {
                    if tx.send(incoming).await.is_err() {
                        break Ok(());
                    }
                }
            }
            _ => {}
        }
    };

    if let Some(handle) = heartbeat {
        handle.abort();
    }
    result
}

type Sink = futures_util::stream::SplitSink<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    WsMessage,
>;

fn spawn_heartbeat(
    sink: Arc<tokio::sync::Mutex<Sink>>,
    seq: Arc<AtomicI64>,
    interval_ms: u64,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let period = Duration::from_millis(interval_ms.max(1000));
        loop {
            tokio::time::sleep(period).await;
            if send_heartbeat(&sink, &seq).await.is_err() {
                return;
            }
        }
    })
}

async fn send_heartbeat(
    sink: &Arc<tokio::sync::Mutex<Sink>>,
    seq: &Arc<AtomicI64>,
) -> anyhow::Result<()> {
    let s = seq.load(Ordering::SeqCst);
    let d = if s < 0 {
        Value::Null
    } else {
        serde_json::json!(s)
    };
    let beat = serde_json::json!({ "op": 1, "d": d });
    sink.lock()
        .await
        .send(WsMessage::text(beat.to_string()))
        .await?;
    Ok(())
}

/// A MESSAGE_CREATE payload → apollo message, or `None` for anything the agent
/// should not answer (bots, including this bot's own messages, and empty text).
pub fn parse_message_create(d: &Value) -> Option<IncomingMessage> {
    if d["author"]["bot"].as_bool().unwrap_or(false) {
        return None;
    }
    let text = d["content"].as_str().unwrap_or("").to_string();
    if text.is_empty() {
        return None;
    }
    Some(IncomingMessage {
        id: d["id"].as_str().unwrap_or("").to_string(),
        sender_id: d["author"]["id"].as_str().unwrap_or("").to_string(),
        sender_name: d["author"]["username"].as_str().map(|s| s.to_string()),
        chat_id: d["channel_id"].as_str().unwrap_or("").to_string(),
        text,
        // No guild_id means a DM, which polling could never see at all.
        is_group: d["guild_id"].as_str().is_some(),
        reply_to: d["referenced_message"]["id"]
            .as_str()
            .map(|s| s.to_string()),
        timestamp: chrono::Utc::now(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bots_and_empty_messages_are_ignored() {
        let bot = serde_json::json!({
            "id": "1", "content": "hi", "channel_id": "c",
            "author": {"id": "2", "username": "b", "bot": true}
        });
        assert!(parse_message_create(&bot).is_none());

        let empty = serde_json::json!({
            "id": "1", "content": "", "channel_id": "c",
            "author": {"id": "2", "username": "u"}
        });
        assert!(parse_message_create(&empty).is_none());
    }

    #[test]
    fn a_dm_is_not_a_group() {
        let dm = serde_json::json!({
            "id": "1", "content": "hello", "channel_id": "c",
            "author": {"id": "2", "username": "u"}
        });
        let msg = parse_message_create(&dm).unwrap();
        assert!(!msg.is_group, "a channel with no guild_id is a DM");
        assert_eq!(msg.text, "hello");

        let guild = serde_json::json!({
            "id": "1", "content": "hello", "channel_id": "c", "guild_id": "g",
            "author": {"id": "2", "username": "u"}
        });
        assert!(parse_message_create(&guild).unwrap().is_group);
    }

    #[test]
    fn intents_request_message_content() {
        // Without the privileged MESSAGE_CONTENT intent every message arrives
        // empty, which is indistinguishable from a broken receiver.
        assert_eq!(DEFAULT_INTENTS & (1 << 15), 1 << 15);
        assert_eq!(DEFAULT_INTENTS & (1 << 12), 1 << 12, "DMs");
    }
}
