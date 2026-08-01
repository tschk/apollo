//! Channel conformance suite.
//!
//! Every `Channel` implementation is driven against a local mock HTTP server
//! (axum, already in the dependency tree — no new crate) and checked for three
//! things:
//!
//! 1. `start()` yields a receiver that actually delivers an inbound message.
//! 2. `send()` performs a real outbound request with the right shape.
//! 3. The reply-delivery contract holds: a channel that does not implement
//!    `DraftChannel` must leave `Delivery` in the `Return` arm, so the agent
//!    loop hands the reply back to the caller instead of dropping it.
//! 4. The media contract holds: `supports_media()` and `send_media()` agree,
//!    so a channel never accepts an attachment it cannot deliver.
//!
//! Tests that are `#[ignore]`d are pinning a *known defect*, not a flaky test.
//! Do not relax the assertion to make one pass — fix the channel.

#![allow(clippy::type_complexity)]

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use apollo::channels::{Channel, Delivery, IncomingMessage, OutgoingMedia, OutgoingMessage};
use axum::body::Bytes;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use tokio::sync::mpsc::Receiver;

// ───────────────────────── mock HTTP server ─────────────────────────

#[derive(Debug, Clone)]
struct Hit {
    method: String,
    path: String,
    query: String,
    auth: Option<String>,
    body: String,
}

type Responder = Arc<dyn Fn(&str) -> (u16, String) + Send + Sync>;

struct Mock {
    base: String,
    hits: Arc<Mutex<Vec<Hit>>>,
}

impl Mock {
    async fn start(responder: Responder) -> Self {
        let hits: Arc<Mutex<Vec<Hit>>> = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&hits);

        let app = axum::Router::new().fallback(axum::routing::any(
            move |method: Method, uri: Uri, headers: HeaderMap, body: Bytes| {
                let recorded = Arc::clone(&recorded);
                let responder = Arc::clone(&responder);
                async move {
                    let body = String::from_utf8_lossy(&body).to_string();
                    let path = uri.path().to_string();
                    recorded.lock().unwrap().push(Hit {
                        method: method.to_string(),
                        path: path.clone(),
                        query: uri.query().unwrap_or("").to_string(),
                        auth: headers
                            .get("authorization")
                            .and_then(|v| v.to_str().ok())
                            .map(str::to_string),
                        body,
                    });
                    let (code, payload) = responder(&path);
                    (
                        StatusCode::from_u16(code).unwrap(),
                        [(axum::http::header::CONTENT_TYPE, "application/json")],
                        payload,
                    )
                }
            },
        ));

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        Mock {
            base: format!("http://{addr}"),
            hits,
        }
    }

    fn hits(&self) -> Vec<Hit> {
        self.hits.lock().unwrap().clone()
    }

    /// Wait until a request whose path contains `needle` has been recorded.
    async fn wait_for(&self, needle: &str) -> Hit {
        for _ in 0..100 {
            if let Some(h) = self.hits().into_iter().find(|h| h.path.contains(needle)) {
                return h;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!(
            "no request to a path containing {needle:?}; saw {:?}",
            self.hits()
                .iter()
                .map(|h| h.path.clone())
                .collect::<Vec<_>>()
        );
    }
}

fn json(body: &str) -> (u16, String) {
    (200, body.to_string())
}

// ───────────────────────── shared assertions ─────────────────────────

/// What `send()` must put on the wire.
struct SendShape {
    method: &'static str,
    path_contains: &'static str,
    body_contains: &'static [&'static str],
    auth: Option<String>,
}

/// Assertion 3 — the reply-delivery contract.
///
/// `DraftChannel` has no default methods, so a channel can only enter the
/// `Delivery::Draft` arm by really implementing draft delivery. Everything
/// else must land in `Delivery::Return` and get its text back.
async fn assert_reply_delivery_contract(channel: &dyn Channel) {
    let name = channel.name().to_string();
    let delivery = Delivery::open(channel, "conformance", "⏳").await;
    match channel.as_draft() {
        None => {
            assert!(
                delivery.draft().is_none(),
                "{name}: no DraftChannel impl but Delivery opened a draft"
            );
            assert_eq!(
                delivery.deliver("conformance", "REPLY").await.unwrap(),
                "REPLY",
                "{name}: reply was not returned to the caller"
            );
        }
        Some(_) => {
            assert!(
                delivery
                    .deliver("conformance", "REPLY")
                    .await
                    .unwrap()
                    .is_empty(),
                "{name}: draft channel must swallow the return value"
            );
        }
    }
}

/// Assertion 2 — `send()` performs a real outbound request of the right shape.
async fn assert_send(channel: &dyn Channel, mock: &Mock, chat_id: &str, shape: &SendShape) {
    let name = channel.name().to_string();
    channel
        .send(OutgoingMessage {
            chat_id: chat_id.to_string(),
            text: "REPLY".to_string(),
            reply_to: None,
        })
        .await
        .unwrap_or_else(|e| panic!("{name}: send() failed: {e}"));

    // Match on method as well as path: a polling channel hits its own send
    // path with GET, and that must not be mistaken for the outbound request.
    let hit = mock
        .hits()
        .into_iter()
        .find(|h| h.path.contains(shape.path_contains) && h.method == shape.method)
        .unwrap_or_else(|| {
            panic!(
                "{name}: send() made no {} request to a path containing {:?}; saw {:?}",
                shape.method,
                shape.path_contains,
                mock.hits()
                    .iter()
                    .map(|h| format!("{} {}", h.method, h.path))
                    .collect::<Vec<_>>()
            )
        });

    for fragment in shape.body_contains {
        assert!(
            hit.body.contains(fragment),
            "{name}: outbound body missing {fragment:?}; body was {:?}",
            hit.body
        );
    }
    if let Some(expected) = &shape.auth {
        assert_eq!(
            hit.auth.as_deref(),
            Some(expected.as_str()),
            "{name}: wrong Authorization header"
        );
    }
}

/// Assertion 4 — the media contract.
///
/// A channel either carries attachments or says so. What it must never do is
/// accept `send_media` and quietly deliver a text message with a URL in it,
/// which is what a caller asking for a picture would never detect.
async fn assert_media_contract(channel: &dyn Channel) {
    let name = channel.name().to_string();
    if channel.supports_media() {
        return;
    }
    let result = channel
        .send_media(OutgoingMedia::image_url("conformance", "http://x/y.png"))
        .await;
    assert!(
        result.is_err(),
        "{name}: supports_media() is false but send_media() reported success"
    );
}

/// Assertion 1 — `start()` gives a receiver that really delivers.
async fn assert_receives(name: &str, rx: &mut Receiver<IncomingMessage>) -> IncomingMessage {
    match tokio::time::timeout(Duration::from_secs(10), rx.recv()).await {
        Ok(Some(msg)) => msg,
        Ok(None) => panic!("{name}: start() receiver closed without delivering a message"),
        Err(_) => panic!("{name}: start() receiver never delivered a message"),
    }
}

fn free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().port()
}

/// POST a webhook payload to a channel's own HTTP receiver, retrying until it
/// has finished binding.
async fn post_webhook(port: u16, path: &str, body: serde_json::Value) {
    let url = format!("http://127.0.0.1:{port}{path}");
    let client = reqwest::Client::new();
    for _ in 0..100 {
        if client.post(&url).json(&body).send().await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("webhook receiver on port {port} never accepted a request");
}

// ───────────────────────── telegram ─────────────────────────

#[cfg(feature = "channel-telegram")]
#[tokio::test]
async fn telegram_conformance() {
    use apollo::channels::telegram::TelegramChannel;

    let mock = Mock::start(Arc::new(|path: &str| {
        if path.ends_with("/getUpdates") {
            json(
                r#"{"ok":true,"result":[{"update_id":1,"message":{"message_id":5,
                "chat":{"id":42,"type":"private"},"text":"hello",
                "from":{"id":9,"username":"u"}}}]}"#,
            )
        } else {
            json(r#"{"ok":true,"result":{"message_id":7}}"#)
        }
    }))
    .await;

    let mut channel = TelegramChannel::new("tok".to_string(), 42).with_api_base(&mock.base);

    let mut rx = channel.start().await.unwrap();
    let msg = assert_receives("telegram", &mut rx).await;
    assert_eq!(msg.text, "hello");
    assert_eq!(msg.chat_id, "42");

    assert_send(
        &channel,
        &mock,
        "42",
        &SendShape {
            method: "POST",
            path_contains: "/sendMessage",
            body_contains: &["REPLY", "42"],
            auth: None,
        },
    )
    .await;

    assert_reply_delivery_contract(&channel).await;
    assert_media_contract(&channel).await;

    // Telegram is the one channel that really carries media, so it gets the
    // positive case: a URL attachment goes to sendPhoto as JSON, and a local
    // file is uploaded as multipart.
    assert!(channel.supports_media());
    channel
        .send_media(
            OutgoingMedia::image_url("42", "https://example.invalid/cat.png").with_caption("a cat"),
        )
        .await
        .expect("telegram send_media(url) failed");
    let hit = mock.wait_for("/sendPhoto").await;
    assert!(hit.body.contains("cat.png"), "body was {:?}", hit.body);
    assert!(
        hit.body.contains("a cat"),
        "caption missing: {:?}",
        hit.body
    );

    let dir = std::env::temp_dir().join("apollo-conformance-media");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("note.ogg");
    std::fs::write(&file, b"RIFFvoice").unwrap();
    channel
        .send_media(OutgoingMedia::file(
            "42",
            apollo::channels::MediaKind::Voice,
            &file,
        ))
        .await
        .expect("telegram send_media(path) failed");
    let hit = mock.wait_for("/sendVoice").await;
    assert!(
        hit.body.contains("note.ogg") && hit.body.contains("RIFFvoice"),
        "voice upload was not multipart with the file bytes: {:?}",
        hit.body
    );
    let _ = std::fs::remove_file(&file);

    channel.stop().await.unwrap();
}

// ───────────────────────── cli ─────────────────────────

#[cfg(feature = "channel-cli")]
#[tokio::test]
async fn cli_conformance() {
    use apollo::channels::cli::CliChannel;

    // stdin is not drivable from a test harness, so inbound delivery is not
    // asserted here; send() and the delivery contract are.
    let mut channel = CliChannel::new();
    channel
        .send(OutgoingMessage {
            chat_id: "cli".to_string(),
            text: "REPLY".to_string(),
            reply_to: None,
        })
        .await
        .unwrap();
    assert_reply_delivery_contract(&channel).await;
    assert_media_contract(&channel).await;
    channel.stop().await.unwrap();
}

// ───────────────────────── discord ─────────────────────────

#[cfg(feature = "channel-discord")]
#[tokio::test]
async fn discord_conformance() {
    use apollo::channels::discord::DiscordChannel;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // The poller seeds its cursor from the first response without emitting, so
    // history is not replayed at startup. The message arrives on a later poll.
    let polls = Arc::new(AtomicUsize::new(0));
    let mock = Mock::start(Arc::new(move |path: &str| {
        if !path.contains("/messages") {
            return json("{}");
        }
        if polls.fetch_add(1, Ordering::SeqCst) == 0 {
            json("[]")
        } else {
            json(
                r#"[{"id":"10","channel_id":"C1","content":"hello",
                     "author":{"id":"u1","username":"u","bot":false}}]"#,
            )
        }
    }))
    .await;
    // Pinned to polling: the gateway is the default transport and has its own
    // test below, but polling stays supported for bots without the privileged
    // MESSAGE_CONTENT intent, so it keeps its coverage.
    let mut channel = DiscordChannel::new("tok".to_string(), "C1".to_string())
        .with_api_base(&mock.base)
        .with_transport(apollo::channels::discord::Transport::Polling);

    let mut rx = channel.start().await.unwrap();
    let msg = assert_receives("discord", &mut rx).await;
    assert!(!msg.text.is_empty());

    assert_send(
        &channel,
        &mock,
        "C1",
        &SendShape {
            method: "POST",
            path_contains: "/channels/C1/messages",
            body_contains: &["REPLY"],
            auth: Some("Bot tok".to_string()),
        },
    )
    .await;

    assert_reply_delivery_contract(&channel).await;
    assert_media_contract(&channel).await;
    channel.stop().await.unwrap();
}

// ───────────────────────── slack ─────────────────────────

#[cfg(feature = "channel-slack")]
#[tokio::test]
async fn slack_conformance() {
    use apollo::channels::slack::SlackChannel;

    let mock = Mock::start(Arc::new(|path: &str| {
        if path.ends_with("/conversations.history") {
            json(
                r#"{"ok":true,"messages":[{"ts":"111","user":"U1","text":"hello","channel":"C1"}]}"#,
            )
        } else {
            json(r#"{"ok":true}"#)
        }
    }))
    .await;

    let mut channel = SlackChannel::new("tok")
        .with_channel("C1")
        .with_api_base(&mock.base);

    let mut rx = channel.start().await.unwrap();
    let poll = mock.wait_for("conversations.history").await;
    assert!(
        poll.query.contains("channel="),
        "slack: conversations.history polled without a channel parameter (query {:?})",
        poll.query
    );

    let msg = assert_receives("slack", &mut rx).await;
    assert_eq!(msg.text, "hello");
    assert!(
        !msg.chat_id.is_empty(),
        "slack: inbound message has no chat_id"
    );

    assert_send(
        &channel,
        &mock,
        "C1",
        &SendShape {
            method: "POST",
            path_contains: "/chat.postMessage",
            body_contains: &["\"channel\":\"C1\"", "REPLY"],
            auth: Some("Bearer tok".to_string()),
        },
    )
    .await;

    assert_reply_delivery_contract(&channel).await;
    assert_media_contract(&channel).await;
    channel.stop().await.unwrap();
}

// ───────────────────────── google chat ─────────────────────────

#[cfg(feature = "channel-googlechat")]
#[tokio::test]
async fn googlechat_conformance() {
    use apollo::channels::googlechat::GoogleChatChannel;

    // Throwaway RSA key generated for this test only — it signs nothing real
    // and guards no account.
    const TEST_KEY: &str = include_str!("fixtures/googlechat_test_key.pem");

    let mock = Mock::start(Arc::new(|path: &str| {
        if path.ends_with("/token") {
            json(r#"{"access_token":"issued-token","expires_in":3600}"#)
        } else {
            json("{}")
        }
    }))
    .await;
    let port = free_port();
    let service_account = serde_json::json!({
        "client_email": "bot@apollo.iam.gserviceaccount.com",
        "private_key": TEST_KEY,
        "token_uri": format!("{}/token", mock.base),
    })
    .to_string();
    let mut channel = GoogleChatChannel::new(service_account)
        .with_api_base(&mock.base)
        .with_bind_addr(SocketAddr::from(([127, 0, 0, 1], port)));

    let mut rx = channel.start().await.unwrap();
    post_webhook(
        port,
        "/googlechat/webhook",
        serde_json::json!({
            "type": "MESSAGE",
            "message": {"name": "m1", "text": "hello"},
            "user": {"name": "users/1", "displayName": "U"},
            "space": {"name": "spaces/AAA", "type": "ROOM"},
        }),
    )
    .await;
    let msg = assert_receives("googlechat", &mut rx).await;
    assert_eq!(msg.text, "hello");

    channel
        .send(OutgoingMessage {
            chat_id: "spaces/AAA".to_string(),
            text: "REPLY".to_string(),
            reply_to: None,
        })
        .await
        .unwrap();
    let hit = mock.wait_for("/messages").await;
    assert_eq!(
        hit.auth.as_deref(),
        Some("Bearer issued-token"),
        "googlechat: send() must use the exchanged OAuth2 token, not the key itself"
    );
    assert!(
        !mock.hits().iter().any(|h| h
            .auth
            .as_deref()
            .is_some_and(|a| a.contains("BEGIN PRIVATE KEY"))),
        "googlechat: service-account key leaked into an Authorization header"
    );

    assert_reply_delivery_contract(&channel).await;
    assert_media_contract(&channel).await;
    channel.stop().await.unwrap();
}

// ───────────────────────── irc ─────────────────────────

#[cfg(feature = "channel-irc")]
#[tokio::test]
async fn irc_conformance() {
    use apollo::channels::irc::IrcChannel;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let lines: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&lines);

    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (reader, mut writer) = stream.into_split();
        let mut reader = BufReader::new(reader).lines();
        let _ = writer.write_all(b":u!u@h PRIVMSG #test :hello\r\n").await;
        while let Ok(Some(line)) = reader.next_line().await {
            seen.lock().unwrap().push(line);
        }
    });

    let mut channel = IrcChannel::new("127.0.0.1", "#test", "bot").with_port(port);
    let mut rx = channel.start().await.unwrap();
    let msg = assert_receives("irc", &mut rx).await;
    assert_eq!(msg.text, "hello");

    channel
        .send(OutgoingMessage {
            chat_id: "#test".to_string(),
            text: "REPLY".to_string(),
            reply_to: None,
        })
        .await
        .unwrap();

    let mut sent_privmsg = false;
    for _ in 0..40 {
        if lines
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.starts_with("PRIVMSG") && l.contains("REPLY"))
        {
            sent_privmsg = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        sent_privmsg,
        "irc: send() never wrote a PRIVMSG to the server"
    );

    assert_reply_delivery_contract(&channel).await;
    assert_media_contract(&channel).await;
    channel.stop().await.unwrap();
}

// ───────────────────────── matrix ─────────────────────────

#[cfg(feature = "channel-matrix")]
#[tokio::test]
async fn matrix_conformance() {
    use apollo::channels::matrix::MatrixChannel;

    let mock = Mock::start(Arc::new(|path: &str| {
        if path.ends_with("/sync") {
            json(
                r#"{"next_batch":"s1","rooms":{"join":{"!r:h":{"timeline":{"events":[
                {"type":"m.room.message","event_id":"$1","sender":"@u:h",
                 "content":{"body":"hello"}}]}}}}}"#,
            )
        } else {
            json(r#"{"event_id":"$2"}"#)
        }
    }))
    .await;

    let mut channel = MatrixChannel::new(&mock.base, "tok").with_room("!r:h");
    let mut rx = channel.start().await.unwrap();
    let msg = assert_receives("matrix", &mut rx).await;
    assert_eq!(msg.text, "hello");
    assert_eq!(msg.chat_id, "!r:h");

    assert_send(
        &channel,
        &mock,
        "!r:h",
        &SendShape {
            method: "PUT",
            path_contains: "/send/m.room.message/",
            body_contains: &["m.text", "REPLY"],
            auth: Some("Bearer tok".to_string()),
        },
    )
    .await;

    assert_reply_delivery_contract(&channel).await;
    assert_media_contract(&channel).await;
    channel.stop().await.unwrap();
}

// ───────────────────────── signal ─────────────────────────

#[cfg(feature = "channel-signal")]
#[tokio::test]
async fn signal_conformance() {
    use apollo::channels::signal::SignalChannel;

    let mock = Mock::start(Arc::new(|path: &str| {
        if path.contains("/v1/receive/") {
            json(
                r#"[{"envelope":{"timestamp":1,"source":"+15550001",
                "dataMessage":{"message":"hello"}}}]"#,
            )
        } else {
            json("{}")
        }
    }))
    .await;

    let mut channel = SignalChannel::new(&mock.base, "+15550000");
    let mut rx = channel.start().await.unwrap();
    let msg = assert_receives("signal", &mut rx).await;
    assert_eq!(msg.text, "hello");

    assert_send(
        &channel,
        &mock,
        "+15550001",
        &SendShape {
            method: "POST",
            path_contains: "/v2/send",
            body_contains: &["REPLY", "+15550001"],
            auth: None,
        },
    )
    .await;

    assert_reply_delivery_contract(&channel).await;
    assert_media_contract(&channel).await;
    channel.stop().await.unwrap();
}

// ───────────────────────── whatsapp ─────────────────────────

#[cfg(feature = "channel-whatsapp")]
#[tokio::test]
async fn whatsapp_conformance() {
    use apollo::channels::whatsapp::WhatsAppChannel;

    let mock = Mock::start(Arc::new(|_: &str| json(r#"{"messages":[{"id":"m1"}]}"#))).await;
    let port = free_port();
    let mut channel = WhatsAppChannel::new("tok", "PHONE1", "verify")
        .with_api_base(&mock.base)
        .with_bind_addr(SocketAddr::from(([127, 0, 0, 1], port)));

    let mut rx = channel.start().await.unwrap();
    post_webhook(
        port,
        "/webhook",
        serde_json::json!({
            "entry": [{"changes": [{"value": {"messages": [
                {"id": "m1", "from": "+15550001", "text": {"body": "hello"}}
            ]}}]}]
        }),
    )
    .await;
    let msg = assert_receives("whatsapp", &mut rx).await;
    assert_eq!(msg.text, "hello");

    assert_send(
        &channel,
        &mock,
        "+15550001",
        &SendShape {
            method: "POST",
            path_contains: "/PHONE1/messages",
            body_contains: &["messaging_product", "REPLY"],
            auth: Some("Bearer tok".to_string()),
        },
    )
    .await;

    assert_reply_delivery_contract(&channel).await;
    assert_media_contract(&channel).await;
    channel.stop().await.unwrap();
}

// ───────────────────────── microsoft teams ─────────────────────────

#[cfg(feature = "channel-msteams")]
#[tokio::test]
async fn msteams_conformance() {
    use apollo::channels::msteams::TeamsChannel;

    let mock = Mock::start(Arc::new(|path: &str| {
        if path.ends_with("/token") {
            json(r#"{"access_token":"issued-token"}"#)
        } else {
            json("{}")
        }
    }))
    .await;

    let port = free_port();
    let mut channel = TeamsChannel::new("app", "secret")
        .with_api_base(&mock.base)
        .with_bind_addr(SocketAddr::from(([127, 0, 0, 1], port)));

    let mut rx = channel.start().await.unwrap();
    post_webhook(
        port,
        "/api/messages",
        serde_json::json!({
            "type": "message",
            "id": "m1",
            "text": "hello",
            "from": {"id": "u1", "name": "U"},
            "conversation": {"id": "c1", "conversationType": "personal"},
        }),
    )
    .await;
    let msg = assert_receives("msteams", &mut rx).await;
    assert_eq!(msg.text, "hello");

    assert_send(
        &channel,
        &mock,
        "c1",
        &SendShape {
            method: "POST",
            path_contains: "/conversations/c1/activities",
            body_contains: &["REPLY"],
            auth: Some("Bearer issued-token".to_string()),
        },
    )
    .await;

    assert_reply_delivery_contract(&channel).await;
    assert_media_contract(&channel).await;
    channel.stop().await.unwrap();
}

// ───────────────────────── discord gateway ─────────────────────────

/// A mock Discord gateway: HELLO, then a MESSAGE_CREATE once the client
/// identifies. Asserts the client really identifies rather than assuming it.
#[cfg(feature = "channel-discord")]
async fn mock_gateway(dispatch: serde_json::Value) -> (String, Arc<Mutex<Vec<String>>>) {
    use axum::extract::ws::{Message, WebSocketUpgrade};

    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&seen);

    let app = axum::Router::new().route(
        "/",
        axum::routing::any(move |ws: WebSocketUpgrade| {
            let recorded = Arc::clone(&recorded);
            let dispatch = dispatch.clone();
            async move {
                ws.on_upgrade(move |mut socket| async move {
                    let hello = serde_json::json!({
                        "op": 10, "d": {"heartbeat_interval": 45000}
                    });
                    if socket
                        .send(Message::Text(hello.to_string().into()))
                        .await
                        .is_err()
                    {
                        return;
                    }
                    while let Some(Ok(msg)) = socket.recv().await {
                        if let Message::Text(text) = msg {
                            let is_identify = serde_json::from_str::<serde_json::Value>(&text)
                                .map(|v| v["op"].as_u64() == Some(2))
                                .unwrap_or(false);
                            recorded.lock().unwrap().push(text.to_string());
                            if is_identify {
                                let _ = socket
                                    .send(Message::Text(dispatch.to_string().into()))
                                    .await;
                            }
                        }
                    }
                })
            }
        }),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("ws://{addr}/"), seen)
}

#[cfg(feature = "channel-discord")]
#[tokio::test]
async fn discord_gateway_receives_and_identifies() {
    use apollo::channels::discord::{DiscordChannel, Transport};

    let (url, seen) = mock_gateway(serde_json::json!({
        "op": 0, "s": 1, "t": "MESSAGE_CREATE",
        "d": {
            "id": "10", "channel_id": "C1", "content": "hello from gateway",
            "author": {"id": "u1", "username": "u"}
        }
    }))
    .await;

    let mut channel = DiscordChannel::new("tok".to_string(), "C1".to_string())
        .with_transport(Transport::Gateway)
        .with_gateway_url(&url);

    let mut rx = channel.start().await.unwrap();
    let msg = assert_receives("discord-gateway", &mut rx).await;
    assert_eq!(msg.text, "hello from gateway");
    assert_eq!(msg.chat_id, "C1");
    assert!(!msg.is_group, "no guild_id means a DM");

    // The IDENTIFY has to carry the token and the privileged content intent,
    // or a real gateway either rejects it or delivers empty messages.
    let identify: serde_json::Value = seen
        .lock()
        .unwrap()
        .iter()
        .filter_map(|t| serde_json::from_str::<serde_json::Value>(t).ok())
        .find(|v| v["op"].as_u64() == Some(2))
        .expect("client never sent IDENTIFY");
    assert_eq!(identify["d"]["token"].as_str(), Some("tok"));
    let intents = identify["d"]["intents"].as_u64().unwrap();
    assert_eq!(
        intents & (1 << 15),
        1 << 15,
        "MESSAGE_CONTENT intent missing"
    );

    channel.stop().await.unwrap();
}

// ───────────────────────── media uploads ─────────────────────────

#[cfg(feature = "channel-discord")]
#[tokio::test]
async fn discord_uploads_media_as_multipart() {
    use apollo::channels::discord::{DiscordChannel, Transport};
    use apollo::channels::MediaKind;

    let mock = Mock::start(Arc::new(|_: &str| json(r#"{"id":"99"}"#))).await;
    let channel = DiscordChannel::new("tok".to_string(), "C1".to_string())
        .with_api_base(&mock.base)
        .with_transport(Transport::Polling);

    let dir = std::env::temp_dir().join("apollo-discord-media");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("chart.png");
    std::fs::write(&file, b"PNGDATA").unwrap();

    let id = channel
        .send_media(OutgoingMedia::file("C1", MediaKind::Image, &file).with_caption("the chart"))
        .await
        .expect("discord send_media failed");
    assert_eq!(id.as_deref(), Some("99"));

    let hit = mock.wait_for("/channels/C1/messages").await;
    assert_eq!(hit.method, "POST");
    assert!(hit.body.contains("chart.png"), "{:?}", hit.body);
    assert!(hit.body.contains("PNGDATA"), "file bytes missing");
    assert!(hit.body.contains("payload_json"), "{:?}", hit.body);
    assert!(hit.body.contains("the chart"), "caption missing");
    let _ = std::fs::remove_file(&file);
}

#[cfg(feature = "channel-slack")]
#[tokio::test]
async fn slack_uploads_media_through_the_external_flow() {
    use apollo::channels::slack::SlackChannel;
    use apollo::channels::MediaKind;

    // The upload URL Slack hands back must point at this same mock, and the
    // mock's address is only known after it binds — so the responder reads it
    // from a cell filled in immediately after start.
    let base_cell: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
    let responder_base = Arc::clone(&base_cell);
    let mock = Mock::start(Arc::new(move |path: &str| {
        if path.contains("files.getUploadURLExternal") {
            let base = responder_base.lock().unwrap().clone();
            json(&format!(
                r#"{{"ok":true,"upload_url":"{base}/upload","file_id":"F1"}}"#
            ))
        } else {
            json(r#"{"ok":true}"#)
        }
    }))
    .await;
    *base_cell.lock().unwrap() = mock.base.clone();

    let channel = SlackChannel::new("tok")
        .with_channel("C1")
        .with_api_base(&mock.base);

    let dir = std::env::temp_dir().join("apollo-slack-media");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("notes.txt");
    std::fs::write(&file, b"SLACKBYTES").unwrap();

    let id = channel
        .send_media(OutgoingMedia::file("C1", MediaKind::Document, &file).with_caption("see notes"))
        .await
        .expect("slack send_media failed");
    assert_eq!(id.as_deref(), Some("F1"));

    // All three steps must have happened, in the deprecated-API-free order.
    let reserve = mock.wait_for("files.getUploadURLExternal").await;
    assert!(
        reserve.query.contains("filename=notes.txt"),
        "{:?}",
        reserve.query
    );
    assert!(reserve.query.contains("length=10"), "{:?}", reserve.query);

    let upload = mock.wait_for("/upload").await;
    assert!(upload.body.contains("SLACKBYTES"), "bytes never uploaded");

    let complete = mock.wait_for("files.completeUploadExternal").await;
    assert!(complete.body.contains("\"F1\""), "{:?}", complete.body);
    assert!(complete.body.contains("C1"), "{:?}", complete.body);
    assert!(complete.body.contains("see notes"), "{:?}", complete.body);

    let _ = std::fs::remove_file(&file);
}
