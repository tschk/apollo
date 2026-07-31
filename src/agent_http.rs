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

/// Everything a client needs to render a status bar.
///
/// Fields the running agent cannot report are absent rather than zeroed:
/// `AgentRunner` keeps its memory backend private, so per-chat message counts
/// and the provider name are not observable from here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StateBody {
    pub model: String,
    pub engine: String,
    pub mode: String,
    pub cost_usd: f64,
    pub total_tokens: usize,
    pub call_count: usize,
    /// Input tokens of the most recent model call — what the agent actually
    /// sent as context on its last turn.
    pub context_tokens: usize,
    /// apollo's own compaction budget (`agent.max_context_chars`) expressed in
    /// tokens at four characters per token. It is the threshold the status bar
    /// should measure against, not the provider's hard window.
    pub context_window: usize,
    pub context_pct: u8,
}

#[derive(Debug, Deserialize)]
pub struct ModelRequestBody {
    pub model: String,
}

#[derive(Debug, Deserialize)]
pub struct ChatIdRequestBody {
    #[serde(default = "default_chat_id")]
    pub chat_id: String,
}

fn mode_name(mode: &crate::agent::mode::AgentMode) -> &'static str {
    use crate::agent::mode::AgentMode;
    match mode {
        AgentMode::Auto => "auto",
        AgentMode::BypassPermissions => "bypass",
        AgentMode::Coding { .. } => "coding",
        AgentMode::Swarm { .. } => "swarm",
    }
}

fn context_percent(used: usize, window: usize) -> u8 {
    if window == 0 {
        return 0;
    }
    ((used * 100) / window).min(100) as u8
}

pub async fn build_state(runner: &AgentRunner) -> StateBody {
    let summary = runner.get_cost_summary().await;
    let context_tokens = runner
        .cost_tracker()
        .history(1)
        .await
        .last()
        .map(|record| record.input_tokens)
        .unwrap_or(0);
    let context_window = runner.agent_config.max_context_chars / 4;
    StateBody {
        model: runner.get_model(),
        engine: match runner.agent_config.engine() {
            crate::config::AgentEngine::Rx4 => "rx4".into(),
            crate::config::AgentEngine::Legacy => "legacy".into(),
        },
        mode: mode_name(&runner.get_mode()).into(),
        cost_usd: summary.total_cost,
        total_tokens: summary.total_tokens,
        call_count: summary.call_count,
        context_tokens,
        context_window,
        context_pct: context_percent(context_tokens, context_window),
    }
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
            .route("/v1/state", get(state_handler))
            .route("/v1/model", post(model_handler))
            .route("/v1/compact", post(compact_handler))
            .route("/v1/clear", post(clear_handler))
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

async fn state_handler(
    State(runner): State<Arc<AgentRunner>>,
    headers: axum::http::HeaderMap,
) -> Result<Json<StateBody>, StatusCode> {
    authorize(&headers)?;
    Ok(Json(build_state(&runner).await))
}

async fn model_handler(
    State(runner): State<Arc<AgentRunner>>,
    headers: axum::http::HeaderMap,
    Json(body): Json<ModelRequestBody>,
) -> Result<Json<StateBody>, StatusCode> {
    authorize(&headers)?;
    let model = body.model.trim();
    if model.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    if model == "default" || model == "reset" {
        runner.reset_model();
    } else {
        runner.set_model(model);
    }
    tracing::info!("model switched over HTTP to {}", runner.get_model());
    Ok(Json(build_state(&runner).await))
}

/// Why compaction and history clearing are not served yet.
///
/// Both need the conversation store, and `AgentRunner` owns its
/// `Arc<dyn MemoryBackend>` privately — there is no accessor to reach it from
/// this module. Answering 501 with the reason keeps the contract explicit
/// instead of pretending the work happened.
const NO_MEMORY_ACCESS: &str =
    "unavailable: AgentRunner exposes no accessor for its memory backend, so the \
     server cannot reach this chat's conversation store";

#[derive(Debug, Serialize)]
pub struct UnavailableBody {
    pub error: &'static str,
}

fn unavailable() -> (StatusCode, Json<UnavailableBody>) {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(UnavailableBody {
            error: NO_MEMORY_ACCESS,
        }),
    )
}

async fn compact_handler(
    headers: axum::http::HeaderMap,
    Json(body): Json<ChatIdRequestBody>,
) -> Result<(StatusCode, Json<UnavailableBody>), StatusCode> {
    authorize(&headers)?;
    let _ = body.chat_id;
    Ok(unavailable())
}

async fn clear_handler(
    headers: axum::http::HeaderMap,
    Json(body): Json<ChatIdRequestBody>,
) -> Result<(StatusCode, Json<UnavailableBody>), StatusCode> {
    authorize(&headers)?;
    let _ = body.chat_id;
    Ok(unavailable())
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
            .send(Message::text(
                serde_json::json!({"type":"error","message":"invalid JSON"}).to_string(),
            ))
            .await;
        return;
    };
    if body.message.trim().is_empty() {
        let _ = socket
            .send(Message::text(
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
                    if socket.send(Message::text(json)).await.is_err() {
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
                        let _ = socket.send(Message::text(payload)).await;
                    }
                    Ok(Err(e)) => {
                        let payload = serde_json::to_string(&AgentStreamEvent::Error {
                            message: e.to_string(),
                        })
                        .unwrap_or_else(|_| {
                            serde_json::json!({"type":"error","message": e.to_string()}).to_string()
                        });
                        let _ = socket.send(Message::text(payload)).await;
                    }
                    Err(e) => {
                        let payload = serde_json::json!({"type":"error","message": e.to_string()});
                        let _ = socket.send(Message::text(payload.to_string())).await;
                    }
                }
                break;
            }
        }
    }

    while let Ok(ev) = stream_rx.try_recv() {
        if let Ok(json) = serde_json::to_string(&ev) {
            let _ = socket.send(Message::text(json)).await;
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

    #[test]
    fn a_model_request_needs_only_the_model_field() {
        let body: ModelRequestBody = serde_json::from_str(r#"{"model":"claude-opus-4"}"#).unwrap();
        assert_eq!(body.model, "claude-opus-4");
        assert!(serde_json::from_str::<ModelRequestBody>("{}").is_err());
    }

    #[test]
    fn a_chat_id_request_defaults_to_the_embed_chat() {
        let body: ChatIdRequestBody = serde_json::from_str("{}").unwrap();
        assert_eq!(body.chat_id, "embed");
        let body: ChatIdRequestBody = serde_json::from_str(r#"{"chat_id":"tui"}"#).unwrap();
        assert_eq!(body.chat_id, "tui");
    }

    #[test]
    fn state_round_trips_through_json_with_every_field() {
        let state = StateBody {
            model: "claude-sonnet-4-5".into(),
            engine: "legacy".into(),
            mode: "auto".into(),
            cost_usd: 0.25,
            total_tokens: 1234,
            call_count: 3,
            context_tokens: 8000,
            context_window: 32_000,
            context_pct: 25,
        };
        let json = serde_json::to_string(&state).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        for key in [
            "model",
            "engine",
            "mode",
            "cost_usd",
            "total_tokens",
            "call_count",
            "context_tokens",
            "context_window",
            "context_pct",
        ] {
            assert!(value.get(key).is_some(), "missing field: {key}");
        }
        assert_eq!(serde_json::from_str::<StateBody>(&json).unwrap(), state);
    }

    #[test]
    fn context_percent_saturates_and_survives_a_zero_window() {
        assert_eq!(context_percent(0, 1000), 0);
        assert_eq!(context_percent(700, 1000), 70);
        assert_eq!(context_percent(5000, 1000), 100);
        assert_eq!(context_percent(500, 0), 0);
    }

    #[test]
    fn every_agent_mode_has_a_stable_name() {
        use crate::agent::mode::AgentMode;
        assert_eq!(mode_name(&AgentMode::Auto), "auto");
        assert_eq!(mode_name(&AgentMode::BypassPermissions), "bypass");
        assert_eq!(
            mode_name(&AgentMode::Coding {
                plan_approval: false,
                project_path: None,
            }),
            "coding"
        );
        assert_eq!(mode_name(&AgentMode::Swarm { parallelism: 2 }), "swarm");
    }

    #[test]
    fn the_unavailable_body_explains_itself() {
        let (status, Json(body)) = unavailable();
        assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
        assert!(body.error.contains("memory backend"));
    }
}
