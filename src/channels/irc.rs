//! IRC channel — simple IRC protocol client

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

use super::traits::*;

pub struct IrcChannel {
    server: String,
    port: u16,
    nick: String,
    channel: String,
    password: Option<String>,
}

impl IrcChannel {
    pub fn new(
        server: impl Into<String>,
        channel: impl Into<String>,
        nick: impl Into<String>,
    ) -> Self {
        Self {
            server: server.into(),
            port: 6667,
            nick: nick.into(),
            channel: channel.into(),
            password: None,
        }
    }

    pub fn with_port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }

    pub fn with_password(mut self, password: impl Into<String>) -> Self {
        self.password = Some(password.into());
        self
    }
}

#[async_trait]
impl Channel for IrcChannel {
    fn name(&self) -> &str {
        "irc"
    }

    async fn start(&mut self) -> anyhow::Result<mpsc::Receiver<IncomingMessage>> {
        let (tx, rx) = mpsc::channel(32);
        let server = self.server.clone();
        let port = self.port;
        let nick = self.nick.clone();
        let channel = self.channel.clone();
        let password = self.password.clone();

        tokio::spawn(async move {
            let stream = match TcpStream::connect(format!("{}:{}", server, port)).await {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!("IRC connect failed: {}", e);
                    return;
                }
            };

            let (reader, mut writer) = stream.into_split();
            let mut reader = BufReader::new(reader);

            // Register
            if let Some(pass) = &password {
                let _ = writer
                    .write_all(format!("PASS {}\r\n", pass).as_bytes())
                    .await;
            }
            let _ = writer
                .write_all(format!("NICK {}\r\n", nick).as_bytes())
                .await;
            let _ = writer
                .write_all(format!("USER {} 0 * :aclaw bot\r\n", nick).as_bytes())
                .await;
            let _ = writer
                .write_all(format!("JOIN {}\r\n", channel).as_bytes())
                .await;

            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) => break,
                    Ok(_) => {
                        let trimmed = line.trim();

                        // Handle PING
                        if trimmed.starts_with("PING") {
                            let response = trimmed.replace("PING", "PONG");
                            let _ = writer
                                .write_all(format!("{}\r\n", response).as_bytes())
                                .await;
                            continue;
                        }

                        // Parse PRIVMSG
                        if let Some(privmsg) = parse_privmsg(trimmed) {
                            let incoming = IncomingMessage {
                                id: uuid::Uuid::new_v4().to_string(),
                                sender_id: privmsg.nick.clone(),
                                sender_name: Some(privmsg.nick),
                                chat_id: privmsg.target,
                                text: privmsg.message,
                                is_group: true,
                                reply_to: None,
                                timestamp: chrono::Utc::now(),
                            };

                            if tx.send(incoming).await.is_err() {
                                return;
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        Ok(rx)
    }

    async fn send(&self, _message: OutgoingMessage) -> anyhow::Result<Option<String>> {
        // IRC send would need a shared writer handle
        // For now, log the message
        tracing::info!("IRC send: {}", _message.text);
        Ok(None)
    }

    async fn stop(&mut self) -> anyhow::Result<()> {
        Ok(())
    }
}

struct IrcPrivMsg {
    nick: String,
    target: String,
    message: String,
}

fn parse_privmsg(line: &str) -> Option<IrcPrivMsg> {
    // :nick!user@host PRIVMSG #channel :message
    if !line.contains("PRIVMSG") {
        return None;
    }

    let parts: Vec<&str> = line.splitn(4, ' ').collect();
    if parts.len() < 4 {
        return None;
    }

    let prefix = parts[0].trim_start_matches(':');
    let nick = prefix.split('!').next()?.to_string();
    let target = parts[2].to_string();
    let message = parts[3].trim_start_matches(':').to_string();

    Some(IrcPrivMsg {
        nick,
        target,
        message,
    })
}
