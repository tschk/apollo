//! MCP server mode — exposes apollo as an MCP server over stdio or HTTP.
//! Other AI clients can connect to prompt apollo or use its tools.

use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

use crate::agent::loop_runner::AgentRunner;
use crate::channels::traits::{Channel, IncomingMessage, OutgoingMessage};
use crate::tools::traits::{Tool, ToolSpec};

#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    #[allow(dead_code)]
    jsonrpc: String,
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Option<Value>,
}

#[derive(Debug, Serialize)]
struct JsonRpcResponse {
    jsonrpc: String,
    id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcErrorObj>,
}

#[derive(Debug, Serialize)]
struct JsonRpcErrorObj {
    code: i32,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
}

impl JsonRpcResponse {
    fn success(id: Option<Value>, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }

    fn error(id: Option<Value>, code: i32, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(JsonRpcErrorObj {
                code,
                message: message.into(),
                data: None,
            }),
        }
    }
}

fn tool_to_mcp_schema(spec: &ToolSpec) -> Value {
    serde_json::json!({
        "name": spec.name,
        "description": spec.description,
        "inputSchema": spec.parameters
    })
}

/// A no-op Channel used for HTTP chat requests when no Telegram token is provided.
struct HttpChannel;

#[async_trait::async_trait]
impl Channel for HttpChannel {
    fn name(&self) -> &str {
        "http"
    }
    async fn start(&mut self) -> anyhow::Result<mpsc::Receiver<IncomingMessage>> {
        let (_tx, rx) = mpsc::channel(1);
        Ok(rx)
    }
    async fn send(&self, _msg: OutgoingMessage) -> anyhow::Result<Option<String>> {
        Ok(None)
    }
    async fn stop(&mut self) -> anyhow::Result<()> {
        Ok(())
    }
}

/// Live-update channel that posts directly to Telegram during agent execution.
/// Sends ⏳ on first tool call, edits with tool progress, finalizes with the response.
struct TelegramHttpChannel {
    token: String,
    client: reqwest::Client,
    /// Rate-limit progress edits: track last edit time per chat
    last_edit: std::sync::Mutex<Option<std::time::Instant>>,
}

impl TelegramHttpChannel {
    fn new(token: String) -> Self {
        Self {
            token,
            client: crate::http::shared(),
            last_edit: std::sync::Mutex::new(None),
        }
    }

    fn api_url(&self, method: &str) -> String {
        format!("https://api.telegram.org/bot{}/{}", self.token, method)
    }

    async fn tg_post(
        &self,
        method: &str,
        body: serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        let resp = self
            .client
            .post(self.api_url(method))
            .json(&body)
            .send()
            .await?;
        Ok(resp.json().await?)
    }
}

#[async_trait::async_trait]
impl Channel for TelegramHttpChannel {
    fn name(&self) -> &str {
        "telegram-http"
    }

    async fn start(&mut self) -> anyhow::Result<mpsc::Receiver<IncomingMessage>> {
        let (_tx, rx) = mpsc::channel(1);
        Ok(rx)
    }

    async fn send(&self, msg: OutgoingMessage) -> anyhow::Result<Option<String>> {
        let body = serde_json::json!({
            "chat_id": msg.chat_id,
            "text": msg.text,
        });
        let resp = self.tg_post("sendMessage", body).await?;
        Ok(resp["result"]["message_id"]
            .as_i64()
            .map(|id| id.to_string()))
    }

    fn as_draft(&self) -> Option<&dyn crate::channels::DraftChannel> {
        Some(self)
    }

    async fn stop(&mut self) -> anyhow::Result<()> {
        Ok(())
    }
}

#[async_trait::async_trait]
impl crate::channels::DraftChannel for TelegramHttpChannel {
    async fn send_draft(&self, chat_id: &str, text: &str) -> anyhow::Result<Option<String>> {
        let resp = self
            .tg_post(
                "sendMessage",
                serde_json::json!({
                    "chat_id": chat_id,
                    "text": text,
                }),
            )
            .await?;
        Ok(resp["result"]["message_id"]
            .as_i64()
            .map(|id| id.to_string()))
    }

    async fn update_draft_progress(
        &self,
        chat_id: &str,
        message_id: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        // Rate-limit to one edit per 1.5s to avoid Telegram 429s
        {
            let mut last = self.last_edit.lock().unwrap();
            if let Some(t) = *last {
                if t.elapsed().as_millis() < 1500 {
                    return Ok(());
                }
            }
            *last = Some(std::time::Instant::now());
        }

        let msg_id: i64 = message_id.parse().unwrap_or(0);
        let _ = self
            .tg_post(
                "editMessageText",
                serde_json::json!({
                    "chat_id": chat_id,
                    "message_id": msg_id,
                    "text": text,
                }),
            )
            .await;
        Ok(())
    }

    async fn finalize_draft(
        &self,
        chat_id: &str,
        message_id: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        let msg_id: i64 = message_id.parse().unwrap_or(0);
        let html = md_to_telegram_html(text);

        // Try HTML first, fall back to plain
        let r = self
            .tg_post(
                "editMessageText",
                serde_json::json!({
                    "chat_id": chat_id,
                    "message_id": msg_id,
                    "text": &html,
                    "parse_mode": "HTML",
                }),
            )
            .await?;

        if !r["ok"].as_bool().unwrap_or(false) {
            let _ = self
                .tg_post(
                    "editMessageText",
                    serde_json::json!({
                        "chat_id": chat_id,
                        "message_id": msg_id,
                        "text": strip_html(&html),
                    }),
                )
                .await;
        }
        Ok(())
    }
}

/// Convert GitHub Flavoured Markdown to Telegram HTML.
fn md_to_telegram_html(md: &str) -> String {
    let mut out = Vec::new();
    let mut in_code = false;
    let mut code_buf: Vec<&str> = Vec::new();

    for line in md.lines() {
        if line.starts_with("```") {
            if !in_code {
                in_code = true;
                code_buf.clear();
            } else {
                let code = esc_html(&code_buf.join("\n"));
                out.push(format!("<pre>{}</pre>", code));
                code_buf.clear();
                in_code = false;
            }
            continue;
        }
        if in_code {
            code_buf.push(line);
            continue;
        }
        // Table separator → skip
        if line.starts_with('|') && line.contains("---") {
            continue;
        }
        // Table row → flatten
        if line.starts_with('|') {
            let cells: Vec<&str> = line
                .split('|')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect();
            if !cells.is_empty() {
                out.push(inline_to_html(&cells.join("  •  ")));
            }
            continue;
        }
        // Horizontal rule → blank
        if line
            .chars()
            .all(|c| c == '-' || c == '*' || c == '_' || c == ' ')
            && line.len() >= 3
        {
            out.push(String::new());
            continue;
        }
        // Heading
        if let Some(rest) = line
            .strip_prefix("### ")
            .or_else(|| line.strip_prefix("## "))
            .or_else(|| line.strip_prefix("# "))
        {
            out.push(format!("<b>{}</b>", inline_to_html(rest)));
            continue;
        }
        // Bullet
        if let Some(rest) = line
            .strip_prefix("- ")
            .or_else(|| line.strip_prefix("* "))
            .or_else(|| line.strip_prefix("• "))
        {
            out.push(format!("• {}", inline_to_html(rest)));
            continue;
        }
        out.push(inline_to_html(line));
    }
    if in_code && !code_buf.is_empty() {
        out.push(format!("<pre>{}</pre>", esc_html(&code_buf.join("\n"))));
    }

    // Collapse 3+ blank lines to 2
    let joined = out.join("\n");
    let mut collapsed = String::with_capacity(joined.len());
    let mut blank_count = 0;
    for line in joined.lines() {
        if line.trim().is_empty() {
            blank_count += 1;
            if blank_count <= 2 {
                collapsed.push('\n');
            }
        } else {
            blank_count = 0;
            collapsed.push_str(line);
            collapsed.push('\n');
        }
    }
    collapsed.trim().to_string()
}

fn esc_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn inline_to_html(text: &str) -> String {
    let mut out = String::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Inline code
        if bytes[i] == b'`' {
            if let Some(j) = text[i + 1..].find('`') {
                out.push_str(&format!(
                    "<code>{}</code>",
                    esc_html(&text[i + 1..i + 1 + j])
                ));
                i = i + 1 + j + 1;
                continue;
            }
        }
        // Bold **...**
        if bytes.get(i) == Some(&b'*') && bytes.get(i + 1) == Some(&b'*') {
            if let Some(j) = text[i + 2..].find("**") {
                out.push_str(&format!("<b>{}</b>", esc_html(&text[i + 2..i + 2 + j])));
                i = i + 2 + j + 2;
                continue;
            }
        }
        // Link [text](url)
        if bytes[i] == b'[' {
            if let Some(j) = text[i + 1..].find("](") {
                let link_text = &text[i + 1..i + 1 + j];
                let rest = &text[i + 1 + j + 2..];
                if let Some(k) = rest.find(')') {
                    let url = &rest[..k];
                    out.push_str(&format!("<a href=\"{}\">{}</a>", url, esc_html(link_text)));
                    i = i + 1 + j + 2 + k + 1;
                    continue;
                }
            }
        }
        // Default: escape and emit
        let c = text[i..].chars().next().unwrap_or(' ');
        out.push_str(&esc_html(&c.to_string()));
        i += c.len_utf8();
    }
    out
}

fn strip_html(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

#[derive(Clone)]
struct McpState {
    tools: Arc<Vec<Arc<dyn Tool>>>,
    provider: Option<Arc<dyn crate::providers::traits::Provider>>,
    model: Option<String>,
    runner: Option<Arc<AgentRunner>>,
}

#[derive(Debug, Deserialize)]
struct HttpChatRequest {
    text: String,
    chat_id: String,
    /// Optional Telegram bot token — enables live progress updates during execution
    telegram_token: Option<String>,
}

#[derive(Debug, Serialize)]
struct HttpChatResponse {
    text: String,
}

/// Run apollo as an MCP server over stdio.
pub async fn run_mcp_server(
    tools: Vec<Arc<dyn Tool>>,
    provider: Option<Arc<dyn crate::providers::traits::Provider>>,
    model: Option<String>,
) -> anyhow::Result<()> {
    let stdin = BufReader::new(tokio::io::stdin());
    let mut stdout = tokio::io::stdout();
    let mut lines = stdin.lines();

    let tools = Arc::new(tools);

    tracing::info!("MCP server started on stdio");

    while let Ok(Some(line)) = lines.next_line().await {
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }

        let request: JsonRpcRequest = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(_) => {
                let resp = JsonRpcResponse::error(None, -32700, "Parse error");
                write_response(&mut stdout, &resp).await?;
                continue;
            }
        };

        let response = handle_request(&request, &tools, &provider, &model).await;
        write_response(&mut stdout, &response).await?;
    }

    Ok(())
}

/// Run apollo as an MCP server over HTTP (for Cloudflare Container deployment).
async fn health_handler() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
        "service": "apollo",
        "timestamp": chrono::Utc::now().to_rfc3339(),
    }))
}

pub async fn run_mcp_server_http(
    tools: Vec<Arc<dyn Tool>>,
    provider: Option<Arc<dyn crate::providers::traits::Provider>>,
    model: Option<String>,
    runner: Option<Arc<AgentRunner>>,
    port: u16,
) -> anyhow::Result<()> {
    let state = McpState {
        tools: Arc::new(tools),
        provider,
        model,
        runner,
    };

    let app = Router::new()
        .route("/health", get(health_handler))
        .route("/chat", post(handle_http_chat))
        .route("/mcp", post(handle_http_mcp))
        .route("/", post(handle_http_mcp))
        .with_state(state);

    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], port));
    tracing::info!("MCP HTTP server listening on {}", addr);
    eprintln!("MCP HTTP server listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

async fn handle_http_chat(
    State(state): State<McpState>,
    Json(req): Json<HttpChatRequest>,
) -> (StatusCode, Json<HttpChatResponse>) {
    let Some(runner) = state.runner else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(HttpChatResponse {
                text: "Agent not initialized".to_string(),
            }),
        );
    };

    let msg = IncomingMessage {
        id: uuid::Uuid::new_v4().to_string(),
        sender_id: req.chat_id.clone(),
        sender_name: None,
        chat_id: req.chat_id.clone(),
        text: req.text,
        is_group: false,
        reply_to: None,
        timestamp: chrono::Utc::now(),
    };

    // Use TelegramHttpChannel for live progress if token provided, else plain HTTP channel
    let result = if let Some(token) = req.telegram_token {
        let channel = TelegramHttpChannel::new(token);
        runner.handle_message(&msg, &channel).await
    } else {
        runner.handle_message(&msg, &HttpChannel).await
    };

    match result {
        Ok(text) => (StatusCode::OK, Json(HttpChatResponse { text })),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(HttpChatResponse {
                text: format!("Error: {e}"),
            }),
        ),
    }
}

async fn handle_http_mcp(
    State(state): State<McpState>,
    Json(request): Json<JsonRpcRequest>,
) -> impl IntoResponse {
    let response = handle_request(&request, &state.tools, &state.provider, &state.model).await;
    Json(response)
}

async fn write_response(
    stdout: &mut tokio::io::Stdout,
    response: &JsonRpcResponse,
) -> anyhow::Result<()> {
    let json = serde_json::to_string(response)?;
    stdout.write_all(json.as_bytes()).await?;
    stdout.write_all(b"\n").await?;
    stdout.flush().await?;
    Ok(())
}

async fn handle_request(
    req: &JsonRpcRequest,
    tools: &[Arc<dyn Tool>],
    provider: &Option<Arc<dyn crate::providers::traits::Provider>>,
    model: &Option<String>,
) -> JsonRpcResponse {
    match req.method.as_str() {
        "initialize" => handle_initialize(req.id.clone()),
        "notifications/initialized" => JsonRpcResponse::success(req.id.clone(), Value::Null),
        "tools/list" => handle_tools_list(req.id.clone(), tools, provider.is_some()),
        "tools/call" => {
            handle_tools_call(req.id.clone(), req.params.as_ref(), tools, provider, model).await
        }
        "shutdown" => {
            tracing::info!("MCP server shutting down");
            JsonRpcResponse::success(req.id.clone(), Value::Null)
        }
        _ => JsonRpcResponse::error(req.id.clone(), -32601, "Method not found"),
    }
}

fn handle_initialize(id: Option<Value>) -> JsonRpcResponse {
    JsonRpcResponse::success(
        id,
        serde_json::json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {
                "tools": {}
            },
            "serverInfo": {
                "name": "apollo",
                "version": env!("CARGO_PKG_VERSION")
            }
        }),
    )
}

fn handle_tools_list(
    id: Option<Value>,
    tools: &[Arc<dyn Tool>],
    has_provider: bool,
) -> JsonRpcResponse {
    let mut mcp_tools: Vec<Value> = tools
        .iter()
        .map(|t| tool_to_mcp_schema(&t.spec()))
        .collect();

    // Add "ask" tool if we have a provider (allows other AIs to prompt apollo)
    if has_provider {
        mcp_tools.push(serde_json::json!({
            "name": "ask",
            "description": "Send a message to the apollo AI agent and get a response. Use this to prompt apollo for complex tasks like coding, research, or analysis.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "message": {
                        "type": "string",
                        "description": "The message/prompt to send to apollo"
                    }
                },
                "required": ["message"]
            }
        }));
    }

    JsonRpcResponse::success(id, serde_json::json!({ "tools": mcp_tools }))
}

async fn handle_tools_call(
    id: Option<Value>,
    params: Option<&Value>,
    tools: &[Arc<dyn Tool>],
    provider: &Option<Arc<dyn crate::providers::traits::Provider>>,
    model: &Option<String>,
) -> JsonRpcResponse {
    let params = match params {
        Some(p) => p,
        None => {
            return JsonRpcResponse::error(id, -32602, "Missing params");
        }
    };

    let name = match params.get("name").and_then(|n| n.as_str()) {
        Some(n) => n,
        None => {
            return JsonRpcResponse::error(id, -32602, "Missing tool name");
        }
    };

    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or(serde_json::json!({}));

    // Handle the "ask" meta-tool
    if name == "ask" {
        if let Some(provider) = provider {
            let message = arguments
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("");
            let model_name = model.as_deref().unwrap_or("claude-sonnet-4-5");

            match provider.simple_chat(message, model_name).await {
                Ok(response) => {
                    return JsonRpcResponse::success(
                        id,
                        serde_json::json!({
                            "content": [{"type": "text", "text": response}]
                        }),
                    );
                }
                Err(e) => {
                    return JsonRpcResponse::success(
                        id,
                        serde_json::json!({
                            "content": [{"type": "text", "text": format!("Error: {}", e)}],
                            "isError": true
                        }),
                    );
                }
            }
        } else {
            return JsonRpcResponse::error(id, -32602, "No provider configured for ask tool");
        }
    }

    // Find and execute the matching tool
    let tool = match tools.iter().find(|t| t.name() == name) {
        Some(t) => t,
        None => {
            return JsonRpcResponse::error(id, -32602, format!("Unknown tool: {}", name));
        }
    };

    let args_str = serde_json::to_string(&arguments).unwrap_or_default();
    match tool.execute(&args_str).await {
        Ok(result) => JsonRpcResponse::success(
            id,
            serde_json::json!({
                "content": [{"type": "text", "text": result.output}],
                "isError": result.is_error
            }),
        ),
        Err(e) => JsonRpcResponse::success(
            id,
            serde_json::json!({
                "content": [{"type": "text", "text": format!("Tool error: {}", e)}],
                "isError": true
            }),
        ),
    }
}
