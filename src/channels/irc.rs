//! IRC channel — simple IRC protocol client

use std::sync::Arc;

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::tcp::OwnedWriteHalf;
use tokio::net::TcpStream;
use tokio::sync::{mpsc, Mutex};

use super::traits::*;

/// The write half of the IRC connection, shared between the read loop (which
/// answers PING) and `send()` (which writes PRIVMSG). `None` until `start()`.
type SharedWriter = Arc<Mutex<Option<OwnedWriteHalf>>>;

pub struct IrcChannel {
    server: String,
    port: u16,
    nick: String,
    channel: String,
    password: Option<String>,
    writer: SharedWriter,
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
            writer: Arc::new(Mutex::new(None)),
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

        let stream = TcpStream::connect(format!("{}:{}", self.server, self.port))
            .await
            .map_err(|e| {
                anyhow::anyhow!("IRC connect to {}:{} failed: {e}", self.server, self.port)
            })?;
        let (reader, mut writer) = stream.into_split();

        // Register before anything else can write, so the shared writer is
        // only published once the connection is usable.
        if let Some(pass) = &self.password {
            writer
                .write_all(format!("PASS {}\r\n", pass).as_bytes())
                .await?;
        }
        writer
            .write_all(format!("NICK {}\r\n", self.nick).as_bytes())
            .await?;
        writer
            .write_all(format!("USER {} 0 * :aclaw bot\r\n", self.nick).as_bytes())
            .await?;
        writer
            .write_all(format!("JOIN {}\r\n", self.channel).as_bytes())
            .await?;

        *self.writer.lock().await = Some(writer);
        let shared = Arc::clone(&self.writer);

        tokio::spawn(async move {
            let mut reader = BufReader::new(reader);
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
                            if let Some(w) = shared.lock().await.as_mut() {
                                let _ = w.write_all(format!("{}\r\n", response).as_bytes()).await;
                            }
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

    async fn send(&self, message: OutgoingMessage) -> anyhow::Result<Option<String>> {
        let target = if message.chat_id.is_empty() {
            self.channel.as_str()
        } else {
            message.chat_id.as_str()
        };

        let mut guard = self.writer.lock().await;
        let writer = guard
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("IRC send before start(): no connection"))?;

        for line in irc_payload_lines(&message.text) {
            writer
                .write_all(format!("PRIVMSG {} :{}\r\n", target, line).as_bytes())
                .await?;
        }
        writer.flush().await?;
        Ok(None)
    }

    async fn stop(&mut self) -> anyhow::Result<()> {
        if let Some(mut writer) = self.writer.lock().await.take() {
            let _ = writer.write_all(b"QUIT :bye\r\n").await;
            let _ = writer.shutdown().await;
        }
        Ok(())
    }
}

/// IRC carries one line per PRIVMSG and caps the whole line at 512 bytes, so
/// embedded newlines are split and long lines are chunked by character.
fn irc_payload_lines(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.split('\n') {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        let chars: Vec<char> = line.chars().collect();
        for chunk in chars.chunks(400) {
            out.push(chunk.iter().collect());
        }
    }
    out
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
