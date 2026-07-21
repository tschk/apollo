//! Matrix channel — Matrix protocol via client-server API

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::mpsc;

use super::traits::*;

pub struct MatrixChannel {
    homeserver: String,
    access_token: String,
    room_id: Option<String>,
}

impl MatrixChannel {
    pub fn new(homeserver: impl Into<String>, access_token: impl Into<String>) -> Self {
        Self {
            homeserver: homeserver.into().trim_end_matches('/').to_string(),
            access_token: access_token.into(),
            room_id: None,
        }
    }

    pub fn with_room(mut self, room_id: impl Into<String>) -> Self {
        self.room_id = Some(room_id.into());
        self
    }
}

#[async_trait]
impl Channel for MatrixChannel {
    fn name(&self) -> &str {
        "matrix"
    }

    async fn start(&mut self) -> anyhow::Result<mpsc::Receiver<IncomingMessage>> {
        let (tx, rx) = mpsc::channel(32);
        let homeserver = self.homeserver.clone();
        let token = self.access_token.clone();

        tokio::spawn(async move {
            let client = reqwest::Client::new();
            let mut since = String::new();

            loop {
                let mut url = format!("{}/_matrix/client/r0/sync?timeout=30000", homeserver);
                if !since.is_empty() {
                    url.push_str(&format!("&since={}", since));
                }

                let resp = client
                    .get(&url)
                    .header("Authorization", format!("Bearer {}", token))
                    .send()
                    .await;

                if let Ok(resp) = resp {
                    if let Ok(data) = resp.json::<Value>().await {
                        if let Some(next) = data["next_batch"].as_str() {
                            since = next.to_string();
                        }

                        if let Some(rooms) = data["rooms"]["join"].as_object() {
                            for (room_id, room) in rooms {
                                if let Some(events) = room["timeline"]["events"].as_array() {
                                    for event in events {
                                        if event["type"].as_str() != Some("m.room.message") {
                                            continue;
                                        }
                                        let content = &event["content"];
                                        let text =
                                            content["body"].as_str().unwrap_or("").to_string();
                                        if text.is_empty() {
                                            continue;
                                        }

                                        let incoming = IncomingMessage {
                                            id: event["event_id"]
                                                .as_str()
                                                .unwrap_or("")
                                                .to_string(),
                                            sender_id: event["sender"]
                                                .as_str()
                                                .unwrap_or("")
                                                .to_string(),
                                            sender_name: None,
                                            chat_id: room_id.clone(),
                                            text,
                                            is_group: true,
                                            reply_to: None,
                                            timestamp: chrono::Utc::now(),
                                        };

                                        let _ = tx.send(incoming).await;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        });

        Ok(rx)
    }

    async fn send(&self, message: OutgoingMessage) -> anyhow::Result<Option<String>> {
        let client = reqwest::Client::new();
        let txn_id = uuid::Uuid::new_v4().to_string();

        let body = serde_json::json!({
            "msgtype": "m.text",
            "body": &message.text,
        });

        client
            .put(format!(
                "{}/_matrix/client/r0/rooms/{}/send/m.room.message/{}",
                self.homeserver, message.chat_id, txn_id
            ))
            .header("Authorization", format!("Bearer {}", self.access_token))
            .json(&body)
            .send()
            .await?;

        Ok(None)
    }

    async fn stop(&mut self) -> anyhow::Result<()> {
        Ok(())
    }
}
