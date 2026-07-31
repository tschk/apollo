use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

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

/// Reject anything that arrives with an `Origin` header.
///
/// This endpoint drives the agent, which has shell, edit and file tools over
/// the workspace. Loopback is not a boundary against a browser: any page the
/// user visits can reach 127.0.0.1, and WebSocket upgrades ignore the
/// same-origin policy entirely, so CORS alone would not cover
/// `/v1/chat/stream`.
///
/// This runs alongside the bearer token in `check_auth`, not instead of it.
/// The token is the actual authentication; refusing `Origin` additionally
/// means a page that somehow learned the token still cannot use it from a
/// browser.
///
/// Native clients (the TUI, apollo-ui, curl) never send `Origin`; browsers
/// always do and cannot forge its absence. Refusing the header therefore blocks
/// web-page-driven access on both the POST and the WebSocket path without
/// affecting any legitimate caller.
fn reject_browser_origin(headers: &axum::http::HeaderMap) -> Result<(), StatusCode> {
    if headers.contains_key(axum::http::header::ORIGIN) {
        tracing::warn!("rejected apollo HTTP request carrying an Origin header");
        return Err(StatusCode::FORBIDDEN);
    }
    Ok(())
}

static HTTP_TOKEN: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Where the shared bearer token lives.
///
/// Per-user rather than per-workspace, because the listen port is per-user
/// too: one running agent serves whatever workspace it was started in, and a
/// client has no way to know which that was.
pub fn token_path() -> Option<std::path::PathBuf> {
    let home = std::env::var_os("HOME").filter(|h| !h.is_empty())?;
    Some(std::path::PathBuf::from(home).join(".apollo/http-token"))
}

/// Load the bearer token, generating and persisting one on first run.
///
/// `APOLLO_HTTP_TOKEN` wins when set, so a supervisor can inject the same
/// value into the server and its clients without touching disk.
pub fn load_or_create_token() -> anyhow::Result<String> {
    if let Ok(token) = std::env::var("APOLLO_HTTP_TOKEN") {
        if !token.trim().is_empty() {
            return Ok(token.trim().to_string());
        }
    }

    let path = token_path().ok_or_else(|| anyhow::anyhow!("cannot locate HOME for token file"))?;
    if let Ok(existing) = std::fs::read_to_string(&path) {
        if !existing.trim().is_empty() {
            return Ok(existing.trim().to_string());
        }
    }

    let token = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, &token)?;
    // Readable only by the owner — anyone who can read it can drive the agent.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    tracing::info!("wrote a new apollo HTTP token to {}", path.display());
    Ok(token)
}

/// Compare in constant time, so a caller cannot learn the token byte by byte
/// from how long a rejection takes.
fn secret_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn check_auth(headers: &axum::http::HeaderMap) -> Result<(), StatusCode> {
    let Some(expected) = HTTP_TOKEN.get() else {
        // No token was established, so the server cannot authenticate anyone.
        // Refuse rather than serving the agent openly.
        tracing::error!("apollo HTTP has no token configured; refusing request");
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    verify_token(expected, headers)
}

fn verify_token(expected: &str, headers: &axum::http::HeaderMap) -> Result<(), StatusCode> {
    let presented = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default();

    if secret_eq(presented.trim(), expected) {
        Ok(())
    } else {
        tracing::warn!("rejected apollo HTTP request with a missing or invalid token");
        Err(StatusCode::UNAUTHORIZED)
    }
}

/// Both gates every agent-driving route must pass.
fn authorize(headers: &axum::http::HeaderMap) -> Result<(), StatusCode> {
    reject_browser_origin(headers)?;
    check_auth(headers)
}

pub fn spawn_http_server(runner: Arc<AgentRunner>) {
    let addr = http_listen_addr();
    match load_or_create_token() {
        Ok(token) => {
            let _ = HTTP_TOKEN.set(token);
        }
        Err(e) => {
            tracing::error!("apollo http token: {e}; /v1/chat will refuse all requests");
        }
    }
    tokio::spawn(async move {
        let app = Router::new()
            .route("/health", get(|| async { "ok" }))
            .route("/v1/chat", post(chat_handler))
            .route("/v1/chat/stream", get(ws_chat_upgrade))
            .route("/shutdown", post(shutdown_handler))
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

/// Stop a detached server started by `apollo tui`.
///
/// Authenticated like the chat routes: whoever can drive the agent can also
/// stop it, and nobody else can.
async fn shutdown_handler(headers: axum::http::HeaderMap) -> Result<&'static str, StatusCode> {
    authorize(&headers)?;
    tracing::info!("shutdown requested over HTTP");
    tokio::spawn(async {
        // Let the response flush before the process goes away.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        std::process::exit(0);
    });
    Ok("stopping")
}

async fn chat_handler(
    State(runner): State<Arc<AgentRunner>>,
    headers: axum::http::HeaderMap,
    Json(body): Json<ChatRequestBody>,
) -> Result<Json<ChatResponseBody>, StatusCode> {
    authorize(&headers)?;
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
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    if let Err(status) = authorize(&headers) {
        return status.into_response();
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{header, HeaderMap, HeaderValue};

    fn headers(pairs: &[(header::HeaderName, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(name.clone(), HeaderValue::from_str(value).unwrap());
        }
        map
    }

    #[test]
    fn secret_eq_matches_only_identical_secrets() {
        assert!(secret_eq("abc", "abc"));
        assert!(!secret_eq("abc", "abd"));
        assert!(!secret_eq("abc", "ab"));
        assert!(!secret_eq("", "abc"));
        assert!(secret_eq("", ""));
    }

    #[test]
    fn a_correct_bearer_token_is_accepted() {
        let h = headers(&[(header::AUTHORIZATION, "Bearer s3cret")]);
        assert!(verify_token("s3cret", &h).is_ok());
    }

    #[test]
    fn a_wrong_missing_or_malformed_token_is_rejected() {
        for h in [
            headers(&[(header::AUTHORIZATION, "Bearer wrong")]),
            headers(&[(header::AUTHORIZATION, "s3cret")]),
            headers(&[(header::AUTHORIZATION, "Basic s3cret")]),
            headers(&[]),
        ] {
            assert_eq!(verify_token("s3cret", &h), Err(StatusCode::UNAUTHORIZED));
        }
    }

    #[test]
    fn an_origin_header_is_refused_even_with_a_valid_token() {
        let h = headers(&[
            (header::AUTHORIZATION, "Bearer s3cret"),
            (header::ORIGIN, "https://evil.example"),
        ]);
        assert_eq!(reject_browser_origin(&h), Err(StatusCode::FORBIDDEN));
    }

    #[test]
    fn requests_without_an_origin_pass_the_browser_check() {
        assert!(reject_browser_origin(&headers(&[])).is_ok());
    }
}
