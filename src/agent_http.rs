use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::{Method, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tower_http::cors::{Any, CorsLayer};

use crate::agent::stream::AgentStreamEvent;
use crate::agent::AgentRunner;
use crate::channels::http_inject::HttpInjectChannel;
use crate::channels::IncomingMessage;

#[derive(Debug, Deserialize)]
pub struct ChatRequestBody {
    pub message: String,
    #[serde(default = "default_chat_id")]
    pub chat_id: String,
}

fn default_chat_id() -> String {
    "embed".into()
}

#[derive(Debug, Serialize)]
pub struct ChatResponseBody {
    pub response: String,
}

pub async fn chat_once(
    runner: &AgentRunner,
    message: &str,
    chat_id: &str,
) -> anyhow::Result<String> {
    let msg = IncomingMessage {
        id: uuid::Uuid::new_v4().to_string(),
        sender_id: "http".into(),
        sender_name: Some("HTTP".into()),
        chat_id: chat_id.to_string(),
        text: message.to_string(),
        is_group: false,
        reply_to: None,
        timestamp: chrono::Utc::now(),
    };
    let channel = HttpInjectChannel::new();
    runner.handle_message(&msg, &channel).await
}

pub fn http_listen_addr() -> SocketAddr {
    let port = std::env::var("APOLLO_HTTP_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(31338);
    SocketAddr::from(([127, 0, 0, 1], port))
}

fn cors_layer() -> CorsLayer {
    CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
        .allow_headers(Any)
}

pub fn spawn_http_server(runner: Arc<AgentRunner>) {
    let addr = http_listen_addr();
    tokio::spawn(async move {
        let app = Router::new()
            .route("/health", get(|| async { "ok" }))
            .route("/v1/chat", post(chat_handler))
            .route("/v1/chat/stream", get(ws_chat_upgrade))
            .layer(cors_layer())
            .with_state(runner);
        let listener = match tokio::net::TcpListener::bind(addr).await {
            Ok(l) => l,
            Err(e) => {
                tracing::error!("apollo http bind {}: {}", addr, e);
                return;
            }
        };
        tracing::info!(
            "apollo agent HTTP http://{}/v1/chat · WS /v1/chat/stream",
            addr
        );
        if let Err(e) = axum::serve(listener, app).await {
            tracing::error!("apollo http server: {}", e);
        }
    });
}

async fn chat_handler(
    State(runner): State<Arc<AgentRunner>>,
    Json(body): Json<ChatRequestBody>,
) -> Result<Json<ChatResponseBody>, StatusCode> {
    if body.message.trim().is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    match chat_once(&runner, body.message.trim(), &body.chat_id).await {
        Ok(response) => Ok(Json(ChatResponseBody { response })),
        Err(e) => {
            tracing::error!("http chat: {}", e);
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

async fn ws_chat_upgrade(
    ws: WebSocketUpgrade,
    State(runner): State<Arc<AgentRunner>>,
) -> impl axum::response::IntoResponse {
    ws.on_upgrade(move |socket| handle_ws_chat(socket, runner))
}

async fn handle_ws_chat(mut socket: WebSocket, runner: Arc<AgentRunner>) {
    let Some(Ok(Message::Text(text))) = socket.recv().await else {
        return;
    };
    let Ok(body) = serde_json::from_str::<ChatRequestBody>(&text) else {
        let _ = socket
            .send(Message::Text(
                serde_json::json!({"type":"error","message":"invalid JSON"}).to_string(),
            ))
            .await;
        return;
    };
    if body.message.trim().is_empty() {
        let _ = socket
            .send(Message::Text(
                serde_json::json!({"type":"error","message":"empty message"}).to_string(),
            ))
            .await;
        return;
    }

    let (stream_tx, mut stream_rx) = mpsc::unbounded_channel::<AgentStreamEvent>();
    let runner_bg = Arc::clone(&runner);
    let message = body.message.trim().to_string();
    let chat_id = body.chat_id.clone();
    let mut chat_task = tokio::spawn(async move {
        runner_bg.set_stream_sink(Some(stream_tx));
        let result = chat_once(&runner_bg, &message, &chat_id).await;
        runner_bg.set_stream_sink(None);
        result
    });

    loop {
        tokio::select! {
            Some(ev) = stream_rx.recv() => {
                if let Ok(json) = serde_json::to_string(&ev) {
                    if socket.send(Message::Text(json)).await.is_err() {
                        return;
                    }
                }
            }
            result = &mut chat_task => {
                match result {
                    Ok(Ok(response)) => {
                        let payload = serde_json::to_string(&AgentStreamEvent::Done {
                            response: response.clone(),
                        })
                        .unwrap_or_else(|_| {
                            serde_json::json!({"type":"done","response": response}).to_string()
                        });
                        let _ = socket.send(Message::Text(payload)).await;
                    }
                    Ok(Err(e)) => {
                        let payload = serde_json::to_string(&AgentStreamEvent::Error {
                            message: e.to_string(),
                        })
                        .unwrap_or_else(|_| {
                            serde_json::json!({"type":"error","message": e.to_string()}).to_string()
                        });
                        let _ = socket.send(Message::Text(payload)).await;
                    }
                    Err(e) => {
                        let payload = serde_json::json!({"type":"error","message": e.to_string()});
                        let _ = socket.send(Message::Text(payload.to_string())).await;
                    }
                }
                break;
            }
        }
    }

    while let Ok(ev) = stream_rx.try_recv() {
        if let Ok(json) = serde_json::to_string(&ev) {
            let _ = socket.send(Message::Text(json)).await;
        }
    }
}
